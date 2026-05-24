mod config;
mod mail_eds_protocol;
mod model;
mod password;
mod prediction;
mod settings;
mod sources;

use crate::config::{ConfigStore, EmailBackendPreference, EmailConfig};
use crate::model::PowerOperation;
use crate::model::{
    Action, DesktopControlOperation, EntryBadge, QueryInput, ResultItem, SearchMode,
};
use crate::model::{PasswordOperation, WindowFocusTarget};
use crate::password::{
    Credential, TypeStep, default_login_steps, format_generated_pass_entry, generate_password,
    parse_credential, pass_insert_command, run_program_input, wl_copy_command,
    wtype_commands_for_steps, xclip_command, xdotool_commands_for_steps,
};
use crate::settings::open_config_panel;
use crate::sources::{
    SearchSnapshot, Sources, append_deferred_results, evolution_helper_command, focus_window,
    focused_window_target, no_results_item, pass_prediction_key, run_mail_helper_action,
    sort_and_limit_results,
};
use anyhow::{Context, Result};
use clap::Parser;
use gtk4::gdk;
use gtk4::gdk::prelude::*;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, Entry, EventControllerKey, Image, Label,
    ListBox, ListBoxRow, Orientation, Overlay, ScrolledWindow, SelectionMode, Spinner,
};
use gtk4_layer_shell::LayerShell;
use std::cell::Cell;
use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;
use std::time::Instant;

static APP_CONFIG: OnceLock<Arc<ConfigStore>> = OnceLock::new();

#[derive(Parser, Debug)]
#[command(name = "Luma")]
#[command(
    about = "Unified predictive desktop launcher for apps, windows, files, passwords, email, SSH, commands, web, and libqalculate"
)]
struct Cli {
    #[arg(long, value_enum)]
    mode: Option<SearchMode>,

    #[arg(long)]
    query: Option<String>,
}

#[cfg(test)]
const LAUNCHER_SURFACE_MARGIN_PX: i32 = 56;
const LAUNCHER_SHADOW_Y_OFFSET_PX: i32 = 18;
const LAUNCHER_SHADOW_BLUR_PX: i32 = 44;
const LAUNCHER_SURFACE_MARGIN_BOTTOM_PX: i32 =
    LAUNCHER_SHADOW_BLUR_PX + LAUNCHER_SHADOW_Y_OFFSET_PX + 8;
const AUTOTYPE_AFTER_CLOSE_DELAY_MS: u64 = 180;

#[derive(Clone, Debug)]
struct AddPasswordDraft {
    entry: String,
    username: Option<String>,
    url: Option<String>,
    step: AddPasswordStep,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AddPasswordStep {
    Username,
    Url,
}

const DEFERRED_SEARCH_IDLE_DELAY_MS: u64 = 180;

#[derive(Clone)]
struct SearchController {
    entry: Entry,
    spinner: Spinner,
    sources: Arc<Sources>,
    list: ListBox,
    scroller: ScrolledWindow,
    current_results: Rc<RefCell<Vec<ResultItem>>>,
    clipboard_url: Rc<RefCell<Option<String>>>,
    add_password_draft: Rc<RefCell<Option<AddPasswordDraft>>>,
    mode: SearchMode,
    state: Rc<RefCell<SearchAsyncState>>,
    update_tx: std::sync::mpsc::Sender<SearchUpdate>,
    update_rx: Rc<RefCell<std::sync::mpsc::Receiver<SearchUpdate>>>,
}

#[derive(Debug, Default)]
struct SearchAsyncState {
    generation: u64,
    pending_timeout: Option<glib::SourceId>,
}

#[derive(Debug)]
struct SearchUpdate {
    generation: u64,
    phase: SearchUpdatePhase,
    snapshot: SearchSnapshot,
    deferred_results: Vec<ResultItem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SearchUpdatePhase {
    Immediate,
    Deferred,
}

impl SearchController {
    fn new(
        entry: Entry,
        spinner: Spinner,
        sources: Arc<Sources>,
        list: ListBox,
        scroller: ScrolledWindow,
        current_results: Rc<RefCell<Vec<ResultItem>>>,
        clipboard_url: Rc<RefCell<Option<String>>>,
        add_password_draft: Rc<RefCell<Option<AddPasswordDraft>>>,
        mode: SearchMode,
    ) -> Self {
        let (update_tx, update_rx) = std::sync::mpsc::channel();
        Self {
            entry,
            spinner,
            sources,
            list,
            scroller,
            current_results,
            clipboard_url,
            add_password_draft,
            mode,
            state: Rc::new(RefCell::new(SearchAsyncState::default())),
            update_tx,
            update_rx: Rc::new(RefCell::new(update_rx)),
        }
    }

    fn start_update_poller(&self) {
        let controller = self.clone();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            controller.drain_updates();
            glib::ControlFlow::Continue
        });
    }

    fn refresh(&self) {
        if self.add_password_draft.borrow().is_some() {
            return;
        }

        let query = self.entry.text().to_string();
        let clipboard_url = self.clipboard_url.borrow().clone();
        let generation = self.bump_generation();
        set_background_processing(&self.spinner, true);

        let sources = self.sources.clone();
        let tx = self.update_tx.clone();
        let mode = self.mode;
        thread::spawn(move || {
            let snapshot = sources.search_snapshot(&query, mode, clipboard_url.as_deref());
            let _ = tx.send(SearchUpdate {
                generation,
                phase: SearchUpdatePhase::Immediate,
                snapshot,
                deferred_results: Vec::new(),
            });
        });
    }

    fn drain_updates(&self) {
        loop {
            let update = { self.update_rx.borrow_mut().try_recv() };

            match update {
                Ok(update) => self.apply_deferred_results(update),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }

    fn bump_generation(&self) -> u64 {
        let mut state = self.state.borrow_mut();
        state.generation = state.generation.saturating_add(1);
        let _ = state.pending_timeout.take();
        state.generation
    }

    fn schedule_deferred(&self, snapshot: SearchSnapshot, generation: u64) {
        let controller = self.clone();
        let snapshot_for_timeout = snapshot.clone();
        let source_id = glib::timeout_add_local_once(
            Duration::from_millis(DEFERRED_SEARCH_IDLE_DELAY_MS),
            move || {
                if controller.state.borrow().generation != generation {
                    return;
                }

                controller.state.borrow_mut().pending_timeout = None;

                let sources = controller.sources.clone();
                let snapshot = snapshot_for_timeout.clone();
                let tx = controller.update_tx.clone();
                thread::spawn(move || {
                    let deferred_results = sources.search_deferred_results(&snapshot);
                    let _ = tx.send(SearchUpdate {
                        generation,
                        phase: SearchUpdatePhase::Deferred,
                        snapshot,
                        deferred_results,
                    });
                });
            },
        );
        self.state.borrow_mut().pending_timeout = Some(source_id);
    }

    fn apply_deferred_results(&self, update: SearchUpdate) {
        let SearchUpdate {
            generation,
            phase,
            snapshot,
            deferred_results,
        } = update;

        if self.state.borrow().generation != generation {
            return;
        }

        set_background_processing(
            &self.spinner,
            background_processing_after_update(phase, !snapshot.deferred.is_empty()),
        );

        if phase == SearchUpdatePhase::Immediate {
            let results = snapshot.immediate_results.clone();
            if snapshot.deferred.is_empty() {
                let results = finalize_loaded_results(results, &snapshot.query);
                self.render_results(results);
            } else {
                let results = pending_deferred_results(results);
                self.render_results(results);
                self.schedule_deferred(snapshot, generation);
            }
            return;
        }

        let mut results =
            append_deferred_results(snapshot.immediate_results.clone(), deferred_results);
        if results.is_empty() {
            results.push(no_results_item(&snapshot.query));
        }
        self.render_results(results);
    }

    fn render_results(&self, results: Vec<ResultItem>) {
        // Keep the user's arrow-key selection anchored to its item when a rebuild
        // is triggered by deferred results arriving, rather than snapping to row 0.
        let previous_selection = self
            .list
            .selected_row()
            .map(|row| row.index())
            .filter(|index| *index >= 0)
            .and_then(|index| self.current_results.borrow().get(index as usize).cloned());
        rebuild_results(
            &self.list,
            &self.scroller,
            &results,
            previous_selection.as_ref(),
        );
        self.current_results.replace(results);
    }
}

fn layer_shell_enabled(display_is_wayland: bool, protocol_supported: bool) -> bool {
    display_is_wayland && protocol_supported
}

fn layer_shell_supported() -> bool {
    let display_is_wayland =
        gdk::Display::default().is_some_and(|display| display.backend().is_wayland());
    if !display_is_wayland {
        return false;
    }

    layer_shell_enabled(display_is_wayland, gtk4_layer_shell::is_supported())
}

pub(crate) fn app_config() -> Option<Arc<ConfigStore>> {
    APP_CONFIG.get().cloned()
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error:?}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cli = Cli::parse_from(&args);
    configure_gdk_backend();
    let config = Arc::new(ConfigStore::load());
    let _ = APP_CONFIG.set(config.clone());
    let sources = Arc::new(Sources::load(config.clone()));
    let mode = cli.mode.unwrap_or_else(|| config.current().default_mode);
    let application = Application::builder()
        .application_id("me.robindecker.Luma")
        .build();
    application.add_main_option(
        "mode",
        glib::Char::from(b'\0'),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "Launcher mode",
        Some("MODE"),
    );
    application.add_main_option(
        "query",
        glib::Char::from(b'\0'),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "Initial query",
        Some("QUERY"),
    );

    application.connect_activate(move |app| {
        build_ui(app, mode, cli.query.clone(), sources.clone());
    });

    application.run_with_args(&args);
    Ok(())
}

fn configure_gdk_backend() {
    if std::env::var_os("GDK_BACKEND").is_some() {
        return;
    }

    if let Some(backend) = gdk_backend_for_session(
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
        std::env::var_os("DISPLAY").is_some(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
    ) {
        // GTK reads this very early during application startup. We set it
        // before any GTK objects are constructed so the runtime picks the
        // compositor that matches the current session instead of guessing from
        // stray mixed-session environment variables.
        unsafe {
            std::env::set_var("GDK_BACKEND", backend);
        }
    }
}

fn gdk_backend_for_session(
    session_type: Option<&str>,
    display_set: bool,
    wayland_display_set: bool,
) -> Option<&'static str> {
    match session_type.map(|value| value.to_ascii_lowercase()) {
        Some(session) if session == "x11" => display_set.then_some("x11"),
        Some(session) if session == "wayland" => wayland_display_set.then_some("wayland"),
        _ => {
            if wayland_display_set && !display_set {
                Some("wayland")
            } else if display_set {
                Some("x11")
            } else {
                None
            }
        }
    }
}

fn build_ui(
    app: &Application,
    mode: SearchMode,
    initial_query: Option<String>,
    sources: Arc<Sources>,
) {
    let config = app_config()
        .map(|store| store.current())
        .unwrap_or_default();
    let window = ApplicationWindow::builder()
        .application(app)
        .default_width(config.ui.width_px)
        .default_height(config.ui.height_px)
        .decorated(false)
        .resizable(false)
        .title("Luma")
        .build();

    if config.ui.use_layer_shell && layer_shell_supported() {
        window.init_layer_shell();
        window.set_layer(gtk4_layer_shell::Layer::Overlay);
        // The launcher should behave like a modal overlay. On-demand focus is
        // compositor-defined and can leave the entry without a working key grab.
        window.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::Exclusive);
        window.set_namespace(Some("Luma"));
        window.set_anchor(gtk4_layer_shell::Edge::Top, true);
        window.set_margin(gtk4_layer_shell::Edge::Top, config.ui.top_margin_px);
    }

    apply_css();
    let previous_focus_target = Rc::new(focused_window_target());

    let outer = GtkBox::new(Orientation::Vertical, 10);
    outer.add_css_class("launcher-shell");
    outer.set_halign(Align::Center);
    outer.set_size_request(config.ui.width_px, -1);
    outer.set_margin_top(config.ui.surface_margin_px);
    outer.set_margin_bottom(LAUNCHER_SURFACE_MARGIN_BOTTOM_PX);
    outer.set_margin_start(config.ui.surface_margin_px);
    outer.set_margin_end(config.ui.surface_margin_px);

    let entry = Entry::builder()
        .placeholder_text(placeholder_for_mode(mode))
        .build();
    entry.add_css_class("launcher-entry");
    entry.set_icon_from_icon_name(
        gtk4::EntryIconPosition::Primary,
        Some("system-search-symbolic"),
    );
    if let Some(query) = initial_query.as_deref() {
        entry.set_text(query);
    }

    let entry_overlay = Overlay::new();
    entry_overlay.set_child(Some(&entry));
    let search_spinner = Spinner::builder()
        .halign(Align::End)
        .valign(Align::Center)
        .margin_end(14)
        .build();
    search_spinner.add_css_class("launcher-search-spinner");
    search_spinner.set_visible(false);
    search_spinner.set_tooltip_text(Some("Background search is still running"));
    entry_overlay.add_overlay(&search_spinner);

    let list = ListBox::new();
    list.set_selection_mode(SelectionMode::Single);
    list.add_css_class("launcher-results");

    let scroller = ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .min_content_height(300)
        .child(&list)
        .build();
    scroller.add_css_class("launcher-scroller");

    outer.append(&entry_overlay);
    outer.append(&scroller);
    window.set_child(Some(&outer));

    let current_results = Rc::new(RefCell::new(Vec::<ResultItem>::new()));
    let add_password_draft = Rc::new(RefCell::new(None::<AddPasswordDraft>));
    let clipboard_url = Rc::new(RefCell::new(None::<String>));
    let search = SearchController::new(
        entry.clone(),
        search_spinner.clone(),
        sources.clone(),
        list.clone(),
        scroller.clone(),
        current_results.clone(),
        clipboard_url.clone(),
        add_password_draft.clone(),
        mode,
    );
    search.start_update_poller();

    if profiling_enabled() {
        install_frame_profiler(&window);
    }

    {
        let search = search.clone();
        let add_password_draft = add_password_draft.clone();
        entry.connect_changed(move |_| {
            if add_password_draft.borrow().is_some() {
                return;
            }

            search.refresh();
        });
    }

    {
        let list = list.clone();
        let status_list = list.clone();
        let scroller = scroller.clone();
        let window = window.clone();
        let sources = sources.clone();
        let entry = entry.clone();
        let add_password_draft = add_password_draft.clone();
        let current_results = current_results.clone();
        let previous_focus_target = previous_focus_target.clone();
        list.connect_row_activated(move |_, row| {
            let item = {
                let results = current_results.borrow();
                results.get(row.index() as usize).cloned()
            };
            if let Some(item) = item {
                activate_item(
                    &window,
                    &sources,
                    item,
                    &status_list,
                    &scroller,
                    &current_results,
                    &entry,
                    &add_password_draft,
                    previous_focus_target.as_ref().as_ref(),
                );
            }
        });
    }

    {
        let list = list.clone();
        let status_list = list.clone();
        let scroller = scroller.clone();
        let window = window.clone();
        let sources = sources.clone();
        let activate_entry = entry.clone();
        let current_results = current_results.clone();
        let previous_focus_target = previous_focus_target.clone();
        let add_password_draft = add_password_draft.clone();
        entry.connect_activate(move |_| {
            if add_password_draft.borrow().is_some() {
                advance_add_password_flow(
                    &activate_entry,
                    &sources,
                    &status_list,
                    &scroller,
                    &current_results,
                    &add_password_draft,
                    mode,
                );
                return;
            }

            let query = activate_entry.text().to_string();
            let selected = {
                let results = current_results.borrow();
                list.selected_row()
                    .and_then(|row| results.get(row.index() as usize).cloned())
                    .or_else(|| {
                        if query.is_empty() {
                            None
                        } else {
                            results.first().cloned()
                        }
                    })
            };

            if let Some(item) = selected {
                activate_item(
                    &window,
                    &sources,
                    item,
                    &status_list,
                    &scroller,
                    &current_results,
                    &activate_entry,
                    &add_password_draft,
                    previous_focus_target.as_ref().as_ref(),
                );
            }
        });
    }

    {
        let entry = entry.clone();
        let list = list.clone();
        let scroller = scroller.clone();
        let current_results = current_results.clone();
        let keys = EventControllerKey::new();
        let key_window = window.clone();
        keys.connect_key_pressed(move |_, key, _, _| match key {
            gdk::Key::Escape => {
                key_window.close();
                glib::Propagation::Stop
            }
            gdk::Key::Down => {
                let result_count = current_results.borrow().len() as i32;
                move_selection(&list, &scroller, 1, result_count);
                entry.grab_focus();
                glib::Propagation::Stop
            }
            gdk::Key::Up => {
                let result_count = current_results.borrow().len() as i32;
                move_selection(&list, &scroller, -1, result_count);
                entry.grab_focus();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        window.add_controller(keys);
    }

    {
        let focus_armed = Rc::new(Cell::new(false));

        {
            let focus_armed = focus_armed.clone();
            let window = window.clone();
            entry.connect_has_focus_notify(move |entry| {
                if entry.has_focus() {
                    focus_armed.set(true);
                } else if focus_armed.get() && window.is_visible() {
                    window.close();
                }
            });
        }

        {
            let focus_armed = focus_armed.clone();
            window.connect_is_active_notify(move |window| {
                if focus_armed.get() && !window.is_active() && window.is_visible() {
                    window.close();
                }
            });
        }
    }

    search.refresh();
    window.present();
    request_initial_focus(&window, &entry);
    schedule_clipboard_url_loads(&search);
}

fn schedule_clipboard_url_loads(search: &SearchController) {
    for delay_ms in [80_u64, 220, 500] {
        let search = search.clone();
        glib::timeout_add_local_once(Duration::from_millis(delay_ms), move || {
            if search.clipboard_url.borrow().is_some() {
                return;
            }
            load_clipboard_url(&search);
        });
    }
}

fn load_clipboard_url(search: &SearchController) {
    let Some(display) = gdk::Display::default() else {
        return;
    };

    let search = search.clone();
    display
        .clipboard()
        .read_text_async(None::<&gio::Cancellable>, move |result| {
            let Ok(Some(text)) = result else {
                return;
            };
            let text = text.trim();
            if text.is_empty() {
                return;
            }

            search.clipboard_url.replace(Some(text.to_string()));
            if search.add_password_draft.borrow().is_none() {
                search.refresh();
            }
        });
}

fn request_initial_focus(window: &ApplicationWindow, entry: &Entry) {
    for delay_ms in [0_u64, 25, 100, 250] {
        let window = window.clone();
        let entry = entry.clone();
        glib::timeout_add_local_once(Duration::from_millis(delay_ms), move || {
            if !window.is_visible() || entry.has_focus() {
                return;
            }

            window.present();
            entry.grab_focus_without_selecting();
        });
    }
}

fn profiling_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("LUMA_PROFILE").is_some())
}

fn ms(value: Duration) -> f64 {
    value.as_secs_f64() * 1000.0
}

thread_local! {
    static ICON_LOOKUP_TIME: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    // GLib monotonic-clock timestamps (microseconds) so the frame profiler can
    // line each rebuild up against the frame the compositor actually presents.
    static LAST_REBUILD_DONE_US: Cell<i64> = const { Cell::new(0) };
    static LAST_FRAME_TIME_US: Cell<i64> = const { Cell::new(0) };
}

// Logs how long the compositor takes to turn a finished rebuild into a
// presented frame, isolating window-manager/present latency from the
// main-thread rebuild cost measured in `rebuild_results`.
fn install_frame_profiler(window: &ApplicationWindow) {
    let id = window.add_tick_callback(|_widget, clock| {
        let frame_time = clock.frame_time();
        let previous = LAST_FRAME_TIME_US.replace(frame_time);
        let rebuild_us = LAST_REBUILD_DONE_US.replace(0);
        if rebuild_us != 0 {
            let to_frame = (frame_time - rebuild_us) as f64 / 1000.0;
            let interval = if previous != 0 {
                (frame_time - previous) as f64 / 1000.0
            } else {
                f64::NAN
            };
            let present = clock
                .current_timings()
                .map(|timings| timings.presentation_time())
                .filter(|value| *value != 0)
                .map(|value| (value - frame_time) as f64 / 1000.0);
            match present {
                Some(present) => eprintln!(
                    "[luma-profile] present  rebuild->frame={to_frame:>6.2}ms  frame_interval={interval:>6.2}ms  present_latency={present:>6.2}ms"
                ),
                None => eprintln!(
                    "[luma-profile] present  rebuild->frame={to_frame:>6.2}ms  frame_interval={interval:>6.2}ms  present_latency=unavailable(x11)"
                ),
            }
        }
        glib::ControlFlow::Continue
    });
    // Keep the callback alive for the (short) process lifetime regardless of
    // whether TickCallbackId behaves as an RAII guard.
    std::mem::forget(id);
}

// Two result rows refer to the same logical entry when their source and visible
// text match. Used to keep a row selected across rebuilds (e.g. when deferred
// search results arrive) instead of snapping back to the top.
fn same_result(left: &ResultItem, right: &ResultItem) -> bool {
    left.source == right.source && left.title == right.title && left.subtitle == right.subtitle
}

fn preserved_selection_index(previous: Option<&ResultItem>, results: &[ResultItem]) -> usize {
    previous
        .and_then(|prev| results.iter().position(|item| same_result(item, prev)))
        .unwrap_or(0)
}

fn rebuild_results(
    list: &ListBox,
    scroller: &ScrolledWindow,
    results: &[ResultItem],
    previous_selection: Option<&ResultItem>,
) {
    let profile = profiling_enabled();
    if profile {
        ICON_LOOKUP_TIME.with(|cell| cell.set(Duration::ZERO));
    }
    let total_start = Instant::now();

    let teardown_start = Instant::now();
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let teardown = teardown_start.elapsed();

    let build_start = Instant::now();
    for item in results {
        let row = build_row(item);
        list.append(&row);
    }
    let build = build_start.elapsed();

    let selected = preserved_selection_index(previous_selection, results) as i32;
    if let Some(row) = list.row_at_index(selected) {
        list.select_row(Some(&row));
        scroll_row_into_view(list, scroller, &row);
    }

    if profile {
        let icon = ICON_LOOKUP_TIME.with(Cell::get);
        let total = total_start.elapsed();
        LAST_REBUILD_DONE_US.with(|cell| cell.set(glib::monotonic_time()));
        eprintln!(
            "[luma-profile] rebuild  rows={:>2}  total={:>6.2}ms  teardown={:>6.2}ms  build={:>6.2}ms  icon_lookup={:>6.2}ms",
            results.len(),
            ms(total),
            ms(teardown),
            ms(build),
            ms(icon),
        );
    }
}

fn finalize_loaded_results(results: Vec<ResultItem>, query: &QueryInput) -> Vec<ResultItem> {
    let mut results = sort_and_limit_results(results);
    if results.is_empty() {
        results.push(no_results_item(query));
    }
    results
}

fn pending_deferred_results(results: Vec<ResultItem>) -> Vec<ResultItem> {
    sort_and_limit_results(results)
}

fn background_processing_after_update(phase: SearchUpdatePhase, has_deferred_plan: bool) -> bool {
    phase == SearchUpdatePhase::Immediate && has_deferred_plan
}

fn set_background_processing(spinner: &Spinner, active: bool) {
    spinner.set_visible(active);
    if active {
        spinner.start();
    } else {
        spinner.stop();
    }
}

fn build_row(item: &ResultItem) -> ListBoxRow {
    let row = ListBoxRow::new();
    row.add_css_class("launcher-row");
    if matches!(&item.action, Action::None) {
        row.add_css_class("launcher-row-status");
    }

    let layout = GtkBox::new(Orientation::Horizontal, 14);
    layout.set_margin_top(8);
    layout.set_margin_bottom(8);
    layout.set_margin_start(10);
    layout.set_margin_end(10);

    let icon = if profiling_enabled() {
        let start = Instant::now();
        let icon = Image::from_icon_name(&item.icon_name);
        ICON_LOOKUP_TIME.with(|cell| cell.set(cell.get() + start.elapsed()));
        icon
    } else {
        Image::from_icon_name(&item.icon_name)
    };
    icon.set_pixel_size(24);
    icon.add_css_class("launcher-icon");
    icon.set_halign(Align::Center);
    icon.set_valign(Align::Center);

    let icon_wrap = GtkBox::new(Orientation::Vertical, 0);
    icon_wrap.add_css_class("launcher-icon-wrap");
    icon_wrap.set_valign(Align::Center);
    icon_wrap.set_halign(Align::Center);
    icon_wrap.append(&icon);

    let text_col = GtkBox::new(Orientation::Vertical, 2);
    text_col.set_hexpand(true);
    text_col.set_valign(Align::Center);

    let title_row = GtkBox::new(Orientation::Horizontal, 6);

    let title = Label::new(Some(&item.title));
    title.add_css_class("launcher-title");
    title.set_halign(Align::Start);
    title.set_hexpand(true);
    title.set_xalign(0.0);
    title.set_wrap(false);
    title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    title_row.append(&title);

    for badge in &item.badges {
        title_row.append(&badge_widget(*badge));
    }

    if let Some(accessory) = item
        .accessory
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let accessory_label = Label::new(Some(accessory));
        accessory_label.add_css_class("launcher-accessory");
        accessory_label.set_halign(Align::End);
        accessory_label.set_valign(Align::Center);
        accessory_label.set_wrap(false);
        title_row.append(&accessory_label);
    }

    text_col.append(&title_row);

    let subtitle = item.subtitle.trim();
    if !subtitle.is_empty() {
        let subtitle_label = Label::new(Some(subtitle));
        subtitle_label.add_css_class("launcher-subtitle");
        subtitle_label.set_halign(Align::Start);
        subtitle_label.set_hexpand(true);
        subtitle_label.set_xalign(0.0);
        subtitle_label.set_wrap(false);
        subtitle_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        text_col.append(&subtitle_label);
    }

    if let Some(tooltip) = row_tooltip_text(item) {
        row.set_tooltip_text(Some(&tooltip));
    }

    layout.append(&icon_wrap);
    layout.append(&text_col);
    row.set_child(Some(&layout));
    row
}

fn badge_widget(badge: EntryBadge) -> Image {
    let icon_name = match badge {
        EntryBadge::Unread => "mail-unread-symbolic",
        EntryBadge::Attachment => "mail-attachment-symbolic",
    };
    let image = Image::from_icon_name(icon_name);
    image.set_pixel_size(14);
    image.set_valign(Align::Center);
    image.add_css_class("launcher-badge");
    image.add_css_class(match badge {
        EntryBadge::Unread => "launcher-badge-unread",
        EntryBadge::Attachment => "launcher-badge-attachment",
    });
    image
}

fn row_tooltip_text(item: &ResultItem) -> Option<String> {
    let subtitle = item.subtitle.trim();
    let source = item.source.trim();

    match (subtitle.is_empty(), source.is_empty()) {
        (true, true) => None,
        (false, true) => Some(subtitle.to_string()),
        (true, false) => Some(source.to_string()),
        (false, false) => Some(format!("{subtitle}\n{source}")),
    }
}

fn move_selection(list: &ListBox, scroller: &ScrolledWindow, delta: i32, result_count: i32) {
    if result_count <= 0 {
        return;
    }

    let current = list.selected_row().map(|row| row.index()).unwrap_or(0);
    let next = (current + delta).clamp(0, result_count - 1);
    if let Some(row) = list.row_at_index(next) {
        list.select_row(Some(&row));
        scroll_row_into_view(list, scroller, &row);
    }
}

fn scroll_row_into_view(list: &ListBox, scroller: &ScrolledWindow, row: &ListBoxRow) {
    let adjustment = scroller.vadjustment();
    let visible_top = adjustment.value();
    let visible_bottom = visible_top + adjustment.page_size();
    let Some(bounds) = row.compute_bounds(list) else {
        return;
    };
    let row_top = f64::from(bounds.y());
    let row_bottom = row_top + f64::from(bounds.height());

    let next_value = if row_top < visible_top {
        row_top
    } else if row_bottom > visible_bottom {
        row_bottom - adjustment.page_size()
    } else {
        return;
    };

    let max_value = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
    adjustment.set_value(next_value.clamp(adjustment.lower(), max_value));
}

fn activate_item(
    window: &ApplicationWindow,
    sources: &Sources,
    item: ResultItem,
    list: &ListBox,
    scroller: &ScrolledWindow,
    current_results: &Rc<RefCell<Vec<ResultItem>>>,
    entry_widget: &Entry,
    add_password_draft: &Rc<RefCell<Option<AddPasswordDraft>>>,
    previous_focus_target: Option<&WindowFocusTarget>,
) {
    if let Action::Power {
        operation,
        confirmed: false,
    } = item.action
    {
        if power_requires_confirmation(operation) {
            let results = power_confirmation_results(operation);
            rebuild_results(list, scroller, &results, None);
            current_results.replace(results);
            return;
        }
    }

    if let Action::Password {
        entry,
        operation: PasswordOperation::Inspect,
    } = &item.action
    {
        match load_pass_credential(entry) {
            Ok(credential) => {
                let results = inspected_password_results(&credential);
                rebuild_results(list, scroller, &results, None);
                current_results.replace(results);
            }
            Err(error) => show_status_result(
                list,
                scroller,
                current_results,
                action_failure_result(&error.root_cause().to_string()),
            ),
        }
        return;
    }

    if let Action::PasswordActions { entry } = &item.action {
        match load_pass_credential(entry) {
            Ok(credential) => {
                let results = inspected_password_results(&credential);
                rebuild_results(list, scroller, &results, None);
                current_results.replace(results);
            }
            Err(error) => show_status_result(
                list,
                scroller,
                current_results,
                action_failure_result(&error.root_cause().to_string()),
            ),
        }
        return;
    }

    if let Action::AddPassword { entry, url } = &item.action {
        start_add_password_flow(
            entry_widget,
            list,
            scroller,
            current_results,
            add_password_draft,
            entry,
            url.clone(),
        );
        return;
    }

    if let Err(error) = execute_action(window, item.action.clone(), previous_focus_target) {
        show_status_result(
            list,
            scroller,
            current_results,
            action_failure_result(&error.root_cause().to_string()),
        );
    } else {
        sources.record_activation(&item);
    }
}

fn start_add_password_flow(
    entry_widget: &Entry,
    list: &ListBox,
    scroller: &ScrolledWindow,
    current_results: &Rc<RefCell<Vec<ResultItem>>>,
    add_password_draft: &Rc<RefCell<Option<AddPasswordDraft>>>,
    new_entry: &str,
    prefilled_url: Option<String>,
) {
    let draft = AddPasswordDraft {
        entry: new_entry.trim().to_string(),
        username: None,
        url: prefilled_url,
        step: AddPasswordStep::Username,
    };
    add_password_draft.replace(Some(draft.clone()));
    entry_widget.set_placeholder_text(Some("Optional username or email"));
    entry_widget.set_text("");
    entry_widget.grab_focus();
    show_status_result(
        list,
        scroller,
        current_results,
        add_password_prompt_result(
            &format!("Username/email for {}", draft.entry),
            "Leave blank and press Enter to use the entry basename",
        ),
    );
}

fn advance_add_password_flow(
    entry_widget: &Entry,
    sources: &Sources,
    list: &ListBox,
    scroller: &ScrolledWindow,
    current_results: &Rc<RefCell<Vec<ResultItem>>>,
    add_password_draft: &Rc<RefCell<Option<AddPasswordDraft>>>,
    mode: SearchMode,
) {
    let input = entry_widget.text().trim().to_string();
    let mut draft_ref = add_password_draft.borrow_mut();
    let Some(draft) = draft_ref.as_mut() else {
        return;
    };

    match draft.step {
        AddPasswordStep::Username => {
            draft.username = non_empty_string(&input);
            if draft.url.is_some() {
                let draft = draft.clone();
                drop(draft_ref);
                finish_add_password_flow(
                    entry_widget,
                    sources,
                    list,
                    scroller,
                    current_results,
                    add_password_draft,
                    mode,
                    draft,
                );
                return;
            }

            draft.step = AddPasswordStep::Url;
            let title = format!("URL for {}", draft.entry);
            drop(draft_ref);
            entry_widget.set_placeholder_text(Some("Optional URL"));
            entry_widget.set_text("");
            show_status_result(
                list,
                scroller,
                current_results,
                add_password_prompt_result(
                    &title,
                    "Leave blank and press Enter to save without a URL",
                ),
            );
        }
        AddPasswordStep::Url => {
            draft.url = non_empty_string(&input);
            let draft = draft.clone();
            drop(draft_ref);
            finish_add_password_flow(
                entry_widget,
                sources,
                list,
                scroller,
                current_results,
                add_password_draft,
                mode,
                draft,
            );
        }
    }
}

fn finish_add_password_flow(
    entry_widget: &Entry,
    sources: &Sources,
    list: &ListBox,
    scroller: &ScrolledWindow,
    current_results: &Rc<RefCell<Vec<ResultItem>>>,
    add_password_draft: &Rc<RefCell<Option<AddPasswordDraft>>>,
    mode: SearchMode,
    draft: AddPasswordDraft,
) {
    add_password_draft.replace(None);
    entry_widget.set_placeholder_text(Some(placeholder_for_mode(mode)));
    entry_widget.set_text("");

    match create_generated_password_entry(sources, &draft) {
        Ok(credential) => {
            let results = inspected_password_results(&credential);
            rebuild_results(list, scroller, &results, None);
            current_results.replace(results);
        }
        Err(error) => show_status_result(
            list,
            scroller,
            current_results,
            action_failure_result(&error.root_cause().to_string()),
        ),
    }
}

fn create_generated_password_entry(
    sources: &Sources,
    draft: &AddPasswordDraft,
) -> Result<Credential> {
    validate_new_password_entry(&draft.entry)?;
    if sources.password_entry_exists(&draft.entry) || pass_entry_exists_on_disk(&draft.entry)? {
        anyhow::bail!("password entry already exists");
    }

    let password = generate_password()?;
    let content =
        format_generated_pass_entry(&password, draft.username.as_deref(), draft.url.as_deref());
    run_program_input(pass_insert_command(&draft.entry, &content))?;
    sources.refresh_pass_entries();
    parse_credential(&draft.entry, &content)
}

fn validate_new_password_entry(entry: &str) -> Result<()> {
    if entry.trim().is_empty() {
        anyhow::bail!("password entry name cannot be empty");
    }
    if entry.starts_with('/') {
        anyhow::bail!("password entry name must be relative");
    }
    if entry
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        anyhow::bail!("password entry name contains an invalid path component");
    }
    Ok(())
}

fn pass_entry_exists_on_disk(entry: &str) -> Result<bool> {
    validate_new_password_entry(entry)?;
    let Some(store_dir) = password_store_dir() else {
        return Ok(false);
    };
    Ok(store_dir.join(format!("{entry}.gpg")).exists())
}

fn password_store_dir() -> Option<std::path::PathBuf> {
    app_config()
        .and_then(|config| {
            config
                .current()
                .integrations
                .password_store_dir
                .and_then(|value| {
                    let trimmed = value.trim().to_string();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(std::path::PathBuf::from(trimmed))
                    }
                })
        })
        .or_else(|| std::env::var_os("PASSWORD_STORE_DIR").map(std::path::PathBuf::from))
        .or_else(|| dirs::home_dir().map(|home| home.join(".password-store")))
}

fn non_empty_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn add_password_prompt_result(title: &str, subtitle: &str) -> ResultItem {
    ResultItem {
        prediction_key: None,
        title: title.to_string(),
        subtitle: subtitle.to_string(),
        source: "Passwords",
        icon_name: "dialog-password-symbolic".to_string(),
        score: 0,
        action: Action::None,
        ..Default::default()
    }
}

fn show_status_result(
    list: &ListBox,
    scroller: &ScrolledWindow,
    current_results: &Rc<RefCell<Vec<ResultItem>>>,
    item: ResultItem,
) {
    let results = vec![item];
    rebuild_results(list, scroller, &results, None);
    current_results.replace(results);
}

fn action_failure_result(message: &str) -> ResultItem {
    ResultItem {
        prediction_key: None,
        title: format!("Action failed: {message}"),
        subtitle: String::new(),
        source: "Status",
        icon_name: "dialog-error-symbolic".to_string(),
        score: 0,
        action: Action::None,
        ..Default::default()
    }
}

fn execute_action(
    window: &ApplicationWindow,
    action: Action,
    previous_focus_target: Option<&WindowFocusTarget>,
) -> Result<()> {
    match action {
        Action::LaunchApp { desktop_id } => launch_desktop_app(&desktop_id)?,
        Action::FocusWindow { target } => {
            let status = focus_window(&target).context("failed to focus selected window")?;
            if !status.success() {
                anyhow::bail!("window focus command failed");
            }
        }
        Action::OpenFile { path } => {
            let file = gio::File::for_path(path);
            gio::AppInfo::launch_default_for_uri(&file.uri(), gio::AppLaunchContext::NONE)
                .context("failed to open file")?;
        }
        Action::Ssh { host } => launch_ssh(&host)?,
        Action::OpenConfigPanel => open_config_panel(window)?,
        Action::CopyPass { entry } => {
            let secret = load_pass_secret(&entry)?;
            copy_secret(&secret)?;
            window.close();
            return Ok(());
        }
        Action::Password { entry, operation } => {
            execute_password_operation(window, &entry, operation, previous_focus_target)?;
            return Ok(());
        }
        Action::PasswordActions { .. } => {
            anyhow::bail!("password action menu must open from the launcher UI");
        }
        Action::AddPassword { .. } => {
            anyhow::bail!("password creation must start from the launcher UI");
        }
        Action::RunCommand { command } => {
            Command::new("sh")
                .args(["-lc", &command])
                .spawn()
                .context("failed to spawn command")?;
        }
        Action::Power { operation, .. } => {
            execute_power_operation(operation)?;
        }
        Action::DesktopControl { operation } => {
            execute_desktop_control_operation(&operation)?;
        }
        Action::WebSearch { query } => {
            let base = web_search_url();
            let url = format!("{base}{}", urlencoding::encode(&query));
            gio::AppInfo::launch_default_for_uri(&url, gio::AppLaunchContext::NONE)
                .context("failed to open search URL")?;
        }
        Action::OpenUrl { url } => {
            open_mail_or_url(&url)?;
        }
        Action::CopyText { text } => {
            copy_to_clipboard(&text);
        }
        Action::None => return Ok(()),
    }

    window.close();
    Ok(())
}

fn power_confirmation_results(operation: PowerOperation) -> Vec<ResultItem> {
    vec![
        ResultItem {
            prediction_key: None,
            title: format!("Confirm {}", power_operation_title(operation)),
            subtitle: power_operation_confirmation(operation).to_string(),
            source: "Power",
            icon_name: power_operation_icon(operation).to_string(),
            score: 100,
            action: Action::Power {
                operation,
                confirmed: true,
            },
            ..Default::default()
        },
        ResultItem {
            prediction_key: None,
            title: "Cancel".to_string(),
            subtitle: "Keep the current session untouched".to_string(),
            source: "Power",
            icon_name: "process-stop-symbolic".to_string(),
            score: 90,
            action: Action::None,
            ..Default::default()
        },
    ]
}

fn power_requires_confirmation(operation: PowerOperation) -> bool {
    !matches!(operation, PowerOperation::Lock)
}

fn power_operation_title(operation: PowerOperation) -> &'static str {
    match operation {
        PowerOperation::Lock => "Lock",
        PowerOperation::Suspend => "Suspend",
        PowerOperation::Logout => "Logout",
        PowerOperation::Reboot => "Reboot",
        PowerOperation::Shutdown => "Shutdown",
    }
}

fn power_operation_confirmation(operation: PowerOperation) -> &'static str {
    match operation {
        PowerOperation::Lock => "Blank the screen and keep the session running",
        PowerOperation::Suspend => "Lock first, then suspend the machine",
        PowerOperation::Logout => "Close the current desktop session now",
        PowerOperation::Reboot => "Restart the system now",
        PowerOperation::Shutdown => "Power off the system now",
    }
}

fn power_operation_icon(operation: PowerOperation) -> &'static str {
    match operation {
        PowerOperation::Lock => "system-lock-screen-symbolic",
        PowerOperation::Suspend => "media-playback-pause-symbolic",
        PowerOperation::Logout => "system-log-out-symbolic",
        PowerOperation::Reboot => "system-reboot-symbolic",
        PowerOperation::Shutdown => "system-shutdown-symbolic",
    }
}

fn execute_power_operation(operation: PowerOperation) -> Result<()> {
    match operation {
        PowerOperation::Lock => lock_session(),
        PowerOperation::Suspend => {
            let _ = lock_session();
            thread::sleep(Duration::from_secs(1));
            spawn_system_command("systemctl", &["suspend"], "failed to suspend")
        }
        PowerOperation::Logout => logout_session(),
        PowerOperation::Reboot => {
            spawn_system_command("systemctl", &["reboot"], "failed to reboot")
        }
        PowerOperation::Shutdown => {
            spawn_system_command("systemctl", &["poweroff"], "failed to power off")
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DesktopControlCommand {
    program: &'static str,
    args: Vec<String>,
}

impl DesktopControlCommand {
    fn new(program: &'static str, args: &[&str]) -> Self {
        Self {
            program,
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
        }
    }
}

fn desktop_control_commands(operation: &DesktopControlOperation) -> Vec<DesktopControlCommand> {
    match operation {
        DesktopControlOperation::MediaPlayPause => {
            vec![DesktopControlCommand::new("playerctl", &["play-pause"])]
        }
        DesktopControlOperation::MediaNext => {
            vec![DesktopControlCommand::new("playerctl", &["next"])]
        }
        DesktopControlOperation::MediaPrevious => {
            vec![DesktopControlCommand::new("playerctl", &["previous"])]
        }
        DesktopControlOperation::VolumeUp => vec![DesktopControlCommand::new(
            "wpctl",
            &["set-volume", "@DEFAULT_AUDIO_SINK@", "5%+"],
        )],
        DesktopControlOperation::VolumeDown => vec![DesktopControlCommand::new(
            "wpctl",
            &["set-volume", "@DEFAULT_AUDIO_SINK@", "5%-"],
        )],
        DesktopControlOperation::VolumeMute => vec![DesktopControlCommand::new(
            "wpctl",
            &["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"],
        )],
        DesktopControlOperation::BrightnessUp => vec![DesktopControlCommand::new(
            "brightnessctl",
            &["--class=backlight", "set", "+10%"],
        )],
        DesktopControlOperation::BrightnessDown => vec![DesktopControlCommand::new(
            "brightnessctl",
            &["--class=backlight", "set", "10%-"],
        )],
        DesktopControlOperation::AudioSettings => {
            vec![DesktopControlCommand::new("pavucontrol", &[])]
        }
        DesktopControlOperation::BluetoothTogglePower => vec![DesktopControlCommand::new(
            "sh",
            &[
                "-lc",
                "state=$(bluetoothctl show | awk '/Powered:/ {print $2}'); if [ \"$state\" = yes ]; then bluetoothctl power off; else bluetoothctl power on; fi",
            ],
        )],
        DesktopControlOperation::NetworkSettings => {
            vec![DesktopControlCommand::new("nm-connection-editor", &[])]
        }
        DesktopControlOperation::PowerProfileCycle => vec![DesktopControlCommand::new(
            "sh",
            &[
                "-lc",
                "current=$(powerprofilesctl get); case \"$current\" in performance) next=balanced ;; balanced) next=power-saver ;; *) next=performance ;; esac; powerprofilesctl set \"$next\"",
            ],
        )],
        DesktopControlOperation::PowerProfileSet { profile } => vec![DesktopControlCommand {
            program: "powerprofilesctl",
            args: vec!["set".to_string(), profile.clone()],
        }],
        DesktopControlOperation::ScreenshotArea => vec![DesktopControlCommand::new(
            "sh",
            &[
                "-lc",
                "dir=\"$HOME/Pictures/Screenshots\"; mkdir -p \"$dir\"; ts=$(date +'%Y-%m-%d %H-%M-%S'); file=\"$dir/Screenshot from $ts.png\"; region=$(slurp) || exit 1; grim -g \"$region\" \"$file\" && wl-copy -t image/png < \"$file\"; notify-send \"Screenshot\" \"Saved to $file\" >/dev/null 2>&1 || true",
            ],
        )],
        DesktopControlOperation::ScreenshotScreen => vec![DesktopControlCommand::new(
            "sh",
            &[
                "-lc",
                "dir=\"$HOME/Pictures/Screenshots\"; mkdir -p \"$dir\"; ts=$(date +'%Y-%m-%d %H-%M-%S'); file=\"$dir/Screenshot from $ts.png\"; grim \"$file\" && wl-copy -t image/png < \"$file\"; notify-send \"Screenshot\" \"Saved to $file\" >/dev/null 2>&1 || true",
            ],
        )],
        DesktopControlOperation::ColorPicker => {
            vec![DesktopControlCommand::new("hyprpicker", &["-a"])]
        }
        DesktopControlOperation::NotificationHistoryPop => {
            vec![DesktopControlCommand::new("dunstctl", &["history-pop"])]
        }
        DesktopControlOperation::NotificationCloseAll => {
            vec![DesktopControlCommand::new("dunstctl", &["close-all"])]
        }
        DesktopControlOperation::NotificationPauseToggle => vec![DesktopControlCommand::new(
            "sh",
            &[
                "-lc",
                "paused=$(dunstctl is-paused); if [ \"$paused\" = true ]; then dunstctl set-paused false; else dunstctl set-paused true; fi",
            ],
        )],
    }
}

fn execute_desktop_control_operation(operation: &DesktopControlOperation) -> Result<()> {
    for command in desktop_control_commands(operation) {
        Command::new(command.program)
            .args(&command.args)
            .spawn()
            .with_context(|| format!("failed to spawn {}", command.program))?;
    }
    Ok(())
}

fn lock_session() -> Result<()> {
    if is_hyprland_session() && !process_running_for_user("hyprlock") {
        if spawn_optional_command("hyprlock", &[])?.is_some() {
            return Ok(());
        }
    }

    if lock_current_logind_session()? {
        return Ok(());
    }

    if !process_running_for_user("hyprlock") && spawn_optional_command("hyprlock", &[])?.is_some() {
        return Ok(());
    }

    anyhow::bail!("no lock command is available for the current session");
}

fn logout_session() -> Result<()> {
    if is_hyprland_session() && spawn_optional_command("hyprctl", &["dispatch", "exit"])?.is_some()
    {
        return Ok(());
    }

    if is_niri_session()
        && spawn_optional_command("niri", &["msg", "action", "quit", "--skip-confirmation"])?
            .is_some()
    {
        return Ok(());
    }

    if is_bspwm_session() && spawn_optional_command("bspc", &["quit"])?.is_some() {
        return Ok(());
    }

    if let Some(session_id) = current_logind_session_id() {
        spawn_system_command(
            "loginctl",
            &["terminate-session", &session_id],
            "failed to terminate current session",
        )?;
        return Ok(());
    }

    anyhow::bail!("no safe logout method found for the current session");
}

fn lock_current_logind_session() -> Result<bool> {
    if let Some(session_id) = current_logind_session_id() {
        return spawn_optional_command("loginctl", &["lock-session", &session_id])
            .map(|child| child.is_some());
    }

    spawn_optional_command("loginctl", &["lock-session"]).map(|child| child.is_some())
}

fn current_logind_session_id() -> Option<String> {
    if let Ok(session_id) = std::env::var("XDG_SESSION_ID") {
        if !session_id.trim().is_empty() {
            return Some(session_id);
        }
    }

    let user = std::env::var("USER").ok()?;
    let output = Command::new("loginctl")
        .args(["show-user", &user, "--property=Display", "--value"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let session_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if session_id.is_empty() || session_id == "n/a" {
        None
    } else {
        Some(session_id)
    }
}

fn is_hyprland_session() -> bool {
    std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() || desktop_matches("hyprland")
}

fn is_niri_session() -> bool {
    std::env::var("NIRI_SOCKET").is_ok() || desktop_matches("niri")
}

fn is_bspwm_session() -> bool {
    std::env::var("BSPWM_SOCKET").is_ok() || desktop_matches("bspwm")
}

fn desktop_matches(wanted: &str) -> bool {
    let raw = std::env::var("XDG_CURRENT_DESKTOP")
        .or_else(|_| std::env::var("XDG_SESSION_DESKTOP"))
        .or_else(|_| std::env::var("DESKTOP_SESSION"))
        .unwrap_or_default();

    raw.split([':', ';'])
        .any(|token| token.eq_ignore_ascii_case(wanted))
}

fn process_running_for_user(process_name: &str) -> bool {
    let Ok(uid) = std::env::var("UID") else {
        return false;
    };
    if uid.trim().is_empty() {
        return false;
    }

    Command::new("pgrep")
        .args(["-u", &uid, "-x", process_name])
        .status()
        .is_ok_and(|status| status.success())
}

fn spawn_optional_command(program: &str, args: &[&str]) -> Result<Option<std::process::Child>> {
    match Command::new(program).args(args).spawn() {
        Ok(child) => Ok(Some(child)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to spawn {program}")),
    }
}

fn spawn_system_command(program: &str, args: &[&str], message: &str) -> Result<()> {
    spawn_optional_command(program, args)
        .with_context(|| message.to_string())?
        .with_context(|| format!("{program} is not installed"))?;
    Ok(())
}

fn execute_password_operation(
    window: &ApplicationWindow,
    entry: &str,
    operation: PasswordOperation,
    previous_focus_target: Option<&WindowFocusTarget>,
) -> Result<()> {
    let credential = load_pass_credential(entry)?;

    match operation {
        PasswordOperation::AutotypeLogin => {
            type_secret_steps(
                window,
                previous_focus_target,
                default_login_steps(&credential),
            )?;
        }
        PasswordOperation::CopyPassword => {
            copy_secret(&credential.password)?;
            window.close();
        }
        PasswordOperation::CopyUsername => {
            copy_secret(&credential.username)?;
            window.close();
        }
        PasswordOperation::TypePassword => {
            type_secret_steps(
                window,
                previous_focus_target,
                vec![TypeStep::Text(credential.password)],
            )?;
        }
        PasswordOperation::TypeUsername => {
            type_secret_steps(
                window,
                previous_focus_target,
                vec![TypeStep::Text(credential.username)],
            )?;
        }
        PasswordOperation::OpenUrl => {
            let url = credential
                .url
                .context("pass entry does not contain a URL")?;
            gio::AppInfo::launch_default_for_uri(&url, gio::AppLaunchContext::NONE)
                .context("failed to open URL")?;
            window.close();
        }
        PasswordOperation::CopyUrl => {
            let url = credential
                .url
                .context("pass entry does not contain a URL")?;
            copy_secret(&url)?;
            window.close();
        }
        PasswordOperation::CopyOtp => {
            let otp = load_pass_otp(entry)?;
            copy_secret(&otp)?;
            window.close();
        }
        PasswordOperation::TypeOtp => {
            let otp = load_pass_otp(entry)?;
            type_secret_steps(window, previous_focus_target, vec![TypeStep::Text(otp)])?;
        }
        PasswordOperation::CustomAutotype => {
            let steps = custom_autotype_steps(entry, &credential)?;
            type_secret_steps(window, previous_focus_target, steps)?;
        }
        PasswordOperation::Inspect => unreachable!("inspect is handled before action execution"),
    }

    Ok(())
}

fn type_secret_steps(
    window: &ApplicationWindow,
    previous_focus_target: Option<&WindowFocusTarget>,
    steps: Vec<TypeStep>,
) -> Result<()> {
    let target = previous_focus_target
        .context("no previously focused window was captured")?
        .clone();
    if !type_backend_available() {
        anyhow::bail!("wtype or xdotool is required for password autotype");
    }

    let application_hold = window.application().map(|app| app.hold());
    window.close();

    glib::timeout_add_local_once(
        Duration::from_millis(AUTOTYPE_AFTER_CLOSE_DELAY_MS),
        move || {
            if let Err(error) = run_type_secret_steps(&target, steps) {
                eprintln!("password autotype failed: {error:?}");
            }
            drop(application_hold);
        },
    );

    Ok(())
}

fn run_type_secret_steps(target: &WindowFocusTarget, steps: Vec<TypeStep>) -> Result<()> {
    let status = focus_window(target).context("failed to refocus previous window")?;
    if !status.success() {
        anyhow::bail!("failed to refocus previous window");
    }

    let use_x11_backend = use_x11_type_backend(target) && command_exists("xdotool");
    thread::sleep(Duration::from_millis(if use_x11_backend {
        500
    } else {
        80
    }));

    let commands = if use_x11_backend {
        xdotool_commands_for_steps(&steps)
    } else if wayland_available() && command_exists("wtype") {
        wtype_commands_for_steps(&steps)
    } else {
        xdotool_commands_for_steps(&steps)
    };

    for command in commands {
        run_program_input(command)?;
    }

    Ok(())
}

fn type_backend_available() -> bool {
    (wayland_available() && command_exists("wtype")) || command_exists("xdotool")
}

fn use_x11_type_backend(target: &WindowFocusTarget) -> bool {
    matches!(
        target,
        WindowFocusTarget::Hyprland { xwayland: true, .. } | WindowFocusTarget::X11 { .. }
    )
}

fn copy_secret(text: &str) -> Result<()> {
    if wayland_available() && command_exists("wl-copy") {
        run_program_input(wl_copy_command(text, password_clip_timeout_seconds()))
    } else if command_exists("xclip") {
        run_program_input(xclip_command(text))?;
        clear_xclip_clipboard_after(password_clip_timeout_seconds());
        Ok(())
    } else {
        anyhow::bail!("wl-copy or xclip is required to copy password data");
    }
}

fn clear_xclip_clipboard_after(timeout_seconds: u64) {
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(timeout_seconds));
        let _ = Command::new("xclip")
            .args(["-selection", "clipboard", "-in"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                if let Some(stdin) = child.stdin.take() {
                    drop(stdin);
                }
                child.wait().map(|_| ())
            });
    });
}

fn wayland_available() -> bool {
    wayland_available_for_session(
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        is_hyprland_session() || is_niri_session(),
    )
}

fn wayland_available_for_session(
    session_type: Option<&str>,
    wayland_display_set: bool,
    known_wayland_compositor: bool,
) -> bool {
    wayland_display_set
        && (known_wayland_compositor
            || !session_type.is_some_and(|session| session.eq_ignore_ascii_case("x11")))
}

fn command_exists(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let path = dir.join(program);
            path.is_file() && is_executable(&path)
        })
    })
}

fn password_clip_timeout_seconds() -> u64 {
    app_config()
        .map(|config| config.current().integrations.password_clip_timeout_seconds)
        .filter(|seconds| *seconds > 0)
        .unwrap_or_else(|| {
            std::env::var("PASSWORD_STORE_CLIP_TIME")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|seconds| *seconds > 0)
                .unwrap_or(15)
        })
}

fn inspected_password_results(credential: &Credential) -> Vec<ResultItem> {
    let mut rows = vec![
        password_action_result(
            &credential.entry,
            "Autotype login",
            "Type username, Tab, and password without submitting",
            PasswordOperation::AutotypeLogin,
            1_000,
            Some(pass_prediction_key(&credential.entry)),
        ),
        password_action_result(
            &credential.entry,
            "Copy password",
            "Copy password and clear it after the password-store timeout",
            PasswordOperation::CopyPassword,
            950,
            None,
        ),
        password_action_result(
            &credential.entry,
            "Copy username",
            "Copy username metadata or the entry basename",
            PasswordOperation::CopyUsername,
            940,
            None,
        ),
        password_action_result(
            &credential.entry,
            "Type password",
            "Type only the password into the focused window",
            PasswordOperation::TypePassword,
            930,
            None,
        ),
        password_action_result(
            &credential.entry,
            "Type username",
            "Type only the username into the focused window",
            PasswordOperation::TypeUsername,
            920,
            None,
        ),
    ];

    if credential.url.is_some() {
        rows.push(password_action_result(
            &credential.entry,
            "Open URL",
            "Open this entry's URL in the default browser",
            PasswordOperation::OpenUrl,
            910,
            None,
        ));
        rows.push(password_action_result(
            &credential.entry,
            "Copy URL",
            "Copy this entry's URL",
            PasswordOperation::CopyUrl,
            900,
            None,
        ));
    }

    if credential.otp_uri.is_some() {
        rows.push(password_action_result(
            &credential.entry,
            "Copy OTP",
            "Generate and copy a one-time password with pass-otp",
            PasswordOperation::CopyOtp,
            890,
            None,
        ));
        rows.push(password_action_result(
            &credential.entry,
            "Type OTP",
            "Generate and type a one-time password with pass-otp",
            PasswordOperation::TypeOtp,
            880,
            None,
        ));
    }

    if credential.autotype.is_some() {
        rows.push(password_action_result(
            &credential.entry,
            "Custom autotype",
            "Run this entry's autotype template",
            PasswordOperation::CustomAutotype,
            870,
            None,
        ));
    }

    rows
}

fn password_action_result(
    entry: &str,
    title: &str,
    subtitle: &str,
    operation: PasswordOperation,
    score: i32,
    prediction_key: Option<String>,
) -> ResultItem {
    ResultItem {
        prediction_key,
        title: format!("{title}: {entry}"),
        subtitle: subtitle.to_string(),
        source: "Passwords",
        icon_name: "dialog-password-symbolic".to_string(),
        score,
        action: Action::Password {
            entry: entry.to_string(),
            operation,
        },
        ..Default::default()
    }
}

fn custom_autotype_steps(entry: &str, credential: &Credential) -> Result<Vec<TypeStep>> {
    let template = credential
        .autotype
        .as_deref()
        .context("pass entry does not contain an autotype template")?;
    let mut steps = Vec::new();

    let mut tokens = template.split_whitespace().peekable();
    while let Some(token) = tokens.next() {
        match token {
            ":tab" => steps.push(TypeStep::Key("Tab")),
            ":space" => steps.push(TypeStep::Text(" ".to_string())),
            ":enter" => steps.push(TypeStep::Key("Return")),
            ":delay" => steps.push(TypeStep::Delay(1_000)),
            "pass" | "password" => steps.push(TypeStep::Text(credential.password.clone())),
            "user" | "username" => steps.push(TypeStep::Text(credential.username.clone())),
            "path" => steps.push(TypeStep::Text(entry.to_string())),
            "basename" | "filename" => {
                steps.push(TypeStep::Text(crate::password::fallback_username(entry)));
            }
            ":otp" => {
                if matches!(tokens.peek(), Some(&"pass") | Some(&"gopass")) {
                    tokens.next();
                }
                steps.push(TypeStep::Text(load_pass_otp(entry)?));
            }
            key => {
                let Some(value) = credential.fields.get(&key.to_ascii_lowercase()) else {
                    anyhow::bail!("unknown autotype token: {key}");
                };
                steps.push(TypeStep::Text(value.clone()));
            }
        }
    }

    Ok(steps)
}

fn launch_desktop_app(desktop_id: &str) -> Result<()> {
    if let Some(app) = gio::DesktopAppInfo::new(desktop_id) {
        app.launch(&[], gio::AppLaunchContext::NONE)
            .context("failed to launch desktop app")?;
        return Ok(());
    }

    let app = gio::AppInfo::all()
        .into_iter()
        .find(|app| app.id().as_deref() == Some(desktop_id))
        .context("desktop application no longer exists")?;
    app.launch(&[], gio::AppLaunchContext::NONE)
        .context("failed to launch desktop app")?;
    Ok(())
}

fn launch_ssh(host: &str) -> Result<()> {
    let terminal = app_config()
        .and_then(|config| {
            let value = config.current().integrations.ssh_terminal;
            let trimmed = value.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .or_else(|| {
            std::env::var("DOT_LAUNCHER_TERMINAL")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| default_ssh_terminal(dirs::home_dir().as_deref()));

    Command::new(&terminal)
        .args(["-e", "ssh", host])
        .spawn()
        .context("failed to launch ssh session")?;
    Ok(())
}

fn web_search_url() -> String {
    app_config()
        .map(|config| config.current().integrations.web_search_url)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            std::env::var("DOT_LAUNCHER_SEARCH_URL")
                .unwrap_or_else(|_| "https://duckduckgo.com/?q=".to_string())
        })
}

fn open_mail_or_url(url: &str) -> Result<()> {
    if let Some((action, message_id)) = parse_luma_mail_helper_uri(url) {
        let email_config = app_config_email_config();
        let Some(command) = evolution_helper_command(&email_config) else {
            anyhow::bail!("Evolution mail helper is not configured");
        };

        run_mail_helper_action(
            &command,
            &action,
            &message_id,
            email_config.evolution_helper_timeout_ms,
        )?;
        return Ok(());
    }

    match email_open_strategy(url, &app_config_email_config()) {
        EmailOpenStrategy::ThunderbirdUrl => {
            if spawn_optional_command("thunderbird", &[url])?.is_some() {
                return Ok(());
            }
        }
        EmailOpenStrategy::ThunderbirdFile(path) => {
            let path = path.to_string_lossy().to_string();
            if spawn_optional_command("thunderbird", &["-file", &path])?.is_some() {
                return Ok(());
            }
        }
        EmailOpenStrategy::EvolutionUrl => {
            if spawn_optional_command("evolution", &[url])?.is_some() {
                return Ok(());
            }
        }
        EmailOpenStrategy::EvolutionFile(path) => {
            let path = path.to_string_lossy().to_string();
            if spawn_optional_command("evolution", &["--component=mail", "--view", &path])?
                .is_some()
            {
                return Ok(());
            }
        }
        EmailOpenStrategy::DefaultUri => {}
    }

    gio::AppInfo::launch_default_for_uri(url, gio::AppLaunchContext::NONE)
        .context("failed to open mail or URL")
}

fn parse_luma_mail_helper_uri(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("luma-mail-eds://")?;
    let (action, query) = rest.split_once('?').unwrap_or((rest, ""));
    if action.trim().is_empty() {
        return None;
    }

    let mut message_id = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        if key == "message_id" {
            message_id = urlencoding::decode(value)
                .ok()
                .map(|value| value.into_owned());
            break;
        }
    }

    message_id.map(|message_id| (action.to_string(), message_id))
}

fn app_config_email_config() -> EmailConfig {
    app_config()
        .map(|config| config.current().integrations.email)
        .unwrap_or_default()
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum EmailOpenStrategy {
    ThunderbirdUrl,
    ThunderbirdFile(std::path::PathBuf),
    EvolutionUrl,
    EvolutionFile(std::path::PathBuf),
    DefaultUri,
}

fn email_open_strategy(url: &str, config: &EmailConfig) -> EmailOpenStrategy {
    if url.starts_with("imap-message://")
        || url.starts_with("mailbox-message://")
        || url.starts_with("message://")
    {
        return EmailOpenStrategy::ThunderbirdUrl;
    }

    if url.starts_with("mailto:") {
        return match preferred_mail_client(config) {
            Some(PreferredMailClient::Evolution) => EmailOpenStrategy::EvolutionUrl,
            Some(PreferredMailClient::Thunderbird) => EmailOpenStrategy::ThunderbirdUrl,
            None => EmailOpenStrategy::DefaultUri,
        };
    }

    if let Some(path) = file_uri_to_path(url) {
        return match preferred_mail_client(config) {
            Some(PreferredMailClient::Evolution) => EmailOpenStrategy::EvolutionFile(path),
            Some(PreferredMailClient::Thunderbird) => EmailOpenStrategy::ThunderbirdFile(path),
            None => EmailOpenStrategy::DefaultUri,
        };
    }

    EmailOpenStrategy::DefaultUri
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreferredMailClient {
    Thunderbird,
    Evolution,
}

fn preferred_mail_client(config: &EmailConfig) -> Option<PreferredMailClient> {
    let thunderbird = config.thunderbird_enabled;
    let evolution = config.evolution_enabled;

    match config.preferred_backend {
        EmailBackendPreference::Thunderbird | EmailBackendPreference::Auto => {
            if thunderbird {
                Some(PreferredMailClient::Thunderbird)
            } else if evolution {
                Some(PreferredMailClient::Evolution)
            } else {
                None
            }
        }
        EmailBackendPreference::Evolution => {
            if evolution {
                Some(PreferredMailClient::Evolution)
            } else if thunderbird {
                Some(PreferredMailClient::Thunderbird)
            } else {
                None
            }
        }
        EmailBackendPreference::LocalMail => {
            if thunderbird {
                Some(PreferredMailClient::Thunderbird)
            } else if evolution {
                Some(PreferredMailClient::Evolution)
            } else {
                None
            }
        }
    }
}

fn file_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let file = gio::File::for_uri(uri);
    file.path()
}

fn placeholder_for_mode(mode: SearchMode) -> &'static str {
    match mode {
        SearchMode::All => {
            "Search apps, files, passwords, email, SSH, commands, web, and calculations"
        }
        SearchMode::Apps => "Launch an application",
        SearchMode::Windows => "Switch to an active window",
        SearchMode::Files => "Search files with LocalSearch",
        SearchMode::Ssh => "Search SSH hosts",
        SearchMode::Pass => "Search password-store entries",
        SearchMode::Email => "Search email messages",
        SearchMode::Commands => "Run a command",
        SearchMode::Web => "Search the web or open a URL",
        SearchMode::Calc => "Evaluate a libqalculate expression",
        SearchMode::Controls => "Search desktop controls",
    }
}

fn copy_to_clipboard(text: &str) {
    if let Some(display) = gdk::Display::default() {
        display.clipboard().set_text(text);
    }
}

fn load_pass_secret(entry: &str) -> Result<String> {
    parse_credential(entry, &load_pass_output(&["show", entry])?)
        .map(|credential| credential.password)
}

fn load_pass_credential(entry: &str) -> Result<Credential> {
    parse_credential(entry, &load_pass_output(&["show", entry])?)
}

fn load_pass_otp(entry: &str) -> Result<String> {
    load_pass_output(&["otp", entry]).map(|output| output.trim().to_string())
}

fn load_pass_output(args: &[&str]) -> Result<String> {
    let output = Command::new("pass")
        .args(args)
        .output()
        .context("failed to run pass")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!(
            "{}",
            if stderr.is_empty() {
                "pass failed"
            } else {
                stderr.as_str()
            }
        );
    }

    String::from_utf8(output.stdout).context("pass returned non-UTF-8 output")
}

fn default_ssh_terminal(home: Option<&std::path::Path>) -> String {
    if let Some(home) = home {
        let launcher = home.join(".dotfiles/scripts/launch_kitty.sh");
        if is_executable(&launcher) {
            return launcher.to_string_lossy().to_string();
        }
    }

    "kitty".to_string()
}

fn is_executable(path: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };

    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

fn launcher_css() -> &'static str {
    r#"
      window {
        background: transparent;
      }

      .launcher-shell {
        background: linear-gradient(180deg, rgba(19, 23, 33, 0.78), rgba(12, 15, 24, 0.92));
        border: 1px solid rgba(255, 255, 255, 0.10);
        border-radius: 18px;
        box-shadow: 0 18px 44px rgba(0, 0, 0, 0.32);
        padding: 0.8rem;
      }

      .launcher-entry {
        min-height: 54px;
        font-size: 1.08rem;
        padding: 0.35rem 2.55rem 0.35rem 0.82rem;
        border-radius: 12px;
        border: 1px solid rgba(255, 255, 255, 0.09);
        background: rgba(255, 255, 255, 0.07);
        color: rgba(247, 249, 255, 0.98);
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.04);
      }

      .launcher-entry:focus-within {
        border-color: rgba(142, 188, 255, 0.55);
        background: rgba(255, 255, 255, 0.10);
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08),
                    0 0 0 3px rgba(106, 160, 255, 0.14);
      }

      .launcher-search-spinner {
        color: rgba(190, 213, 255, 0.88);
        min-width: 22px;
        min-height: 22px;
      }

      .launcher-results {
        background: transparent;
      }

      .launcher-row {
        margin-bottom: 5px;
        border-radius: 12px;
        border: 1px solid rgba(255, 255, 255, 0.02);
        background: rgba(255, 255, 255, 0.02);
      }

      .launcher-row:selected {
        background: linear-gradient(90deg, rgba(120, 168, 255, 0.16), rgba(255, 255, 255, 0.08));
        border-color: rgba(142, 188, 255, 0.22);
      }

      .launcher-row-status {
        background: rgba(255, 255, 255, 0.04);
        border: 1px dashed rgba(255, 255, 255, 0.08);
      }

      .launcher-row-status:selected {
        background: rgba(255, 255, 255, 0.07);
      }

      .launcher-icon-wrap {
        min-width: 34px;
        border-radius: 10px;
        background: rgba(255, 255, 255, 0.07);
        border: 1px solid rgba(255, 255, 255, 0.04);
        padding: 6px;
      }

      .launcher-icon {
        color: rgba(240, 244, 255, 0.96);
      }

      .launcher-title {
        font-size: 1rem;
        font-weight: 650;
      }

      .launcher-subtitle {
        font-size: 0.86rem;
        color: rgba(210, 219, 237, 0.70);
      }

      .launcher-accessory {
        font-size: 0.82rem;
        color: rgba(190, 213, 255, 0.78);
      }

      .launcher-badge {
        color: rgba(210, 219, 237, 0.80);
      }

      .launcher-badge-unread {
        color: rgba(120, 168, 255, 0.95);
      }

      .settings-shell {
        background: linear-gradient(180deg, rgba(18, 22, 31, 0.94), rgba(10, 13, 21, 0.98));
        border: 1px solid rgba(255, 255, 255, 0.10);
        border-radius: 22px;
        box-shadow: 0 22px 58px rgba(0, 0, 0, 0.40);
      }

      .settings-scroller {
        background: transparent;
      }

      .settings-header {
        padding-bottom: 4px;
      }

      .settings-title {
        font-size: 1.55rem;
        font-weight: 720;
        color: rgba(247, 249, 255, 0.98);
      }

      .settings-subtitle {
        color: rgba(210, 219, 237, 0.78);
      }

      .settings-card {
        background: rgba(255, 255, 255, 0.04);
        border: 1px solid rgba(255, 255, 255, 0.08);
        border-radius: 18px;
        padding: 16px;
      }

      .settings-card-title {
        font-size: 1.05rem;
        font-weight: 680;
        color: rgba(247, 249, 255, 0.98);
      }

      .settings-card-subtitle {
        color: rgba(210, 219, 237, 0.76);
      }

      .settings-row {
        min-height: 46px;
        padding-top: 4px;
        padding-bottom: 4px;
      }

      .settings-row-title {
        font-weight: 600;
        color: rgba(247, 249, 255, 0.96);
      }

      .settings-row-subtitle {
        color: rgba(210, 219, 237, 0.72);
      }

      .settings-status {
        color: rgba(210, 219, 237, 0.76);
      }
    "#
}

fn apply_css() {
    let css = launcher_css();
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(css);
    gtk4::style_context_add_provider_for_display(
        &gdk::Display::default().expect("display"),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

#[cfg(test)]
mod tests {
    use super::{
        EmailOpenStrategy, LAUNCHER_SHADOW_BLUR_PX, LAUNCHER_SHADOW_Y_OFFSET_PX,
        LAUNCHER_SURFACE_MARGIN_BOTTOM_PX, LAUNCHER_SURFACE_MARGIN_PX, SearchUpdatePhase,
        action_failure_result, background_processing_after_update, default_ssh_terminal,
        desktop_control_commands, email_open_strategy, inspected_password_results, launcher_css,
        layer_shell_enabled, pending_deferred_results, power_confirmation_results,
        power_requires_confirmation, preserved_selection_index, row_tooltip_text,
        wayland_available_for_session,
    };
    use crate::config::{EmailBackendPreference, EmailConfig};
    use crate::model::{
        Action, DesktopControlOperation, PasswordOperation, PowerOperation, ResultItem,
        WindowFocusTarget,
    };
    use crate::password::parse_credential;
    use std::fs::{self, File};

    #[test]
    fn prefers_launch_kitty_wrapper_when_it_is_executable() {
        let temp_home = std::env::temp_dir().join(format!(
            "Luma-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let wrapper = temp_home.join(".dotfiles/scripts/launch_kitty.sh");
        fs::create_dir_all(wrapper.parent().expect("wrapper parent")).expect("create wrapper dir");
        File::create(&wrapper).expect("create wrapper");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&wrapper)
                .expect("wrapper metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&wrapper, permissions).expect("set executable bit");
        }

        assert_eq!(
            default_ssh_terminal(Some(&temp_home)),
            wrapper.to_string_lossy()
        );

        fs::remove_dir_all(&temp_home).expect("cleanup temp home");
    }

    #[test]
    fn falls_back_to_kitty_without_wrapper() {
        assert_eq!(default_ssh_terminal(None), "kitty");
    }

    fn selection_test_item(source: &'static str, title: &str) -> ResultItem {
        ResultItem {
            prediction_key: None,
            title: title.to_string(),
            subtitle: String::new(),
            source,
            icon_name: String::new(),
            score: 0,
            action: Action::None,
            ..Default::default()
        }
    }

    #[test]
    fn selection_follows_the_chosen_item_when_async_results_change_order() {
        let app = selection_test_item("Applications", "Reddit Client");
        let bookmark = selection_test_item("Bookmarks", "Reddit");
        let email = selection_test_item("Email", "Open email: Reddit digest");

        // The user had arrowed down to the bookmark in the immediate results;
        // the async email result then arrives and reorders the merged list.
        let merged = vec![app, email, bookmark.clone()];

        assert_eq!(preserved_selection_index(Some(&bookmark), &merged), 2);
    }

    #[test]
    fn selection_falls_back_to_top_when_previous_item_is_gone() {
        let bookmark = selection_test_item("Bookmarks", "Reddit");
        let merged = vec![
            selection_test_item("Applications", "Reddit Client"),
            selection_test_item("Email", "Open email: Reddit digest"),
        ];

        assert_eq!(preserved_selection_index(Some(&bookmark), &merged), 0);
    }

    #[test]
    fn selection_defaults_to_top_when_nothing_was_selected() {
        let merged = vec![selection_test_item("Applications", "Reddit Client")];

        assert_eq!(preserved_selection_index(None, &merged), 0);
    }

    #[test]
    fn pending_deferred_search_keeps_visible_results_without_searching_row() {
        let app = selection_test_item("Applications", "Reddit Client");

        let results = pending_deferred_results(vec![app.clone()]);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source, app.source);
        assert_eq!(results[0].title, app.title);
        assert!(
            results
                .iter()
                .all(|item| item.source != "Status" && !item.title.starts_with("Searching"))
        );
    }

    #[test]
    fn deferred_update_phase_finishes_processing_even_when_no_rows_return() {
        assert!(!background_processing_after_update(
            SearchUpdatePhase::Deferred,
            true
        ));
    }

    #[test]
    fn action_failures_render_as_status_results() {
        let item = action_failure_result("permission denied");

        assert_eq!(item.title, "Action failed: permission denied");
        assert!(matches!(item.action, Action::None));
        assert_eq!(item.source, "Status");
        assert!(item.subtitle.is_empty());
    }

    #[test]
    fn row_tooltip_preserves_hidden_result_details() {
        let item = ResultItem {
            prediction_key: None,
            title: "Firefox".to_string(),
            subtitle: "Web Browser".to_string(),
            source: "Applications",
            icon_name: "firefox".to_string(),
            score: 100,
            action: Action::None,
            ..Default::default()
        };

        assert_eq!(
            row_tooltip_text(&item).as_deref(),
            Some("Web Browser\nApplications")
        );
    }

    #[test]
    fn launcher_surface_reserves_room_for_the_css_shadow() {
        assert!(LAUNCHER_SURFACE_MARGIN_PX >= LAUNCHER_SHADOW_BLUR_PX);
        assert!(
            LAUNCHER_SURFACE_MARGIN_BOTTOM_PX
                >= LAUNCHER_SHADOW_BLUR_PX + LAUNCHER_SHADOW_Y_OFFSET_PX
        );
        assert!(launcher_css().contains("box-shadow: 0 18px 44px rgba(0, 0, 0, 0.32);"));
    }

    #[test]
    fn layer_shell_requires_wayland_and_protocol_support() {
        assert!(layer_shell_enabled(true, true));
        assert!(!layer_shell_enabled(false, true));
        assert!(!layer_shell_enabled(true, false));
        assert!(!layer_shell_enabled(false, false));
    }

    #[test]
    fn x11_session_ignores_stray_wayland_display_for_autotype() {
        assert!(!wayland_available_for_session(Some("x11"), true, false));
    }

    #[test]
    fn hyprland_session_uses_wayland_autotype_when_session_type_is_stale() {
        assert!(wayland_available_for_session(Some("x11"), true, true));
    }

    #[test]
    fn x11_session_prefers_x11_gdk_backend() {
        assert_eq!(
            super::gdk_backend_for_session(Some("x11"), true, true),
            Some("x11")
        );
    }

    #[test]
    fn wayland_session_prefers_wayland_gdk_backend() {
        assert_eq!(
            super::gdk_backend_for_session(Some("wayland"), true, true),
            Some("wayland")
        );
    }

    #[test]
    fn xwayland_hyprland_targets_use_the_x11_type_backend() {
        assert!(super::use_x11_type_backend(&WindowFocusTarget::Hyprland {
            address: "0xabc".to_string(),
            xwayland: true,
        }));
        assert!(!super::use_x11_type_backend(&WindowFocusTarget::Hyprland {
            address: "0xabc".to_string(),
            xwayland: false,
        }));
    }

    #[test]
    fn thunderbird_message_urls_open_with_thunderbird() {
        let config = EmailConfig {
            preferred_backend: EmailBackendPreference::Evolution,
            thunderbird_enabled: true,
            evolution_enabled: true,
            local_mail_enabled: true,
            ..EmailConfig::default()
        };

        assert_eq!(
            email_open_strategy("imap-message://imap://example.com/INBOX#123", &config),
            EmailOpenStrategy::ThunderbirdUrl
        );
        assert_eq!(
            email_open_strategy("mailto:robin@example.com", &config),
            EmailOpenStrategy::EvolutionUrl
        );
    }

    #[test]
    fn local_file_mail_prefers_the_selected_client() {
        let evolution_config = EmailConfig {
            preferred_backend: EmailBackendPreference::Evolution,
            thunderbird_enabled: true,
            evolution_enabled: true,
            local_mail_enabled: true,
            ..EmailConfig::default()
        };
        let thunderbird_config = EmailConfig {
            preferred_backend: EmailBackendPreference::Thunderbird,
            thunderbird_enabled: true,
            evolution_enabled: true,
            local_mail_enabled: true,
            ..EmailConfig::default()
        };

        assert!(matches!(
            email_open_strategy("file:///tmp/message.eml", &evolution_config),
            EmailOpenStrategy::EvolutionFile(_)
        ));
        assert!(matches!(
            email_open_strategy("file:///tmp/message.eml", &thunderbird_config),
            EmailOpenStrategy::ThunderbirdFile(_)
        ));
    }

    #[test]
    fn power_actions_confirm_session_ending_operations() {
        assert!(!power_requires_confirmation(PowerOperation::Lock));
        assert!(power_requires_confirmation(PowerOperation::Suspend));
        assert!(power_requires_confirmation(PowerOperation::Logout));
        assert!(power_requires_confirmation(PowerOperation::Reboot));
        assert!(power_requires_confirmation(PowerOperation::Shutdown));
    }

    #[test]
    fn power_confirmation_results_execute_native_power_actions() {
        let results = power_confirmation_results(PowerOperation::Shutdown);

        assert_eq!(results[0].title, "Confirm Shutdown");
        assert!(matches!(
            results[0].action,
            Action::Power {
                operation: PowerOperation::Shutdown,
                confirmed: true,
            }
        ));
        assert!(matches!(results[1].action, Action::None));
    }

    #[test]
    fn desktop_control_operations_map_to_native_commands() {
        let media = desktop_control_commands(&DesktopControlOperation::MediaPlayPause);
        assert_eq!(media[0].program, "playerctl");
        assert_eq!(media[0].args, vec!["play-pause"]);

        let volume = desktop_control_commands(&DesktopControlOperation::VolumeUp);
        assert_eq!(volume[0].program, "wpctl");
        assert_eq!(
            volume[0].args,
            vec!["set-volume", "@DEFAULT_AUDIO_SINK@", "5%+"]
        );

        let bluetooth = desktop_control_commands(&DesktopControlOperation::BluetoothTogglePower);
        assert_eq!(bluetooth[0].program, "sh");
        assert!(bluetooth[0].args.join(" ").contains("bluetoothctl power"));

        let color = desktop_control_commands(&DesktopControlOperation::ColorPicker);
        assert_eq!(color[0].program, "hyprpicker");
        assert_eq!(color[0].args, vec!["-a"]);
    }

    #[test]
    fn inspected_password_results_include_metadata_specific_actions() {
        let credential = parse_credential(
            "github/work",
            "secret\nuser: robin\nurl: https://github.com\notpauth://totp/GitHub:robin?secret=ABC\nautotype: user :tab pass\n",
        )
        .expect("credential");

        let operations = inspected_password_results(&credential)
            .into_iter()
            .filter_map(|item| match item.action {
                Action::Password { operation, .. } => Some(operation),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(operations.contains(&PasswordOperation::OpenUrl));
        assert!(operations.contains(&PasswordOperation::CopyUrl));
        assert!(operations.contains(&PasswordOperation::CopyOtp));
        assert!(operations.contains(&PasswordOperation::TypeOtp));
        assert!(operations.contains(&PasswordOperation::CustomAutotype));
    }

    #[test]
    fn inspected_password_results_records_autotype_against_entry_prediction_key() {
        let credential =
            parse_credential("github/work", "secret\nuser: robin\n").expect("credential");

        let rows = inspected_password_results(&credential);

        assert_eq!(rows[0].title, "Autotype login: github/work");
        assert_eq!(rows[0].prediction_key.as_deref(), Some("pass:github/work"));
    }
}

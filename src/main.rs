mod actions;
mod config;
mod mail_eds_protocol;
mod model;
mod password;
mod prediction;
mod settings;
mod sources;
mod ui;

use crate::actions::{
    copy_pass_entry, execute_desktop_control_operation, execute_password_operation,
    execute_power_operation, inspected_password_results, load_pass_credential,
    power_confirmation_results, power_requires_confirmation,
};
use crate::config::{ConfigStore, EmailBackendPreference, EmailConfig};
use crate::model::{Action, PackageManager, ResultItem, SearchMode};
use crate::model::{PasswordOperation, WindowFocusTarget};
use crate::password::{
    Credential, format_generated_pass_entry, generate_password, parse_credential,
    pass_insert_command, run_program_input,
};
use crate::settings::open_config_panel;
use crate::sources::{
    SearchSnapshot, Sources, append_deferred_results, evolution_helper_command, focus_window,
    focused_window_target, no_results_item, run_mail_helper_action,
};
use crate::ui::results::{
    background_processing_after_update, finalize_loaded_results, install_frame_profiler,
    move_selection, pending_deferred_results, profiling_enabled, rebuild_results,
    set_background_processing,
};
use crate::ui::theme::apply_css;
use anyhow::{Context, Result};
use clap::Parser;
use gtk4::gdk;
use gtk4::gdk::prelude::*;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, Entry, EventControllerKey, ListBox,
    Orientation, Overlay, ScrolledWindow, SelectionMode, Spinner,
};
use gtk4_layer_shell::LayerShell;
use std::cell::Cell;
use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SearchWork {
    ImmediateSnapshot,
    DeferredProviders,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SearchWorkExecution {
    Inline,
    BackgroundThread,
}

fn search_work_execution(work: SearchWork) -> SearchWorkExecution {
    match work {
        SearchWork::ImmediateSnapshot => SearchWorkExecution::Inline,
        SearchWork::DeferredProviders => SearchWorkExecution::BackgroundThread,
    }
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

        let sources = self.sources.clone();
        let mode = self.mode;
        match search_work_execution(SearchWork::ImmediateSnapshot) {
            SearchWorkExecution::Inline => {
                let snapshot = sources.search_snapshot(&query, mode, clipboard_url.as_deref());
                self.apply_search_update(SearchUpdate {
                    generation,
                    phase: SearchUpdatePhase::Immediate,
                    snapshot,
                    deferred_results: Vec::new(),
                });
            }
            SearchWorkExecution::BackgroundThread => {
                set_background_processing(&self.spinner, true);
                let tx = self.update_tx.clone();
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
        }
    }

    fn drain_updates(&self) {
        loop {
            let update = { self.update_rx.borrow_mut().try_recv() };

            match update {
                Ok(update) => self.apply_search_update(update),
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
                match search_work_execution(SearchWork::DeferredProviders) {
                    SearchWorkExecution::Inline => {
                        let deferred_results = sources.search_deferred_results(&snapshot);
                        let _ = tx.send(SearchUpdate {
                            generation,
                            phase: SearchUpdatePhase::Deferred,
                            snapshot,
                            deferred_results,
                        });
                    }
                    SearchWorkExecution::BackgroundThread => {
                        thread::spawn(move || {
                            let deferred_results = sources.search_deferred_results(&snapshot);
                            let _ = tx.send(SearchUpdate {
                                generation,
                                phase: SearchUpdatePhase::Deferred,
                                snapshot,
                                deferred_results,
                            });
                        });
                    }
                }
            },
        );
        self.state.borrow_mut().pending_timeout = Some(source_id);
    }

    fn apply_search_update(&self, update: SearchUpdate) {
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
            copy_pass_entry(&entry)?;
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
        Action::InstallPackage { package, manager } => {
            launch_package_install(manager, &package)?;
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

fn spawn_optional_command(program: &str, args: &[&str]) -> Result<Option<std::process::Child>> {
    match Command::new(program).args(args).spawn() {
        Ok(child) => Ok(Some(child)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to spawn {program}")),
    }
}

fn command_exists(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let path = dir.join(program);
            path.is_file() && is_executable(&path)
        })
    })
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
    let terminal = launcher_terminal();

    Command::new(&terminal)
        .args(["-e", "ssh", host])
        .spawn()
        .context("failed to launch ssh session")?;
    Ok(())
}

fn launcher_terminal() -> String {
    app_config()
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
        .unwrap_or_else(|| default_ssh_terminal(dirs::home_dir().as_deref()))
}

fn launch_package_install(manager: PackageManager, package: &str) -> Result<()> {
    let terminal = launcher_terminal();
    let install_args = package_install_command(manager, package);
    Command::new(&terminal)
        .arg("-e")
        .args(install_args)
        .spawn()
        .context("failed to launch package install")?;
    Ok(())
}

fn package_install_command(manager: PackageManager, package: &str) -> Vec<&str> {
    match manager {
        PackageManager::Pacman => vec!["sudo", "pacman", "-S", package],
        PackageManager::Paru => vec!["paru", "-S", package],
    }
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
        SearchMode::Packages => "Search pacman/paru packages",
    }
}

fn copy_to_clipboard(text: &str) {
    if let Some(display) = gdk::Display::default() {
        display.clipboard().set_text(text);
    }
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

#[cfg(test)]
mod tests {
    use super::{
        EmailOpenStrategy, LAUNCHER_SHADOW_BLUR_PX, LAUNCHER_SHADOW_Y_OFFSET_PX,
        LAUNCHER_SURFACE_MARGIN_BOTTOM_PX, LAUNCHER_SURFACE_MARGIN_PX, SearchUpdatePhase,
        SearchWork, SearchWorkExecution, action_failure_result, default_ssh_terminal,
        email_open_strategy, inspected_password_results, layer_shell_enabled,
        package_install_command, power_confirmation_results, power_requires_confirmation,
        search_work_execution,
    };
    use crate::actions::{
        desktop_controls::desktop_control_commands, password::use_x11_type_backend,
        password::wayland_available_for_session,
    };
    use crate::config::{EmailBackendPreference, EmailConfig};
    use crate::model::{
        Action, DesktopControlOperation, PackageManager, PasswordOperation, PowerOperation,
        ResultItem, WindowFocusTarget,
    };
    use crate::password::parse_credential;
    use crate::ui::results::{
        background_processing_after_update, pending_deferred_results, preserved_selection_index,
        row_tooltip_text,
    };
    use crate::ui::theme::fallback_css;
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

    #[test]
    fn package_install_command_uses_the_selected_manager() {
        assert_eq!(
            package_install_command(PackageManager::Paru, "visual-studio-code-bin"),
            vec!["paru", "-S", "visual-studio-code-bin"]
        );
        assert_eq!(
            package_install_command(PackageManager::Pacman, "firefox"),
            vec!["sudo", "pacman", "-S", "firefox"]
        );
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
    fn immediate_search_snapshots_are_applied_inline() {
        assert_eq!(
            search_work_execution(SearchWork::ImmediateSnapshot),
            SearchWorkExecution::Inline
        );
        assert_eq!(
            search_work_execution(SearchWork::DeferredProviders),
            SearchWorkExecution::BackgroundThread
        );
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
        assert!(fallback_css().contains("box-shadow: 0 18px 44px rgba(0, 0, 0, 0.32);"));
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
        assert!(use_x11_type_backend(&WindowFocusTarget::Hyprland {
            address: "0xabc".to_string(),
            xwayland: true,
        }));
        assert!(!use_x11_type_backend(&WindowFocusTarget::Hyprland {
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

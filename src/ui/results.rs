use crate::SearchUpdatePhase;
use crate::model::{Action, EntryBadge, QueryInput, ResultItem};
use crate::sources::{no_results_item, sort_visible_results};
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, ContentFit, Image, Label, ListBox, ListBoxRow, Orientation, Overflow,
    Picture, Scale, ScrolledWindow, Spinner,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Sends slider changes at most this often while dragging.
const LEVEL_SEND_INTERVAL: Duration = Duration::from_millis(60);
/// Live updates leave a slider alone this long after the user moved it.
const LEVEL_USER_HOLD: Duration = Duration::from_millis(1_500);

type LevelSink = Rc<dyn Fn(&Action, u8)>;

struct LevelRow {
    index: usize,
    scale: Scale,
    value: Label,
    syncing: Rc<Cell<bool>>,
    touched: Rc<Cell<Option<Instant>>>,
}

thread_local! {
    static LEVEL_SINK: RefCell<Option<LevelSink>> = const { RefCell::new(None) };
    static LEVEL_ROWS: RefCell<Vec<LevelRow>> = const { RefCell::new(Vec::new()) };
}

/// Where level sliders send `(row action, percent)`; the launcher window
/// installs one that forwards to Vigil.
pub(crate) fn set_level_sink(sink: LevelSink) {
    LEVEL_SINK.with(|slot| slot.replace(Some(sink)));
}

/// Moves sliders to fresh live values without rebuilding their rows, unless
/// the user is adjusting them.
pub(crate) fn sync_levels(results: &[ResultItem]) {
    LEVEL_ROWS.with(|rows| {
        for row in rows.borrow().iter() {
            let Some(level) = results.get(row.index).and_then(|item| item.level) else {
                continue;
            };
            if row
                .touched
                .get()
                .is_some_and(|touched| touched.elapsed() < LEVEL_USER_HOLD)
                || row.scale.value().round() as u8 == level
            {
                continue;
            }
            row.syncing.set(true);
            row.scale.set_value(f64::from(level));
            row.value.set_label(&format!("{level}%"));
            row.syncing.set(false);
        }
    });
}

/// Nudges the slider in row `index` by `delta` percent; false if it has none.
pub(crate) fn nudge_level(index: usize, delta: f64) -> bool {
    LEVEL_ROWS.with(|rows| {
        let rows = rows.borrow();
        let Some(row) = rows.iter().find(|row| row.index == index) else {
            return false;
        };
        row.scale
            .set_value((row.scale.value() + delta).clamp(0.0, 100.0));
        true
    })
}

fn level_slider(item: &ResultItem, index: usize, level: u8) -> (GtkBox, Label) {
    let scale = Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
    scale.set_value(f64::from(level));
    scale.set_draw_value(false);
    scale.set_hexpand(true);
    scale.set_increments(1.0, 5.0);
    scale.set_can_focus(false);
    scale.add_css_class("launcher-level-scale");
    let value = Label::new(Some(&format!("{level}%")));
    value.add_css_class("launcher-level-value");

    let syncing = Rc::new(Cell::new(false));
    let touched = Rc::new(Cell::new(None));
    let pending = Rc::new(Cell::new(false));
    {
        let action = item.action.clone();
        let (syncing, touched, value) = (syncing.clone(), touched.clone(), value.clone());
        scale.connect_value_changed(move |scale| {
            let percent = scale.value().round() as u8;
            value.set_label(&format!("{percent}%"));
            if syncing.get() {
                return;
            }
            touched.set(Some(Instant::now()));
            if pending.replace(true) {
                return;
            }
            let (action, pending, scale) = (action.clone(), pending.clone(), scale.clone());
            glib::timeout_add_local_once(LEVEL_SEND_INTERVAL, move || {
                pending.set(false);
                let percent = scale.value().round() as u8;
                LEVEL_SINK.with(|sink| {
                    if let Some(sink) = sink.borrow().as_ref() {
                        sink(&action, percent);
                    }
                });
            });
        });
    }

    let slider = GtkBox::new(Orientation::Horizontal, 8);
    slider.add_css_class("launcher-level");
    slider.append(&scale);
    LEVEL_ROWS.with(|rows| {
        rows.borrow_mut().push(LevelRow {
            index,
            scale,
            value: value.clone(),
            syncing,
            touched,
        })
    });
    (slider, value)
}

thread_local! {
    static ICON_LOOKUP_TIME: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    // GLib monotonic-clock timestamps (microseconds) so the frame profiler can
    // line each rebuild up against the frame the compositor actually presents.
    static LAST_REBUILD_DONE_US: Cell<i64> = const { Cell::new(0) };
    static LAST_FRAME_TIME_US: Cell<i64> = const { Cell::new(0) };
}

pub(crate) fn profiling_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("LUMA_PROFILE").is_some())
}

fn ms(value: Duration) -> f64 {
    value.as_secs_f64() * 1000.0
}

// Logs how long the compositor takes to turn a finished rebuild into a
// presented frame, isolating window-manager/present latency from the
// main-thread rebuild cost measured in `rebuild_results`.
pub(crate) fn install_frame_profiler(window: &gtk4::ApplicationWindow) {
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

// Preserve hover, keyboard focus, scroll position, and CSS transitions when a
// live-context poll leaves the visible results unchanged. Actions are refreshed
// separately by the controller even when the widgets can be reused.
pub(crate) fn same_presentation(previous: &[ResultItem], next: &[ResultItem]) -> bool {
    previous.len() == next.len()
        && previous.iter().zip(next).all(|(left, right)| {
            same_result(left, right)
                && left.icon_name == right.icon_name
                && left.thumbnail == right.thumbnail
                && left.level.is_some() == right.level.is_some()
                // A level's accessory is its live value, which `sync_levels`
                // updates in place so a drag is never interrupted.
                && (left.level.is_some() || left.accessory == right.accessory)
                && left.badges == right.badges
                && matches!(left.action, Action::None) == matches!(right.action, Action::None)
        })
}

pub(crate) fn preserved_selection_index(
    previous: Option<&ResultItem>,
    results: &[ResultItem],
) -> usize {
    previous
        .and_then(|prev| results.iter().position(|item| same_result(item, prev)))
        .unwrap_or(0)
}

pub(crate) fn rebuild_results(
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
    LEVEL_ROWS.with(|rows| rows.borrow_mut().clear());
    let teardown = teardown_start.elapsed();

    let build_start = Instant::now();
    for (index, item) in results.iter().enumerate() {
        let row = build_row(item, index);
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

pub(crate) fn finalize_loaded_results(
    results: Vec<ResultItem>,
    query: &QueryInput,
) -> Vec<ResultItem> {
    let mut results = sort_visible_results(results);
    if results.is_empty() {
        results.push(no_results_item(query));
    }
    results
}

pub(crate) fn pending_deferred_results(results: Vec<ResultItem>) -> Vec<ResultItem> {
    sort_visible_results(results)
}

pub(crate) fn background_processing_after_update(
    phase: SearchUpdatePhase,
    has_deferred_plan: bool,
) -> bool {
    phase == SearchUpdatePhase::Immediate && has_deferred_plan
}

pub(crate) fn set_background_processing(spinner: &Spinner, active: bool) {
    spinner.set_visible(active);
    if active {
        spinner.start();
    } else {
        spinner.stop();
    }
}

fn build_row(item: &ResultItem, index: usize) -> ListBoxRow {
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
    match item.thumbnail.as_deref() {
        Some(path) => {
            let picture = Picture::for_filename(path);
            picture.set_content_fit(ContentFit::Cover);
            picture.set_size_request(56, 34);
            picture.add_css_class("launcher-thumbnail");
            icon_wrap.add_css_class("has-thumbnail");
            icon_wrap.set_overflow(Overflow::Hidden);
            icon_wrap.append(&picture);
        }
        None => icon_wrap.append(&icon),
    }

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

    let level = item
        .level
        .map(|level| level_slider(item, index, level.min(100)));
    if let Some((_, value)) = &level {
        title_row.append(value);
    } else if let Some(accessory) = item
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
    if let Some((slider, _)) = level {
        row.add_css_class("launcher-row-level");
        text_col.append(&slider);
    } else if !subtitle.is_empty() {
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

pub(crate) fn row_tooltip_text(item: &ResultItem) -> Option<String> {
    let subtitle = item.subtitle.trim();
    let source = item.source.trim();

    match (subtitle.is_empty(), source.is_empty()) {
        (true, true) => None,
        (false, true) => Some(subtitle.to_string()),
        (true, false) => Some(source.to_string()),
        (false, false) => Some(format!("{subtitle}\n{source}")),
    }
}

pub(crate) fn move_selection(
    list: &ListBox,
    scroller: &ScrolledWindow,
    delta: i32,
    result_count: i32,
) {
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

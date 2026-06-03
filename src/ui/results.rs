use crate::SearchUpdatePhase;
use crate::model::{Action, EntryBadge, QueryInput, ResultItem};
use crate::sources::{no_results_item, sort_and_limit_results};
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Image, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow, Spinner,
};
use std::cell::Cell;
use std::time::{Duration, Instant};

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

pub(crate) fn finalize_loaded_results(
    results: Vec<ResultItem>,
    query: &QueryInput,
) -> Vec<ResultItem> {
    let mut results = sort_and_limit_results(results);
    if results.is_empty() {
        results.push(no_results_item(query));
    }
    results
}

pub(crate) fn pending_deferred_results(results: Vec<ResultItem>) -> Vec<ResultItem> {
    sort_and_limit_results(results)
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

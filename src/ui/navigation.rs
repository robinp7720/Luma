use crate::model::{QueryInput, SearchMode};
use gtk4::{self as gtk, gdk, prelude::*};

const MODES: [(&str, &str, SearchMode); 5] = [
    ("All", "all:", SearchMode::All),
    ("Apps", "app:", SearchMode::Apps),
    ("Windows", "win:", SearchMode::Windows),
    ("Controls", "control:", SearchMode::Controls),
    ("Files", "file:", SearchMode::Files),
];

pub(crate) const ALL_MODES: &[(&str, &str)] = &[
    ("All", "all:"),
    ("Applications", "app:"),
    ("Windows", "win:"),
    ("Controls", "control:"),
    ("Files", "file:"),
    ("Passwords", "pass:"),
    ("Email", "mail:"),
    ("SSH", "ssh:"),
    ("Commands", "cmd:"),
    ("Web", "web:"),
    ("Calculator", "calc:"),
    ("Packages", "pkg:"),
    ("Keyboard shortcuts", "keys:"),
    ("Clipboard", "clip:"),
    ("Timers", "timer:"),
    ("Notifications", "notif:"),
    ("Wallpapers", "wall:"),
    ("Bookmarks", "bookmark:"),
    ("Recent Files", "recent:"),
];

pub(crate) fn select_prefix(entry: &gtk::Entry, default_mode: SearchMode, prefix: &str) {
    let text = QueryInput::parse(&entry.text(), default_mode).text;
    entry.set_text(&format!("{prefix} {text}"));
    entry.grab_focus();
    entry.set_position(-1);
}

#[derive(Clone)]
pub(crate) struct SearchNavigation {
    pub root: gtk::Box,
    buttons: Vec<gtk::ToggleButton>,
    picker: gtk::MenuButton,
}

impl SearchNavigation {
    pub fn new(entry: &gtk::Entry, default_mode: SearchMode) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        root.add_css_class("launcher-navigation");
        let mut buttons = Vec::<gtk::ToggleButton>::new();
        for (index, (label, _, mode)) in MODES.iter().enumerate() {
            let button = gtk::ToggleButton::with_label(label);
            button.add_css_class("launcher-mode");
            button.set_focus_on_click(false);
            button.set_tooltip_text(Some(&format!("{label} · Ctrl+{}", index + 1)));
            if let Some(first) = buttons.first() {
                button.set_group(Some(first));
            }
            let entry_for_click = entry.downgrade();
            let mode = *mode;
            button.connect_clicked(move |_| {
                let Some(entry_for_click) = entry_for_click.upgrade() else {
                    return;
                };
                let query = query_for_mode(&entry_for_click.text(), default_mode, mode);
                entry_for_click.grab_focus();
                entry_for_click.set_text(&query);
                entry_for_click.set_position(-1);
                entry_for_click.select_region(-1, -1);
            });
            root.append(&button);
            buttons.push(button);
        }
        let picker = gtk::MenuButton::builder()
            .label("More…")
            .tooltip_text("Search all modes · Ctrl+K")
            .build();
        if let Some(button) = picker.first_child() {
            button.add_css_class("launcher-mode");
        }
        let popover = gtk::Popover::new();
        popover.add_css_class("launcher-mode-popover");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let filter = gtk::SearchEntry::builder()
            .placeholder_text("Find a mode…")
            .build();
        content.append(&filter);
        let choices = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let mut mode_buttons = Vec::new();
        for &(label, prefix) in ALL_MODES {
            let button = gtk::Button::with_label(&format!("{label}  {prefix}"));
            button.set_halign(gtk::Align::Fill);
            button.add_css_class("launcher-mode-choice");
            let entry = entry.downgrade();
            let popover = popover.downgrade();
            button.connect_clicked(move |_| {
                let (Some(entry), Some(popover)) = (entry.upgrade(), popover.upgrade()) else {
                    return;
                };
                popover.popdown();
                select_prefix(&entry, default_mode, prefix);
            });
            choices.append(&button);
            mode_buttons.push((label.to_lowercase(), prefix.to_string(), button));
        }
        let mode_buttons = std::rc::Rc::new(mode_buttons);
        let filtered_buttons = mode_buttons.clone();
        filter.connect_changed(move |filter| {
            let text = filter.text().to_lowercase();
            for (label, prefix, button) in filtered_buttons.iter() {
                button.set_visible(label.contains(&text) || prefix.contains(&text));
            }
        });
        let keys = gtk::EventControllerKey::new();
        let key_buttons = mode_buttons.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Down {
                if let Some((_, _, button)) = key_buttons
                    .iter()
                    .find(|(_, _, button)| button.is_visible())
                {
                    button.grab_focus();
                }
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        filter.add_controller(keys);
        filter.connect_activate(move |_| {
            if let Some((_, _, button)) = mode_buttons
                .iter()
                .find(|(_, _, button)| button.is_visible())
            {
                button.emit_clicked();
            }
        });
        let scroll = gtk::ScrolledWindow::builder()
            .min_content_width(280)
            .max_content_height(360)
            .propagate_natural_height(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&choices)
            .build();
        content.append(&scroll);
        popover.set_child(Some(&content));
        popover.connect_show(move |_| {
            filter.set_text("");
            filter.grab_focus();
        });
        let entry_for_close = entry.downgrade();
        popover.connect_closed(move |_| {
            let Some(entry) = entry_for_close.upgrade() else {
                return;
            };
            let Some(window) = entry.root().and_downcast::<gtk::ApplicationWindow>() else {
                return;
            };
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
                if window.is_visible() && !window.is_active() {
                    window.close();
                }
            });
        });
        picker.set_popover(Some(&popover));
        root.append(&picker);
        let navigation = Self {
            root,
            buttons,
            picker,
        };
        navigation.update(entry, default_mode);
        let navigation_for_query = navigation.clone();
        entry.connect_changed(move |entry| navigation_for_query.update(entry, default_mode));
        navigation
    }

    fn update(&self, entry: &gtk::Entry, default_mode: SearchMode) {
        let mode = QueryInput::parse(&entry.text(), default_mode).mode;
        for (button, (_, _, candidate)) in self.buttons.iter().zip(MODES) {
            button.set_active(candidate == mode);
        }
    }

    pub fn picker_visible(&self) -> bool {
        self.picker
            .popover()
            .is_some_and(|popover| popover.is_visible())
    }

    pub fn activate_shortcut(&self, key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
        if !self.root.is_sensitive() || !modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
            return false;
        }
        if key == gdk::Key::k {
            self.picker.popup();
            return true;
        }
        let index = match key {
            gdk::Key::_1 => 0,
            gdk::Key::_2 => 1,
            gdk::Key::_3 => 2,
            gdk::Key::_4 => 3,
            gdk::Key::_5 => 4,
            _ => return false,
        };
        self.buttons[index].emit_clicked();
        true
    }
}

fn query_for_mode(raw: &str, default_mode: SearchMode, target: SearchMode) -> String {
    let query = QueryInput::parse(raw, default_mode);
    let prefix = MODES.iter().find(|(_, _, mode)| *mode == target).unwrap().1;
    if target == SearchMode::All && default_mode == SearchMode::All {
        query.text
    } else {
        format!("{prefix} {}", query.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_discoverable_mode_has_a_working_prefix() {
        for &(label, prefix) in ALL_MODES {
            let query = QueryInput::parse(&format!("{prefix} example"), SearchMode::All);
            assert_eq!(query.text, "example", "{label} prefix must be recognized");
            if label != "All" && label != "Bookmarks" && label != "Recent Files" {
                assert_ne!(query.mode, SearchMode::All, "{label} must narrow the mode");
            }
        }
    }

    #[test]
    fn switching_scope_preserves_search_text_and_replaces_the_prefix() {
        assert_eq!(
            query_for_mode("win: firefox", SearchMode::All, SearchMode::Apps),
            "app: firefox"
        );
        assert_eq!(
            query_for_mode("file: project notes", SearchMode::All, SearchMode::All),
            "project notes"
        );
    }

    #[test]
    fn all_can_escape_a_dedicated_launch_mode() {
        let query = query_for_mode("firefox", SearchMode::Windows, SearchMode::All);
        assert_eq!(
            QueryInput::parse(&query, SearchMode::Windows).mode,
            SearchMode::All
        );
        assert_eq!(
            QueryInput::parse(&query, SearchMode::Windows).text,
            "firefox"
        );
    }
}

use crate::model::{QueryInput, SearchMode};
use gtk4::{self as gtk, gdk, prelude::*};

const MODES: [(&str, &str, SearchMode); 5] = [
    ("All", "all:", SearchMode::All),
    ("Apps", "app:", SearchMode::Apps),
    ("Windows", "win:", SearchMode::Windows),
    ("Controls", "control:", SearchMode::Controls),
    ("Files", "file:", SearchMode::Files),
];

#[derive(Clone)]
pub(crate) struct SearchNavigation {
    pub root: gtk::Box,
    buttons: Vec<gtk::ToggleButton>,
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
        let navigation = Self { root, buttons };
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

    pub fn activate_shortcut(&self, key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
        if !self.root.is_sensitive() || !modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
            return false;
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

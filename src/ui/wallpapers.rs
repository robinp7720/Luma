use crate::config::WallpaperConfig;
use crate::sources::wallpapers::{WallpaperOutput, WallpaperSession, command_available};
use gtk4::{self as gtk, prelude::*};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(crate) struct WallpaperControls {
    pub root: gtk::Box,
    pub display: gtk::DropDown,
    pub theme: gtk::CheckButton,
    updating: Rc<Cell<bool>>,
    outputs: Rc<RefCell<Vec<WallpaperOutput>>>,
}
impl WallpaperControls {
    pub fn new(config: &WallpaperConfig) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&gtk::Label::new(Some("Display")));
        let display = gtk::DropDown::from_strings(&[]);
        display.set_hexpand(true);
        display.set_tooltip_text(Some("Display to apply the selected wallpaper to"));
        row.append(&display);
        root.append(&row);
        let theme = gtk::CheckButton::with_label("Update desktop colors when applying");
        theme.set_active(config.theme.update_on_selection);
        theme.set_sensitive(command_available(&config.theme.command));
        root.append(&theme);
        root.set_visible(false);
        Self {
            root,
            display,
            theme,
            updating: Rc::new(Cell::new(false)),
            outputs: Default::default(),
        }
    }
    pub fn sync(&self, session: &Mutex<WallpaperSession>) {
        // Release the mutex before setting GTK properties (signals are synchronous).
        let state = session.lock().unwrap().clone();
        self.updating.set(true);
        if *self.outputs.borrow() != state.outputs {
            let labels: Vec<_> = state.outputs.iter().map(|o| o.label.as_str()).collect();
            self.display.set_model(Some(&gtk::StringList::new(&labels)));
            self.outputs.replace(state.outputs.clone());
        }
        let selected = state
            .outputs
            .iter()
            .position(|o| Some(&o.id) == state.selected_output.as_ref());
        self.display.set_selected(
            selected
                .map(|i| i as u32)
                .unwrap_or(gtk::INVALID_LIST_POSITION),
        );
        self.display.set_sensitive(!state.outputs.is_empty());
        self.updating.set(false);
    }
    pub fn connect(
        &self,
        session: Arc<Mutex<WallpaperSession>>,
        refresh: impl Fn() + Clone + 'static,
    ) -> Vec<(gtk::glib::Object, gtk::glib::SignalHandlerId)> {
        let updating = self.updating.clone();
        let outputs = self.outputs.clone();
        let state = session.clone();
        let on_display = refresh.clone();
        let display_handler = self.display.connect_selected_notify(move |display| {
            if updating.get() {
                return;
            }
            let id = outputs
                .borrow()
                .get(display.selected() as usize)
                .map(|o| o.id.clone());
            state.lock().unwrap().selected_output = id;
            on_display();
        });
        let theme_handler = self.theme.connect_toggled(move |theme| {
            session.lock().unwrap().update_theme = Some(theme.is_active());
            refresh();
        });
        vec![
            (self.display.clone().upcast(), display_handler),
            (self.theme.clone().upcast(), theme_handler),
        ]
    }
}

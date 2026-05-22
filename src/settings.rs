use crate::app_config;
use crate::config::{EmailBackendPreference, FileSearchBackendChoice};
use crate::model::SearchMode;
use anyhow::{Context, Result};
use gtk4::prelude::*;
use gtk4::{
    Align, ApplicationWindow, Box as GtkBox, Button, DropDown, Entry, Label, Orientation,
    ScrolledWindow, Separator, SpinButton, Switch,
};

pub fn open_config_panel(parent: &ApplicationWindow) -> Result<()> {
    let Some(app) = parent.application() else {
        anyhow::bail!("launcher application is unavailable");
    };

    if let Some(existing) = app
        .windows()
        .into_iter()
        .find(|window| window.title().as_deref() == Some("Luma Settings"))
    {
        existing.present();
        return Ok(());
    }

    let config_store = app_config().context("config store unavailable")?;
    let config = config_store.current();
    let config_path = config_store
        .path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "~/.config/Luma/config.json".to_string());

    let window = ApplicationWindow::builder()
        .application(&app)
        .title("Luma Settings")
        .default_width(980)
        .default_height(760)
        .resizable(true)
        .transient_for(parent)
        .modal(false)
        .build();

    let root = GtkBox::new(Orientation::Vertical, 0);
    root.add_css_class("settings-shell");

    let header = build_header(&config_path);
    root.append(&header);

    let scroller = ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vexpand(true)
        .build();
    scroller.add_css_class("settings-scroller");

    let content = GtkBox::new(Orientation::Vertical, 18);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    scroller.set_child(Some(&content));

    let defaults_card = build_card(
        "Defaults",
        "Choose the launcher's initial mode and the overall surface sizing.",
    );
    let default_mode = build_mode_dropdown(config.default_mode);
    let width_spin = build_spin(config.ui.width_px as f64, 520.0, 1600.0, 4.0);
    let height_spin = build_spin(config.ui.height_px as f64, 320.0, 1200.0, 4.0);
    let top_margin_spin = build_spin(config.ui.top_margin_px as f64, 0.0, 240.0, 1.0);
    let surface_margin_spin = build_spin(config.ui.surface_margin_px as f64, 0.0, 240.0, 1.0);
    let layer_shell_switch = build_switch(config.ui.use_layer_shell);

    append_setting_row(
        &defaults_card,
        "Default mode",
        "The mode used when Luma starts without a `--mode` argument.",
        &default_mode,
    );
    append_setting_row(
        &defaults_card,
        "Launcher width",
        "The launcher panel width in pixels.",
        &width_spin,
    );
    append_setting_row(
        &defaults_card,
        "Launcher height",
        "The launcher panel height in pixels.",
        &height_spin,
    );
    append_setting_row(
        &defaults_card,
        "Top margin",
        "Extra compositor margin applied when layer-shell is enabled.",
        &top_margin_spin,
    );
    append_setting_row(
        &defaults_card,
        "Surface margin",
        "Outer padding around the launcher contents.",
        &surface_margin_spin,
    );
    append_setting_row(
        &defaults_card,
        "Layer shell",
        "Use an overlay surface instead of a normal floating window.",
        &layer_shell_switch,
    );

    let sources_card = build_card(
        "Sources",
        "Enable or disable the built-in search surfaces individually.",
    );
    let apps_switch = build_switch(config.sources.apps);
    let windows_switch = build_switch(config.sources.windows);
    let files_switch = build_switch(config.sources.files);
    let pass_switch = build_switch(config.sources.pass);
    let email_switch = build_switch(config.sources.email);
    let ssh_switch = build_switch(config.sources.ssh);
    let commands_switch = build_switch(config.sources.commands);
    let bookmarks_switch = build_switch(config.sources.bookmarks);
    let recents_switch = build_switch(config.sources.recents);
    let web_switch = build_switch(config.sources.web);
    let calc_switch = build_switch(config.sources.calc);
    let power_switch = build_switch(config.sources.power);
    let controls_switch = build_switch(config.sources.controls);

    append_setting_row(
        &sources_card,
        "Applications",
        "Search desktop entries and launch apps.",
        &apps_switch,
    );
    append_setting_row(
        &sources_card,
        "Windows",
        "Search active windows from the compositor.",
        &windows_switch,
    );
    append_setting_row(
        &sources_card,
        "Files",
        "Search indexed files via LocalSearch or Tracker.",
        &files_switch,
    );
    append_setting_row(
        &sources_card,
        "Passwords",
        "Search and manage pass entries.",
        &pass_switch,
    );
    append_setting_row(
        &sources_card,
        "Email",
        "Search local Thunderbird mail, Evolution mail, and local maildir messages.",
        &email_switch,
    );
    append_setting_row(
        &sources_card,
        "SSH",
        "Search known hosts and `.ssh/config`.",
        &ssh_switch,
    );
    append_setting_row(
        &sources_card,
        "Commands",
        "Run commands and surface executable suggestions.",
        &commands_switch,
    );
    append_setting_row(
        &sources_card,
        "Bookmarks",
        "Search browser bookmarks from Firefox and Chromium profiles.",
        &bookmarks_switch,
    );
    append_setting_row(
        &sources_card,
        "Recent files",
        "Search recently used files from the desktop registry.",
        &recents_switch,
    );
    append_setting_row(
        &sources_card,
        "Web",
        "Offer browser searches and URL opening.",
        &web_switch,
    );
    append_setting_row(
        &sources_card,
        "Calculator",
        "Evaluate expressions with qalc.",
        &calc_switch,
    );
    append_setting_row(
        &sources_card,
        "Power actions",
        "Show lock, suspend, logout, reboot, and shutdown actions.",
        &power_switch,
    );
    append_setting_row(
        &sources_card,
        "Desktop controls",
        "Show media, audio, Bluetooth, network, power profile, screenshots, color picker, and notification actions.",
        &controls_switch,
    );

    let email_card = build_card(
        "Email",
        "Choose which email sources Luma indexes and how it reaches Evolution mail.",
    );
    let email_backend = build_email_backend_dropdown(config.integrations.email.preferred_backend);
    let thunderbird_email_switch = build_switch(config.integrations.email.thunderbird_enabled);
    let evolution_email_switch = build_switch(config.integrations.email.evolution_enabled);
    let local_mail_switch = build_switch(config.integrations.email.local_mail_enabled);
    let evolution_helper_command_entry = build_entry(
        config
            .integrations
            .email
            .evolution_helper_command
            .as_deref()
            .unwrap_or(""),
    );
    let evolution_helper_timeout_spin = build_spin(
        config.integrations.email.evolution_helper_timeout_ms as f64,
        250.0,
        30_000.0,
        250.0,
    );

    append_setting_row(
        &email_card,
        "Preferred backend",
        "Pick which email source should rank first in mixed results.",
        &email_backend,
    );
    append_setting_row(
        &email_card,
        "Thunderbird",
        "Search Thunderbird's local message database when available.",
        &thunderbird_email_switch,
    );
    append_setting_row(
        &email_card,
        "Evolution",
        "Search mail through the Evolution Data Server helper.",
        &evolution_email_switch,
    );
    append_setting_row(
        &email_card,
        "Local maildir",
        "Search extra maildir or mbox-style roots from the filesystem.",
        &local_mail_switch,
    );
    append_setting_row(
        &email_card,
        "Evolution helper",
        "Command used to talk to the Evolution mail helper. Blank uses luma-mail-eds.",
        &evolution_helper_command_entry,
    );
    append_setting_row(
        &email_card,
        "Helper timeout",
        "Timeout in milliseconds for helper search and action calls.",
        &evolution_helper_timeout_spin,
    );

    let integrations_card = build_card(
        "Integrations",
        "Tune external commands, paths, and search providers.",
    );
    let web_search_entry = build_entry(&config.integrations.web_search_url);
    let ssh_terminal_entry = build_entry(&config.integrations.ssh_terminal);
    let password_store_entry = build_entry(
        config
            .integrations
            .password_store_dir
            .as_deref()
            .unwrap_or(""),
    );
    let password_clip_spin = build_spin(
        config.integrations.password_clip_timeout_seconds as f64,
        1.0,
        600.0,
        1.0,
    );
    let file_backend = build_backend_dropdown(config.integrations.file_search_backend);
    let file_query_spin = build_spin(
        config.integrations.file_search_min_query_chars as f64,
        1.0,
        12.0,
        1.0,
    );

    append_setting_row(
        &integrations_card,
        "Web search URL",
        "Base URL used for the web search action.",
        &web_search_entry,
    );
    append_setting_row(
        &integrations_card,
        "SSH terminal",
        "Terminal command used when opening an SSH session.",
        &ssh_terminal_entry,
    );
    append_setting_row(
        &integrations_card,
        "Password store path",
        "Override `PASSWORD_STORE_DIR` with a fixed password-store path.",
        &password_store_entry,
    );
    append_setting_row(
        &integrations_card,
        "Password clip timeout",
        "Seconds before copied password data expires.",
        &password_clip_spin,
    );
    append_setting_row(
        &integrations_card,
        "File search backend",
        "Choose the backend used for indexed file search.",
        &file_backend,
    );
    append_setting_row(
        &integrations_card,
        "File query length",
        "Minimum query length before shelling out to the file index.",
        &file_query_spin,
    );

    content.append(&defaults_card);
    content.append(&sources_card);
    content.append(&email_card);
    content.append(&integrations_card);
    root.append(&scroller);

    let footer = GtkBox::new(Orientation::Horizontal, 10);
    footer.set_margin_top(12);
    footer.set_margin_bottom(14);
    footer.set_margin_start(18);
    footer.set_margin_end(18);
    footer.set_halign(Align::End);

    let status = Label::new(None);
    status.add_css_class("settings-status");
    status.set_hexpand(true);
    status.set_xalign(0.0);

    let discard_button = Button::with_label("Close");
    let save_button = Button::with_label("Save");
    save_button.add_css_class("suggested-action");

    {
        let window = window.clone();
        discard_button.connect_clicked(move |_| {
            window.close();
        });
    }

    {
        let status = status.clone();
        let config_store = config_store.clone();
        let window = window.clone();
        save_button.connect_clicked(move |_| {
            let mut next = config_store.current();
            next.default_mode = mode_from_dropdown(default_mode.selected());
            next.ui.width_px = width_spin.value().round() as i32;
            next.ui.height_px = height_spin.value().round() as i32;
            next.ui.top_margin_px = top_margin_spin.value().round() as i32;
            next.ui.surface_margin_px = surface_margin_spin.value().round() as i32;
            next.ui.use_layer_shell = layer_shell_switch.is_active();

            next.sources.apps = apps_switch.is_active();
            next.sources.windows = windows_switch.is_active();
            next.sources.files = files_switch.is_active();
            next.sources.pass = pass_switch.is_active();
            next.sources.email = email_switch.is_active();
            next.sources.ssh = ssh_switch.is_active();
            next.sources.commands = commands_switch.is_active();
            next.sources.bookmarks = bookmarks_switch.is_active();
            next.sources.recents = recents_switch.is_active();
            next.sources.web = web_switch.is_active();
            next.sources.calc = calc_switch.is_active();
            next.sources.power = power_switch.is_active();
            next.sources.controls = controls_switch.is_active();

            next.integrations.email.preferred_backend =
                email_backend_from_dropdown(email_backend.selected());
            next.integrations.email.thunderbird_enabled = thunderbird_email_switch.is_active();
            next.integrations.email.evolution_enabled = evolution_email_switch.is_active();
            next.integrations.email.local_mail_enabled = local_mail_switch.is_active();
            next.integrations.email.evolution_helper_command =
                non_empty_text(&evolution_helper_command_entry);
            next.integrations.email.evolution_helper_timeout_ms =
                evolution_helper_timeout_spin.value().round().max(250.0) as u64;

            next.integrations.web_search_url = web_search_entry.text().trim().to_string();
            next.integrations.ssh_terminal = ssh_terminal_entry.text().trim().to_string();
            next.integrations.password_store_dir = non_empty_text(&password_store_entry);
            next.integrations.password_clip_timeout_seconds =
                password_clip_spin.value().round().max(1.0) as u64;
            next.integrations.file_search_backend = backend_from_dropdown(file_backend.selected());
            next.integrations.file_search_min_query_chars =
                file_query_spin.value().round().max(1.0) as usize;

            config_store.replace(next);
            match config_store.save() {
                Ok(()) => {
                    status.set_text(&format!("Saved to {config_path}"));
                    window.set_title(Some("Luma Settings"));
                }
                Err(error) => {
                    status.set_text(&format!("Save failed: {error}"));
                }
            }
        });
    }

    footer.append(&status);
    footer.append(&discard_button);
    footer.append(&save_button);

    root.append(&footer);
    window.set_child(Some(&root));
    window.present();
    Ok(())
}

fn build_header(config_path: &str) -> GtkBox {
    let header = GtkBox::new(Orientation::Vertical, 6);
    header.set_margin_top(20);
    header.set_margin_bottom(8);
    header.set_margin_start(20);
    header.set_margin_end(20);
    header.add_css_class("settings-header");

    let title = Label::new(Some("Luma Settings"));
    title.set_halign(Align::Start);
    title.add_css_class("settings-title");

    let subtitle = Label::new(Some(&format!(
        "Tune the launcher in one place. Settings are saved to {}. Some appearance changes apply on the next launch.",
        config_path
    )));
    subtitle.set_halign(Align::Start);
    subtitle.set_wrap(true);
    subtitle.add_css_class("settings-subtitle");

    header.append(&title);
    header.append(&subtitle);
    header.append(&Separator::new(Orientation::Horizontal));
    header
}

fn build_card(title: &str, subtitle: &str) -> GtkBox {
    let card = GtkBox::new(Orientation::Vertical, 12);
    card.add_css_class("settings-card");
    card.set_margin_top(0);
    card.set_margin_bottom(0);
    card.set_margin_start(0);
    card.set_margin_end(0);

    let header = GtkBox::new(Orientation::Vertical, 4);
    let title_label = Label::new(Some(title));
    title_label.set_halign(Align::Start);
    title_label.add_css_class("settings-card-title");
    let subtitle_label = Label::new(Some(subtitle));
    subtitle_label.set_halign(Align::Start);
    subtitle_label.set_wrap(true);
    subtitle_label.add_css_class("settings-card-subtitle");

    header.append(&title_label);
    header.append(&subtitle_label);

    card.append(&header);
    card
}

fn append_setting_row<T: IsA<gtk4::Widget>>(
    card: &GtkBox,
    label: &str,
    subtitle: &str,
    control: &T,
) {
    let row = GtkBox::new(Orientation::Horizontal, 14);
    row.add_css_class("settings-row");

    let text = GtkBox::new(Orientation::Vertical, 3);
    text.set_hexpand(true);
    let title = Label::new(Some(label));
    title.set_halign(Align::Start);
    title.add_css_class("settings-row-title");
    let subtitle_label = Label::new(Some(subtitle));
    subtitle_label.set_halign(Align::Start);
    subtitle_label.set_wrap(true);
    subtitle_label.add_css_class("settings-row-subtitle");

    text.append(&title);
    text.append(&subtitle_label);

    row.append(&text);
    row.append(control);
    card.append(&row);
}

fn build_switch(active: bool) -> Switch {
    let switch = Switch::new();
    switch.set_active(active);
    switch
}

fn build_entry(text: &str) -> Entry {
    let entry = Entry::new();
    entry.set_hexpand(true);
    entry.set_text(text);
    entry
}

fn build_spin(value: f64, min: f64, max: f64, step: f64) -> SpinButton {
    let spin = SpinButton::with_range(min, max, step);
    spin.set_value(value);
    spin.set_digits(0);
    spin.set_hexpand(false);
    spin
}

fn build_mode_dropdown(current: SearchMode) -> DropDown {
    let dropdown = DropDown::from_strings(&[
        "All",
        "Applications",
        "Windows",
        "Files",
        "SSH",
        "Passwords",
        "Email",
        "Commands",
        "Web",
        "Calculator",
        "Controls",
    ]);
    dropdown.set_selected(search_mode_index(current) as u32);
    dropdown
}

fn build_backend_dropdown(current: FileSearchBackendChoice) -> DropDown {
    let dropdown = DropDown::from_strings(&["Auto", "LocalSearch", "Tracker3", "Disabled"]);
    dropdown.set_selected(backend_index(current) as u32);
    dropdown
}

fn build_email_backend_dropdown(current: EmailBackendPreference) -> DropDown {
    let dropdown = DropDown::from_strings(&["Thunderbird", "Evolution", "Local mail", "Auto"]);
    dropdown.set_selected(email_backend_index(current) as u32);
    dropdown
}

fn mode_from_dropdown(index: u32) -> SearchMode {
    match index {
        0 => SearchMode::All,
        1 => SearchMode::Apps,
        2 => SearchMode::Windows,
        3 => SearchMode::Files,
        4 => SearchMode::Ssh,
        5 => SearchMode::Pass,
        6 => SearchMode::Email,
        7 => SearchMode::Commands,
        8 => SearchMode::Web,
        9 => SearchMode::Calc,
        _ => SearchMode::Controls,
    }
}

fn search_mode_index(mode: SearchMode) -> usize {
    match mode {
        SearchMode::All => 0,
        SearchMode::Apps => 1,
        SearchMode::Windows => 2,
        SearchMode::Files => 3,
        SearchMode::Ssh => 4,
        SearchMode::Pass => 5,
        SearchMode::Email => 6,
        SearchMode::Commands => 7,
        SearchMode::Web => 8,
        SearchMode::Calc => 9,
        SearchMode::Controls => 10,
    }
}

fn backend_from_dropdown(index: u32) -> FileSearchBackendChoice {
    match index {
        0 => FileSearchBackendChoice::Auto,
        1 => FileSearchBackendChoice::LocalSearch,
        2 => FileSearchBackendChoice::Tracker3,
        _ => FileSearchBackendChoice::Disabled,
    }
}

fn backend_index(choice: FileSearchBackendChoice) -> usize {
    match choice {
        FileSearchBackendChoice::Auto => 0,
        FileSearchBackendChoice::LocalSearch => 1,
        FileSearchBackendChoice::Tracker3 => 2,
        FileSearchBackendChoice::Disabled => 3,
    }
}

fn email_backend_from_dropdown(index: u32) -> EmailBackendPreference {
    match index {
        0 => EmailBackendPreference::Thunderbird,
        1 => EmailBackendPreference::Evolution,
        2 => EmailBackendPreference::LocalMail,
        _ => EmailBackendPreference::Auto,
    }
}

fn email_backend_index(choice: EmailBackendPreference) -> usize {
    match choice {
        EmailBackendPreference::Thunderbird => 0,
        EmailBackendPreference::Evolution => 1,
        EmailBackendPreference::LocalMail => 2,
        EmailBackendPreference::Auto => 3,
    }
}

fn non_empty_text(entry: &Entry) -> Option<String> {
    let text = entry.text();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

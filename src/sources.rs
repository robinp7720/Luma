use crate::config::{ConfigStore, EmailBackendPreference, FileSearchBackendChoice, LauncherConfig};
use crate::mail_eds_protocol::{MailEdsMessageSummary, MailEdsSearchResponse};
use crate::model::{
    Action, DesktopControlOperation, EntryBadge, PackageManager, PowerOperation, QueryInput,
    ResultItem, SearchMode, SourceFilter, browser_target, password_url_draft, score_text,
};
use crate::prediction::{PredictionStore, StoredPrediction};
use anyhow::{Context, Result};
use gtk4::gio;
use gtk4::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod windows;
pub use windows::{focus_window, focused_window_target};
use windows::{
    load_windows, window_prediction_key, window_result_item, window_result_item_with_score,
};

const MAX_APPS: usize = 8;
const MAX_WINDOWS: usize = 8;
const MAX_FILES: usize = 8;
const MAX_SSH: usize = 6;
const MAX_PASS: usize = 8;
const MAX_COMMANDS: usize = 8;
const MAX_EMAIL: usize = 8;
const MAX_POWER_ACTIONS: usize = 5;
const MAX_BOOKMARKS: usize = 8;
const MAX_RECENTS: usize = 8;
const MAX_CONTROLS: usize = 12;
const MAX_PACKAGES: usize = 8;
const MAX_RESULTS: usize = 24;
const MIN_FILE_QUERY_CHARS: usize = 2;
const MIN_PACKAGE_QUERY_CHARS: usize = 2;

// Base scores per source sit below the primary launcher categories (apps 900,
// pass 880, bookmarks 830) so a strong text match on a deferred result cannot
// leapfrog a normal app/bookmark match. score_text (0..=1000) still orders
// results within and across these lower bands.
const EMAIL_BASE_SCORE: i32 = 300;
const FILE_BASE_SCORE: i32 = 280;

// A typed URL is an explicit navigation intent, so its "Open URL" action always
// ranks first. This base sits above the highest score any other result can reach
// for a typed query: max source base (~1100) + score_text (≤1000) + prediction
// boost (≤500). The empty-query prediction rows (base 2000) never appear here
// because a URL query is non-empty.
const URL_BASE_SCORE: i32 = 10_000;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DeferredSearchPlan {
    pub files: bool,
    pub email: bool,
    pub windows: bool,
    pub controls: bool,
    pub recents: bool,
    pub calc: bool,
    pub packages: bool,
}

impl DeferredSearchPlan {
    pub fn is_empty(self) -> bool {
        !self.files
            && !self.email
            && !self.windows
            && !self.controls
            && !self.recents
            && !self.calc
            && !self.packages
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SearchSnapshot {
    pub query: QueryInput,
    pub immediate_results: Vec<ResultItem>,
    pub deferred: DeferredSearchPlan,
}

struct PowerAction {
    operation: PowerOperation,
    title: &'static str,
    subtitle: &'static str,
    icon_name: &'static str,
    search_terms: &'static [&'static str],
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ControlSnapshot {
    pub media: Option<MediaStatus>,
    pub volume: Option<VolumeStatus>,
    pub bluetooth: Option<BluetoothStatus>,
    pub network: Option<NetworkStatus>,
    pub power_profile: Option<String>,
    pub screen_brightness: Option<u8>,
    pub notifications: Vec<NotificationEntry>,
    pub has_playerctl: bool,
    pub has_wpctl: bool,
    pub has_bluetoothctl: bool,
    pub has_nmcli: bool,
    pub has_powerprofilesctl: bool,
    pub has_dunstctl: bool,
    pub has_grim: bool,
    pub has_slurp: bool,
    pub has_hyprpicker: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MediaStatus {
    pub player: String,
    pub status: String,
    pub artist: String,
    pub title: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VolumeStatus {
    pub percent: u8,
    pub muted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BluetoothStatus {
    pub powered: bool,
    pub connected_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NetworkStatus {
    pub kind: String,
    pub connection: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NotificationEntry {
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub search_blob: String,
}

const POWER_ACTIONS: &[PowerAction] = &[
    PowerAction {
        operation: PowerOperation::Lock,
        title: "Lock",
        subtitle: "Blank the screen and keep the session running",
        icon_name: "system-lock-screen-symbolic",
        search_terms: &["lock", "screen lock", "secure", "power", "session"],
    },
    PowerAction {
        operation: PowerOperation::Suspend,
        title: "Suspend",
        subtitle: "Lock first, then suspend the machine",
        icon_name: "media-playback-pause-symbolic",
        search_terms: &["suspend", "sleep", "standby", "power", "session"],
    },
    PowerAction {
        operation: PowerOperation::Logout,
        title: "Logout",
        subtitle: "Close the current desktop session after confirmation",
        icon_name: "system-log-out-symbolic",
        search_terms: &[
            "logout",
            "log out",
            "sign out",
            "exit session",
            "power",
            "session",
        ],
    },
    PowerAction {
        operation: PowerOperation::Reboot,
        title: "Reboot",
        subtitle: "Restart the system after confirmation",
        icon_name: "system-reboot-symbolic",
        search_terms: &["reboot", "restart", "power", "session"],
    },
    PowerAction {
        operation: PowerOperation::Shutdown,
        title: "Shutdown",
        subtitle: "Power off the system after confirmation",
        icon_name: "system-shutdown-symbolic",
        search_terms: &[
            "shutdown",
            "shut down",
            "poweroff",
            "power off",
            "power",
            "session",
        ],
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FileSearchBackend {
    LocalSearch,
    Tracker3,
}

impl FileSearchBackend {
    fn detect() -> Option<Self> {
        if command_exists("localsearch") {
            Some(Self::LocalSearch)
        } else if command_exists("tracker3") {
            Some(Self::Tracker3)
        } else {
            None
        }
    }

    fn run_search(self, query: &str, limit: usize) -> std::io::Result<std::process::Output> {
        match self {
            Self::LocalSearch => Command::new("localsearch")
                .args(["search", "-f", "--limit", &limit.to_string(), query])
                .output(),
            Self::Tracker3 => Command::new("tracker3")
                .args(["search", "--limit", &limit.to_string(), query])
                .output(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageSearchBackend {
    Pacman,
    Paru,
}

impl PackageSearchBackend {
    fn detect() -> Option<Self> {
        if package_command_path("paru").is_some() {
            Some(Self::Paru)
        } else if package_command_path("pacman").is_some() {
            Some(Self::Pacman)
        } else {
            None
        }
    }

    fn manager(self) -> PackageManager {
        match self {
            Self::Pacman => PackageManager::Pacman,
            Self::Paru => PackageManager::Paru,
        }
    }

    fn run_search(self, query: &str) -> std::io::Result<std::process::Output> {
        match self {
            Self::Pacman => {
                Command::new(package_command_path("pacman").unwrap_or_else(|| "pacman".into()))
                    .args(["-Ss", query])
                    .output()
            }
            Self::Paru => {
                Command::new(package_command_path("paru").unwrap_or_else(|| "paru".into()))
                    .args(["-Ss", query])
                    .output()
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppEntry {
    pub desktop_id: String,
    pub name: String,
    pub description: String,
    pub executable: String,
    pub icon_name: String,
    pub search_blob: String,
}

#[derive(Clone, Debug)]
pub struct PassEntry {
    pub name: String,
    pub search_blob: String,
}

#[derive(Clone, Debug)]
pub struct BookmarkEntry {
    pub title: String,
    pub url: String,
    pub search_blob: String,
}

#[derive(Clone, Debug)]
pub struct RecentFileEntry {
    pub title: String,
    pub path: String,
    pub modified: i64,
    pub search_blob: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PackageEntry {
    pub repository: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub installed: bool,
}

#[derive(Clone, Debug)]
pub struct EmailEntry {
    pub subject: String,
    pub sender: String,
    pub sender_email: Option<String>,
    pub folder: String,
    pub date_label: String,
    pub snippet: String,
    pub unread: bool,
    pub has_attachment: bool,
    pub open_url: String,
    pub reply_url: Option<String>,
    pub compose_url: Option<String>,
    pub search_blob: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EmailBackend {
    Thunderbird,
    Evolution,
    LocalMail,
}

#[derive(Clone, Debug)]
pub struct Sources {
    config: Arc<ConfigStore>,
    apps: Vec<AppEntry>,
    ssh_hosts: Vec<String>,
    pass_entries: Arc<Mutex<Vec<PassEntry>>>,
    commands: Vec<String>,
    bookmarks: Vec<BookmarkEntry>,
    recent_files: Vec<RecentFileEntry>,
    package_search_backend: Option<PackageSearchBackend>,
    thunderbird_email_database_paths: Vec<PathBuf>,
    local_email_entries: Vec<EmailEntry>,
    file_search_backend: Option<FileSearchBackend>,
    pass_available: bool,
    qalc_available: bool,
    predictions: Arc<Mutex<PredictionStore>>,
}

impl Sources {
    pub fn load(config: Arc<ConfigStore>) -> Self {
        Self {
            config: config.clone(),
            apps: load_applications(),
            ssh_hosts: load_ssh_hosts(),
            pass_entries: Arc::new(Mutex::new(load_pass_entries(&config.current()))),
            commands: load_commands(),
            bookmarks: load_browser_bookmarks(),
            recent_files: load_recent_files(),
            package_search_backend: PackageSearchBackend::detect(),
            thunderbird_email_database_paths: load_thunderbird_email_databases(
                &config.current().integrations.email,
            ),
            local_email_entries: load_local_email_entries(&config.current().integrations.email),
            file_search_backend: FileSearchBackend::detect(),
            pass_available: command_exists("pass"),
            qalc_available: command_exists("qalc"),
            predictions: Arc::new(Mutex::new(PredictionStore::load())),
        }
    }

    #[cfg(test)]
    pub fn with_config(config: LauncherConfig) -> Self {
        Self::load(Arc::new(ConfigStore::disabled(config)))
    }

    pub fn refresh_pass_entries(&self) {
        if !self.pass_available {
            return;
        }

        if let Ok(mut entries) = self.pass_entries.lock() {
            *entries = load_pass_entries(&self.current_config());
        }
    }

    pub fn password_entry_exists(&self, entry: &str) -> bool {
        self.pass_entries
            .lock()
            .is_ok_and(|entries| entries.iter().any(|candidate| candidate.name == entry))
    }

    fn current_config(&self) -> LauncherConfig {
        self.config.current()
    }

    fn configured_file_search_backend(&self) -> Option<FileSearchBackend> {
        match self.current_config().integrations.file_search_backend {
            FileSearchBackendChoice::Disabled => None,
            FileSearchBackendChoice::Auto => self.file_search_backend,
            FileSearchBackendChoice::LocalSearch => {
                if command_exists("localsearch") {
                    Some(FileSearchBackend::LocalSearch)
                } else {
                    None
                }
            }
            FileSearchBackendChoice::Tracker3 => {
                if command_exists("tracker3") {
                    Some(FileSearchBackend::Tracker3)
                } else {
                    None
                }
            }
        }
    }

    pub fn search(&self, raw_query: &str, cli_mode: SearchMode) -> Vec<ResultItem> {
        self.search_with_clipboard_url(raw_query, cli_mode, None)
    }

    pub(crate) fn search_snapshot(
        &self,
        raw_query: &str,
        cli_mode: SearchMode,
        clipboard_url: Option<&str>,
    ) -> SearchSnapshot {
        let query = QueryInput::parse(raw_query, cli_mode);
        let mut results = Vec::new();
        let now = current_unix_time();

        if query.text.is_empty() {
            results.extend(self.default_results(&query, now, clipboard_url));
            return SearchSnapshot {
                query,
                immediate_results: results,
                deferred: DeferredSearchPlan::default(),
            };
        }

        match query.source_filter {
            SourceFilter::Bookmarks => {
                results.extend(self.search_bookmarks(&query, now));
                return SearchSnapshot {
                    query: query.clone(),
                    immediate_results: finalize_search_results(results, &query, true),
                    deferred: DeferredSearchPlan::default(),
                };
            }
            SourceFilter::Recents => {
                results.extend(self.search_recent_files(&query, now));
                return SearchSnapshot {
                    query: query.clone(),
                    immediate_results: finalize_search_results(results, &query, true),
                    deferred: DeferredSearchPlan::default(),
                };
            }
            SourceFilter::All => {}
        }

        if query.mode.includes(SearchMode::Apps) {
            results.extend(self.search_apps(&query, now));
        }

        let package_search_deferred = self.should_defer_package_search(&query);

        let windows_search_deferred = self.should_defer_windows_search(&query);
        if !windows_search_deferred && query.mode.includes(SearchMode::Windows) {
            results.extend(self.search_windows(&query, now));
        }

        if query.mode.includes(SearchMode::Ssh) {
            results.extend(self.search_ssh(&query, now));
        }

        if query.mode.includes(SearchMode::Pass) {
            results.extend(self.search_pass(&query, now));
        }

        if matches!(query.mode, SearchMode::All | SearchMode::Commands) {
            results.extend(self.search_power(&query, now));
        }

        let controls_search_deferred = self.should_defer_controls_search(&query);
        if !controls_search_deferred && query.mode.includes(SearchMode::Controls) {
            results.extend(self.search_controls(&query, now));
        }

        if query.mode == SearchMode::All {
            results.extend(self.search_bookmarks(&query, now));
            if !self.should_defer_recent_files_search(&query) {
                results.extend(self.search_recent_files(&query, now));
            }
        }

        if query.mode == SearchMode::Commands {
            results.extend(self.search_commands(&query, now));
        } else if query.mode == SearchMode::All {
            if let Some(result) = self.search_all_mode_command(&query, now) {
                results.push(result);
            }
        }

        if let Some(result) = self.search_settings(&query, now) {
            results.push(result);
        }

        let calc_search_deferred = self.should_defer_calc_search(&query);
        if !calc_search_deferred && query.mode.includes(SearchMode::Calc) {
            if let Some(result) = self.search_calc(&query, now) {
                results.push(result);
            }
        }

        if !package_search_deferred && query.mode == SearchMode::Packages {
            results.extend(self.package_search_status_result(&query));
        }

        if let Some(result) = self.search_url(&query, now) {
            results.push(result);
        }

        if query.mode == SearchMode::Web {
            results.push(self.search_web(&query, now));
        }

        let file_search_deferred = self.should_defer_file_search(&query);
        if !file_search_deferred && query.mode == SearchMode::Files {
            results.extend(self.file_search_status_result(&query));
        }

        let email_search_deferred = self.should_defer_email_search(&query);
        if !email_search_deferred && query.mode == SearchMode::Email {
            results.extend(self.email_search_status_result(&query));
        }

        let recent_files_deferred = self.should_defer_recent_files_search(&query);

        SearchSnapshot {
            query,
            immediate_results: results,
            deferred: DeferredSearchPlan {
                files: file_search_deferred,
                email: email_search_deferred,
                windows: windows_search_deferred,
                controls: controls_search_deferred,
                recents: recent_files_deferred,
                calc: calc_search_deferred,
                packages: package_search_deferred,
            },
        }
    }

    pub(crate) fn search_deferred_results(&self, snapshot: &SearchSnapshot) -> Vec<ResultItem> {
        let now = current_unix_time();
        let mut results = Vec::new();

        if snapshot.deferred.files {
            results.extend(self.search_files(&snapshot.query, now));
        }

        if snapshot.deferred.email {
            results.extend(self.search_email(&snapshot.query, now));
        }

        if snapshot.deferred.windows {
            results.extend(self.search_windows(&snapshot.query, now));
        }

        if snapshot.deferred.controls {
            results.extend(self.search_controls(&snapshot.query, now));
        }

        if snapshot.deferred.recents {
            results.extend(self.search_recent_files(&snapshot.query, now));
        }

        if snapshot.deferred.calc {
            if let Some(result) = self.search_calc(&snapshot.query, now) {
                results.push(result);
            }
        }

        if snapshot.deferred.packages {
            results.extend(self.search_packages(&snapshot.query, now));
        }

        results
    }

    pub fn search_with_clipboard_url(
        &self,
        raw_query: &str,
        cli_mode: SearchMode,
        clipboard_url: Option<&str>,
    ) -> Vec<ResultItem> {
        let snapshot = self.search_snapshot(raw_query, cli_mode, clipboard_url);
        let mut results = snapshot.immediate_results.clone();
        results.extend(self.search_deferred_results(&snapshot));
        finalize_search_results(results, &snapshot.query, true)
    }

    fn should_defer_file_search(&self, query: &QueryInput) -> bool {
        if !query.mode.includes(SearchMode::Files) {
            return false;
        }

        let config = self.current_config();
        config.sources.files
            && self.configured_file_search_backend().is_some()
            && query.text.chars().count() >= MIN_FILE_QUERY_CHARS
    }

    fn should_defer_email_search(&self, query: &QueryInput) -> bool {
        if !query.mode.includes(SearchMode::Email) {
            return false;
        }

        let config = self.current_config();
        let email_config = &config.integrations.email;
        let evolution_helper_available =
            email_config.evolution_enabled && evolution_helper_command(email_config).is_some();
        config.sources.email
            && ((email_config.thunderbird_enabled
                && !self.thunderbird_email_database_paths.is_empty())
                || evolution_helper_available
                || (email_config.local_mail_enabled && !self.local_email_entries.is_empty()))
    }

    fn should_defer_windows_search(&self, query: &QueryInput) -> bool {
        query.mode == SearchMode::All && self.current_config().sources.windows
    }

    fn should_defer_controls_search(&self, query: &QueryInput) -> bool {
        query.mode == SearchMode::All && self.current_config().sources.controls
    }

    fn should_defer_recent_files_search(&self, query: &QueryInput) -> bool {
        query.mode == SearchMode::All && self.current_config().sources.recents
    }

    fn should_defer_calc_search(&self, query: &QueryInput) -> bool {
        query.mode == SearchMode::All
            && self.current_config().sources.calc
            && self.qalc_available
            && looks_like_math(&query.text)
    }

    fn should_defer_package_search(&self, query: &QueryInput) -> bool {
        if !query.mode.includes(SearchMode::Packages) {
            return false;
        }

        self.current_config().sources.packages
            && package_backend_for_query(query, self.package_search_backend).is_some()
            && query.text.chars().count() >= MIN_PACKAGE_QUERY_CHARS
    }

    fn file_search_status_result(&self, query: &QueryInput) -> Vec<ResultItem> {
        let config = self.current_config();
        if !config.sources.files {
            return vec![instruction_result(
                "File search is disabled",
                "Open settings to re-enable file search",
                "Files",
                "system-search-symbolic",
                520,
            )];
        }

        if query.text.chars().count() < MIN_FILE_QUERY_CHARS {
            return vec![instruction_result(
                "Keep typing to search files",
                "Type at least 2 characters before querying the file index",
                "Files",
                "system-search-symbolic",
                520,
            )];
        }

        if self.configured_file_search_backend().is_none() {
            return vec![ResultItem {
                prediction_key: None,
                title: "Indexed file search unavailable".to_string(),
                subtitle: "Install LocalSearch to enable indexed file search".to_string(),
                source: "Files",
                icon_name: "system-search-symbolic".to_string(),
                score: 500,
                action: Action::None,
                ..Default::default()
            }];
        }

        Vec::new()
    }

    fn package_search_status_result(&self, query: &QueryInput) -> Vec<ResultItem> {
        let config = self.current_config();
        if !config.sources.packages {
            return vec![instruction_result(
                "Package search is disabled",
                "Open settings to re-enable pacman/paru package results",
                "Packages",
                "system-software-install-symbolic",
                520,
            )];
        }

        if query.text.chars().count() < MIN_PACKAGE_QUERY_CHARS {
            return vec![instruction_result(
                "Keep typing to search packages",
                "Type at least 2 characters before querying pacman or paru",
                "Packages",
                "system-software-install-symbolic",
                520,
            )];
        }

        if package_backend_for_query(query, self.package_search_backend).is_none() {
            return vec![instruction_result(
                "Package search unavailable",
                "Install pacman or paru to search installable packages",
                "Packages",
                "system-software-install-symbolic",
                520,
            )];
        }

        Vec::new()
    }

    fn email_search_status_result(&self, query: &QueryInput) -> Vec<ResultItem> {
        let config = self.current_config();
        if !config.sources.email {
            return vec![instruction_result(
                "Email search is disabled",
                "Open settings to re-enable email search",
                "Email",
                "mail-unread-symbolic",
                500,
            )];
        }

        let email_config = &config.integrations.email;
        let evolution_helper_available =
            email_config.evolution_enabled && evolution_helper_command(email_config).is_some();
        let any_backend_available = (email_config.thunderbird_enabled
            && !self.thunderbird_email_database_paths.is_empty())
            || evolution_helper_available
            || (email_config.local_mail_enabled && !self.local_email_entries.is_empty());
        if !any_backend_available && query.mode == SearchMode::Email {
            return vec![instruction_result(
                "No email source found",
                "Enable Thunderbird, Evolution helper, or local mail roots in settings",
                "Email",
                "mail-unread-symbolic",
                65,
            )];
        }

        Vec::new()
    }

    pub fn record_activation(&self, item: &ResultItem) {
        let Some(key) = item.prediction_key.clone() else {
            return;
        };
        if matches!(item.action, Action::None) {
            return;
        }

        let prediction = StoredPrediction {
            key,
            title: item.title.clone(),
            subtitle: item.subtitle.clone(),
            source: item.source.to_string(),
            icon_name: item.icon_name.clone(),
            action: item.action.clone(),
        };

        if let Ok(mut predictions) = self.predictions.lock() {
            let _ = predictions.record(prediction, current_unix_time());
        }
    }

    fn default_results(
        &self,
        query: &QueryInput,
        now: u64,
        clipboard_url: Option<&str>,
    ) -> Vec<ResultItem> {
        let config = self.current_config();
        let mut results = Vec::new();
        let mode = query.mode;

        match query.source_filter {
            SourceFilter::Bookmarks => {
                results.push(instruction_result(
                    "Bookmark search",
                    "Type a bookmark title or URL fragment",
                    "Bookmarks",
                    "user-bookmarks-symbolic",
                    65,
                ));
                return results;
            }
            SourceFilter::Recents => {
                results.push(instruction_result(
                    "Recent file search",
                    "Type a recently used file name or path fragment",
                    "Recent Files",
                    "document-open-recent-symbolic",
                    65,
                ));
                return results;
            }
            SourceFilter::All => {}
        }

        if matches!(mode, SearchMode::All | SearchMode::Pass)
            && config.sources.pass
            && self.pass_available
            && let Some(draft) = clipboard_url
                .and_then(password_url_draft)
                .filter(|draft| !self.password_entry_exists(&draft.entry))
        {
            results.push(add_password_result(&draft.entry, Some(draft.url)));
        }

        if mode == SearchMode::All {
            results.extend(self.top_prediction_results(now));
            results.push(self.settings_result(now));
        }

        if mode.includes(SearchMode::Apps) && config.sources.apps {
            results.extend(self.apps.iter().take(8).map(|app| ResultItem {
                prediction_key: Some(app_prediction_key(&app.desktop_id)),
                title: app.name.clone(),
                subtitle: if app.description.is_empty() {
                    app.executable.clone()
                } else {
                    app.description.clone()
                },
                source: "Applications",
                icon_name: app.icon_name.clone(),
                score: 80,
                action: Action::LaunchApp {
                    desktop_id: app.desktop_id.clone(),
                },
                ..Default::default()
            }));
        }

        if mode == SearchMode::Email {
            if !config.sources.email {
                results.push(instruction_result(
                    "Email search is disabled",
                    "Open settings to re-enable email results",
                    "Email",
                    "mail-unread-symbolic",
                    65,
                ));
            } else if self.thunderbird_email_database_paths.is_empty()
                && self.local_email_entries.is_empty()
                && evolution_helper_command(&config.integrations.email).is_none()
            {
                results.push(instruction_result(
                    "No email source found",
                    "Install Thunderbird, enable Evolution, or point Luma at a local maildir to search email",
                    "Email",
                    "mail-unread-symbolic",
                    65,
                ));
            } else {
                results.push(instruction_result(
                    "Email mode",
                    "Type a subject, sender, folder, or body fragment to search mail",
                    "Email",
                    "mail-unread-symbolic",
                    65,
                ));
            }
        }

        if mode == SearchMode::Windows {
            if !config.sources.windows {
                results.push(instruction_result(
                    "Window search is disabled",
                    "Open settings to re-enable active window switching",
                    "Windows",
                    "view-grid-symbolic",
                    65,
                ));
                return results;
            }
            let windows = load_windows();
            if windows.is_empty() {
                results.push(instruction_result(
                    "No active windows found",
                    "Hyprland or Niri did not report switchable windows",
                    "Windows",
                    "view-grid-symbolic",
                    65,
                ));
            } else {
                results.extend(
                    windows
                        .into_iter()
                        .take(MAX_WINDOWS)
                        .map(window_result_item),
                );
            }
        }

        if mode.includes(SearchMode::Ssh) && config.sources.ssh {
            results.extend(self.ssh_hosts.iter().take(4).map(|host| ResultItem {
                prediction_key: Some(ssh_prediction_key(host)),
                title: host.clone(),
                subtitle: "Open an SSH session".to_string(),
                source: "SSH",
                icon_name: "network-server-symbolic".to_string(),
                score: 70,
                action: Action::Ssh { host: host.clone() },
                ..Default::default()
            }));
        }

        if mode == SearchMode::Pass {
            if !config.sources.pass {
                results.push(instruction_result(
                    "Password search is disabled",
                    "Open settings to re-enable pass integration",
                    "Passwords",
                    "dialog-password-symbolic",
                    65,
                ));
                return results;
            }
            if !self.pass_available {
                results.push(instruction_result(
                    "pass is not installed",
                    "Install pass to search password-store entries",
                    "Passwords",
                    "dialog-password-symbolic",
                    65,
                ));
            } else if self
                .pass_entries
                .lock()
                .is_ok_and(|entries| entries.is_empty())
            {
                results.push(instruction_result(
                    "Password store is empty",
                    "Add entries to ~/.password-store or set PASSWORD_STORE_DIR",
                    "Passwords",
                    "dialog-password-symbolic",
                    65,
                ));
            } else {
                results.push(instruction_result(
                    "Password mode",
                    "Type an entry name and press Enter to autotype its login",
                    "Passwords",
                    "dialog-password-symbolic",
                    65,
                ));
            }
        }

        if mode == SearchMode::Files {
            if !config.sources.files {
                results.push(instruction_result(
                    "File search is disabled",
                    "Open settings to re-enable file search",
                    "Files",
                    "system-search-symbolic",
                    65,
                ));
            } else if self.configured_file_search_backend().is_some() {
                results.push(instruction_result(
                    "File mode",
                    "Type a name or path fragment to search indexed files",
                    "Files",
                    "system-search-symbolic",
                    65,
                ));
            } else {
                results.push(instruction_result(
                    "Indexed file search unavailable",
                    "Install LocalSearch to enable indexed file search",
                    "Files",
                    "system-search-symbolic",
                    65,
                ));
            }
        }

        if mode == SearchMode::Commands {
            if !config.sources.commands {
                results.push(instruction_result(
                    "Command search is disabled",
                    "Open settings to re-enable shell commands",
                    "Commands",
                    "utilities-terminal-symbolic",
                    65,
                ));
                return results;
            }
            results.push(instruction_result(
                "Command mode",
                "Type a shell command and press Enter to run it",
                "Commands",
                "utilities-terminal-symbolic",
                65,
            ));
        }

        if mode == SearchMode::Web {
            if !config.sources.web {
                results.push(instruction_result(
                    "Web search is disabled",
                    "Open settings to re-enable browser search",
                    "Web",
                    "web-browser-symbolic",
                    65,
                ));
                return results;
            }
            results.push(instruction_result(
                "Web mode",
                "Type a query and press Enter to search the web",
                "Web",
                "web-browser-symbolic",
                65,
            ));
        }

        if mode == SearchMode::Calc {
            if !config.sources.calc {
                results.push(instruction_result(
                    "Calculator search is disabled",
                    "Open settings to re-enable calculator results",
                    "Calculator",
                    "accessories-calculator-symbolic",
                    65,
                ));
                return results;
            }
            if self.qalc_available {
                results.push(instruction_result(
                    "Calculator mode",
                    "Type an expression like 2+2 and press Enter to copy the result",
                    "Calculator",
                    "accessories-calculator-symbolic",
                    65,
                ));
            } else {
                results.push(instruction_result(
                    "qalc is not installed",
                    "Install libqalculate to enable calculator results",
                    "Calculator",
                    "accessories-calculator-symbolic",
                    65,
                ));
            }
        }

        if mode == SearchMode::Packages {
            results.extend(self.package_search_status_result(&query));
        }

        if mode == SearchMode::Controls {
            if !config.sources.controls {
                results.push(instruction_result(
                    "Desktop controls are disabled",
                    "Open settings to re-enable desktop controls",
                    "Controls",
                    "preferences-desktop-symbolic",
                    65,
                ));
            } else {
                let query = QueryInput {
                    mode,
                    source_filter: query.source_filter,
                    package_manager: query.package_manager,
                    text: String::new(),
                };
                results.extend(self.search_controls(&query, now));
            }
        }

        results
    }

    fn search_apps(&self, query: &QueryInput, now: u64) -> Vec<ResultItem> {
        if !self.current_config().sources.apps {
            return Vec::new();
        }

        let mut items = self
            .apps
            .iter()
            .filter_map(|app| {
                let score = score_text(&app.search_blob, &query.text)?;
                let prediction_key = app_prediction_key(&app.desktop_id);
                Some(ResultItem {
                    prediction_key: Some(prediction_key.clone()),
                    title: app.name.clone(),
                    subtitle: if app.description.is_empty() {
                        app.executable.clone()
                    } else {
                        app.description.clone()
                    },
                    source: "Applications",
                    icon_name: app.icon_name.clone(),
                    score: score + 900 + self.prediction_boost(&prediction_key, now),
                    action: Action::LaunchApp {
                        desktop_id: app.desktop_id.clone(),
                    },
                    ..Default::default()
                })
            })
            .collect::<Vec<_>>();

        sort_results(&mut items, MAX_APPS);
        items
    }

    fn search_windows(&self, query: &QueryInput, now: u64) -> Vec<ResultItem> {
        if !self.current_config().sources.windows {
            return Vec::new();
        }

        let mut items = load_windows()
            .into_iter()
            .filter_map(|window| {
                let score = score_text(&window.search_blob, &query.text)?;
                let prediction_key = window_prediction_key(&window);
                let boosted_score = score + 860 + self.prediction_boost(&prediction_key, now);
                Some(window_result_item_with_score(window, boosted_score))
            })
            .collect::<Vec<_>>();

        sort_results(&mut items, MAX_WINDOWS);
        items
    }

    fn search_files(&self, query: &QueryInput, now: u64) -> Vec<ResultItem> {
        let config = self.current_config();
        if !config.sources.files {
            if query.mode == SearchMode::Files {
                return vec![instruction_result(
                    "File search is disabled",
                    "Open settings to re-enable file search",
                    "Files",
                    "system-search-symbolic",
                    520,
                )];
            }
            return Vec::new();
        }

        if query.text.chars().count() < MIN_FILE_QUERY_CHARS {
            if query.mode == SearchMode::Files {
                return vec![instruction_result(
                    "Keep typing to search files",
                    "Type at least 2 characters before querying the file index",
                    "Files",
                    "system-search-symbolic",
                    520,
                )];
            }
            return Vec::new();
        }

        let Some(backend) = self.configured_file_search_backend() else {
            if query.mode == SearchMode::Files {
                return vec![ResultItem {
                    prediction_key: None,
                    title: "Indexed file search unavailable".to_string(),
                    subtitle: "Install LocalSearch to enable indexed file search".to_string(),
                    source: "Files",
                    icon_name: "system-search-symbolic".to_string(),
                    score: 500,
                    action: Action::None,
                    ..Default::default()
                }];
            }
            return Vec::new();
        };

        let Ok(output) = backend.run_search(&query.text, MAX_FILES) else {
            return Vec::new();
        };

        if !output.status.success() {
            return Vec::new();
        }

        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_file_search_line)
            .take(MAX_FILES)
            .map(|path| {
                let file_name = Path::new(&path)
                    .file_name()
                    .and_then(|part| part.to_str())
                    .unwrap_or(path.as_str())
                    .to_string();
                let accessory = file_modified_label(&path, now);
                ResultItem {
                    prediction_key: Some(file_prediction_key(&path)),
                    title: file_name,
                    subtitle: path.clone(),
                    source: "Files",
                    icon_name: "folder-documents-symbolic".to_string(),
                    score: FILE_BASE_SCORE
                        + self.prediction_boost(&file_prediction_key(&path), now),
                    action: Action::OpenFile { path },
                    accessory,
                    ..Default::default()
                }
            })
            .collect()
    }

    fn search_packages(&self, query: &QueryInput, now: u64) -> Vec<ResultItem> {
        let config = self.current_config();
        if !config.sources.packages {
            if query.mode == SearchMode::Packages {
                return vec![instruction_result(
                    "Package search is disabled",
                    "Open settings to re-enable pacman/paru package results",
                    "Packages",
                    "system-software-install-symbolic",
                    500,
                )];
            }
            return Vec::new();
        }

        if query.text.chars().count() < MIN_PACKAGE_QUERY_CHARS {
            if query.mode == SearchMode::Packages {
                return self.package_search_status_result(query);
            }
            return Vec::new();
        }

        let Some(backend) = package_backend_for_query(query, self.package_search_backend) else {
            if query.mode == SearchMode::Packages {
                return self.package_search_status_result(query);
            }
            return Vec::new();
        };

        let Ok(output) = backend.run_search(&query.text) else {
            return Vec::new();
        };

        if !output.status.success() && output.stdout.is_empty() {
            return Vec::new();
        }

        package_search_results_from_output(&String::from_utf8_lossy(&output.stdout), backend, now)
    }

    fn search_ssh(&self, query: &QueryInput, now: u64) -> Vec<ResultItem> {
        if !self.current_config().sources.ssh {
            return Vec::new();
        }

        let mut items = self
            .ssh_hosts
            .iter()
            .filter_map(|host| {
                let score = score_text(host, &query.text)?;
                let prediction_key = ssh_prediction_key(host);
                Some(ResultItem {
                    prediction_key: Some(prediction_key.clone()),
                    title: host.clone(),
                    subtitle: "Open an SSH session".to_string(),
                    source: "SSH",
                    icon_name: "network-server-symbolic".to_string(),
                    score: score + 720 + self.prediction_boost(&prediction_key, now),
                    action: Action::Ssh { host: host.clone() },
                    ..Default::default()
                })
            })
            .collect::<Vec<_>>();

        sort_results(&mut items, MAX_SSH);
        items
    }

    fn search_pass(&self, query: &QueryInput, now: u64) -> Vec<ResultItem> {
        let config = self.current_config();
        if !config.sources.pass {
            if query.mode == SearchMode::Pass {
                return vec![instruction_result(
                    "Password search is disabled",
                    "Open settings to re-enable pass integration",
                    "Passwords",
                    "dialog-password-symbolic",
                    500,
                )];
            }
            return Vec::new();
        }

        if !self.pass_available {
            if query.mode == SearchMode::Pass {
                return vec![ResultItem {
                    prediction_key: None,
                    title: "pass is not installed".to_string(),
                    subtitle: "Install pass to search password-store entries".to_string(),
                    source: "Passwords",
                    icon_name: "dialog-password-symbolic".to_string(),
                    score: 500,
                    action: Action::None,
                    ..Default::default()
                }];
            }
            return Vec::new();
        }

        let mut items = Vec::new();
        if query.mode == SearchMode::Pass
            && !query.text.is_empty()
            && !self
                .pass_entries
                .lock()
                .is_ok_and(|entries| entries.iter().any(|entry| entry.name == query.text))
        {
            items.push(add_password_result(&query.text, None));
        }

        let pass_entries = self
            .pass_entries
            .lock()
            .map(|entries| entries.clone())
            .unwrap_or_default();
        for entry in &pass_entries {
            let Some(score) = score_text(&entry.search_blob, &query.text) else {
                continue;
            };
            let prediction_key = pass_prediction_key(&entry.name);
            let boosted_score = score + 880 + self.prediction_boost(&prediction_key, now);
            items.push(password_entry_result(&entry.name, boosted_score));
        }

        sort_results(&mut items, MAX_PASS);
        items
    }

    fn search_email(&self, query: &QueryInput, now: u64) -> Vec<ResultItem> {
        let config = self.current_config();
        if !config.sources.email {
            if query.mode == SearchMode::Email {
                return vec![instruction_result(
                    "Email search is disabled",
                    "Open settings to re-enable email search",
                    "Email",
                    "mail-unread-symbolic",
                    500,
                )];
            }
            return Vec::new();
        }

        let email_config = &config.integrations.email;
        let evolution_helper_available =
            email_config.evolution_enabled && evolution_helper_command(email_config).is_some();
        let any_backend_available = (email_config.thunderbird_enabled
            && !self.thunderbird_email_database_paths.is_empty())
            || evolution_helper_available
            || (email_config.local_mail_enabled && !self.local_email_entries.is_empty());
        let mut seen_open_urls = HashSet::new();
        let mut items = Vec::new();
        if email_config.thunderbird_enabled && !self.thunderbird_email_database_paths.is_empty() {
            items.extend(self.search_thunderbird_email(
                query,
                now,
                email_config.preferred_backend,
                &mut seen_open_urls,
            ));
        }
        if email_config.evolution_enabled {
            items.extend(search_evolution_email_entries(
                query,
                now,
                email_config,
                &mut seen_open_urls,
            ));
        }
        if email_config.local_mail_enabled && !self.local_email_entries.is_empty() {
            items.extend(self.search_local_email_entries(
                &self.local_email_entries,
                query,
                now,
                EmailBackend::LocalMail,
                email_config.preferred_backend,
                &mut seen_open_urls,
            ));
        }

        if !any_backend_available && query.mode == SearchMode::Email {
            items.push(instruction_result(
                "No email source found",
                "Enable Thunderbird, Evolution helper, or local mail roots in settings",
                "Email",
                "mail-unread-symbolic",
                65,
            ));
        }

        sort_results(&mut items, MAX_EMAIL);
        items
    }

    fn search_thunderbird_email(
        &self,
        query: &QueryInput,
        now: u64,
        preferred_backend: EmailBackendPreference,
        seen_open_urls: &mut HashSet<String>,
    ) -> Vec<ResultItem> {
        if !command_exists("sqlite3") {
            return Vec::new();
        }

        let mut items = Vec::new();
        for database in &self.thunderbird_email_database_paths {
            let Ok(output) = Command::new("sqlite3")
                .args([
                    "-readonly",
                    "-separator",
                    "\t",
                    &thunderbird_database_uri(database),
                    &thunderbird_email_search_sql(&query.text, MAX_EMAIL * 2),
                ])
                .output()
            else {
                continue;
            };

            if !output.status.success() {
                continue;
            }

            items.extend(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter_map(parse_thunderbird_email_row)
                    .filter_map(|entry| {
                        if !seen_open_urls.insert(entry.open_url.clone()) {
                            return None;
                        }

                        let score = score_text(&entry.search_blob, &query.text)?;
                        Some(
                            email_result_items(
                                &entry,
                                score
                                    + EMAIL_BASE_SCORE
                                    + email_backend_bonus(
                                        preferred_backend,
                                        EmailBackend::Thunderbird,
                                    )
                                    + self.prediction_boost(
                                        &email_prediction_key(&entry.open_url),
                                        now,
                                    ),
                                query.mode == SearchMode::Email,
                            )
                            .into_iter(),
                        )
                    })
                    .flatten(),
            );
        }

        items
    }

    fn search_local_email_entries(
        &self,
        entries: &[EmailEntry],
        query: &QueryInput,
        now: u64,
        backend: EmailBackend,
        preferred_backend: EmailBackendPreference,
        seen_open_urls: &mut HashSet<String>,
    ) -> Vec<ResultItem> {
        entries
            .iter()
            .filter_map(|entry| {
                if !seen_open_urls.insert(entry.open_url.clone()) {
                    return None;
                }

                let score = score_text(&entry.search_blob, &query.text)?;
                Some(email_result_items(
                    entry,
                    score
                        + EMAIL_BASE_SCORE
                        + email_backend_bonus(preferred_backend, backend)
                        + self.prediction_boost(&email_prediction_key(&entry.open_url), now),
                    query.mode == SearchMode::Email,
                ))
            })
            .flatten()
            .collect()
    }

    fn search_commands(&self, query: &QueryInput, now: u64) -> Vec<ResultItem> {
        if !self.current_config().sources.commands {
            if query.mode == SearchMode::Commands {
                return vec![instruction_result(
                    "Command search is disabled",
                    "Open settings to re-enable shell commands",
                    "Commands",
                    "utilities-terminal-symbolic",
                    500,
                )];
            }
            return Vec::new();
        }

        let mut items = Vec::new();
        let run_prediction_key = command_prediction_key(&query.text);
        items.push(ResultItem {
            prediction_key: Some(run_prediction_key.clone()),
            title: format!("Run “{}”", query.text),
            subtitle: "Execute in the background with sh -lc".to_string(),
            source: "Commands",
            icon_name: "utilities-terminal-symbolic".to_string(),
            score: 930 + self.prediction_boost(&run_prediction_key, now),
            action: Action::RunCommand {
                command: query.text.clone(),
            },
            ..Default::default()
        });

        let mut suggestions = self
            .commands
            .iter()
            .filter_map(|command| {
                let score = score_text(command, &query.text)?;
                let prediction_key = command_prediction_key(command);
                Some(ResultItem {
                    prediction_key: Some(prediction_key.clone()),
                    title: command.clone(),
                    subtitle: String::new(),
                    source: "Commands",
                    icon_name: "utilities-terminal-symbolic".to_string(),
                    score: score + 700 + self.prediction_boost(&prediction_key, now),
                    action: Action::RunCommand {
                        command: command.clone(),
                    },
                    ..Default::default()
                })
            })
            .collect::<Vec<_>>();
        sort_results(&mut suggestions, MAX_COMMANDS);
        items.extend(suggestions);

        items
    }

    fn search_power(&self, query: &QueryInput, now: u64) -> Vec<ResultItem> {
        if !self.current_config().sources.power {
            return Vec::new();
        }

        let mut items = POWER_ACTIONS
            .iter()
            .filter_map(|action| {
                power_action_score(action, &query.text).map(|score| (action, score))
            })
            .map(|(action, score)| {
                let prediction_key = power_prediction_key(action.operation);
                ResultItem {
                    prediction_key: Some(prediction_key.clone()),
                    title: action.title.to_string(),
                    subtitle: action.subtitle.to_string(),
                    source: "Power",
                    icon_name: action.icon_name.to_string(),
                    score: score + 950 + self.prediction_boost(&prediction_key, now),
                    action: Action::Power {
                        operation: action.operation,
                        confirmed: false,
                    },
                    ..Default::default()
                }
            })
            .collect::<Vec<_>>();

        sort_results(&mut items, MAX_POWER_ACTIONS);
        items
    }

    fn search_controls(&self, query: &QueryInput, now: u64) -> Vec<ResultItem> {
        if !self.current_config().sources.controls {
            if query.mode == SearchMode::Controls {
                return vec![instruction_result(
                    "Desktop controls are disabled",
                    "Open settings to re-enable desktop controls",
                    "Controls",
                    "preferences-desktop-symbolic",
                    500,
                )];
            }
            return Vec::new();
        }

        let snapshot = load_control_snapshot();
        control_results_from_snapshot(&snapshot, query, now, |key| self.prediction_boost(key, now))
    }

    fn search_bookmarks(&self, query: &QueryInput, now: u64) -> Vec<ResultItem> {
        if !self.current_config().sources.bookmarks {
            if query.mode == SearchMode::All {
                return Vec::new();
            }
            return Vec::new();
        }

        let mut items = self
            .bookmarks
            .iter()
            .filter_map(|bookmark| {
                let score = score_text(&bookmark.search_blob, &query.text)?;
                let prediction_key = bookmark_prediction_key(&bookmark.url);
                Some(ResultItem {
                    prediction_key: Some(prediction_key.clone()),
                    title: bookmark.title.clone(),
                    subtitle: display_domain(&bookmark.url),
                    source: "Bookmarks",
                    icon_name: "user-bookmarks-symbolic".to_string(),
                    score: score + 830 + self.prediction_boost(&prediction_key, now),
                    action: Action::OpenUrl {
                        url: bookmark.url.clone(),
                    },
                    ..Default::default()
                })
            })
            .collect::<Vec<_>>();

        sort_results(&mut items, MAX_BOOKMARKS);
        items
    }

    fn search_recent_files(&self, query: &QueryInput, now: u64) -> Vec<ResultItem> {
        if !self.current_config().sources.recents {
            return Vec::new();
        }

        let mut items = self
            .recent_files
            .iter()
            .enumerate()
            .filter_map(|(index, recent)| {
                let score = score_text(&recent.search_blob, &query.text)?;
                let prediction_key = recent_prediction_key(&recent.path);
                let recency_score = (MAX_RECENTS.saturating_sub(index.min(MAX_RECENTS)) * 5) as i32;
                Some(ResultItem {
                    prediction_key: Some(prediction_key.clone()),
                    title: recent.title.clone(),
                    subtitle: recent.path.clone(),
                    source: "Recent Files",
                    icon_name: "document-open-recent-symbolic".to_string(),
                    score: score
                        + 790
                        + recency_score
                        + self.prediction_boost(&prediction_key, now),
                    action: Action::OpenFile {
                        path: recent.path.clone(),
                    },
                    accessory: file_modified_label(&recent.path, now),
                    ..Default::default()
                })
            })
            .collect::<Vec<_>>();

        sort_results(&mut items, MAX_RECENTS);
        items
    }

    fn search_all_mode_command(&self, query: &QueryInput, now: u64) -> Option<ResultItem> {
        if !self.current_config().sources.commands {
            return None;
        }

        let mut words = query.text.split_whitespace();
        let program = words.next()?;
        words.next()?;

        if !self.commands.iter().any(|command| command == program) {
            return None;
        }

        let prediction_key = command_prediction_key(&query.text);
        Some(ResultItem {
            prediction_key: Some(prediction_key.clone()),
            title: format!("Run \"{}\"", query.text),
            subtitle: "Execute in the background with sh -lc".to_string(),
            source: "Commands",
            icon_name: "utilities-terminal-symbolic".to_string(),
            score: 930 + self.prediction_boost(&prediction_key, now),
            action: Action::RunCommand {
                command: query.text.clone(),
            },
            ..Default::default()
        })
    }

    fn search_calc(&self, query: &QueryInput, now: u64) -> Option<ResultItem> {
        if !self.current_config().sources.calc {
            return None;
        }

        if !self.qalc_available {
            return None;
        }

        if !looks_like_math(&query.text) && query.mode == SearchMode::All {
            return None;
        }

        let output = Command::new("qalc")
            .args(["-t", "--terse", &query.text])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if result.is_empty() || result.eq_ignore_ascii_case("error") {
            return None;
        }

        let prediction_key = calc_prediction_key(&query.text);
        Some(ResultItem {
            prediction_key: Some(prediction_key.clone()),
            title: result.clone(),
            subtitle: format!("Result for {}", query.text),
            source: "Calculator",
            icon_name: "accessories-calculator-symbolic".to_string(),
            score: 1_100 + self.prediction_boost(&prediction_key, now),
            action: Action::CopyText { text: result },
            ..Default::default()
        })
    }

    fn search_url(&self, query: &QueryInput, now: u64) -> Option<ResultItem> {
        if !self.current_config().sources.web {
            return None;
        }

        if !matches!(query.mode, SearchMode::All | SearchMode::Web) {
            return None;
        }

        let url = browser_target(&query.text)?;
        let prediction_key = url_prediction_key(&url);
        Some(ResultItem {
            prediction_key: Some(prediction_key.clone()),
            title: format!("Open {url}"),
            subtitle: "Open URL in the default browser".to_string(),
            source: "Web",
            icon_name: "web-browser-symbolic".to_string(),
            score: URL_BASE_SCORE + self.prediction_boost(&prediction_key, now),
            action: Action::OpenUrl { url },
            ..Default::default()
        })
    }

    fn search_web(&self, query: &QueryInput, now: u64) -> ResultItem {
        if !self.current_config().sources.web {
            return instruction_result(
                "Web search is disabled",
                "Open settings to re-enable browser search",
                "Web",
                "web-browser-symbolic",
                120,
            );
        }

        let prediction_key = web_prediction_key(&query.text);
        ResultItem {
            prediction_key: Some(prediction_key.clone()),
            title: format!("Search the web for “{}”", query.text),
            subtitle: "Open the default browser".to_string(),
            source: "Web",
            icon_name: "web-browser-symbolic".to_string(),
            score: 120 + self.prediction_boost(&prediction_key, now),
            action: Action::WebSearch {
                query: query.text.clone(),
            },
            ..Default::default()
        }
    }

    fn search_settings(&self, query: &QueryInput, now: u64) -> Option<ResultItem> {
        if query.mode != SearchMode::All {
            return None;
        }

        let score = score_text("settings preferences config panel", &query.text)?;
        Some(self.settings_result_with_score(score + 640, now))
    }

    fn prediction_boost(&self, key: &str, now: u64) -> i32 {
        self.predictions
            .lock()
            .map(|predictions| predictions.boost_for_key(key, now))
            .unwrap_or_default()
    }

    fn top_prediction_results(&self, now: u64) -> Vec<ResultItem> {
        self.predictions
            .lock()
            .map(|predictions| predictions.top_results(8, now))
            .unwrap_or_default()
    }

    fn settings_result(&self, now: u64) -> ResultItem {
        self.settings_result_with_score(260, now)
    }

    fn settings_result_with_score(&self, score: i32, now: u64) -> ResultItem {
        let prediction_key = "settings:panel".to_string();
        ResultItem {
            prediction_key: Some(prediction_key.clone()),
            title: "Open settings".to_string(),
            subtitle: "Configure sources, integrations, and launcher behavior".to_string(),
            source: "Settings",
            icon_name: "preferences-system-symbolic".to_string(),
            score: score + self.prediction_boost(&prediction_key, now),
            action: Action::OpenConfigPanel,
            ..Default::default()
        }
    }
}

fn load_applications() -> Vec<AppEntry> {
    let mut apps = gio::AppInfo::all()
        .into_iter()
        .filter(|app| app.should_show())
        .filter_map(|app| {
            let desktop_id = app.id()?.to_string();
            let name = app.display_name().to_string();
            let executable = app.executable().to_string_lossy().to_string();
            let description = app
                .description()
                .map(|text| text.to_string())
                .unwrap_or_default();
            let icon_name = app
                .icon()
                .and_then(|icon| icon.dynamic_cast::<gio::ThemedIcon>().ok())
                .and_then(|icon| icon.names().first().map(|name| name.to_string()))
                .unwrap_or_else(|| "application-x-executable-symbolic".to_string());

            Some(AppEntry {
                search_blob: format!("{name} {description} {executable}").to_ascii_lowercase(),
                desktop_id,
                name,
                description,
                executable,
                icon_name,
            })
        })
        .collect::<Vec<_>>();

    apps.sort_by(|left, right| left.name.cmp(&right.name));
    apps
}

fn load_ssh_hosts() -> Vec<String> {
    let mut hosts = BTreeSet::new();
    if let Some(home) = dirs::home_dir() {
        parse_ssh_config(&home.join(".ssh/config"), &mut hosts);
        parse_known_hosts(&home.join(".ssh/known_hosts"), &mut hosts);
        parse_known_hosts(&home.join(".ssh/known_hosts.old"), &mut hosts);
    }
    hosts.into_iter().collect()
}

fn load_browser_bookmarks() -> Vec<BookmarkEntry> {
    let mut by_url = BTreeMap::new();

    for entry in load_firefox_bookmarks()
        .into_iter()
        .chain(load_chromium_bookmarks())
    {
        by_url.entry(entry.url.clone()).or_insert(entry);
    }

    let mut bookmarks = by_url.into_values().collect::<Vec<_>>();
    bookmarks.sort_by(|left, right| {
        left.title
            .to_ascii_lowercase()
            .cmp(&right.title.to_ascii_lowercase())
            .then_with(|| left.url.cmp(&right.url))
    });
    bookmarks
}

fn load_firefox_bookmarks() -> Vec<BookmarkEntry> {
    if !command_exists("sqlite3") {
        return Vec::new();
    }

    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let profiles_dir = home.join(".mozilla/firefox");
    let Ok(profiles) = fs::read_dir(profiles_dir) else {
        return Vec::new();
    };

    let query = "select replace(coalesce(b.title,''), char(9), ' '), p.url \
        from moz_bookmarks b join moz_places p on p.id = b.fk \
        where b.type = 1 and p.url not like 'place:%'";

    profiles
        .flatten()
        .map(|profile| profile.path().join("places.sqlite"))
        .filter(|path| path.is_file())
        .filter_map(|path| {
            let database = format!("file:{}?immutable=1", path.to_string_lossy());
            let output = Command::new("sqlite3")
                .args(["-separator", "\t", &database, query])
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            Some(parse_firefox_bookmark_rows(&String::from_utf8_lossy(
                &output.stdout,
            )))
        })
        .flatten()
        .collect()
}

fn load_chromium_bookmarks() -> Vec<BookmarkEntry> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    let roots = [
        home.join(".config/google-chrome"),
        home.join(".config/chromium"),
        home.join(".config/BraveSoftware/Brave-Browser"),
        home.join(".config/vivaldi"),
    ];

    roots
        .into_iter()
        .flat_map(chromium_bookmark_files)
        .filter_map(|path| fs::read_to_string(path).ok())
        .flat_map(|contents| parse_chromium_bookmarks_json(&contents))
        .collect()
}

fn chromium_bookmark_files(root: PathBuf) -> Vec<PathBuf> {
    let Ok(profiles) = fs::read_dir(root) else {
        return Vec::new();
    };

    profiles
        .flatten()
        .map(|profile| profile.path().join("Bookmarks"))
        .filter(|path| path.is_file())
        .collect()
}

fn app_prediction_key(desktop_id: &str) -> String {
    format!("app:{desktop_id}")
}

fn package_prediction_key(repository: &str, name: &str) -> String {
    format!("package:{repository}/{name}")
}

fn package_backend_for_query(
    query: &QueryInput,
    detected: Option<PackageSearchBackend>,
) -> Option<PackageSearchBackend> {
    match query.package_manager {
        Some(PackageManager::Pacman) => Some(PackageSearchBackend::Pacman),
        Some(PackageManager::Paru) => Some(PackageSearchBackend::Paru),
        None => detected,
    }
}

fn package_command_path(binary: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH");
    let path = path.as_ref().map(|value| value.to_string_lossy());
    let fallback_dirs = [
        Path::new("/usr/bin"),
        Path::new("/usr/local/bin"),
        Path::new("/bin"),
    ];
    package_command_path_with_fallbacks(binary, path.as_deref(), &fallback_dirs)
}

fn package_command_path_with_fallbacks(
    binary: &str,
    path: Option<&str>,
    fallback_dirs: &[&Path],
) -> Option<PathBuf> {
    path.unwrap_or_default()
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(Path::new)
        .chain(fallback_dirs.iter().copied())
        .map(|dir| dir.join(binary))
        .find(|candidate| path_is_executable(candidate))
}

fn path_is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
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

fn file_prediction_key(path: &str) -> String {
    format!("file:{path}")
}

fn ssh_prediction_key(host: &str) -> String {
    format!("ssh:{host}")
}

pub(crate) fn pass_prediction_key(entry: &str) -> String {
    format!("pass:{entry}")
}

fn bookmark_prediction_key(url: &str) -> String {
    format!("bookmark:{url}")
}

fn recent_prediction_key(path: &str) -> String {
    format!("recent:{path}")
}

fn email_prediction_key(open_url: &str) -> String {
    format!("email:{open_url}")
}

fn email_backend_bonus(preferred: EmailBackendPreference, backend: EmailBackend) -> i32 {
    let rank = match preferred {
        EmailBackendPreference::Thunderbird | EmailBackendPreference::Auto => match backend {
            EmailBackend::Thunderbird => 3,
            EmailBackend::Evolution => 2,
            EmailBackend::LocalMail => 1,
        },
        EmailBackendPreference::Evolution => match backend {
            EmailBackend::Evolution => 3,
            EmailBackend::Thunderbird => 2,
            EmailBackend::LocalMail => 1,
        },
        EmailBackendPreference::LocalMail => match backend {
            EmailBackend::LocalMail => 3,
            EmailBackend::Evolution => 2,
            EmailBackend::Thunderbird => 1,
        },
    };

    rank * 10
}

fn email_result_items(entry: &EmailEntry, score: i32, include_secondary: bool) -> Vec<ResultItem> {
    let date_label = entry.date_label.trim();
    let mut rows = vec![ResultItem {
        prediction_key: Some(email_prediction_key(&entry.open_url)),
        title: format!("Open email: {}", entry.subject),
        subtitle: email_primary_subtitle(&entry.sender, &entry.folder, &entry.snippet),
        source: "Email",
        icon_name: "mail-unread-symbolic".to_string(),
        score: score + 80,
        action: Action::OpenUrl {
            url: entry.open_url.clone(),
        },
        accessory: (!date_label.is_empty()).then(|| date_label.to_string()),
        badges: email_badges(entry),
    }];

    if include_secondary {
        if let Some(sender_email) = &entry.sender_email {
            let reply_subject = if entry.subject.is_empty() {
                "Re:".to_string()
            } else {
                format!("Re: {}", entry.subject)
            };
            let reply_url = entry
                .reply_url
                .clone()
                .unwrap_or_else(|| mailto_reply_url(sender_email, &reply_subject));
            let compose_url = entry
                .compose_url
                .clone()
                .unwrap_or_else(|| mailto_compose_url(sender_email));
            rows.extend([
                email_result_item(
                    format!("Reply to {}", entry.sender),
                    email_subtitle(sender_email, &entry.folder, &entry.date_label),
                    reply_url,
                    score + 50,
                    None,
                ),
                email_result_item(
                    format!("Compose to {}", entry.sender),
                    email_subtitle(sender_email, &entry.folder, &entry.date_label),
                    compose_url,
                    score + 45,
                    None,
                ),
                ResultItem {
                    prediction_key: None,
                    title: format!("Copy sender: {}", sender_email),
                    subtitle: "Copy the sender address to the clipboard".to_string(),
                    source: "Email",
                    icon_name: "edit-copy-symbolic".to_string(),
                    score: score + 40,
                    action: Action::CopyText {
                        text: sender_email.clone(),
                    },
                    ..Default::default()
                },
            ]);
        }
    }

    rows
}

fn email_result_item(
    title: String,
    subtitle: String,
    open_url: String,
    score: i32,
    prediction_key: Option<String>,
) -> ResultItem {
    ResultItem {
        prediction_key,
        title,
        subtitle,
        source: "Email",
        icon_name: "mail-unread-symbolic".to_string(),
        score,
        action: Action::OpenUrl { url: open_url },
        ..Default::default()
    }
}

pub(crate) fn parse_package_search_output(output: &str) -> Vec<PackageEntry> {
    let mut entries = Vec::new();
    let mut current: Option<PackageEntry> = None;

    for line in output.lines() {
        if line.trim().is_empty() {
            continue;
        }

        if line.starts_with(char::is_whitespace) {
            if let Some(entry) = current.as_mut() {
                let description = line.trim();
                if !description.is_empty() {
                    entry.description = description.to_string();
                }
            }
            continue;
        }

        if let Some(entry) = current.take() {
            entries.push(entry);
        }

        let Some((repository, rest)) = line.split_once('/') else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let Some(name) = parts.next() else {
            continue;
        };
        let Some(version) = parts.next() else {
            continue;
        };
        let installed = rest.contains("[installed]");

        current = Some(PackageEntry {
            repository: repository.to_string(),
            name: name.to_string(),
            version: version.to_string(),
            description: String::new(),
            installed,
        });
    }

    if let Some(entry) = current {
        entries.push(entry);
    }

    entries
}

pub(crate) fn package_search_results_from_output(
    output: &str,
    backend: PackageSearchBackend,
    _now: u64,
) -> Vec<ResultItem> {
    let mut items = parse_package_search_output(output)
        .into_iter()
        .enumerate()
        .take(MAX_PACKAGES)
        .map(|(index, entry)| {
            let order_score = (MAX_PACKAGES.saturating_sub(index) as i32) * 10;
            package_result_item(entry, backend, order_score)
        })
        .collect::<Vec<_>>();
    sort_results(&mut items, MAX_PACKAGES);
    items
}

fn package_result_item(
    entry: PackageEntry,
    backend: PackageSearchBackend,
    order_score: i32,
) -> ResultItem {
    let prediction_key = package_prediction_key(&entry.repository, &entry.name);
    let status = if entry.installed { " installed" } else { "" };
    let subtitle = if entry.description.is_empty() {
        format!("{} {}{}", entry.repository, entry.version, status)
    } else {
        format!(
            "{} {}{} - {}",
            entry.repository, entry.version, status, entry.description
        )
    };

    ResultItem {
        prediction_key: Some(prediction_key),
        title: entry.name.clone(),
        subtitle,
        source: "Packages",
        icon_name: "system-software-install-symbolic".to_string(),
        score: 160 + order_score + package_rank_adjustment(&entry),
        action: Action::InstallPackage {
            package: entry.name,
            manager: backend.manager(),
        },
        ..Default::default()
    }
}

fn package_rank_adjustment(entry: &PackageEntry) -> i32 {
    let installed_penalty = if entry.installed { -100 } else { 0 };
    let repo_bonus = if entry.repository == "aur" { 0 } else { 20 };
    repo_bonus + installed_penalty
}

fn email_subtitle(sender: &str, folder: &str, date_label: &str) -> String {
    let mut parts = Vec::new();
    if !sender.trim().is_empty() {
        parts.push(sender.trim().to_string());
    }
    if !folder.trim().is_empty() {
        parts.push(folder.trim().to_string());
    }
    if !date_label.trim().is_empty() {
        parts.push(date_label.trim().to_string());
    }
    parts.join(" · ")
}

/// Subtitle for the primary "Open email" row: sender, folder and a body preview. The date
/// moves to the row accessory; the line is ellipsized so a long snippet truncates cleanly.
fn email_primary_subtitle(sender: &str, folder: &str, snippet: &str) -> String {
    let mut parts = Vec::new();
    if !sender.trim().is_empty() {
        parts.push(sender.trim().to_string());
    }
    if !folder.trim().is_empty() {
        parts.push(folder.trim().to_string());
    }
    if !snippet.trim().is_empty() {
        parts.push(snippet.trim().to_string());
    }
    parts.join(" · ")
}

fn email_badges(entry: &EmailEntry) -> Vec<EntryBadge> {
    let mut badges = Vec::new();
    if entry.unread {
        badges.push(EntryBadge::Unread);
    }
    if entry.has_attachment {
        badges.push(EntryBadge::Attachment);
    }
    badges
}

fn mailto_compose_url(sender_email: &str) -> String {
    format!("mailto:{}", urlencoding::encode(sender_email))
}

fn mailto_reply_url(sender_email: &str, subject: &str) -> String {
    let mut url = format!("mailto:{}", urlencoding::encode(sender_email));
    let mut params = Vec::new();
    if !subject.trim().is_empty() {
        params.push(format!("subject={}", urlencoding::encode(subject)));
    }
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }
    url
}

fn thunderbird_email_search_sql(query: &str, limit: usize) -> String {
    let escaped = sql_quote(query);
    format!(
        "select m.date, m.messageKey, l.folderURI, l.name, c.c1subject, c.c3author, m.flags, c.c0body \
         from messages m \
         join folderLocations l on l.id = m.folderID \
         join messagesText_content c on c.docid = m.id \
         where lower(coalesce(c.c1subject,'') || ' ' || coalesce(c.c3author,'') || ' ' || coalesce(c.c0body,'') || ' ' || coalesce(l.name,'')) \
         like '%' || lower({escaped}) || '%' \
         order by m.date desc \
         limit {limit}"
    )
}

fn parse_thunderbird_email_row(raw: &str) -> Option<EmailEntry> {
    let mut fields = raw.split('\t');
    let date = fields.next()?.trim().parse::<i64>().ok()?;
    let message_key = fields.next()?.trim().parse::<u64>().ok()?;
    let folder_uri = fields.next()?.trim().to_string();
    let folder_name = fields.next()?.trim().to_string();
    let subject = fields.next()?.trim().to_string();
    let author = fields.next()?.trim().to_string();
    let flags = fields.next()?.trim().parse::<i64>().unwrap_or_default();
    let body = fields.next()?.trim().to_string();

    let subject = if subject.is_empty() {
        "(no subject)".to_string()
    } else {
        subject
    };
    let sender = if author.is_empty() {
        "Unknown sender".to_string()
    } else {
        author.clone()
    };
    let sender_email = extract_email_address(&author);
    let open_url = thunderbird_message_uri(&folder_uri, message_key);
    let folder_label = if folder_name.is_empty() {
        folder_uri.clone()
    } else {
        folder_name
    };
    let date_label = email_date_label(date as u64, current_unix_seconds());
    let search_blob = format!("{subject} {sender} {folder_label} {body}").to_ascii_lowercase();
    Some(EmailEntry {
        subject,
        sender,
        sender_email,
        folder: folder_label,
        date_label,
        snippet: email_snippet(&body),
        unread: flags & THUNDERBIRD_FLAG_READ == 0,
        has_attachment: flags & THUNDERBIRD_FLAG_ATTACHMENT != 0,
        open_url,
        reply_url: None,
        compose_url: None,
        search_blob,
    })
}

/// `nsMsgMessageFlags::Read` — set once a Thunderbird message has been read.
const THUNDERBIRD_FLAG_READ: i64 = 0x0000_0001;
/// `nsMsgMessageFlags::Attachment` — set when a Thunderbird message has attachments.
const THUNDERBIRD_FLAG_ATTACHMENT: i64 = 0x1000_0000;

/// Collapse a message body to a short, single-line preview for display.
fn email_snippet(body: &str) -> String {
    const MAX_SNIPPET_CHARS: usize = 120;
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > MAX_SNIPPET_CHARS {
        let truncated: String = collapsed.chars().take(MAX_SNIPPET_CHARS).collect();
        format!("{}…", truncated.trim_end())
    } else {
        collapsed
    }
}

fn email_date_label(message_date_micros: u64, now: u64) -> String {
    let message_seconds = message_date_micros / 1_000_000;
    relative_age_label(message_seconds, now)
}

/// Render a compact relative date for a timestamp in epoch seconds.
fn relative_age_label(timestamp_seconds: u64, now: u64) -> String {
    let age_seconds = now.saturating_sub(timestamp_seconds);
    if age_seconds < 60 {
        "just now".to_string()
    } else if age_seconds < 3_600 {
        format!("{}m ago", age_seconds / 60)
    } else if age_seconds < 86_400 {
        format!("{}h ago", age_seconds / 3_600)
    } else if age_seconds < 172_800 {
        "yesterday".to_string()
    } else {
        format!("{}d ago", age_seconds / 86_400)
    }
}

/// The bare host of a URL for compact display (scheme, userinfo, port, path and a
/// leading `www.` stripped). Falls back to the original string when there is no host.
fn display_domain(url: &str) -> String {
    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host);
    let host = host.trim().trim_end_matches('.');
    let host = host.strip_prefix("www.").unwrap_or(host);
    if host.is_empty() {
        url.trim().to_string()
    } else {
        host.to_string()
    }
}

/// Relative modified-time label for a file on disk, or `None` if it can't be read.
fn file_modified_label(path: &str, now: u64) -> Option<String> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let seconds = modified.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(relative_age_label(seconds, now))
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn thunderbird_message_uri(folder_uri: &str, message_key: u64) -> String {
    let scheme = if folder_uri.starts_with("imap://") {
        "imap-message://"
    } else if folder_uri.starts_with("mailbox://") {
        "mailbox-message://"
    } else {
        "message://"
    };
    format!("{scheme}{folder_uri}#{message_key}")
}

fn thunderbird_database_uri(path: &Path) -> String {
    format!("file:{}?immutable=1", path.to_string_lossy())
}

fn extract_email_address(author: &str) -> Option<String> {
    let candidate = author
        .split_once('<')
        .and_then(|(_, rest)| rest.split_once('>'))
        .map(|(value, _)| value)
        .unwrap_or(author)
        .trim();
    if candidate.contains('@') {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn header_value(headers: &str, target: &str) -> Option<String> {
    let target = target.to_ascii_lowercase();
    let mut current_key = String::new();
    let mut current_value = String::new();

    for line in headers.lines() {
        if line.starts_with([' ', '\t']) {
            if !current_key.is_empty() {
                current_value.push(' ');
                current_value.push_str(line.trim());
            }
            continue;
        }

        if !current_key.is_empty() && current_key == target {
            return Some(current_value.trim().to_string());
        }

        let Some((key, value)) = line.split_once(':') else {
            current_key.clear();
            current_value.clear();
            continue;
        };
        current_key = key.trim().to_ascii_lowercase();
        current_value = value.trim().to_string();
    }

    if !current_key.is_empty() && current_key == target {
        return Some(current_value.trim().to_string());
    }

    None
}

fn sql_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn password_entry_result(entry: &str, score: i32) -> ResultItem {
    ResultItem {
        prediction_key: Some(pass_prediction_key(entry)),
        title: entry.to_string(),
        subtitle: String::new(),
        source: "Passwords",
        icon_name: "dialog-password-symbolic".to_string(),
        score,
        action: Action::PasswordActions {
            entry: entry.to_string(),
        },
        ..Default::default()
    }
}

fn add_password_result(entry: &str, url: Option<String>) -> ResultItem {
    ResultItem {
        prediction_key: None,
        title: format!("Add password: {entry}"),
        subtitle: "Generate a password and save it to password-store".to_string(),
        source: "Passwords",
        icon_name: "list-add-symbolic".to_string(),
        score: 1_700,
        action: Action::AddPassword {
            entry: entry.to_string(),
            url,
        },
        ..Default::default()
    }
}

fn command_prediction_key(command: &str) -> String {
    format!("cmd:{command}")
}

fn power_prediction_key(operation: PowerOperation) -> String {
    format!("power:{}", power_operation_id(operation))
}

fn power_action_score(action: &PowerAction, query: &str) -> Option<i32> {
    action
        .search_terms
        .iter()
        .filter_map(|term| score_text(term, query))
        .max()
}

fn control_results_from_snapshot(
    snapshot: &ControlSnapshot,
    query: &QueryInput,
    now: u64,
    prediction_boost: impl Fn(&str) -> i32,
) -> Vec<ResultItem> {
    let mut items = Vec::new();
    let empty_query = query.text.is_empty();

    let media_subtitle = snapshot.media.as_ref().map(media_subtitle);
    if snapshot.has_playerctl {
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Play/Pause media",
                subtitle: media_subtitle
                    .as_deref()
                    .unwrap_or("Toggle the active media player"),
                icon_name: "media-playback-start-symbolic",
                search_terms: &["media", "music", "play", "pause", "toggle", "player"],
                operation: DesktopControlOperation::MediaPlayPause,
                prediction_key: Some("control:media-play-pause".to_string()),
                base_score: 760,
            },
            now,
            &prediction_boost,
        );
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Next media track",
                subtitle: media_subtitle
                    .as_deref()
                    .unwrap_or("Skip the active media player"),
                icon_name: "media-skip-forward-symbolic",
                search_terms: &["media", "music", "next", "skip", "track"],
                operation: DesktopControlOperation::MediaNext,
                prediction_key: Some("control:media-next".to_string()),
                base_score: 650,
            },
            now,
            &prediction_boost,
        );
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Previous media track",
                subtitle: media_subtitle
                    .as_deref()
                    .unwrap_or("Go back in the active media player"),
                icon_name: "media-skip-backward-symbolic",
                search_terms: &["media", "music", "previous", "back", "track"],
                operation: DesktopControlOperation::MediaPrevious,
                prediction_key: Some("control:media-previous".to_string()),
                base_score: 640,
            },
            now,
            &prediction_boost,
        );
    }

    let volume_subtitle = snapshot
        .volume
        .as_ref()
        .map(volume_subtitle)
        .unwrap_or_else(|| "Adjust the default audio sink".to_string());
    if snapshot.has_wpctl {
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Mute volume",
                subtitle: &volume_subtitle,
                icon_name: "audio-volume-muted-symbolic",
                search_terms: &["volume", "audio", "sound", "mute", "speaker"],
                operation: DesktopControlOperation::VolumeMute,
                prediction_key: Some("control:volume-mute".to_string()),
                base_score: 750,
            },
            now,
            &prediction_boost,
        );
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Raise volume",
                subtitle: &volume_subtitle,
                icon_name: "audio-volume-high-symbolic",
                search_terms: &["volume", "audio", "sound", "raise", "louder", "up"],
                operation: DesktopControlOperation::VolumeUp,
                prediction_key: Some("control:volume-up".to_string()),
                base_score: 700,
            },
            now,
            &prediction_boost,
        );
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Lower volume",
                subtitle: &volume_subtitle,
                icon_name: "audio-volume-low-symbolic",
                search_terms: &["volume", "audio", "sound", "lower", "quieter", "down"],
                operation: DesktopControlOperation::VolumeDown,
                prediction_key: Some("control:volume-down".to_string()),
                base_score: 700,
            },
            now,
            &prediction_boost,
        );
    }
    if command_exists("pavucontrol") {
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Open audio settings",
                subtitle: &volume_subtitle,
                icon_name: "multimedia-volume-control-symbolic",
                search_terms: &["audio", "sound", "volume", "pavucontrol", "settings"],
                operation: DesktopControlOperation::AudioSettings,
                prediction_key: Some("control:audio-settings".to_string()),
                base_score: 610,
            },
            now,
            &prediction_boost,
        );
    }

    if let Some(percent) = snapshot.screen_brightness {
        let brightness_subtitle = format!("Screen brightness {percent}%");
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Raise brightness",
                subtitle: &brightness_subtitle,
                icon_name: "display-brightness-symbolic",
                search_terms: &[
                    "brightness",
                    "screen",
                    "display",
                    "backlight",
                    "raise",
                    "brighter",
                ],
                operation: DesktopControlOperation::BrightnessUp,
                prediction_key: Some("control:brightness-up".to_string()),
                base_score: 700,
            },
            now,
            &prediction_boost,
        );
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Lower brightness",
                subtitle: &brightness_subtitle,
                icon_name: "display-brightness-symbolic",
                search_terms: &[
                    "brightness",
                    "screen",
                    "display",
                    "backlight",
                    "lower",
                    "dimmer",
                ],
                operation: DesktopControlOperation::BrightnessDown,
                prediction_key: Some("control:brightness-down".to_string()),
                base_score: 700,
            },
            now,
            &prediction_boost,
        );
    }

    if snapshot.has_bluetoothctl {
        let bluetooth_subtitle = snapshot
            .bluetooth
            .as_ref()
            .map(bluetooth_subtitle)
            .unwrap_or_else(|| "Toggle the Bluetooth controller".to_string());
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Toggle Bluetooth",
                subtitle: &bluetooth_subtitle,
                icon_name: "bluetooth-active-symbolic",
                search_terms: &["bluetooth", "bt", "wireless", "controller", "power"],
                operation: DesktopControlOperation::BluetoothTogglePower,
                prediction_key: Some("control:bluetooth-toggle".to_string()),
                base_score: 730,
            },
            now,
            &prediction_boost,
        );
    }

    if snapshot.has_nmcli {
        let network_subtitle = snapshot
            .network
            .as_ref()
            .map(network_subtitle)
            .unwrap_or_else(|| "Open NetworkManager connection settings".to_string());
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Open network settings",
                subtitle: &network_subtitle,
                icon_name: "network-workgroup-symbolic",
                search_terms: &["network", "wifi", "wi-fi", "ethernet", "connection", "vpn"],
                operation: DesktopControlOperation::NetworkSettings,
                prediction_key: Some("control:network-settings".to_string()),
                base_score: 690,
            },
            now,
            &prediction_boost,
        );
    }

    if snapshot.has_powerprofilesctl {
        let profile_subtitle = snapshot
            .power_profile
            .as_ref()
            .map(|profile| format!("Current profile: {profile}"))
            .unwrap_or_else(|| "Cycle the active power profile".to_string());
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Cycle power profile",
                subtitle: &profile_subtitle,
                icon_name: "power-profile-balanced-symbolic",
                search_terms: &[
                    "power profile",
                    "performance",
                    "balanced",
                    "power saver",
                    "battery",
                ],
                operation: DesktopControlOperation::PowerProfileCycle,
                prediction_key: Some("control:power-profile-cycle".to_string()),
                base_score: 680,
            },
            now,
            &prediction_boost,
        );
        for profile in ["performance", "balanced", "power-saver"] {
            let title = format!("Set power profile: {profile}");
            push_control_action(
                &mut items,
                query,
                ControlActionSpec {
                    title: &title,
                    subtitle: &profile_subtitle,
                    icon_name: "power-profile-balanced-symbolic",
                    search_terms: &["power profile", profile],
                    operation: DesktopControlOperation::PowerProfileSet {
                        profile: profile.to_string(),
                    },
                    prediction_key: Some(format!("control:power-profile:{profile}")),
                    base_score: 560,
                },
                now,
                &prediction_boost,
            );
        }
    }

    if snapshot.has_grim && snapshot.has_slurp {
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Screenshot area",
                subtitle: "Select a region, save it, and copy it to the clipboard",
                icon_name: "camera-photo-symbolic",
                search_terms: &[
                    "screenshot",
                    "screen shot",
                    "capture",
                    "area",
                    "region",
                    "grim",
                    "slurp",
                ],
                operation: DesktopControlOperation::ScreenshotArea,
                prediction_key: Some("control:screenshot-area".to_string()),
                base_score: 720,
            },
            now,
            &prediction_boost,
        );
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Screenshot screen",
                subtitle: "Save the current screen and copy it to the clipboard",
                icon_name: "camera-photo-symbolic",
                search_terms: &["screenshot", "screen shot", "capture", "screen", "grim"],
                operation: DesktopControlOperation::ScreenshotScreen,
                prediction_key: Some("control:screenshot-screen".to_string()),
                base_score: 660,
            },
            now,
            &prediction_boost,
        );
    }

    if snapshot.has_hyprpicker {
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Pick screen color",
                subtitle: "Copy a color from the screen to the clipboard",
                icon_name: "color-select-symbolic",
                search_terms: &[
                    "color",
                    "colour",
                    "picker",
                    "pick",
                    "hyprpicker",
                    "eyedropper",
                ],
                operation: DesktopControlOperation::ColorPicker,
                prediction_key: Some("control:color-picker".to_string()),
                base_score: 710,
            },
            now,
            &prediction_boost,
        );
    }

    if snapshot.has_dunstctl {
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Pause/resume notifications",
                subtitle: "Toggle Dunst notification pause state",
                icon_name: "preferences-system-notifications-symbolic",
                search_terms: &[
                    "notifications",
                    "notification",
                    "dunst",
                    "pause",
                    "resume",
                    "do not disturb",
                ],
                operation: DesktopControlOperation::NotificationPauseToggle,
                prediction_key: Some("control:notifications-pause-toggle".to_string()),
                base_score: 670,
            },
            now,
            &prediction_boost,
        );
        push_control_action(
            &mut items,
            query,
            ControlActionSpec {
                title: "Close notifications",
                subtitle: "Close all visible Dunst notifications",
                icon_name: "preferences-system-notifications-symbolic",
                search_terms: &[
                    "notifications",
                    "notification",
                    "dunst",
                    "close",
                    "dismiss",
                    "clear",
                ],
                operation: DesktopControlOperation::NotificationCloseAll,
                prediction_key: Some("control:notifications-close-all".to_string()),
                base_score: 630,
            },
            now,
            &prediction_boost,
        );
        for notification in &snapshot.notifications {
            if empty_query {
                continue;
            }
            let Some(score) = score_text(&notification.search_blob, &query.text) else {
                continue;
            };
            items.push(ResultItem {
                prediction_key: None,
                title: format!("Notification: {}", notification.summary),
                subtitle: notification_subtitle(notification),
                source: "Controls",
                icon_name: "preferences-system-notifications-symbolic".to_string(),
                score: score + 540,
                action: Action::DesktopControl {
                    operation: DesktopControlOperation::NotificationHistoryPop,
                },
                ..Default::default()
            });
        }
    }

    sort_results(&mut items, MAX_CONTROLS);
    items
}

struct ControlActionSpec<'a> {
    title: &'a str,
    subtitle: &'a str,
    icon_name: &'a str,
    search_terms: &'a [&'a str],
    operation: DesktopControlOperation,
    prediction_key: Option<String>,
    base_score: i32,
}

fn push_control_action(
    items: &mut Vec<ResultItem>,
    query: &QueryInput,
    spec: ControlActionSpec<'_>,
    _now: u64,
    prediction_boost: &impl Fn(&str) -> i32,
) {
    let search_blob = std::iter::once(spec.title)
        .chain(std::iter::once(spec.subtitle))
        .chain(spec.search_terms.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    let Some(score) = score_text(&search_blob, &query.text) else {
        return;
    };
    let boost = spec
        .prediction_key
        .as_deref()
        .map(prediction_boost)
        .unwrap_or_default();
    items.push(ResultItem {
        prediction_key: spec.prediction_key,
        title: spec.title.to_string(),
        subtitle: spec.subtitle.to_string(),
        source: "Controls",
        icon_name: spec.icon_name.to_string(),
        score: spec.base_score + score + boost,
        action: Action::DesktopControl {
            operation: spec.operation,
        },
        ..Default::default()
    });
}

fn media_subtitle(status: &MediaStatus) -> String {
    let label = if status.artist.is_empty() {
        status.title.clone()
    } else {
        format!("{} - {}", status.artist, status.title)
    };
    format!("{} - {} ({})", status.player, label, status.status)
}

fn volume_subtitle(status: &VolumeStatus) -> String {
    if status.muted {
        format!("Volume {}% - muted", status.percent)
    } else {
        format!("Volume {}%", status.percent)
    }
}

fn bluetooth_subtitle(status: &BluetoothStatus) -> String {
    if !status.powered {
        "Off".to_string()
    } else if status.connected_count == 0 {
        "On".to_string()
    } else {
        format!("On - {} connected", status.connected_count)
    }
}

fn network_subtitle(status: &NetworkStatus) -> String {
    if status.kind == "wifi" {
        format!("Wi-Fi - {}", status.connection)
    } else {
        format!("{} - {}", title_case_ascii(&status.kind), status.connection)
    }
}

fn notification_subtitle(notification: &NotificationEntry) -> String {
    if notification.body.is_empty() {
        notification.app_name.clone()
    } else {
        format!("{} - {}", notification.app_name, notification.body)
    }
}

fn title_case_ascii(value: &str) -> String {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
}

fn power_operation_id(operation: PowerOperation) -> &'static str {
    match operation {
        PowerOperation::Lock => "lock",
        PowerOperation::Suspend => "suspend",
        PowerOperation::Logout => "logout",
        PowerOperation::Reboot => "reboot",
        PowerOperation::Shutdown => "shutdown",
    }
}

fn url_prediction_key(url: &str) -> String {
    format!("url:{url}")
}

fn web_prediction_key(query: &str) -> String {
    format!("web:{query}")
}

fn calc_prediction_key(expression: &str) -> String {
    format!("calc:{expression}")
}

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

fn parse_ssh_config(path: &Path, hosts: &mut BTreeSet<String>) {
    let Ok(contents) = fs::read_to_string(path) else {
        return;
    };

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        if !matches!(parts.next(), Some(keyword) if keyword.eq_ignore_ascii_case("host")) {
            continue;
        }

        for alias in parts {
            if alias.contains('*') || alias.contains('?') || alias.starts_with('!') {
                continue;
            }
            hosts.insert(alias.to_string());
        }
    }
}

fn parse_known_hosts(path: &Path, hosts: &mut BTreeSet<String>) {
    let Ok(contents) = fs::read_to_string(path) else {
        return;
    };

    for line in contents.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }

        let Some(field) = line.split_whitespace().next() else {
            continue;
        };

        if field.starts_with('|') {
            continue;
        }

        for host in field.split(',') {
            let cleaned = host.trim_matches(|ch| ch == '[' || ch == ']');
            let cleaned = cleaned.split(':').next().unwrap_or(cleaned).trim();
            if !cleaned.is_empty() {
                hosts.insert(cleaned.to_string());
            }
        }
    }
}

fn load_commands() -> Vec<String> {
    let mut commands = BTreeSet::new();
    let mut seen_dirs = HashSet::new();

    for dir in env::var_os("PATH")
        .unwrap_or_default()
        .to_string_lossy()
        .split(':')
        .map(PathBuf::from)
    {
        if !seen_dirs.insert(dir.clone()) {
            continue;
        }

        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }

            if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                commands.insert(name.to_string());
            }
        }
    }

    commands.into_iter().collect()
}

fn load_pass_entries(config: &LauncherConfig) -> Vec<PassEntry> {
    let Some(store_dir) = password_store_dir(config) else {
        return Vec::new();
    };

    let mut stack = vec![store_dir.clone()];
    let mut entries = Vec::new();

    while let Some(dir) = stack.pop() {
        let Ok(children) = fs::read_dir(&dir) else {
            continue;
        };

        for child in children.flatten() {
            let path = child.path();
            let Ok(file_type) = child.file_type() else {
                continue;
            };

            if file_type.is_dir() {
                stack.push(path);
                continue;
            }

            if !file_type.is_file() {
                continue;
            }

            let Some(name) = pass_entry_name(&store_dir, &path) else {
                continue;
            };

            entries.push(PassEntry {
                search_blob: name.to_ascii_lowercase(),
                name,
            });
        }
    }

    entries.sort_by(|left, right| left.name.cmp(&right.name));
    entries
}

fn load_recent_files() -> Vec<RecentFileEntry> {
    let Some(data_dir) = dirs::data_dir() else {
        return Vec::new();
    };
    let path = data_dir.join("recently-used.xbel");
    fs::read_to_string(path)
        .map(|contents| parse_recent_files_xbel(&contents))
        .unwrap_or_default()
}

fn load_thunderbird_email_databases(email_config: &crate::config::EmailConfig) -> Vec<PathBuf> {
    if !email_config.thunderbird_enabled {
        return Vec::new();
    }

    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    let roots = [home.join(".thunderbird"), home.join(".mozilla/thunderbird")];
    let mut databases = Vec::new();

    for root in roots {
        let Ok(profiles) = fs::read_dir(root) else {
            continue;
        };

        for profile in profiles.flatten() {
            let path = profile.path().join("global-messages-db.sqlite");
            if path.is_file() {
                databases.push(path);
            }
        }
    }

    databases.sort();
    databases.dedup();
    databases
}

fn load_local_email_entries(email_config: &crate::config::EmailConfig) -> Vec<EmailEntry> {
    if !email_config.local_mail_enabled {
        return Vec::new();
    }

    let roots = local_email_roots(email_config);
    load_local_email_entries_from_roots(&roots)
}

fn load_local_email_entries_from_roots(roots: &[PathBuf]) -> Vec<EmailEntry> {
    if roots.is_empty() {
        return Vec::new();
    }

    let mut entries = Vec::new();
    for root in roots {
        collect_local_email_entries(root, &mut entries);
    }

    entries.sort_by(|left, right| {
        right
            .date_label
            .cmp(&left.date_label)
            .then_with(|| left.subject.cmp(&right.subject))
    });
    entries.truncate(MAX_EMAIL * 8);
    entries
}

fn local_email_roots(_email_config: &crate::config::EmailConfig) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.extend([
            home.join("Maildir"),
            home.join(".local/share/mail"),
            home.join(".mail"),
            home.join("Mail"),
        ]);
    }

    roots.retain(|path| path.exists());
    roots
}

pub(crate) fn evolution_helper_command(
    email_config: &crate::config::EmailConfig,
) -> Option<Vec<String>> {
    let command = email_config
        .evolution_helper_command
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(parse_command_line)
        .and_then(std::result::Result::ok)
        .or_else(|| {
            if command_exists("luma-mail-eds") {
                Some(vec!["luma-mail-eds".to_string()])
            } else {
                None
            }
        })?;

    Some(command)
}

fn search_evolution_email_entries(
    query: &QueryInput,
    now: u64,
    email_config: &crate::config::EmailConfig,
    seen_open_urls: &mut HashSet<String>,
) -> Vec<ResultItem> {
    let Some(command) = evolution_helper_command(email_config) else {
        return Vec::new();
    };

    let Some(response) = run_mail_helper_search(
        &command,
        &query.text,
        MAX_EMAIL * 2,
        email_config.evolution_helper_timeout_ms,
    ) else {
        return Vec::new();
    };

    if !response.ok {
        return Vec::new();
    }

    response
        .results
        .into_iter()
        .filter_map(|summary| {
            let entry = evolution_summary_to_entry(summary, now)?;
            if !seen_open_urls.insert(entry.open_url.clone()) {
                return None;
            }
            let score = score_text(&entry.search_blob, &query.text)?;
            Some((entry, score))
        })
        .flat_map(|(entry, score)| {
            email_result_items(
                &entry,
                score
                    + EMAIL_BASE_SCORE
                    + email_backend_bonus(email_config.preferred_backend, EmailBackend::Evolution),
                query.mode == SearchMode::Email,
            )
        })
        .collect()
}

fn evolution_summary_to_entry(summary: MailEdsMessageSummary, now: u64) -> Option<EmailEntry> {
    let open_url = evolution_helper_action_url("open", &summary.message_id);
    let reply_url = summary
        .replyable
        .then(|| evolution_helper_action_url("reply", &summary.message_id));
    let compose_url = summary
        .composable
        .then(|| evolution_helper_action_url("compose", &summary.message_id));
    let sender = if summary.sender.trim().is_empty() {
        summary
            .sender_email
            .as_ref()
            .map(|value| format!("Sender <{value}>"))
            .unwrap_or_else(|| "Unknown sender".to_string())
    } else {
        summary.sender
    };
    let folder = if summary.folder_uri.trim().is_empty() {
        "Mail".to_string()
    } else {
        folder_label_from_uri(&summary.folder_uri)
    };
    let search_blob = format!(
        "{} {} {} {}",
        summary.subject, sender, folder, summary.snippet
    )
    .to_ascii_lowercase();
    let date_label = if summary.date_label.trim().is_empty() {
        email_date_label(now.saturating_sub(1), now)
    } else {
        summary.date_label
    };

    Some(EmailEntry {
        subject: summary.subject,
        sender,
        sender_email: summary.sender_email,
        folder,
        date_label,
        snippet: email_snippet(&summary.snippet),
        unread: summary.unread,
        has_attachment: summary.has_attachment,
        open_url,
        reply_url,
        compose_url,
        search_blob,
    })
}

fn folder_label_from_uri(folder_uri: &str) -> String {
    folder_uri
        .rsplit('/')
        .next()
        .filter(|label| !label.trim().is_empty())
        .unwrap_or(folder_uri)
        .to_string()
}

fn evolution_helper_action_url(action: &str, message_id: &str) -> String {
    format!(
        "luma-mail-eds://{}?message_id={}",
        action,
        urlencoding::encode(message_id)
    )
}

fn parse_command_line(command: &str) -> std::result::Result<Vec<String>, std::io::Error> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escape = false;

    for ch in command.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }

        match ch {
            '\\' => escape = true,
            '"' => in_quotes = !in_quotes,
            ch if ch.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    args.push(current.clone());
                    current.clear();
                }
            }
            ch => current.push(ch),
        }
    }

    if escape || in_quotes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unterminated quoted command",
        ));
    }

    if !current.is_empty() {
        args.push(current);
    }

    if args.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "empty helper command",
        ));
    }

    Ok(args)
}

fn run_mail_helper_search(
    command: &[String],
    query: &str,
    limit: usize,
    timeout_ms: u64,
) -> Option<MailEdsSearchResponse> {
    let mut cmd = Command::new(command.first()?);
    if command.len() > 1 {
        cmd.args(&command[1..]);
    }
    cmd.args(["search", "--query", query, "--limit", &limit.to_string()]);

    let output = run_command_with_timeout(cmd, timeout_ms).ok()?;
    if !output.status.success() {
        return None;
    }

    serde_json::from_slice::<MailEdsSearchResponse>(&output.stdout).ok()
}

pub(crate) fn run_mail_helper_action(
    command: &[String],
    subcommand: &str,
    message_id: &str,
    timeout_ms: u64,
) -> Result<()> {
    let mut cmd = Command::new(command.first().context("missing mail helper command")?);
    if command.len() > 1 {
        cmd.args(&command[1..]);
    }
    cmd.args([subcommand, "--message-id", message_id]);

    let output = run_command_with_timeout(cmd, timeout_ms)
        .with_context(|| format!("failed to run mail helper {subcommand}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let message = stderr.trim().to_string();
        let message = if message.is_empty() {
            stdout.trim().to_string()
        } else {
            message
        };
        let message = if message.is_empty() {
            format!("mail helper {subcommand} failed")
        } else {
            message
        };
        return Err(anyhow::anyhow!("{message}"));
    }

    Ok(())
}

fn run_command_with_timeout(
    mut command: Command,
    timeout_ms: u64,
) -> std::io::Result<std::process::Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));

    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "mail helper timed out",
            ));
        }

        thread::sleep(Duration::from_millis(25));
    }
}

fn collect_local_email_entries(root: &Path, entries: &mut Vec<EmailEntry>) {
    let Ok(metadata) = fs::metadata(root) else {
        return;
    };

    if metadata.is_file() {
        if let Some(entry) = parse_local_email_file(root) {
            entries.push(entry);
        }
        return;
    }

    let Ok(children) = fs::read_dir(root) else {
        return;
    };

    for child in children.flatten() {
        let path = child.path();
        if path.is_dir() {
            let dir_name = path
                .file_name()
                .and_then(|part| part.to_str())
                .unwrap_or("");
            if matches!(dir_name, "cur" | "new") {
                collect_maildir_messages(&path, entries);
            } else {
                collect_local_email_entries(&path, entries);
            }
        } else if path.is_file() {
            if path.extension().and_then(|part| part.to_str()) == Some("eml") {
                if let Some(entry) = parse_local_email_file(&path) {
                    entries.push(entry);
                }
            }
        }
    }
}

fn collect_maildir_messages(root: &Path, entries: &mut Vec<EmailEntry>) {
    let Ok(children) = fs::read_dir(root) else {
        return;
    };

    for child in children.flatten() {
        let path = child.path();
        if path.is_file()
            && let Some(entry) = parse_local_email_file(&path)
        {
            entries.push(entry);
        }
    }
}

fn parse_local_email_file(path: &Path) -> Option<EmailEntry> {
    let contents = fs::read_to_string(path).ok()?;
    let (headers, body) = split_email_headers_and_body(&contents)?;
    let subject = header_value(headers, "subject").unwrap_or_else(|| "(no subject)".to_string());
    let sender = header_value(headers, "from")
        .or_else(|| header_value(headers, "sender"))
        .unwrap_or_else(|| "Unknown sender".to_string());
    let sender_email = extract_email_address(&sender);
    let folder = path
        .parent()
        .and_then(
            |parent| match parent.file_name().and_then(|part| part.to_str()) {
                Some("cur") | Some("new") => parent
                    .parent()
                    .and_then(|grandparent| grandparent.file_name())
                    .and_then(|part| part.to_str())
                    .map(|part| part.to_string()),
                _ => parent
                    .file_name()
                    .and_then(|part| part.to_str())
                    .map(|part| part.to_string()),
            },
        )
        .unwrap_or_else(|| "Mail".to_string());
    let date_label = header_value(headers, "date")
        .map(|date| date.split('(').next().unwrap_or(&date).trim().to_string())
        .unwrap_or_default();
    let search_blob = format!("{subject} {sender} {folder} {body}").to_ascii_lowercase();
    let open_url = gio::File::for_path(path).uri();
    let headers_lower = headers.to_ascii_lowercase();
    let has_attachment = headers_lower.contains("multipart/mixed")
        || headers_lower.contains("content-disposition: attachment");

    Some(EmailEntry {
        subject,
        sender,
        sender_email,
        folder,
        date_label,
        snippet: email_snippet(body),
        unread: maildir_is_unread(path),
        has_attachment,
        open_url: open_url.to_string(),
        reply_url: None,
        compose_url: None,
        search_blob,
    })
}

/// Infer whether a Maildir message is unread from its filename flags / folder.
fn maildir_is_unread(path: &Path) -> bool {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    match file_name.rsplit_once(":2,") {
        // The info section after ":2," holds flags; 'S' (Seen) means the message is read.
        Some((_, flags)) => !flags.contains('S'),
        // No info section: messages in `new/` are unread, anything else is assumed read.
        None => path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .map(|name| name == "new")
            .unwrap_or(false),
    }
}

fn split_email_headers_and_body(raw: &str) -> Option<(&str, &str)> {
    if let Some(boundary) = raw.find("\r\n\r\n") {
        return Some((&raw[..boundary], &raw[boundary + 4..]));
    }

    let boundary = raw.find("\n\n")?;
    Some((&raw[..boundary], &raw[boundary + 2..]))
}

fn parse_chromium_bookmarks_json(raw: &str) -> Vec<BookmarkEntry> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    if let Some(roots) = value.get("roots").and_then(serde_json::Value::as_object) {
        for root in roots.values() {
            collect_chromium_bookmarks(root, &mut entries);
        }
    }
    entries
}

fn collect_chromium_bookmarks(value: &serde_json::Value, entries: &mut Vec<BookmarkEntry>) {
    if value.get("type").and_then(serde_json::Value::as_str) == Some("url") {
        if let Some(url) = string_field(value, "url") {
            let title = string_field(value, "name").unwrap_or_else(|| url.clone());
            if let Some(entry) = bookmark_entry(title, url) {
                entries.push(entry);
            }
        }
    }

    if let Some(children) = value.get("children").and_then(serde_json::Value::as_array) {
        for child in children {
            collect_chromium_bookmarks(child, entries);
        }
    }
}

fn parse_firefox_bookmark_rows(raw: &str) -> Vec<BookmarkEntry> {
    raw.lines()
        .filter_map(|line| {
            let (title, url) = line.split_once('\t')?;
            bookmark_entry(title.trim().to_string(), url.trim().to_string())
        })
        .collect()
}

fn bookmark_entry(title: String, url: String) -> Option<BookmarkEntry> {
    let url = url.trim();
    if url.is_empty() || url.starts_with("place:") {
        return None;
    }

    let title = if title.trim().is_empty() {
        url.to_string()
    } else {
        title.trim().to_string()
    };

    Some(BookmarkEntry {
        search_blob: format!("{title} {url}").to_ascii_lowercase(),
        title,
        url: url.to_string(),
    })
}

fn parse_recent_files_xbel(raw: &str) -> Vec<RecentFileEntry> {
    use xml::reader::{EventReader, XmlEvent};

    let parser = EventReader::from_str(raw);
    let mut entries = Vec::new();
    let mut href = None::<String>;
    let mut modified = 0;
    let mut title = String::new();
    let mut in_title = false;

    for event in parser {
        match event {
            Ok(XmlEvent::StartElement {
                name, attributes, ..
            }) if name.local_name == "bookmark" => {
                href = attributes
                    .iter()
                    .find(|attribute| attribute.name.local_name == "href")
                    .map(|attribute| attribute.value.clone());
                modified = attributes
                    .iter()
                    .find(|attribute| attribute.name.local_name == "modified")
                    .or_else(|| {
                        attributes
                            .iter()
                            .find(|attribute| attribute.name.local_name == "visited")
                    })
                    .and_then(|attribute| parse_xbel_timestamp(&attribute.value))
                    .unwrap_or_default();
                title.clear();
            }
            Ok(XmlEvent::StartElement { name, .. }) if name.local_name == "title" => {
                in_title = true;
                title.clear();
            }
            Ok(XmlEvent::Characters(text)) if in_title => title.push_str(&text),
            Ok(XmlEvent::EndElement { name }) if name.local_name == "title" => {
                in_title = false;
            }
            Ok(XmlEvent::EndElement { name }) if name.local_name == "bookmark" => {
                if let Some(href) = href.take() {
                    if let Some(entry) = recent_file_entry(&href, &title, modified) {
                        entries.push(entry);
                    }
                }
                title.clear();
                modified = 0;
                in_title = false;
            }
            _ => {}
        }
    }

    entries.sort_by(|left, right| {
        right
            .modified
            .cmp(&left.modified)
            .then_with(|| left.title.cmp(&right.title))
    });
    let mut seen_paths = BTreeSet::new();
    entries.retain(|entry| seen_paths.insert(entry.path.clone()));
    entries
}

fn recent_file_entry(href: &str, title: &str, modified: i64) -> Option<RecentFileEntry> {
    let path = file_uri_to_path(href)?;
    let title = if title.trim().is_empty() {
        Path::new(&path)
            .file_name()
            .and_then(|part| part.to_str())
            .unwrap_or(path.as_str())
            .to_string()
    } else {
        title.trim().to_string()
    };

    Some(RecentFileEntry {
        search_blob: format!("{title} {path}").to_ascii_lowercase(),
        title,
        path,
        modified,
    })
}

fn file_uri_to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let path = rest
        .strip_prefix("localhost/")
        .map(|path| format!("/{path}"))
        .unwrap_or_else(|| rest.to_string());
    if !path.starts_with('/') {
        return None;
    }
    urlencoding::decode(&path)
        .ok()
        .map(|path| path.into_owned())
        .filter(|path| !path.is_empty())
}

fn parse_xbel_timestamp(raw: &str) -> Option<i64> {
    let digits = raw
        .chars()
        .filter(char::is_ascii_digit)
        .take(14)
        .collect::<String>();
    if digits.len() < 8 {
        return None;
    }
    digits.parse().ok()
}

fn password_store_dir(config: &LauncherConfig) -> Option<PathBuf> {
    config
        .integrations
        .password_store_dir
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("PASSWORD_STORE_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| dirs::home_dir().map(|home| home.join(".password-store")))
        .filter(|path| path.is_dir())
}

fn pass_entry_name(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let relative = relative.to_string_lossy();
    let name = relative.strip_suffix(".gpg")?;
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

fn parse_file_search_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let candidate = trimmed
        .split_once(' ')
        .map(|(_, rest)| rest.trim())
        .unwrap_or(trimmed);

    if candidate.starts_with("file://") {
        let file = gio::File::for_uri(candidate);
        if let Some(path) = file.path() {
            return Some(path.to_string_lossy().to_string());
        }

        let decoded = urlencoding::decode(candidate.strip_prefix("file://")?).ok()?;
        return Some(decoded.into_owned());
    }

    Some(candidate.to_string())
}

#[cfg(test)]
mod tests {
    use super::windows::{parse_hypr_windows_json, parse_niri_windows_json, window_focus_command};
    use super::{
        AppEntry, BluetoothStatus, BookmarkEntry, ControlSnapshot, EmailEntry, FileSearchBackend,
        MediaStatus, NetworkStatus, NotificationEntry, PackageSearchBackend, RecentFileEntry,
        Sources, VolumeStatus, append_deferred_results, control_results_from_snapshot,
        display_domain, email_badges, email_primary_subtitle, email_result_items, email_snippet,
        maildir_is_unread, no_results_item, package_backend_for_query,
        package_command_path_with_fallbacks, package_search_results_from_output,
        parse_bluetooth_status, parse_chromium_bookmarks_json, parse_dunst_history,
        parse_file_search_line, parse_firefox_bookmark_rows, parse_nmcli_device_status,
        parse_package_search_output, parse_playerctl_metadata, parse_recent_files_xbel,
        parse_thunderbird_email_row, parse_wpctl_volume, pass_entry_name, relative_age_label,
        thunderbird_database_uri, thunderbird_message_uri,
    };
    use crate::model::{
        Action, DesktopControlOperation, PowerOperation, QueryInput, ResultItem, SearchMode,
        SourceFilter, WindowFocusTarget,
    };
    use crate::prediction::{PredictionStore, StoredPrediction};
    use std::fs::{self, File};
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    fn empty_prediction_store() -> Arc<Mutex<PredictionStore>> {
        Arc::new(Mutex::new(PredictionStore::disabled()))
    }

    #[test]
    fn relative_age_label_buckets_by_elapsed_time() {
        let now = 1_000_000;
        assert_eq!(relative_age_label(now - 10, now), "just now");
        assert_eq!(relative_age_label(now - 120, now), "2m ago");
        assert_eq!(relative_age_label(now - 7_200, now), "2h ago");
        assert_eq!(relative_age_label(now - 90_000, now), "yesterday");
        assert_eq!(relative_age_label(now - 300_000, now), "3d ago");
        // A timestamp in the future must not underflow.
        assert_eq!(relative_age_label(now + 500, now), "just now");
    }

    #[test]
    fn display_domain_strips_scheme_port_path_and_www() {
        assert_eq!(
            display_domain("https://www.example.com/path?q=1"),
            "example.com"
        );
        assert_eq!(display_domain("http://docs.rs:8080/crate"), "docs.rs");
        assert_eq!(
            display_domain("https://user@host.example.org/"),
            "host.example.org"
        );
        // Non-URL input falls back to the original string.
        assert_eq!(display_domain("just a keyword"), "just a keyword");
    }

    #[test]
    fn email_snippet_collapses_whitespace_and_truncates() {
        assert_eq!(
            email_snippet("  hello\n\tthere  world "),
            "hello there world"
        );
        let long = "word ".repeat(60);
        let snippet = email_snippet(&long);
        assert!(snippet.ends_with('…'));
        assert!(snippet.chars().count() <= 121);
    }

    #[test]
    fn email_primary_subtitle_joins_sender_folder_and_snippet() {
        assert_eq!(
            email_primary_subtitle("Robin", "INBOX", "Quarterly numbers"),
            "Robin · INBOX · Quarterly numbers"
        );
        assert_eq!(
            email_primary_subtitle("Robin", "INBOX", "   "),
            "Robin · INBOX"
        );
    }

    #[test]
    fn email_badges_reflect_unread_and_attachment_flags() {
        use crate::model::EntryBadge;
        let mut entry = sample_email_entry();
        entry.unread = true;
        entry.has_attachment = true;
        assert_eq!(
            email_badges(&entry),
            vec![EntryBadge::Unread, EntryBadge::Attachment]
        );

        entry.unread = false;
        entry.has_attachment = false;
        assert!(email_badges(&entry).is_empty());
    }

    #[test]
    fn maildir_is_unread_reads_seen_flag_and_folder() {
        assert!(maildir_is_unread(Path::new("/mail/cur/123.host:2,FR")));
        assert!(!maildir_is_unread(Path::new("/mail/cur/123.host:2,RS")));
        assert!(maildir_is_unread(Path::new("/mail/new/123.host")));
        assert!(!maildir_is_unread(Path::new("/mail/cur/123.host")));
    }

    #[test]
    fn thunderbird_rows_derive_unread_and_attachment_from_flags() {
        // flags 0 → unread, no attachment.
        let unread = parse_thunderbird_email_row(
            "1700000000000000\t42\timap://h/INBOX\tInbox\tHi\tAlice <a@b.com>\t0\tHello there",
        )
        .expect("row parses");
        assert!(unread.unread);
        assert!(!unread.has_attachment);
        assert_eq!(unread.snippet, "Hello there");

        // Read (0x1) + Attachment (0x10000000) = 268435457.
        let read = parse_thunderbird_email_row(
            "1700000000000000\t42\timap://h/INBOX\tInbox\tHi\tAlice <a@b.com>\t268435457\tBody",
        )
        .expect("row parses");
        assert!(!read.unread);
        assert!(read.has_attachment);
    }

    fn sample_email_entry() -> EmailEntry {
        EmailEntry {
            subject: "Subject".to_string(),
            sender: "Robin".to_string(),
            sender_email: Some("robin@example.com".to_string()),
            folder: "INBOX".to_string(),
            date_label: "2h ago".to_string(),
            snippet: "snippet".to_string(),
            unread: false,
            has_attachment: false,
            open_url: "imap-message://imap://h/INBOX#1".to_string(),
            reply_url: None,
            compose_url: None,
            search_blob: "subject robin".to_string(),
        }
    }

    fn prediction_store_with(
        prediction: StoredPrediction,
        now: u64,
    ) -> Arc<Mutex<PredictionStore>> {
        let mut store = PredictionStore::disabled();
        store.record(prediction, now).expect("record prediction");
        Arc::new(Mutex::new(store))
    }

    fn empty_sources() -> Sources {
        let mut sources = Sources::with_config(crate::config::LauncherConfig::default());
        sources.apps = Vec::new();
        sources.ssh_hosts = Vec::new();
        sources.pass_entries = pass_entries(Vec::new());
        sources.commands = Vec::new();
        sources.bookmarks = Vec::new();
        sources.recent_files = Vec::new();
        sources.thunderbird_email_database_paths = Vec::new();
        sources.local_email_entries = Vec::new();
        sources.file_search_backend = None;
        sources.package_search_backend = None;
        sources.pass_available = false;
        sources.qalc_available = false;
        sources.predictions = empty_prediction_store();
        sources
    }

    fn pass_entries(entries: Vec<super::PassEntry>) -> Arc<Mutex<Vec<super::PassEntry>>> {
        Arc::new(Mutex::new(entries))
    }

    #[test]
    fn indexed_paths_are_uri_decoded() {
        let line = "file:///tmp/with%20space%23hash.txt";
        assert_eq!(
            parse_file_search_line(line).as_deref(),
            Some("/tmp/with space#hash.txt")
        );
    }

    #[test]
    fn pacman_and_paru_search_output_parses_package_rows() {
        let output = "\
extra/firefox 139.0-1 [installed]
    Standalone web browser from Mozilla
aur/visual-studio-code-bin 1.101.0-1 (+1925 8.44)
    Visual Studio Code editor
";

        let entries = parse_package_search_output(output);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].repository, "extra");
        assert_eq!(entries[0].name, "firefox");
        assert_eq!(entries[0].version, "139.0-1");
        assert_eq!(
            entries[0].description,
            "Standalone web browser from Mozilla"
        );
        assert!(entries[0].installed);
        assert_eq!(entries[1].repository, "aur");
        assert_eq!(entries[1].name, "visual-studio-code-bin");
        assert!(!entries[1].installed);
    }

    #[test]
    fn package_search_is_deferred_for_all_and_package_modes() {
        let mut sources = empty_sources();
        sources.package_search_backend = Some(PackageSearchBackend::Pacman);

        let all_snapshot = sources.search_snapshot("firefox", SearchMode::All, None);
        assert!(all_snapshot.deferred.packages);
        assert!(
            all_snapshot
                .immediate_results
                .iter()
                .all(|item| item.source != "Packages")
        );

        let package_snapshot = sources.search_snapshot("pkg: firefox", SearchMode::All, None);
        assert_eq!(package_snapshot.query.mode, SearchMode::Packages);
        assert!(package_snapshot.deferred.packages);
    }

    #[test]
    fn package_manager_prefix_overrides_detected_backend() {
        let paru_query = QueryInput::parse("paru: veloren", SearchMode::All);
        assert_eq!(
            package_backend_for_query(&paru_query, Some(PackageSearchBackend::Pacman)),
            Some(PackageSearchBackend::Paru)
        );

        let pacman_query = QueryInput::parse("pacman: firefox", SearchMode::All);
        assert_eq!(
            package_backend_for_query(&pacman_query, Some(PackageSearchBackend::Paru)),
            Some(PackageSearchBackend::Pacman)
        );

        let auto_query = QueryInput::parse("pkg: firefox", SearchMode::All);
        assert_eq!(
            package_backend_for_query(&auto_query, Some(PackageSearchBackend::Paru)),
            Some(PackageSearchBackend::Paru)
        );
    }

    #[test]
    fn package_command_lookup_uses_fallback_dirs_when_path_is_empty() {
        let temp_dir = std::env::temp_dir().join(format!(
            "Luma-package-command-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        fs::create_dir_all(&temp_dir).expect("create fake command dir");
        let paru = temp_dir.join("paru");
        File::create(&paru).expect("create fake paru");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&paru)
                .expect("fake paru metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&paru, permissions).expect("make fake paru executable");
        }

        assert_eq!(
            package_command_path_with_fallbacks("paru", Some(""), &[temp_dir.as_path()]),
            Some(paru)
        );

        fs::remove_dir_all(&temp_dir).expect("cleanup fake command dir");
    }

    #[test]
    fn package_search_output_becomes_install_actions() {
        let output = "\
extra/firefox 139.0-1
    Standalone web browser from Mozilla
";

        let results = package_search_results_from_output(output, PackageSearchBackend::Paru, 1_000);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source, "Packages");
        assert_eq!(results[0].title, "firefox");
        assert_eq!(
            results[0].subtitle,
            "extra 139.0-1 - Standalone web browser from Mozilla"
        );
        assert_eq!(
            results[0].prediction_key.as_deref(),
            Some("package:extra/firefox")
        );
        assert!(matches!(
            &results[0].action,
            Action::InstallPackage { package, manager }
                if package == "firefox" && *manager == crate::model::PackageManager::Paru
        ));
    }

    #[test]
    fn package_results_are_low_priority_independent_of_query_mode() {
        let output = "\
aur/veloren 0.18.0-1 [+21 ~0.00]
    The last stable release of an open-world, open-source multiplayer voxel RPG.
";
        let results = package_search_results_from_output(output, PackageSearchBackend::Paru, 1_000);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "veloren");
        assert!(
            results[0].score < 280,
            "unprefixed package score {} should stay below normal deferred sources",
            results[0].score
        );
    }

    #[test]
    fn deferred_package_results_remain_visible_when_immediate_results_fill_the_limit() {
        let immediate = (0..24)
            .map(|index| scored_item("Applications", &format!("App {index}"), 2_000 - index))
            .collect::<Vec<_>>();
        let deferred = vec![scored_item("Packages", "veloren", 180)];

        let merged = append_deferred_results(immediate, deferred);

        assert_eq!(merged.last().map(|item| item.source), Some("Packages"));
        assert!(merged.iter().any(|item| item.title == "veloren"));
    }

    #[test]
    fn thunderbird_message_uris_keep_the_folder_uri() {
        assert_eq!(
            thunderbird_message_uri("imap://example.com/INBOX", 123),
            "imap-message://imap://example.com/INBOX#123"
        );
        assert_eq!(
            thunderbird_message_uri("mailbox://Local%20Folders/Sent", 77),
            "mailbox-message://mailbox://Local%20Folders/Sent#77"
        );
    }

    #[test]
    fn thunderbird_database_uri_uses_immutable_file_reads() {
        let uri = thunderbird_database_uri(Path::new("/tmp/global-messages-db.sqlite"));
        assert_eq!(uri, "file:/tmp/global-messages-db.sqlite?immutable=1");
    }

    #[test]
    fn email_result_items_offer_open_reply_compose_and_copy_actions() {
        let entry = EmailEntry {
            subject: "Quarterly update".to_string(),
            sender: "Robin <robin@example.com>".to_string(),
            sender_email: Some("robin@example.com".to_string()),
            folder: "INBOX".to_string(),
            date_label: "2h ago".to_string(),
            open_url: "imap-message://imap://example.com/INBOX#123".to_string(),
            reply_url: None,
            compose_url: None,
            snippet: String::new(),
            unread: false,
            has_attachment: false,
            search_blob: "quarterly update robin inbox".to_string(),
        };

        let items = email_result_items(&entry, 100, true);
        assert_eq!(items.len(), 4);
        assert!(matches!(items[0].action, Action::OpenUrl { .. }));
        assert!(matches!(items[1].action, Action::OpenUrl { .. }));
        assert!(matches!(items[2].action, Action::OpenUrl { .. }));
        assert!(matches!(items[3].action, Action::CopyText { .. }));
    }

    #[test]
    fn email_search_returns_rows_for_matching_local_entries() {
        let mut sources = empty_sources();
        sources.local_email_entries = vec![EmailEntry {
            subject: "Quarterly update".to_string(),
            sender: "Robin <robin@example.com>".to_string(),
            sender_email: Some("robin@example.com".to_string()),
            folder: "INBOX".to_string(),
            date_label: "2h ago".to_string(),
            open_url: "imap-message://imap://example.com/INBOX#123".to_string(),
            reply_url: None,
            compose_url: None,
            snippet: String::new(),
            unread: false,
            has_attachment: false,
            search_blob: "quarterly update robin inbox".to_string(),
        }];

        let query = QueryInput::parse("mail: quarterly", SearchMode::All);
        let results = sources.search_email(&query, 1_700_000_000);

        assert!(
            results
                .iter()
                .any(|item| item.title == "Open email: Quarterly update")
        );
        assert!(
            results
                .iter()
                .any(|item| item.title == "Reply to Robin <robin@example.com>")
        );
        assert!(
            results
                .iter()
                .any(|item| item.title == "Compose to Robin <robin@example.com>")
        );
        assert!(
            results
                .iter()
                .any(|item| item.title == "Copy sender: robin@example.com")
        );
    }

    #[test]
    fn controls_mode_surfaces_status_rich_control_rows() {
        let snapshot = ControlSnapshot {
            media: Some(MediaStatus {
                player: "firefox".to_string(),
                status: "Paused".to_string(),
                artist: "Artist".to_string(),
                title: "Track".to_string(),
            }),
            volume: Some(VolumeStatus {
                percent: 61,
                muted: false,
            }),
            bluetooth: Some(BluetoothStatus {
                powered: true,
                connected_count: 2,
            }),
            network: Some(NetworkStatus {
                kind: "ethernet".to_string(),
                connection: "Ethernet connection 1".to_string(),
            }),
            power_profile: Some("performance".to_string()),
            screen_brightness: None,
            notifications: Vec::new(),
            has_playerctl: true,
            has_wpctl: true,
            has_bluetoothctl: true,
            has_nmcli: true,
            has_powerprofilesctl: true,
            has_dunstctl: true,
            has_grim: true,
            has_slurp: true,
            has_hyprpicker: true,
        };
        let query = QueryInput::parse("control:", SearchMode::All);
        let items = control_results_from_snapshot(&snapshot, &query, 1_700_000_000, |_| 0);

        assert!(items.iter().any(|item| item.title == "Play/Pause media"
            && item.subtitle.contains("Artist - Track")
            && matches!(
                item.action,
                Action::DesktopControl {
                    operation: DesktopControlOperation::MediaPlayPause
                }
            )));
        assert!(items.iter().any(|item| item.title == "Mute volume"
            && item.subtitle == "Volume 61%"
            && matches!(
                item.action,
                Action::DesktopControl {
                    operation: DesktopControlOperation::VolumeMute
                }
            )));
        assert!(items.iter().any(|item| item.title == "Toggle Bluetooth"
            && item.subtitle == "On - 2 connected"
            && matches!(
                item.action,
                Action::DesktopControl {
                    operation: DesktopControlOperation::BluetoothTogglePower
                }
            )));
        assert!(
            items
                .iter()
                .any(|item| item.title == "Open network settings"
                    && item.subtitle == "Ethernet - Ethernet connection 1")
        );
        assert!(items.iter().any(|item| item.title == "Pick screen color"));
        assert!(
            items
                .iter()
                .any(|item| item.title == "Pause/resume notifications")
        );
    }

    #[test]
    fn control_search_matches_notification_history_without_prediction_key() {
        let snapshot = ControlSnapshot {
            notifications: vec![NotificationEntry {
                app_name: "kitty".to_string(),
                summary: "Build failed".to_string(),
                body: "cargo test failed".to_string(),
                search_blob: "kitty build failed cargo test failed".to_string(),
            }],
            has_dunstctl: true,
            ..ControlSnapshot::default()
        };
        let query = QueryInput::parse("control: cargo", SearchMode::All);
        let items = control_results_from_snapshot(&snapshot, &query, 1_700_000_000, |_| 0);

        let notification = items
            .iter()
            .find(|item| item.title == "Notification: Build failed")
            .expect("notification result");
        assert_eq!(notification.source, "Controls");
        assert!(notification.prediction_key.is_none());
        assert!(matches!(
            notification.action,
            Action::DesktopControl {
                operation: DesktopControlOperation::NotificationHistoryPop
            }
        ));
    }

    #[test]
    fn controls_include_brightness_rows_only_when_a_backlight_exists() {
        let query = QueryInput::parse("control: brightness", SearchMode::All);
        let without_backlight = control_results_from_snapshot(
            &ControlSnapshot::default(),
            &query,
            1_700_000_000,
            |_| 0,
        );
        assert!(
            without_backlight
                .iter()
                .all(|item| !item.title.contains("brightness"))
        );

        let with_backlight = control_results_from_snapshot(
            &ControlSnapshot {
                screen_brightness: Some(42),
                ..ControlSnapshot::default()
            },
            &query,
            1_700_000_000,
            |_| 0,
        );
        assert!(with_backlight.iter().any(
            |item| item.title == "Raise brightness" && item.subtitle == "Screen brightness 42%"
        ));
        assert!(with_backlight.iter().any(
            |item| item.title == "Lower brightness" && item.subtitle == "Screen brightness 42%"
        ));
    }

    #[test]
    fn control_parsers_extract_status_from_command_output() {
        let media = parse_playerctl_metadata("firefox|Paused|Artist|Track").expect("media");
        assert_eq!(media.player, "firefox");
        assert_eq!(media.status, "Paused");
        assert_eq!(media.artist, "Artist");
        assert_eq!(media.title, "Track");

        let volume = parse_wpctl_volume("Volume: 0.61 [MUTED]").expect("volume");
        assert_eq!(volume.percent, 61);
        assert!(volume.muted);

        let bluetooth = parse_bluetooth_status(
            "Controller AA:BB\n\tPowered: yes\n",
            "Device 01:02 Headphones\nDevice 03:04 Keyboard\n",
        )
        .expect("bluetooth");
        assert!(bluetooth.powered);
        assert_eq!(bluetooth.connected_count, 2);

        let network = parse_nmcli_device_status("ethernet:connected:Ethernet connection 1\n")
            .expect("network");
        assert_eq!(network.kind, "ethernet");
        assert_eq!(network.connection, "Ethernet connection 1");
    }

    #[test]
    fn parses_dunst_history_summaries_and_bodies() {
        let entries = parse_dunst_history(
            r#"{
                "summary" : { "type" : "s", "data" : "Build failed" },
                "body" : { "type" : "s", "data" : "cargo test failed" },
                "appname" : { "type" : "s", "data" : "kitty" }
            }"#,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].summary, "Build failed");
        assert_eq!(entries[0].body, "cargo test failed");
        assert_eq!(entries[0].app_name, "kitty");
        assert_eq!(
            entries[0].search_blob,
            "kitty build failed cargo test failed"
        );
    }

    #[test]
    fn apps_outrank_strongly_matching_email_in_unified_search() {
        let mut sources = empty_sources();
        sources.apps = vec![AppEntry {
            desktop_id: "report-studio.desktop".to_string(),
            name: "Report Studio".to_string(),
            description: "Build reports".to_string(),
            executable: "report-studio".to_string(),
            icon_name: "application-x-executable-symbolic".to_string(),
            search_blob: "report studio".to_string(),
        }];
        sources.local_email_entries = vec![EmailEntry {
            subject: "report".to_string(),
            sender: "Robin <robin@example.com>".to_string(),
            sender_email: Some("robin@example.com".to_string()),
            folder: "INBOX".to_string(),
            date_label: "2h ago".to_string(),
            open_url: "imap-message://imap://example.com/INBOX#1".to_string(),
            reply_url: None,
            compose_url: None,
            snippet: String::new(),
            unread: false,
            has_attachment: false,
            search_blob: "report".to_string(),
        }];

        let results = sources.search("report", SearchMode::All);

        let app_index = results
            .iter()
            .position(|item| item.source == "Applications")
            .expect("an application result");
        let email_index = results
            .iter()
            .position(|item| item.source == "Email")
            .expect("an email result");

        assert!(
            app_index < email_index,
            "expected the application to outrank email, got app at {app_index}, email at {email_index}"
        );
    }

    #[test]
    fn entering_a_url_ranks_the_open_url_action_first() {
        let mut sources = empty_sources();
        sources.bookmarks = vec![BookmarkEntry {
            title: "Example".to_string(),
            url: "https://example.com".to_string(),
            search_blob: "https://example.com example bookmark".to_string(),
        }];

        let results = sources.search("https://example.com", SearchMode::All);

        let first = results.first().expect("at least one result");
        assert_eq!(first.source, "Web");
        assert!(
            matches!(&first.action, Action::OpenUrl { url } if url == "https://example.com"),
            "expected the Open URL action ranked first, got {:?} ({:?})",
            first.title,
            first.action
        );
    }

    #[test]
    fn typing_a_bare_domain_ranks_open_url_above_a_matching_bookmark() {
        let mut sources = empty_sources();
        sources.bookmarks = vec![BookmarkEntry {
            title: "Reddit".to_string(),
            url: "https://reddit.com".to_string(),
            search_blob: "reddit https://reddit.com reddit.com".to_string(),
        }];

        let results = sources.search("reddit.com", SearchMode::All);

        let first = results.first().expect("at least one result");
        assert_eq!(first.source, "Web");
        assert!(
            matches!(&first.action, Action::OpenUrl { url } if url == "https://reddit.com"),
            "expected the Open URL action ranked first, got {:?} ({:?})",
            first.title,
            first.action
        );
    }

    fn scored_item(source: &'static str, title: &str, score: i32) -> ResultItem {
        ResultItem {
            prediction_key: None,
            title: title.to_string(),
            subtitle: String::new(),
            source,
            icon_name: String::new(),
            score,
            action: Action::None,
            ..Default::default()
        }
    }

    #[test]
    fn deferred_results_load_below_immediate_results_without_reordering_them() {
        let immediate = vec![
            scored_item("Applications", "Firefox", 1900),
            scored_item("Web", "Search the web for reddit.com", 120),
        ];
        // The email outscores the low web result, but it arrives later and must
        // load below the already-shown rows so their positions stay stable.
        let deferred = vec![scored_item("Email", "Open email: reddit signup", 900)];

        let merged = append_deferred_results(immediate, deferred);

        let titles: Vec<&str> = merged.iter().map(|item| item.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "Firefox",
                "Search the web for reddit.com",
                "Open email: reddit signup",
            ]
        );
    }

    #[test]
    fn disabling_email_search_removes_email_results() {
        let config = crate::config::LauncherConfig {
            sources: crate::config::SourceToggles {
                email: false,
                ..crate::config::SourceToggles::default()
            },
            ..crate::config::LauncherConfig::default()
        };
        let sources = Sources::with_config(config);

        let results = sources.search("mail: github", SearchMode::All);

        assert!(
            results
                .iter()
                .any(|item| item.title == "Email search is disabled")
        );
        assert!(
            results
                .iter()
                .all(|item| { item.source != "Email" || item.title == "Email search is disabled" })
        );
    }

    #[test]
    fn parses_hyprland_windows_for_switching() {
        let windows = parse_hypr_windows_json(
            r#"[
              {
                "address": "0xabc",
                "class": "kitty",
                "title": "editor",
                "workspace": {"name": "2"},
                "mapped": true,
                "hidden": false,
                "xwayland": true,
                "focusHistoryID": 3
              },
              {
                "address": "0xdef",
                "class": "launcher",
                "title": "hidden",
                "workspace": {"name": "special"},
                "mapped": false,
                "hidden": true,
                "focusHistoryID": 9
              }
            ]"#,
        )
        .expect("parse hypr window json");

        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].title, "editor");
        assert_eq!(windows[0].app_name, "kitty");
        assert_eq!(windows[0].workspace, "2");
        assert_eq!(
            windows[0].focus_target,
            WindowFocusTarget::Hyprland {
                address: "0xabc".to_string(),
                xwayland: true
            }
        );
    }

    #[test]
    fn builds_native_focus_command_for_hyprland_window() {
        let (program, args) = window_focus_command(&WindowFocusTarget::Hyprland {
            address: "0xabc".to_string(),
            xwayland: false,
        });

        assert_eq!(program, "hyprctl");
        assert_eq!(args, vec!["dispatch", "focuswindow", "address:0xabc"]);
    }

    #[test]
    fn parses_niri_windows_for_switching() {
        let windows = parse_niri_windows_json(
            r#"[
              {
                "id": 42,
                "app_id": "firefox",
                "title": "Docs",
                "workspace_id": 7
              }
            ]"#,
        )
        .expect("parse niri window json");

        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].title, "Docs");
        assert_eq!(windows[0].app_name, "firefox");
        assert_eq!(windows[0].workspace, "7");
        assert_eq!(windows[0].focus_target, WindowFocusTarget::Niri { id: 42 });
    }

    #[test]
    fn builds_native_focus_command_for_niri_window() {
        let (program, args) = window_focus_command(&WindowFocusTarget::Niri { id: 42 });

        assert_eq!(program, "niri");
        assert_eq!(args, vec!["msg", "action", "focus-window", "--id", "42"]);
    }

    #[test]
    fn builds_focus_command_for_x11_window() {
        let (program, args) = window_focus_command(&WindowFocusTarget::X11 {
            window_id: "12345".to_string(),
        });

        assert_eq!(program, "xdotool");
        assert_eq!(args, vec!["windowactivate", "--sync", "12345"]);
    }

    #[test]
    fn search_returns_status_item_when_no_matches_exist() {
        let sources = empty_sources();

        let results = sources.search("unlikely-query", SearchMode::Apps);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source, "Status");
        assert!(matches!(results[0].action, Action::None));
    }

    #[test]
    fn bare_urls_surface_as_browser_results_in_all_mode() {
        let sources = empty_sources();

        let results = sources.search("example.com/docs", SearchMode::All);
        assert!(matches!(
            results.first().map(|item| &item.action),
            Some(Action::OpenUrl { url }) if url == "https://example.com/docs"
        ));
    }

    #[test]
    fn no_results_item_uses_mode_specific_guidance() {
        let item = no_results_item(&QueryInput {
            mode: SearchMode::Files,
            source_filter: SourceFilter::All,
            package_manager: None,
            text: "report".to_string(),
        });

        assert_eq!(item.title, "No matches for \"report\"");
        assert!(item.subtitle.contains("file indexer"));
        assert!(matches!(item.action, Action::None));
    }

    #[test]
    fn file_mode_requires_a_minimum_query_length_before_shelling_out() {
        let sources = Sources {
            file_search_backend: Some(FileSearchBackend::LocalSearch),
            ..empty_sources()
        };

        let results = sources.search("/ a", SearchMode::All);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source, "Files");
        assert_eq!(results[0].title, "Keep typing to search files");
        assert!(matches!(results[0].action, Action::None));
    }

    #[test]
    fn all_mode_password_matches_open_password_actions() {
        let sources = Sources {
            pass_entries: pass_entries(vec![super::PassEntry {
                name: "github/work".to_string(),
                search_blob: "github/work".to_string(),
            }]),
            pass_available: true,
            ..empty_sources()
        };

        let results = sources.search("pass: github", SearchMode::All);
        assert!(matches!(
            results.first().map(|item| &item.action),
            Some(Action::PasswordActions { entry }) if entry == "github/work"
        ));
        assert_eq!(
            results[0].prediction_key.as_deref(),
            Some("pass:github/work")
        );
    }

    #[test]
    fn pass_mode_surfaces_one_row_per_matching_entry() {
        let sources = Sources {
            pass_entries: pass_entries(vec![super::PassEntry {
                name: "github/work".to_string(),
                search_blob: "github/work".to_string(),
            }]),
            pass_available: true,
            ..empty_sources()
        };

        let results = sources.search("pass: github", SearchMode::All);
        let password_rows = results
            .iter()
            .filter_map(|item| match &item.action {
                Action::PasswordActions { entry } => Some(entry.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(password_rows, vec!["github/work"]);
        assert!(
            !results
                .iter()
                .any(|item| matches!(item.action, Action::Password { .. }))
        );
    }

    #[test]
    fn pass_queries_offer_to_add_the_typed_entry() {
        let sources = Sources {
            pass_available: true,
            ..empty_sources()
        };

        let results = sources.search("pass: github/work", SearchMode::All);

        assert!(matches!(
            results.first().map(|item| &item.action),
            Some(Action::AddPassword { entry, url }) if entry == "github/work" && url.is_none()
        ));
        assert_eq!(results[0].title, "Add password: github/work");
    }

    #[test]
    fn empty_pass_mode_offers_to_add_clipboard_url_host() {
        let sources = Sources {
            pass_available: true,
            ..empty_sources()
        };

        let results = sources.search_with_clipboard_url(
            "",
            SearchMode::Pass,
            Some("https://login.example.com/path"),
        );

        assert!(matches!(
            results.first().map(|item| &item.action),
            Some(Action::AddPassword { entry, url }) if entry == "login.example.com"
                && url.as_deref() == Some("https://login.example.com/path")
        ));
        assert_eq!(results[0].title, "Add password: login.example.com");
    }

    #[test]
    fn empty_all_mode_offers_to_add_clipboard_url_host() {
        let sources = Sources {
            pass_available: true,
            ..empty_sources()
        };

        let results = sources.search_with_clipboard_url(
            "",
            SearchMode::All,
            Some("https://www.torrentleech.org/torrents/top/index/added/-1%20day/orderby/completed/order/desc"),
        );

        assert!(matches!(
            results.first().map(|item| &item.action),
            Some(Action::AddPassword { entry, url }) if entry == "www.torrentleech.org"
                && url.as_deref() == Some("https://www.torrentleech.org/torrents/top/index/added/-1%20day/orderby/completed/order/desc")
        ));
        assert_eq!(results[0].title, "Add password: www.torrentleech.org");
    }

    #[test]
    fn clipboard_url_add_row_is_suppressed_for_existing_entries() {
        let sources = Sources {
            pass_entries: pass_entries(vec![super::PassEntry {
                name: "login.example.com".to_string(),
                search_blob: "login.example.com".to_string(),
            }]),
            pass_available: true,
            ..empty_sources()
        };

        let results = sources.search_with_clipboard_url(
            "",
            SearchMode::Pass,
            Some("https://login.example.com/path"),
        );

        assert!(
            !results
                .iter()
                .any(|item| matches!(item.action, Action::AddPassword { .. }))
        );
    }

    #[test]
    fn pass_queries_do_not_offer_to_add_existing_entries() {
        let sources = Sources {
            pass_entries: pass_entries(vec![super::PassEntry {
                name: "github/work".to_string(),
                search_blob: "github/work".to_string(),
            }]),
            pass_available: true,
            ..empty_sources()
        };

        let results = sources.search("pass: github/work", SearchMode::All);

        assert!(
            !results
                .iter()
                .any(|item| matches!(item.action, Action::AddPassword { .. }))
        );
    }

    #[test]
    fn learned_matches_are_boosted_in_search_results() {
        let sources = Sources {
            apps: vec![
                AppEntry {
                    desktop_id: "alpha.desktop".to_string(),
                    name: "Alpha Browser".to_string(),
                    description: "Web browser".to_string(),
                    executable: "alpha".to_string(),
                    icon_name: "alpha".to_string(),
                    search_blob: "alpha browser web browser".to_string(),
                },
                AppEntry {
                    desktop_id: "beta.desktop".to_string(),
                    name: "Beta Browser".to_string(),
                    description: "Web browser".to_string(),
                    executable: "beta".to_string(),
                    icon_name: "beta".to_string(),
                    search_blob: "beta browser web browser".to_string(),
                },
            ],
            predictions: prediction_store_with(
                StoredPrediction {
                    key: "app:beta.desktop".to_string(),
                    title: "Beta Browser".to_string(),
                    subtitle: "Web browser".to_string(),
                    source: "Applications".to_string(),
                    icon_name: "beta".to_string(),
                    action: Action::LaunchApp {
                        desktop_id: "beta.desktop".to_string(),
                    },
                },
                super::current_unix_time().saturating_sub(60),
            ),
            ..empty_sources()
        };

        let results = sources.search("browser", SearchMode::Apps);

        assert_eq!(results[0].title, "Beta Browser");
    }

    #[test]
    fn empty_all_mode_starts_with_learned_predictions() {
        let sources = Sources {
            apps: vec![AppEntry {
                desktop_id: "alpha.desktop".to_string(),
                name: "Alpha".to_string(),
                description: "First app".to_string(),
                executable: "alpha".to_string(),
                icon_name: "alpha".to_string(),
                search_blob: "alpha first app".to_string(),
            }],
            predictions: prediction_store_with(
                StoredPrediction {
                    key: "cmd:git status".to_string(),
                    title: "Run \"git status\"".to_string(),
                    subtitle: "Execute in the background with sh -lc".to_string(),
                    source: "Commands".to_string(),
                    icon_name: "utilities-terminal-symbolic".to_string(),
                    action: Action::RunCommand {
                        command: "git status".to_string(),
                    },
                },
                super::current_unix_time().saturating_sub(60),
            ),
            ..empty_sources()
        };

        let results = sources.search("", SearchMode::All);

        assert_eq!(results[0].source, "Commands");
        assert_eq!(results[0].title, "Run \"git status\"");
    }

    #[test]
    fn all_mode_surfaces_command_runner_when_input_starts_with_known_command() {
        let sources = Sources {
            commands: vec!["systemctl".to_string()],
            ..empty_sources()
        };

        let results = sources.search("systemctl suspend", SearchMode::All);

        assert_eq!(results[0].title, "Run \"systemctl suspend\"");
        assert!(matches!(
            &results[0].action,
            Action::RunCommand { command } if command == "systemctl suspend"
        ));
    }

    #[test]
    fn all_mode_surfaces_curated_power_actions() {
        let sources = empty_sources();

        let results = sources.search("reboot", SearchMode::All);

        assert_eq!(results[0].source, "Power");
        assert_eq!(results[0].title, "Reboot");
        assert!(matches!(
            &results[0].action,
            Action::Power {
                operation: PowerOperation::Reboot,
                confirmed: false,
            }
        ));
    }

    #[test]
    fn power_actions_match_common_synonyms() {
        let sources = empty_sources();

        let results = sources.search("sleep", SearchMode::All);

        assert_eq!(results[0].source, "Power");
        assert_eq!(results[0].title, "Suspend");
        assert!(matches!(
            &results[0].action,
            Action::Power {
                operation: PowerOperation::Suspend,
                confirmed: false,
            }
        ));
    }

    #[test]
    fn parses_chromium_bookmark_json_urls() {
        let bookmarks = parse_chromium_bookmarks_json(
            r#"{
              "roots": {
                "bookmark_bar": {
                  "type": "folder",
                  "children": [
                    {"type": "url", "name": "Rust", "url": "https://www.rust-lang.org/"},
                    {"type": "folder", "children": [
                      {"type": "url", "name": "", "url": "https://example.com/docs"}
                    ]}
                  ]
                }
              }
            }"#,
        );

        assert_eq!(bookmarks.len(), 2);
        assert_eq!(bookmarks[0].title, "Rust");
        assert_eq!(bookmarks[0].url, "https://www.rust-lang.org/");
        assert_eq!(bookmarks[1].title, "https://example.com/docs");
    }

    #[test]
    fn parses_firefox_sqlite_rows_as_bookmarks() {
        let bookmarks =
            parse_firefox_bookmark_rows("Rust Docs\thttps://doc.rust-lang.org/\n\tplace:sort=8\n");

        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].title, "Rust Docs");
        assert_eq!(bookmarks[0].url, "https://doc.rust-lang.org/");
    }

    #[test]
    fn parses_recent_files_xbel_and_skips_non_file_uris() {
        let recents = parse_recent_files_xbel(
            r#"<?xml version="1.0"?>
            <xbel>
              <bookmark href="file:///home/robin/Documents/Project%20Plan.pdf" modified="2026-05-05T10:11:12Z">
                <title>Project Plan</title>
              </bookmark>
              <bookmark href="https://example.com" modified="2026-05-06T10:11:12Z">
                <title>Remote</title>
              </bookmark>
              <bookmark href="file:///home/robin/Downloads/raw.txt" modified="2026-05-04T10:11:12Z"/>
            </xbel>"#,
        );

        assert_eq!(recents.len(), 2);
        assert_eq!(recents[0].title, "Project Plan");
        assert_eq!(recents[0].path, "/home/robin/Documents/Project Plan.pdf");
        assert_eq!(recents[1].title, "raw.txt");
    }

    #[test]
    fn all_mode_searches_bookmarks_and_recent_files() {
        let sources = Sources {
            bookmarks: vec![BookmarkEntry {
                title: "Rust Documentation".to_string(),
                url: "https://doc.rust-lang.org/".to_string(),
                search_blob: "rust documentation https://doc.rust-lang.org/".to_string(),
            }],
            recent_files: vec![RecentFileEntry {
                title: "Project Plan".to_string(),
                path: "/home/robin/Documents/Project Plan.pdf".to_string(),
                modified: 20260505101112,
                search_blob: "project plan /home/robin/documents/project plan.pdf".to_string(),
            }],
            ..empty_sources()
        };

        let bookmark_results = sources.search("rust doc", SearchMode::All);
        assert!(matches!(
            bookmark_results.first().map(|item| &item.action),
            Some(Action::OpenUrl { url }) if url == "https://doc.rust-lang.org/"
        ));
        assert_eq!(
            bookmark_results[0].prediction_key.as_deref(),
            Some("bookmark:https://doc.rust-lang.org/")
        );

        let recent_results = sources.search("project plan", SearchMode::All);
        assert!(matches!(
            recent_results.first().map(|item| &item.action),
            Some(Action::OpenFile { path }) if path == "/home/robin/Documents/Project Plan.pdf"
        ));
        assert_eq!(
            recent_results[0].prediction_key.as_deref(),
            Some("recent:/home/robin/Documents/Project Plan.pdf")
        );
    }

    #[test]
    fn explicit_local_prefixes_search_only_the_selected_source() {
        let sources = Sources {
            bookmarks: vec![BookmarkEntry {
                title: "Project Board".to_string(),
                url: "https://example.com/project".to_string(),
                search_blob: "project board https://example.com/project".to_string(),
            }],
            recent_files: vec![RecentFileEntry {
                title: "Project Notes".to_string(),
                path: "/home/robin/project.txt".to_string(),
                modified: 20260505101112,
                search_blob: "project notes /home/robin/project.txt".to_string(),
            }],
            ..empty_sources()
        };

        let bookmark_results = sources.search("bookmark: project", SearchMode::All);
        assert_eq!(bookmark_results.len(), 1);
        assert_eq!(bookmark_results[0].source, "Bookmarks");

        let recent_results = sources.search("recent: project", SearchMode::All);
        assert_eq!(recent_results.len(), 1);
        assert_eq!(recent_results[0].source, "Recent Files");
    }

    #[test]
    fn empty_explicit_local_prefixes_show_instruction_rows() {
        let sources = empty_sources();

        let bookmark_results = sources.search("bookmark:", SearchMode::All);
        assert_eq!(bookmark_results[0].title, "Bookmark search");

        let recent_results = sources.search("recent:", SearchMode::All);
        assert_eq!(recent_results[0].title, "Recent file search");
    }

    #[test]
    fn deferred_search_plans_for_files_and_mail_in_all_mode() {
        let mut sources = empty_sources();
        sources.file_search_backend = Some(FileSearchBackend::LocalSearch);
        sources.local_email_entries = vec![EmailEntry {
            subject: "Reddit digest".to_string(),
            sender: "Reddit <noreply@reddit.com>".to_string(),
            sender_email: Some("noreply@reddit.com".to_string()),
            folder: "INBOX".to_string(),
            date_label: "today".to_string(),
            open_url: "mailbox-message://mailbox://Local Folders/Inbox#1".to_string(),
            reply_url: None,
            compose_url: None,
            snippet: String::new(),
            unread: false,
            has_attachment: false,
            search_blob: "reddit digest inbox".to_string(),
        }];

        let snapshot = sources.search_snapshot("reddit", SearchMode::All, None);

        assert!(snapshot.deferred.files);
        assert!(snapshot.deferred.email);
        assert!(
            snapshot
                .immediate_results
                .iter()
                .all(|item| item.source != "Files" && item.source != "Email")
        );
    }

    #[test]
    fn all_mode_defers_desktop_status_sources_before_immediate_results() {
        let sources = empty_sources();

        let snapshot = sources.search_snapshot("reddit", SearchMode::All, None);

        assert!(
            !snapshot.deferred.is_empty(),
            "all-mode desktop status work should not block immediate app results"
        );
        assert!(
            snapshot
                .immediate_results
                .iter()
                .all(|item| item.source != "Windows" && item.source != "Controls")
        );
    }

    #[test]
    fn all_mode_keeps_app_results_immediate_while_deferring_recent_files() {
        let mut sources = empty_sources();
        sources.apps = vec![AppEntry {
            desktop_id: "firefox.desktop".to_string(),
            name: "Firefox".to_string(),
            description: "Web Browser".to_string(),
            executable: "firefox".to_string(),
            icon_name: "firefox".to_string(),
            search_blob: "firefox web browser".to_string(),
        }];
        sources.recent_files = vec![RecentFileEntry {
            title: "Firefox Notes".to_string(),
            path: "/tmp/firefox-notes.txt".to_string(),
            modified: 0,
            search_blob: "firefox notes".to_string(),
        }];

        let snapshot = sources.search_snapshot("firefox", SearchMode::All, None);

        assert!(
            snapshot
                .immediate_results
                .iter()
                .any(|item| item.source == "Applications" && item.title == "Firefox"),
            "application matches should be available in the first render"
        );
        assert!(
            snapshot
                .immediate_results
                .iter()
                .all(|item| item.source != "Recent Files"),
            "recent-file metadata should not block the first render"
        );
    }

    #[test]
    fn specific_file_and_email_modes_show_status_rows_when_deferred_search_is_not_used() {
        let sources = empty_sources();

        let file_snapshot = sources.search_snapshot("a", SearchMode::Files, None);
        assert!(!file_snapshot.deferred.files);
        assert_eq!(
            file_snapshot.immediate_results[0].title,
            "Keep typing to search files"
        );

        let email_snapshot = sources.search_snapshot("reddit", SearchMode::Email, None);
        assert!(!email_snapshot.deferred.email);
        assert_eq!(
            email_snapshot.immediate_results[0].title,
            "No email source found"
        );
    }

    #[test]
    fn pass_entry_names_are_derived_from_store_paths() {
        let root = Path::new("/tmp/store");
        let path = Path::new("/tmp/store/personal/github.gpg");
        assert_eq!(
            pass_entry_name(root, path).as_deref(),
            Some("personal/github")
        );
    }
}

fn load_control_snapshot() -> ControlSnapshot {
    let has_playerctl = command_exists("playerctl");
    let has_wpctl = command_exists("wpctl");
    let has_bluetoothctl = command_exists("bluetoothctl");
    let has_nmcli = command_exists("nmcli");
    let has_powerprofilesctl = command_exists("powerprofilesctl");
    let has_dunstctl = command_exists("dunstctl");
    let has_grim = command_exists("grim");
    let has_slurp = command_exists("slurp");
    let has_hyprpicker = command_exists("hyprpicker");

    ControlSnapshot {
        media: if has_playerctl {
            command_output_string(
                "playerctl",
                &[
                    "metadata",
                    "--format",
                    "{{playerName}}|{{status}}|{{artist}}|{{title}}",
                ],
            )
            .and_then(|output| parse_playerctl_metadata(&output))
        } else {
            None
        },
        volume: if has_wpctl {
            command_output_string("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])
                .and_then(|output| parse_wpctl_volume(&output))
        } else {
            None
        },
        bluetooth: if has_bluetoothctl {
            let show = command_output_string("bluetoothctl", &["show"]).unwrap_or_default();
            let connected = command_output_string("bluetoothctl", &["devices", "Connected"])
                .unwrap_or_default();
            parse_bluetooth_status(&show, &connected)
        } else {
            None
        },
        network: if has_nmcli {
            command_output_string(
                "nmcli",
                &["-t", "-f", "TYPE,STATE,CONNECTION", "device", "status"],
            )
            .and_then(|output| parse_nmcli_device_status(&output))
        } else {
            None
        },
        power_profile: if has_powerprofilesctl {
            command_output_string("powerprofilesctl", &["get"])
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        } else {
            None
        },
        screen_brightness: if command_exists("brightnessctl") {
            command_output_string("brightnessctl", &["--class=backlight", "-m", "info"])
                .and_then(|output| parse_backlight_brightness(&output))
        } else {
            None
        },
        notifications: if has_dunstctl {
            command_output_string("dunstctl", &["history"])
                .map(|output| parse_dunst_history(&output))
                .unwrap_or_default()
        } else {
            Vec::new()
        },
        has_playerctl,
        has_wpctl,
        has_bluetoothctl,
        has_nmcli,
        has_powerprofilesctl,
        has_dunstctl,
        has_grim,
        has_slurp,
        has_hyprpicker,
    }
}

fn command_output_string(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn parse_playerctl_metadata(output: &str) -> Option<MediaStatus> {
    let mut parts = output.trim().splitn(4, '|');
    let player = parts.next()?.trim();
    let status = parts.next()?.trim();
    let artist = parts.next()?.trim();
    let title = parts.next()?.trim();
    if title.is_empty() {
        return None;
    }
    Some(MediaStatus {
        player: player.to_string(),
        status: status.to_string(),
        artist: artist.to_string(),
        title: title.to_string(),
    })
}

fn parse_wpctl_volume(output: &str) -> Option<VolumeStatus> {
    let trimmed = output.trim();
    let value = trimmed
        .split_whitespace()
        .find_map(|part| part.parse::<f64>().ok())?;
    let percent = (value * 100.0).round().clamp(0.0, 150.0) as u8;
    Some(VolumeStatus {
        percent,
        muted: trimmed.to_ascii_lowercase().contains("muted"),
    })
}

fn parse_bluetooth_status(show_output: &str, connected_output: &str) -> Option<BluetoothStatus> {
    let powered = show_output
        .lines()
        .find_map(|line| line.trim().strip_prefix("Powered:"))
        .map(|value| value.trim() == "yes")?;
    let connected_count = connected_output
        .lines()
        .filter(|line| line.trim_start().starts_with("Device "))
        .count();
    Some(BluetoothStatus {
        powered,
        connected_count,
    })
}

fn parse_nmcli_device_status(output: &str) -> Option<NetworkStatus> {
    output.lines().find_map(|line| {
        let mut parts = line.splitn(3, ':');
        let kind = parts.next()?.trim();
        let state = parts.next()?.trim();
        let connection = parts.next()?.trim();
        (state == "connected" && matches!(kind, "wifi" | "ethernet") && !connection.is_empty())
            .then(|| NetworkStatus {
                kind: kind.to_string(),
                connection: connection.to_string(),
            })
    })
}

fn parse_backlight_brightness(output: &str) -> Option<u8> {
    output.lines().find_map(|line| {
        let parts = line.split(',').collect::<Vec<_>>();
        if parts.len() < 5 || parts.get(1).copied() != Some("backlight") {
            return None;
        }
        parts
            .iter()
            .find_map(|part| part.trim().strip_suffix('%'))
            .and_then(|part| part.parse::<u8>().ok())
    })
}

fn parse_dunst_history(output: &str) -> Vec<NotificationEntry> {
    let mut entries = Vec::new();
    let mut pending_body = String::new();
    let mut current_summary: Option<String> = None;
    let mut current_body = String::new();

    for (key, value) in gvariant_string_pairs(output) {
        match key.as_str() {
            "body" if current_summary.is_none() => pending_body = value,
            "body" => current_body = value,
            "summary" => {
                current_summary = Some(value);
                current_body = std::mem::take(&mut pending_body);
            }
            "appname" => {
                if let Some(summary) = current_summary.take() {
                    let body = std::mem::take(&mut current_body);
                    let app_name = value;
                    let search_blob = format!("{app_name} {summary} {body}").to_ascii_lowercase();
                    entries.push(NotificationEntry {
                        app_name,
                        summary,
                        body,
                        search_blob,
                    });
                }
            }
            _ => {}
        }
    }

    entries
}

fn gvariant_string_pairs(output: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut index = 0;
    while let Some(key_start_rel) = output[index..].find('"') {
        let key_start = index + key_start_rel + 1;
        let Some(key_end_rel) = output[key_start..].find('"') else {
            break;
        };
        let key_end = key_start + key_end_rel;
        let key = &output[key_start..key_end];
        if !matches!(key, "summary" | "body" | "appname") {
            index = key_end + 1;
            continue;
        }
        let Some(data_pos_rel) = output[key_end..].find("\"data\"") else {
            break;
        };
        let data_pos = key_end + data_pos_rel;
        let Some(value_start_rel) = output[data_pos + 6..].find('"') else {
            break;
        };
        let value_start = data_pos + 6 + value_start_rel + 1;
        let Some((value, value_end)) = parse_quoted_string(output, value_start) else {
            break;
        };
        pairs.push((key.to_string(), value));
        index = value_end;
    }
    pairs
}

fn parse_quoted_string(input: &str, mut index: usize) -> Option<(String, usize)> {
    let mut value = String::new();
    while index < input.len() {
        let ch = input[index..].chars().next()?;
        index += ch.len_utf8();
        match ch {
            '"' => return Some((value, index)),
            '\\' => {
                let escaped = input[index..].chars().next()?;
                index += escaped.len_utf8();
                match escaped {
                    'n' => value.push('\n'),
                    't' => value.push('\t'),
                    '"' => value.push('"'),
                    '\\' => value.push('\\'),
                    other => value.push(other),
                }
            }
            other => value.push(other),
        }
    }
    None
}

fn command_exists(binary: &str) -> bool {
    env::var_os("PATH")
        .unwrap_or_default()
        .to_string_lossy()
        .split(':')
        .map(Path::new)
        .any(|dir| dir.join(binary).exists())
}

fn current_unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn looks_like_math(query: &str) -> bool {
    query
        .chars()
        .any(|ch| ch.is_ascii_digit() || "+-*/^=()".contains(ch))
        || query.contains(" to ")
}

pub(crate) fn no_results_item(query: &QueryInput) -> ResultItem {
    let subtitle = match query.source_filter {
        SourceFilter::Bookmarks => "Try a different bookmark title or URL fragment.".to_string(),
        SourceFilter::Recents => "Try a different recently used file name.".to_string(),
        SourceFilter::All => match query.mode {
            SearchMode::All => "Try a broader term or switch to a dedicated mode.".to_string(),
            SearchMode::Apps => "Try a different app name or executable.".to_string(),
            SearchMode::Windows => "Try a window title, app id, or workspace name.".to_string(),
            SearchMode::Files => {
                "Try a different file name or ensure the file indexer has indexed it.".to_string()
            }
            SearchMode::Ssh => {
                "Check ~/.ssh/config and known_hosts for the expected host.".to_string()
            }
            SearchMode::Pass => "Try a different password-store entry name.".to_string(),
            SearchMode::Email => {
                "Try a different subject, sender, folder, or message fragment.".to_string()
            }
            SearchMode::Commands => {
                "Try a different executable name or a full shell command.".to_string()
            }
            SearchMode::Web => "Press Enter to open a browser search result instead.".to_string(),
            SearchMode::Calc => "Try a valid libqalculate expression such as 42/7.".to_string(),
            SearchMode::Controls => {
                "Try media, volume, Bluetooth, network, screenshot, color, or notifications."
                    .to_string()
            }
            SearchMode::Packages => {
                "Try a different package name or install pacman/paru first.".to_string()
            }
        },
    };

    ResultItem {
        prediction_key: None,
        title: format!("No matches for \"{}\"", query.text),
        subtitle,
        source: "Status",
        icon_name: "system-search-symbolic".to_string(),
        score: 0,
        action: Action::None,
        ..Default::default()
    }
}

fn sort_results(results: &mut Vec<ResultItem>, limit: usize) {
    results.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.title.cmp(&right.title))
    });
    results.truncate(limit);
}

pub(crate) fn sort_and_limit_results(mut results: Vec<ResultItem>) -> Vec<ResultItem> {
    results.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.title.cmp(&right.title))
    });
    results.truncate(MAX_RESULTS);
    results
}

/// Combine already-displayed `immediate` results with `deferred` results that
/// finished loading later. The immediate block keeps its established order and
/// the deferred results are sorted among themselves and appended below, so rows
/// the user is looking at never reflow when slower providers return. The total
/// is capped at the same limit as a normal search.
pub(crate) fn append_deferred_results(
    immediate: Vec<ResultItem>,
    deferred: Vec<ResultItem>,
) -> Vec<ResultItem> {
    let mut results = sort_and_limit_results(immediate);
    let mut deferred = sort_and_limit_results(deferred);
    let package_rows = deferred
        .iter()
        .filter(|item| item.source == "Packages")
        .cloned()
        .collect::<Vec<_>>();
    let remaining = MAX_RESULTS.saturating_sub(results.len());
    if remaining > 0 {
        deferred.truncate(remaining);
        results.extend(deferred);
    }
    for package in package_rows {
        if !results
            .iter()
            .any(|item| same_result_identity(item, &package))
        {
            results.push(package);
        }
    }
    results
}

fn same_result_identity(left: &ResultItem, right: &ResultItem) -> bool {
    match (&left.prediction_key, &right.prediction_key) {
        (Some(left), Some(right)) => left == right,
        _ => left.source == right.source && left.title == right.title,
    }
}

pub(crate) fn finalize_search_results(
    results: Vec<ResultItem>,
    query: &QueryInput,
    include_no_results: bool,
) -> Vec<ResultItem> {
    let mut results = sort_and_limit_results(results);
    if include_no_results && results.is_empty() {
        results.push(no_results_item(query));
    }
    if results.is_empty() {
        return results;
    }
    results
}

fn instruction_result(
    title: &str,
    subtitle: &str,
    source: &'static str,
    icon_name: &str,
    score: i32,
) -> ResultItem {
    ResultItem {
        prediction_key: None,
        title: title.to_string(),
        subtitle: subtitle.to_string(),
        source,
        icon_name: icon_name.to_string(),
        score,
        action: Action::None,
        ..Default::default()
    }
}

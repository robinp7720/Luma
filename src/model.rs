use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum, Deserialize, Serialize)]
pub enum SearchMode {
    All,
    Apps,
    Windows,
    Files,
    Ssh,
    Pass,
    Email,
    Commands,
    Web,
    Calc,
    Controls,
    Packages,
}

impl SearchMode {
    pub fn includes(self, other: SearchMode) -> bool {
        matches!(self, SearchMode::All) || self == other
    }
}

#[derive(Clone, Debug, Default)]
pub struct ResultItem {
    pub title: String,
    pub subtitle: String,
    pub source: &'static str,
    pub icon_name: String,
    pub score: i32,
    pub action: Action,
    pub prediction_key: Option<String>,
    /// Short, right-aligned text shown on the title row (e.g. a relative date).
    pub accessory: Option<String>,
    /// Small status markers rendered next to the accessory on the title row.
    pub badges: Vec<EntryBadge>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryBadge {
    Unread,
    Attachment,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum WindowFocusTarget {
    Hyprland { address: String, xwayland: bool },
    Niri { id: u64 },
    X11 { window_id: String },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PowerOperation {
    Lock,
    Suspend,
    Logout,
    Reboot,
    Shutdown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DesktopControlOperation {
    MediaPlayPause,
    MediaNext,
    MediaPrevious,
    VolumeUp,
    VolumeDown,
    VolumeMute,
    BrightnessUp,
    BrightnessDown,
    AudioSettings,
    BluetoothTogglePower,
    NetworkSettings,
    PowerProfileCycle,
    PowerProfileSet { profile: String },
    ScreenshotArea,
    ScreenshotScreen,
    ColorPicker,
    NotificationHistoryPop,
    NotificationCloseAll,
    NotificationPauseToggle,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PackageManager {
    Pacman,
    #[serde(alias = "Yay")]
    Paru,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub enum Action {
    LaunchApp {
        desktop_id: String,
    },
    FocusWindow {
        target: WindowFocusTarget,
    },
    OpenFile {
        path: String,
    },
    Ssh {
        host: String,
    },
    CopyPass {
        entry: String,
    },
    Password {
        entry: String,
        operation: PasswordOperation,
    },
    PasswordActions {
        entry: String,
    },
    AddPassword {
        entry: String,
        url: Option<String>,
    },
    RunCommand {
        command: String,
    },
    Power {
        operation: PowerOperation,
        confirmed: bool,
    },
    DesktopControl {
        operation: DesktopControlOperation,
    },
    InstallPackage {
        package: String,
        manager: PackageManager,
    },
    OpenUrl {
        url: String,
    },
    WebSearch {
        query: String,
    },
    OpenConfigPanel,
    CopyText {
        text: String,
    },
    #[default]
    None,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PasswordUrlDraft {
    pub entry: String,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PasswordOperation {
    AutotypeLogin,
    CopyPassword,
    CopyUsername,
    TypePassword,
    TypeUsername,
    Inspect,
    OpenUrl,
    CopyUrl,
    CopyOtp,
    TypeOtp,
    CustomAutotype,
}

#[derive(Clone, Debug)]
pub struct QueryInput {
    pub mode: SearchMode,
    pub source_filter: SourceFilter,
    pub package_manager: Option<PackageManager>,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceFilter {
    All,
    Bookmarks,
    Recents,
}

impl QueryInput {
    pub fn parse(raw: &str, cli_mode: SearchMode) -> Self {
        let trimmed = raw.trim();
        let (mode, source_filter, package_manager, text) =
            parse_prefixed_query(trimmed).unwrap_or((cli_mode, SourceFilter::All, None, trimmed));
        Self {
            mode,
            source_filter,
            package_manager,
            text: text.to_string(),
        }
    }
}

fn parse_prefixed_query(
    raw: &str,
) -> Option<(SearchMode, SourceFilter, Option<PackageManager>, &str)> {
    if raw.is_empty() {
        return None;
    }

    let mut chars = raw.chars();
    let first = chars.next()?;
    let rest = &raw[first.len_utf8()..];

    match first {
        '>' => {
            return Some((
                SearchMode::Commands,
                SourceFilter::All,
                None,
                rest.trim_start(),
            ));
        }
        '~' => {
            return Some((
                SearchMode::Windows,
                SourceFilter::All,
                None,
                rest.trim_start(),
            ));
        }
        '@' => return Some((SearchMode::Ssh, SourceFilter::All, None, rest.trim_start())),
        '!' => return Some((SearchMode::Pass, SourceFilter::All, None, rest.trim_start())),
        '?' => return Some((SearchMode::Web, SourceFilter::All, None, rest.trim_start())),
        '=' => return Some((SearchMode::Calc, SourceFilter::All, None, rest.trim_start())),
        '/' => {
            let whitespace_prefixed = rest.chars().next().is_none_or(char::is_whitespace);
            if whitespace_prefixed {
                return Some((
                    SearchMode::Files,
                    SourceFilter::All,
                    None,
                    rest.trim_start(),
                ));
            }
        }
        _ => {}
    }

    let lowered = raw.to_ascii_lowercase();
    const PREFIXES: [(&str, SearchMode, SourceFilter, Option<PackageManager>); 28] = [
        ("apps:", SearchMode::Apps, SourceFilter::All, None),
        ("app:", SearchMode::Apps, SourceFilter::All, None),
        ("windows:", SearchMode::Windows, SourceFilter::All, None),
        ("window:", SearchMode::Windows, SourceFilter::All, None),
        ("win:", SearchMode::Windows, SourceFilter::All, None),
        ("files:", SearchMode::Files, SourceFilter::All, None),
        ("file:", SearchMode::Files, SourceFilter::All, None),
        ("ssh:", SearchMode::Ssh, SourceFilter::All, None),
        ("pass:", SearchMode::Pass, SourceFilter::All, None),
        ("password:", SearchMode::Pass, SourceFilter::All, None),
        ("email:", SearchMode::Email, SourceFilter::All, None),
        ("mail:", SearchMode::Email, SourceFilter::All, None),
        ("cmd:", SearchMode::Commands, SourceFilter::All, None),
        ("command:", SearchMode::Commands, SourceFilter::All, None),
        ("web:", SearchMode::Web, SourceFilter::All, None),
        ("calc:", SearchMode::Calc, SourceFilter::All, None),
        ("controls:", SearchMode::Controls, SourceFilter::All, None),
        ("control:", SearchMode::Controls, SourceFilter::All, None),
        ("ctl:", SearchMode::Controls, SourceFilter::All, None),
        ("packages:", SearchMode::Packages, SourceFilter::All, None),
        ("package:", SearchMode::Packages, SourceFilter::All, None),
        ("pkg:", SearchMode::Packages, SourceFilter::All, None),
        (
            "pacman:",
            SearchMode::Packages,
            SourceFilter::All,
            Some(PackageManager::Pacman),
        ),
        (
            "paru:",
            SearchMode::Packages,
            SourceFilter::All,
            Some(PackageManager::Paru),
        ),
        ("bookmarks:", SearchMode::All, SourceFilter::Bookmarks, None),
        ("bookmark:", SearchMode::All, SourceFilter::Bookmarks, None),
        ("recents:", SearchMode::All, SourceFilter::Recents, None),
        ("recent:", SearchMode::All, SourceFilter::Recents, None),
    ];

    PREFIXES
        .iter()
        .find_map(|(prefix, mode, source_filter, package_manager)| {
            lowered.strip_prefix(prefix).map(|_| {
                (
                    *mode,
                    *source_filter,
                    *package_manager,
                    raw[prefix.len()..].trim_start(),
                )
            })
        })
}

pub fn score_text(haystack: &str, query: &str) -> Option<i32> {
    let haystack = haystack.to_ascii_lowercase();
    let query = query.to_ascii_lowercase();

    if query.is_empty() {
        return Some(0);
    }

    if haystack == query {
        return Some(1_000);
    }

    if let Some(rest) = haystack.strip_prefix(&query) {
        return Some(850 - rest.len() as i32);
    }

    if let Some(position) = haystack.find(&query) {
        return Some(600 - position as i32);
    }

    if is_subsequence(&haystack, &query) {
        return Some(400 - (haystack.len() as i32 - query.len() as i32));
    }

    None
}

fn is_subsequence(haystack: &str, needle: &str) -> bool {
    let mut chars = needle.chars();
    let mut current = chars.next();

    for ch in haystack.chars() {
        if current == Some(ch) {
            current = chars.next();
            if current.is_none() {
                return true;
            }
        }
    }

    current.is_none()
}

pub fn browser_target(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return None;
    }

    if has_uri_scheme(trimmed) {
        return Some(trimmed.to_string());
    }

    if trimmed.starts_with("www.") && looks_like_web_host(trimmed) {
        return Some(format!("https://{trimmed}"));
    }

    if looks_like_web_host(trimmed) {
        return Some(format!("https://{trimmed}"));
    }

    None
}

pub fn password_url_draft(raw: &str) -> Option<PasswordUrlDraft> {
    let url = browser_target(raw)?;
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim();
    if authority.is_empty() {
        return None;
    }

    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .split(':')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }

    Some(PasswordUrlDraft { entry: host, url })
}

fn has_uri_scheme(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };

    !scheme.is_empty()
        && !rest.is_empty()
        && scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

fn looks_like_web_host(value: &str) -> bool {
    let authority = value
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('.');

    if authority.is_empty() {
        return false;
    }

    let host = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    let host = host
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()))
        .map(|(host, _)| host)
        .unwrap_or(host);

    if host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }

    if !host.contains('.') {
        return false;
    }

    host.split('.').all(valid_domain_label)
}

fn valid_domain_label(label: &str) -> bool {
    !label.is_empty()
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

#[cfg(test)]
mod tests {
    use super::{QueryInput, SearchMode, SourceFilter, browser_target, password_url_draft};

    #[test]
    fn symbol_prefixes_override_the_default_mode() {
        let query = QueryInput::parse("> git status", SearchMode::Apps);
        assert_eq!(query.mode, SearchMode::Commands);
        assert_eq!(query.text, "git status");
    }

    #[test]
    fn textual_prefixes_are_case_insensitive() {
        let query = QueryInput::parse("SSH: prod-box", SearchMode::All);
        assert_eq!(query.mode, SearchMode::Ssh);
        assert_eq!(query.text, "prod-box");
    }

    #[test]
    fn textual_prefixes_can_select_email_mode() {
        let email = QueryInput::parse("EMAIL: invoices", SearchMode::Apps);
        assert_eq!(email.mode, SearchMode::Email);
        assert_eq!(email.text, "invoices");

        let mail = QueryInput::parse("mail: archive", SearchMode::Apps);
        assert_eq!(mail.mode, SearchMode::Email);
        assert_eq!(mail.text, "archive");
    }

    #[test]
    fn textual_prefixes_can_select_controls_mode() {
        let control = QueryInput::parse("control: volume", SearchMode::Apps);
        assert_eq!(control.mode, SearchMode::Controls);
        assert_eq!(control.text, "volume");

        let controls = QueryInput::parse("controls: bluetooth", SearchMode::All);
        assert_eq!(controls.mode, SearchMode::Controls);
        assert_eq!(controls.text, "bluetooth");

        let short = QueryInput::parse("ctl: screenshot", SearchMode::All);
        assert_eq!(short.mode, SearchMode::Controls);
        assert_eq!(short.text, "screenshot");
    }

    #[test]
    fn textual_prefixes_can_select_packages_mode() {
        let package = QueryInput::parse("package: firefox", SearchMode::Apps);
        assert_eq!(package.mode, SearchMode::Packages);
        assert_eq!(package.text, "firefox");

        let short = QueryInput::parse("pkg: obsidian", SearchMode::All);
        assert_eq!(short.mode, SearchMode::Packages);
        assert_eq!(short.text, "obsidian");
    }

    #[test]
    fn package_manager_prefixes_force_the_matching_backend() {
        let paru = QueryInput::parse("paru: veloren", SearchMode::All);
        assert_eq!(paru.mode, SearchMode::Packages);
        assert_eq!(paru.package_manager, Some(super::PackageManager::Paru));
        assert_eq!(paru.text, "veloren");

        let pacman = QueryInput::parse("pacman: firefox", SearchMode::All);
        assert_eq!(pacman.mode, SearchMode::Packages);
        assert_eq!(pacman.package_manager, Some(super::PackageManager::Pacman));
        assert_eq!(pacman.text, "firefox");

        let auto = QueryInput::parse("pkg: firefox", SearchMode::All);
        assert_eq!(auto.package_manager, None);
    }

    #[test]
    fn local_source_prefixes_filter_all_mode_search() {
        let bookmark = QueryInput::parse("bookmark: rust docs", SearchMode::All);
        assert_eq!(bookmark.mode, SearchMode::All);
        assert_eq!(bookmark.source_filter, SourceFilter::Bookmarks);
        assert_eq!(bookmark.text, "rust docs");

        let recent = QueryInput::parse("RECENTS: report", SearchMode::Apps);
        assert_eq!(recent.mode, SearchMode::All);
        assert_eq!(recent.source_filter, SourceFilter::Recents);
        assert_eq!(recent.text, "report");
    }

    #[test]
    fn pass_prefixes_override_the_default_mode() {
        let symbol_prefixed = QueryInput::parse("! github/work", SearchMode::All);
        assert_eq!(symbol_prefixed.mode, SearchMode::Pass);
        assert_eq!(symbol_prefixed.text, "github/work");

        let text_prefixed = QueryInput::parse("PASS: github/work", SearchMode::Apps);
        assert_eq!(text_prefixed.mode, SearchMode::Pass);
        assert_eq!(text_prefixed.text, "github/work");
    }

    #[test]
    fn window_prefixes_override_the_default_mode() {
        let symbol_prefixed = QueryInput::parse("~ terminal", SearchMode::All);
        assert_eq!(symbol_prefixed.mode, SearchMode::Windows);
        assert_eq!(symbol_prefixed.text, "terminal");

        let text_prefixed = QueryInput::parse("windows: firefox", SearchMode::Apps);
        assert_eq!(text_prefixed.mode, SearchMode::Windows);
        assert_eq!(text_prefixed.text, "firefox");
    }

    #[test]
    fn empty_symbol_prefix_keeps_the_target_mode() {
        let query = QueryInput::parse("=", SearchMode::All);
        assert_eq!(query.mode, SearchMode::Calc);
        assert!(query.text.is_empty());
    }

    #[test]
    fn slash_without_whitespace_stays_a_plain_query() {
        let query = QueryInput::parse("/etc", SearchMode::All);
        assert_eq!(query.mode, SearchMode::All);
        assert_eq!(query.text, "/etc");
    }

    #[test]
    fn browser_target_recognizes_full_urls() {
        assert_eq!(
            browser_target("https://example.com/docs?q=1").as_deref(),
            Some("https://example.com/docs?q=1")
        );
    }

    #[test]
    fn browser_target_adds_https_for_bare_domains() {
        assert_eq!(
            browser_target("example.com/notes").as_deref(),
            Some("https://example.com/notes")
        );
    }

    #[test]
    fn browser_target_rejects_plain_search_terms() {
        assert_eq!(browser_target("firefox"), None);
        assert_eq!(browser_target("two words"), None);
    }

    #[test]
    fn password_url_draft_uses_url_host_as_entry_name() {
        let draft =
            password_url_draft("https://login.example.com/path?q=1").expect("password URL draft");

        assert_eq!(draft.entry, "login.example.com");
        assert_eq!(draft.url, "https://login.example.com/path?q=1");
    }

    #[test]
    fn password_url_draft_handles_encoded_paths() {
        let draft = password_url_draft(
            "https://www.torrentleech.org/torrents/top/index/added/-1%20day/orderby/completed/order/desc",
        )
        .expect("password URL draft");

        assert_eq!(draft.entry, "www.torrentleech.org");
        assert_eq!(
            draft.url,
            "https://www.torrentleech.org/torrents/top/index/added/-1%20day/orderby/completed/order/desc"
        );
    }

    #[test]
    fn password_url_draft_accepts_http_urls() {
        let draft = password_url_draft("http://example.com").expect("password URL draft");

        assert_eq!(draft.entry, "example.com");
        assert_eq!(draft.url, "http://example.com");
    }

    #[test]
    fn password_url_draft_rejects_plain_text() {
        assert_eq!(password_url_draft("not a url"), None);
        assert_eq!(password_url_draft("firefox"), None);
    }
}

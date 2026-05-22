use crate::model::SearchMode;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub enum FileSearchBackendChoice {
    Auto,
    LocalSearch,
    Tracker3,
    Disabled,
}

impl Default for FileSearchBackendChoice {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default)]
pub struct SourceToggles {
    pub apps: bool,
    pub windows: bool,
    pub files: bool,
    pub pass: bool,
    pub email: bool,
    pub ssh: bool,
    pub commands: bool,
    pub bookmarks: bool,
    pub recents: bool,
    pub web: bool,
    pub calc: bool,
    pub power: bool,
}

impl Default for SourceToggles {
    fn default() -> Self {
        Self {
            apps: true,
            windows: true,
            files: true,
            pass: true,
            email: true,
            ssh: true,
            commands: true,
            bookmarks: true,
            recents: true,
            web: true,
            calc: true,
            power: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default)]
pub struct UiConfig {
    pub width_px: i32,
    pub height_px: i32,
    pub top_margin_px: i32,
    pub surface_margin_px: i32,
    pub use_layer_shell: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            width_px: 720,
            height_px: 420,
            top_margin_px: 72,
            surface_margin_px: 56,
            use_layer_shell: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default)]
pub struct EmailConfig {
    pub preferred_backend: EmailBackendPreference,
    pub thunderbird_enabled: bool,
    pub evolution_enabled: bool,
    pub evolution_helper_command: Option<String>,
    pub evolution_helper_timeout_ms: u64,
    pub local_mail_enabled: bool,
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            preferred_backend: EmailBackendPreference::Thunderbird,
            thunderbird_enabled: true,
            evolution_enabled: false,
            evolution_helper_command: None,
            evolution_helper_timeout_ms: 2_000,
            local_mail_enabled: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub enum EmailBackendPreference {
    Thunderbird,
    Evolution,
    LocalMail,
    Auto,
}

impl Default for EmailBackendPreference {
    fn default() -> Self {
        Self::Thunderbird
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default)]
pub struct IntegrationConfig {
    pub web_search_url: String,
    pub ssh_terminal: String,
    pub password_clip_timeout_seconds: u64,
    pub password_store_dir: Option<String>,
    pub file_search_backend: FileSearchBackendChoice,
    pub file_search_min_query_chars: usize,
    pub email: EmailConfig,
}

impl Default for IntegrationConfig {
    fn default() -> Self {
        Self {
            web_search_url: "https://duckduckgo.com/?q=".to_string(),
            ssh_terminal: String::new(),
            password_clip_timeout_seconds: 15,
            password_store_dir: None,
            file_search_backend: FileSearchBackendChoice::Auto,
            file_search_min_query_chars: 2,
            email: EmailConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default)]
pub struct LauncherConfig {
    pub default_mode: SearchMode,
    pub sources: SourceToggles,
    pub ui: UiConfig,
    pub integrations: IntegrationConfig,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            default_mode: SearchMode::All,
            sources: SourceToggles::default(),
            ui: UiConfig::default(),
            integrations: IntegrationConfig::default(),
        }
    }
}

#[derive(Debug)]
pub struct ConfigStore {
    path: Option<PathBuf>,
    config: Mutex<LauncherConfig>,
}

impl ConfigStore {
    pub fn load() -> Self {
        config_state_path()
            .and_then(|path| Self::load_from_path(path).ok())
            .unwrap_or_else(|| Self::disabled(LauncherConfig::default()))
    }

    pub fn load_from_path(path: PathBuf) -> io::Result<Self> {
        Self::load_from_path_with_default(path, LauncherConfig::default())
    }

    pub fn load_from_path_with_default(path: PathBuf, default: LauncherConfig) -> io::Result<Self> {
        let config = match fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or(default),
            Err(error) if error.kind() == io::ErrorKind::NotFound => default,
            Err(error) => return Err(error),
        };

        Ok(Self {
            path: Some(path),
            config: Mutex::new(config),
        })
    }

    pub fn disabled(config: LauncherConfig) -> Self {
        Self {
            path: None,
            config: Mutex::new(config),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn current(&self) -> LauncherConfig {
        self.config
            .lock()
            .map(|config| config.clone())
            .unwrap_or_else(|_| LauncherConfig::default())
    }

    pub fn replace(&self, config: LauncherConfig) {
        if let Ok(mut current) = self.config.lock() {
            *current = config;
        }
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(&self.current())?;
        fs::write(path, contents)
    }
}

fn config_state_path() -> Option<PathBuf> {
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .map(|config| config.join("Luma/config.json"))
}

#[cfg(test)]
mod tests {
    use super::{
        ConfigStore, EmailBackendPreference, EmailConfig, FileSearchBackendChoice,
        IntegrationConfig, LauncherConfig, SourceToggles, UiConfig,
    };
    use crate::model::SearchMode;
    use std::fs;

    fn temp_config_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "Luma-config-{name}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ))
    }

    #[test]
    fn default_config_enables_the_core_surface() {
        let config = LauncherConfig::default();

        assert_eq!(config.default_mode, SearchMode::All);
        assert!(config.sources.apps);
        assert!(config.sources.windows);
        assert!(config.sources.files);
        assert!(config.sources.pass);
        assert!(config.sources.email);
        assert_eq!(
            config.integrations.web_search_url,
            "https://duckduckgo.com/?q="
        );
        assert_eq!(config.integrations.password_clip_timeout_seconds, 15);
        assert_eq!(
            config.integrations.file_search_backend,
            FileSearchBackendChoice::Auto
        );
        assert_eq!(
            config.integrations.email.preferred_backend,
            EmailBackendPreference::Thunderbird
        );
        assert!(config.integrations.email.thunderbird_enabled);
        assert!(!config.integrations.email.evolution_enabled);
        assert!(config.integrations.email.evolution_helper_command.is_none());
        assert_eq!(config.integrations.email.evolution_helper_timeout_ms, 2_000);
        assert!(config.integrations.email.local_mail_enabled);
        assert!(config.ui.use_layer_shell);
    }

    #[test]
    fn config_store_round_trips_json_state() {
        let path = temp_config_path("roundtrip");
        let config = LauncherConfig {
            default_mode: SearchMode::Email,
            sources: SourceToggles {
                email: false,
                ..SourceToggles::default()
            },
            ui: UiConfig {
                width_px: 820,
                ..UiConfig::default()
            },
            integrations: IntegrationConfig {
                web_search_url: "https://search.example/?q=".to_string(),
                file_search_backend: FileSearchBackendChoice::Tracker3,
                email: EmailConfig {
                    preferred_backend: EmailBackendPreference::Evolution,
                    evolution_enabled: true,
                    evolution_helper_command: Some("/usr/bin/luma-mail-eds".to_string()),
                    evolution_helper_timeout_ms: 2_500,
                    ..EmailConfig::default()
                },
                ..IntegrationConfig::default()
            },
        };

        let store = ConfigStore::load_from_path_with_default(path.clone(), config.clone())
            .expect("create store");
        store.save().expect("save config");

        let loaded = ConfigStore::load_from_path(path).expect("reload config");
        assert_eq!(loaded.current().default_mode, SearchMode::Email);
        assert!(!loaded.current().sources.email);
        assert_eq!(loaded.current().ui.width_px, 820);
        assert_eq!(
            loaded.current().integrations.web_search_url,
            "https://search.example/?q="
        );
        assert_eq!(
            loaded.current().integrations.file_search_backend,
            FileSearchBackendChoice::Tracker3
        );
        assert_eq!(
            loaded.current().integrations.email.preferred_backend,
            EmailBackendPreference::Evolution
        );
        assert_eq!(
            loaded
                .current()
                .integrations
                .email
                .evolution_helper_command
                .as_deref(),
            Some("/usr/bin/luma-mail-eds")
        );
        assert_eq!(
            loaded
                .current()
                .integrations
                .email
                .evolution_helper_timeout_ms,
            2_500
        );

        let _ = fs::remove_file(loaded.path().expect("config path"));
    }

    #[test]
    fn invalid_json_falls_back_to_the_provided_default() {
        let path = temp_config_path("invalid");
        fs::write(&path, "{ this is not valid json").expect("write invalid config");

        let store =
            ConfigStore::load_from_path_with_default(path.clone(), LauncherConfig::default())
                .expect("load config");
        assert_eq!(store.current().default_mode, SearchMode::All);

        let _ = fs::remove_file(path);
    }
}

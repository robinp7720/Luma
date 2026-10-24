//! Indexed file and package providers. Subprocesses share cancellation and deadlines.
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FileSearchBackend {
    LocalSearch,
    Tracker3,
}

impl FileSearchBackend {
    pub(super) fn detect() -> Option<Self> {
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
                .search_output(),
            Self::Tracker3 => Command::new("tracker3")
                .args(["search", "--limit", &limit.to_string(), query])
                .search_output(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageSearchBackend {
    Pacman,
    Paru,
}

impl PackageSearchBackend {
    pub(super) fn detect() -> Option<Self> {
        if package_command_path("paru").is_some() {
            Some(Self::Paru)
        } else if package_command_path("pacman").is_some() {
            Some(Self::Pacman)
        } else {
            None
        }
    }

    pub(super) fn manager(self) -> PackageManager {
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
                    .search_output()
            }
            Self::Paru => {
                Command::new(package_command_path("paru").unwrap_or_else(|| "paru".into()))
                    .args(["-Ss", query])
                    .search_output()
            }
        }
    }
}

impl Sources {
    pub(super) fn search_files(&self, query: &QueryInput, now: u64) -> Vec<ResultItem> {
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

        let output = match backend.run_search(&query.text, MAX_FILES) {
            Ok(output) => output,
            Err(error) => return provider_failure("Files", &error.to_string()),
        };

        if !output.status.success() {
            return provider_failure("Files", "The file index could not be queried");
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

    pub(super) fn search_packages(&self, query: &QueryInput, now: u64) -> Vec<ResultItem> {
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

        let output = match backend.run_search(&query.text) {
            Ok(output) => output,
            Err(error) => return provider_failure("Packages", &error.to_string()),
        };

        // pacman/paru exit 1 with no output for a query with no matches.
        if !output.status.success() && !output.stderr.is_empty() && output.stdout.is_empty() {
            return provider_failure(
                "Packages",
                "The package provider could not complete the query",
            );
        }

        package_search_results_from_output(&String::from_utf8_lossy(&output.stdout), backend, now)
    }
}

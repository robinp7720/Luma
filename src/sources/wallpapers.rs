//! Executable wallpaper adapters. Desktop policy belongs to the configured helper.
use super::instruction_result;
use crate::config::WallpaperConfig;
use crate::model::{Action, ResultItem, score_text};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashSet, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

const IMAGE_EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];
const THUMBNAIL_WIDTH: i32 = 112;
const THUMBNAIL_HEIGHT: i32 = 68;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WallpaperState {
    Observed,
    Configured,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct WallpaperOutput {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub focused: bool,
    pub wallpaper: Option<String>,
    pub wallpaper_state: WallpaperState,
}

#[derive(Debug, Deserialize)]
struct StatusResponse {
    version: u32,
    outputs: Vec<WallpaperOutput>,
}
#[derive(Debug, Deserialize)]
struct ApplyResponse {
    version: u32,
    ok: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct WallpaperSession {
    pub outputs: Vec<WallpaperOutput>,
    pub selected_output: Option<String>,
    pub update_theme: Option<bool>,
}

pub(crate) fn expand_home(value: &str) -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        if value == "~" {
            return home;
        }
        if let Some(rest) = value.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

pub(crate) fn command_available(command: &[String]) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(program) = command.first().filter(|s| !s.is_empty()) else {
        return false;
    };
    let executable = |path: &Path| {
        fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    let program = expand_home(program);
    if program.components().count() > 1 || program.is_absolute() {
        return executable(&program);
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| executable(&dir.join(&program))))
}

fn request(
    command: &[String],
    payload: serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let Some(program) = command.first().filter(|s| !s.is_empty()) else {
        bail!("No wallpaper integration configured");
    };
    let mut process = Command::new(expand_home(program));
    process.args(&command[1..]);
    let input = serde_json::to_vec(&payload)?;
    let result = crate::search_runtime::output_with_input(&mut process, timeout, Some(&input))
        .context("Wallpaper integration failed or timed out; check its status before retrying")?;
    if !result.status.success() {
        bail!(
            "Wallpaper integration failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }
    serde_json::from_slice(&result.stdout).context("Wallpaper integration returned invalid JSON")
}

fn read_status(config: &WallpaperConfig) -> Result<Vec<WallpaperOutput>> {
    let response: StatusResponse = serde_json::from_value(request(
        &config.backend.command,
        json!({"version": 1, "operation": "status"}),
        Duration::from_secs(3),
    )?)?;
    if response.version != 1 {
        bail!("Unsupported wallpaper adapter version");
    }
    let mut ids = HashSet::new();
    for output in &response.outputs {
        if output.id.is_empty() || !ids.insert(&output.id) {
            bail!("Wallpaper adapter returned empty or duplicate display IDs");
        }
    }
    Ok(response.outputs)
}

fn checked_apply(command: &[String], payload: serde_json::Value) -> Result<()> {
    let response: ApplyResponse =
        serde_json::from_value(request(command, payload, Duration::from_secs(30))?)?;
    if response.version != 1 || !response.ok {
        bail!("Wallpaper integration did not confirm success");
    }
    Ok(())
}

pub(crate) fn apply_wallpaper(
    config: &WallpaperConfig,
    path: &str,
    output: &str,
    update_theme: bool,
) -> Result<()> {
    checked_apply(
        &config.backend.command,
        json!({"version": 1, "operation": "apply", "output": output, "path": path}),
    )
    .context("Could not apply wallpaper")?;
    if update_theme {
        checked_apply(
            &config.theme.command,
            json!({"version": 1, "operation": "apply", "path": path}),
        )
        .context("Wallpaper applied; theme update failed")?;
    }
    Ok(())
}

pub(crate) fn regenerate_theme(config: &WallpaperConfig) -> Result<()> {
    checked_apply(
        &config.theme.command,
        json!({"version": 1, "operation": "regenerate"}),
    )
}

fn list_wallpapers(config: &WallpaperConfig) -> Vec<PathBuf> {
    let mut pending: Vec<_> = config
        .directories
        .iter()
        .map(|path| expand_home(path))
        .collect();
    let mut visited = HashSet::new();
    let mut paths = Vec::new();
    while let Some(dir) = pending.pop() {
        if crate::search_runtime::cancelled() {
            break;
        }
        let Ok(dir) = dir.canonicalize() else {
            continue;
        };
        if !visited.insert(dir.clone()) {
            continue;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if crate::search_runtime::cancelled() {
                break;
            }
            let path = entry.path();
            if config.recursive && path.is_dir() {
                pending.push(path);
            } else if path.is_file()
                && path
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| IMAGE_EXTENSIONS.contains(&s.to_ascii_lowercase().as_str()))
            {
                if let Ok(path) = path.canonicalize() {
                    paths.push(path);
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}
/// Decoding a 4K wallpaper is slow, so rows load a cached thumbnail that the
/// search worker renders once per wallpaper revision.
fn ensure_thumbnail(source: &Path) -> Option<PathBuf> {
    let modified = fs::metadata(source)
        .and_then(|metadata| metadata.modified())
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    modified.hash(&mut hasher);
    let cache = dirs::cache_dir()?.join("Luma/wallpapers");
    let thumbnail = cache.join(format!("{:016x}.png", hasher.finish()));
    if thumbnail.is_file() {
        return Some(thumbnail);
    }
    fs::create_dir_all(&cache).ok()?;
    let pixbuf = gtk4::gdk_pixbuf::Pixbuf::from_file_at_scale(
        source,
        THUMBNAIL_WIDTH * 2,
        THUMBNAIL_HEIGHT * 2,
        true,
    )
    .ok()?;
    pixbuf.savev(&thumbnail, "png", &[]).ok()?;
    Some(thumbnail)
}

pub(crate) fn wallpaper_results(
    text: &str,
    config: &WallpaperConfig,
    session: &Mutex<WallpaperSession>,
) -> Vec<ResultItem> {
    let message = |title: &str, detail: &str| {
        vec![instruction_result(
            title,
            detail,
            "Wallpapers",
            "preferences-desktop-wallpaper-symbolic",
            65,
        )]
    };
    if !command_available(&config.backend.command) {
        session.lock().unwrap().outputs.clear();
        return message(
            "Configure a wallpaper backend",
            "Set integrations.wallpapers.backend.command and directories in Luma's config.json",
        );
    }
    let outputs = match read_status(config) {
        Ok(outputs) => outputs,
        Err(error) => {
            session.lock().unwrap().outputs.clear();
            return message("Wallpaper displays unavailable", &format!("{error:#}"));
        }
    };
    let (target, update_theme) = {
        let mut session = session.lock().unwrap();
        let target = session
            .selected_output
            .as_ref()
            .and_then(|id| outputs.iter().find(|o| &o.id == id))
            .or_else(|| outputs.iter().find(|o| o.focused))
            .or_else(|| outputs.first())
            .cloned();
        session.outputs = outputs;
        session.selected_output = target.as_ref().map(|o| o.id.clone());
        (
            target,
            session
                .update_theme
                .unwrap_or(config.theme.update_on_selection)
                && command_available(&config.theme.command),
        )
    };
    let Some(target) = target else {
        return message(
            "No wallpaper displays available",
            "Connect a display and retry with Ctrl+R",
        );
    };
    let wallpapers = list_wallpapers(config);
    if wallpapers.is_empty() {
        return message(
            "No wallpapers found",
            "Add JPG, PNG or WebP images to your configured wallpaper directories",
        );
    }
    let current = target
        .wallpaper
        .as_deref()
        .map(expand_home)
        .map(|p| p.canonicalize().unwrap_or(p));
    let mut results = Vec::new();
    for (index, path) in wallpapers.into_iter().enumerate() {
        if crate::search_runtime::cancelled() {
            break;
        }
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let score = if text.trim().is_empty() {
            Some(1000i32.saturating_sub(index as i32))
        } else {
            score_text(&name, text.trim())
        };
        let Some(score) = score else {
            continue;
        };
        let is_current = current.as_deref() == Some(path.as_path());
        let state = match target.wallpaper_state {
            WallpaperState::Observed => "Current",
            WallpaperState::Configured => "Configured",
        };
        results.push(ResultItem {
            title: name,
            subtitle: if is_current {
                format!("{state} on {}", target.label)
            } else {
                format!(
                    "Set on {}{}",
                    target.label,
                    if update_theme {
                        " and update desktop colors"
                    } else {
                        ""
                    }
                )
            },
            source: "Wallpapers",
            icon_name: "preferences-desktop-wallpaper-symbolic".into(),
            thumbnail: ensure_thumbnail(&path).map(|p| p.display().to_string()),
            score,
            action: Action::SetWallpaper {
                path: path.display().to_string(),
                output: target.id.clone(),
                update_theme,
            },
            accessory: is_current.then(|| state.into()),
            ..Default::default()
        });
    }
    if results.is_empty() {
        return message(
            "No matching wallpapers",
            "Try part of a wallpaper file name",
        );
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "luma-wallpaper-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn config(&self, failure: &str) -> WallpaperConfig {
            let helper = self.0.join("adapter.py");
            fs::write(&helper, r#"#!/usr/bin/python3
import json, sys
from pathlib import Path
root = Path(sys.argv[1])
request = json.load(sys.stdin)
with (root / 'calls').open('a') as f: f.write(json.dumps(request) + '\n')
if request['operation'] == 'status':
    print(json.dumps({'version': 1, 'outputs': [
      {'id': 'left', 'label': 'Left', 'focused': True, 'wallpaper': str(root / 'a.png'), 'wallpaper_state': 'configured'},
      {'id': 'right', 'label': 'Right', 'focused': False, 'wallpaper': None, 'wallpaper_state': 'observed'}]}))
elif sys.argv[2] == 'fail':
    print('fixture failure', file=sys.stderr)
    sys.exit(1)
else: print('{"version":1,"ok":true}')
"#).unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
            WallpaperConfig {
                directories: vec![self.0.display().to_string()],
                backend: crate::config::WallpaperCommand {
                    command: vec![
                        helper.display().to_string(),
                        self.0.display().to_string(),
                        failure.into(),
                    ],
                },
                ..Default::default()
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn selection_is_bound_to_explicit_display_and_saved_state_is_honest() {
        let f = Fixture::new();
        let config = f.config("ok");
        fs::write(f.0.join("a.png"), b"").unwrap();
        let state = Mutex::new(WallpaperSession::default());
        let rows = wallpaper_results("", &config, &state);
        assert_eq!(rows[0].accessory.as_deref(), Some("Configured"));
        state.lock().unwrap().selected_output = Some("right".into());
        let rows = wallpaper_results("a", &config, &state);
        assert!(
            matches!(&rows[0].action, Action::SetWallpaper { output, update_theme: false, .. } if output == "right")
        );
        // Changing focus/session selection cannot retarget an already constructed action.
        state.lock().unwrap().selected_output = Some("left".into());
        assert!(
            matches!(&rows[0].action, Action::SetWallpaper { output, .. } if output == "right")
        );
    }
    #[test]
    fn optional_theme_and_partial_failure_are_distinct() {
        let f = Fixture::new();
        let mut config = f.config("ok");
        config.theme.command = config.backend.command.clone();
        *config.theme.command.last_mut().unwrap() = "fail".into();
        let path = "/tmp/a 'quoted' $(literal).png";
        apply_wallpaper(&config, path, "right", false).unwrap();
        assert_eq!(
            fs::read_to_string(f.0.join("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        let error = apply_wallpaper(&config, path, "right", true).unwrap_err();
        assert!(format!("{error:#}").contains("Wallpaper applied; theme update failed"));
        let calls = fs::read_to_string(f.0.join("calls")).unwrap();
        let request: serde_json::Value =
            serde_json::from_str(calls.lines().next().unwrap()).unwrap();
        assert_eq!(request["path"], path);
        assert_eq!(request["output"], "right");
        config.backend.command = config.theme.command.clone();
        let before = calls.lines().count();
        assert!(apply_wallpaper(&config, path, "right", true).is_err());
        assert_eq!(
            fs::read_to_string(f.0.join("calls"))
                .unwrap()
                .lines()
                .count(),
            before + 1
        );
    }
    #[test]
    fn recursive_directories_deduplicate_and_stop_symlink_cycles() {
        let f = Fixture::new();
        let mut config = f.config("ok");
        fs::create_dir(f.0.join("nested")).unwrap();
        fs::write(f.0.join("a.PNG"), b"").unwrap();
        fs::write(f.0.join("nested/b.webp"), b"").unwrap();
        symlink(&f.0, f.0.join("nested/cycle")).unwrap();
        config.directories.push(f.0.display().to_string());
        assert_eq!(list_wallpapers(&config).len(), 1);
        config.recursive = true;
        assert_eq!(list_wallpapers(&config).len(), 2);
    }
    #[test]
    fn rejects_malformed_and_unconfirmed_responses() {
        let command = |body: &str| {
            vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("cat >/dev/null; printf '%s' '{body}'"),
            ]
        };
        for body in [
            "not json",
            "{}",
            r#"{"version":2,"ok":true}"#,
            r#"{"version":1,"ok":false}"#,
        ] {
            assert!(checked_apply(&command(body), json!({"version":1})).is_err());
        }
        let config = WallpaperConfig {
            backend: crate::config::WallpaperCommand {
                command: command(
                    r#"{"version":1,"outputs":[{"id":"a","label":"A","wallpaper_state":"observed"},{"id":"a","label":"B","wallpaper_state":"configured"}]}"#,
                ),
            },
            ..Default::default()
        };
        assert!(
            read_status(&config)
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
    }
}

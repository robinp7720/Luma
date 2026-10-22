//! Wallpaper picker: lists the dotfiles wallpapers, writes the choice into
//! `awww_wallpapers.conf` and restarts the watcher, which reapplies wallpapers
//! and regenerates the matugen theme that every surface follows.

use super::instruction_result;
use crate::model::{Action, ResultItem, score_text};
use anyhow::{Context, Result, anyhow};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

const IMAGE_EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];
const THUMBNAIL_WIDTH: i32 = 112;
const THUMBNAIL_HEIGHT: i32 = 68;

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

fn wallpaper_dir() -> Option<PathBuf> {
    home().map(|home| home.join(".dotfiles/wallpapers"))
}

fn wallpaper_config_path() -> Option<PathBuf> {
    std::env::var_os("AWWW_WALLPAPER_CONFIG")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".dotfiles/scripts/awww_wallpapers.conf")))
}

fn expand_home(value: &str, home: &Path) -> PathBuf {
    match value.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if value == "~" => home.to_path_buf(),
        None => PathBuf::from(value),
    }
}

/// The config keeps `~/` paths so it stays portable between machines.
fn collapse_home(path: &Path, home: &Path) -> String {
    path.strip_prefix(home)
        .map(|rest| format!("~/{}", rest.display()))
        .unwrap_or_else(|_| path.display().to_string())
}

fn config_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let line = line.split('#').next().unwrap_or("").trim();
        line.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
            .map(str::trim)
    })
}

/// The wallpaper the watcher shows on `output`: its override, else `default=`.
pub(crate) fn current_wallpaper(text: &str, output: Option<&str>, home: &Path) -> Option<PathBuf> {
    output
        .and_then(|output| config_value(text, output))
        .or_else(|| config_value(text, "default"))
        .map(|value| expand_home(value, home))
}

/// Points `output` and the theme at `wallpaper`, replacing existing lines in
/// place and keeping comments and other outputs untouched.
pub(crate) fn update_wallpaper_config(text: &str, output: &str, wallpaper: &str) -> String {
    let mut lines = text.lines().map(str::to_string).collect::<Vec<_>>();
    for key in ["theme", output] {
        let entry = format!("{key}={wallpaper}");
        let existing = lines.iter().position(|line| {
            let line = line.trim_start();
            line.strip_prefix(key)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        });
        match existing {
            Some(index) => lines[index] = entry,
            None => lines.push(entry),
        }
    }
    let mut updated = lines.join("\n");
    updated.push('\n');
    updated
}

fn list_wallpapers(dir: &Path) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| {
                    path.is_file()
                        && path
                            .extension()
                            .and_then(|extension| extension.to_str())
                            .is_some_and(|extension| {
                                IMAGE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
                            })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    paths.sort();
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

pub(crate) fn wallpaper_results(text: &str) -> Vec<ResultItem> {
    let (Some(dir), Some(home)) = (wallpaper_dir(), home()) else {
        return Vec::new();
    };
    let wallpapers = list_wallpapers(&dir);
    if wallpapers.is_empty() {
        return vec![instruction_result(
            "No wallpapers found",
            "Add images to ~/.dotfiles/wallpapers to pick them here",
            "Wallpapers",
            "preferences-desktop-wallpaper-symbolic",
            65,
        )];
    }
    let output = focused_output();
    let config = wallpaper_config_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .unwrap_or_default();
    let current = current_wallpaper(&config, output.as_deref(), &home);
    let target = output.as_deref().unwrap_or("this display");

    let query = text.trim();
    let mut results = Vec::new();
    for (index, path) in wallpapers.into_iter().enumerate() {
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let score = if query.is_empty() {
            Some(1000 - index as i32)
        } else {
            score_text(&name, query)
        };
        let Some(score) = score else {
            continue;
        };
        let is_current = current.as_deref() == Some(path.as_path());
        results.push(ResultItem {
            title: name,
            subtitle: if is_current {
                format!("Current on {target}")
            } else {
                format!("Set on {target} and rebuild the theme")
            },
            source: "Wallpapers",
            icon_name: "preferences-desktop-wallpaper-symbolic".to_string(),
            thumbnail: ensure_thumbnail(&path).map(|path| path.display().to_string()),
            score,
            action: Action::SetWallpaper {
                path: collapse_home(&path, &home),
            },
            accessory: is_current.then(|| "Current".to_string()),
            ..Default::default()
        });
    }
    if results.is_empty() {
        results.push(instruction_result(
            "No matching wallpapers",
            "Try part of a wallpaper file name",
            "Wallpapers",
            "preferences-desktop-wallpaper-symbolic",
            65,
        ));
    }
    results
}

fn focused_output() -> Option<String> {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        let output = Command::new("hyprctl")
            .args(["monitors", "-j"])
            .output()
            .ok()?;
        let monitors: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
        return monitors
            .as_array()?
            .iter()
            .find(|monitor| monitor["focused"].as_bool() == Some(true))
            .and_then(|monitor| monitor["name"].as_str())
            .map(str::to_string);
    }
    if std::env::var_os("NIRI_SOCKET").is_some() {
        let output = Command::new("niri")
            .args(["msg", "--json", "focused-output"])
            .output()
            .ok()?;
        let focused: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
        return focused["name"].as_str().map(str::to_string);
    }
    None
}

/// Writes the choice for the focused output and restarts the watcher; the
/// watcher holds a lock while running, so a one-shot run would be skipped.
pub fn apply_wallpaper(wallpaper: &str) -> Result<()> {
    let output = focused_output().ok_or_else(|| anyhow!("no focused output to set"))?;
    let config_path =
        wallpaper_config_path().ok_or_else(|| anyhow!("wallpaper config path is unknown"))?;
    let text = fs::read_to_string(&config_path).unwrap_or_default();
    let updated = update_wallpaper_config(&text, &output, wallpaper);
    let temporary = config_path.with_extension("conf.tmp");
    fs::write(&temporary, updated)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    fs::rename(&temporary, &config_path)
        .with_context(|| format!("failed to replace {}", config_path.display()))?;
    let command = crate::actions::desktop_controls::reapply_theme_command();
    Command::new(&command.program)
        .args(&command.args)
        .spawn()
        .context("failed to restart the wallpaper watcher")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "# awww wallpapers per output\n\
default=~/.dotfiles/wallpapers/a.png\n\
theme=~/.dotfiles/wallpapers/a.png\n\
\n\
DP-2=~/.dotfiles/wallpapers/b.jpg\n\
DP-3=~/.dotfiles/wallpapers/a.png\n";

    #[test]
    fn current_wallpaper_prefers_the_output_override() {
        let home = Path::new("/home/test");
        assert_eq!(
            current_wallpaper(CONFIG, Some("DP-2"), home),
            Some(PathBuf::from("/home/test/.dotfiles/wallpapers/b.jpg"))
        );
        assert_eq!(
            current_wallpaper(CONFIG, Some("HDMI-1"), home),
            Some(PathBuf::from("/home/test/.dotfiles/wallpapers/a.png"))
        );
    }

    #[test]
    fn updating_replaces_output_and_theme_lines_in_place() {
        let updated = update_wallpaper_config(CONFIG, "DP-3", "~/.dotfiles/wallpapers/c.jpg");
        assert_eq!(
            updated,
            "# awww wallpapers per output\n\
default=~/.dotfiles/wallpapers/a.png\n\
theme=~/.dotfiles/wallpapers/c.jpg\n\
\n\
DP-2=~/.dotfiles/wallpapers/b.jpg\n\
DP-3=~/.dotfiles/wallpapers/c.jpg\n"
        );
    }

    #[test]
    fn updating_appends_a_new_output_without_touching_prefixes() {
        let updated = update_wallpaper_config(CONFIG, "DP-", "~/x.png");
        assert!(updated.contains("DP-2=~/.dotfiles/wallpapers/b.jpg\n"));
        assert!(updated.ends_with("DP-=~/x.png\n"));
    }

    #[test]
    fn config_paths_collapse_to_home_shorthand() {
        let home = Path::new("/home/test");
        assert_eq!(
            collapse_home(Path::new("/home/test/.dotfiles/wallpapers/a.png"), home),
            "~/.dotfiles/wallpapers/a.png"
        );
        assert_eq!(collapse_home(Path::new("/srv/a.png"), home), "/srv/a.png");
    }
}

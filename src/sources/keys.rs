use crate::model::{Action, ResultItem, score_text};
use serde::Deserialize;
use std::process::Command;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutEntry {
    pub description: String,
    pub combo: String,
}

#[derive(Debug, Deserialize)]
struct HyprBind {
    modmask: u32,
    key: String,
    #[serde(default)]
    has_description: bool,
    #[serde(default)]
    description: String,
    #[serde(default)]
    submap: String,
}

// Modifier bits from Hyprland's `hyprctl binds` modmask, in display order.
const HYPR_MODIFIERS: [(u32, &str); 4] = [(64, "Super"), (4, "Ctrl"), (8, "Alt"), (1, "Shift")];

/// Only Hyprland reports its bindings; Niri shows its own overlay instead.
pub(crate) fn load_shortcuts() -> Vec<ShortcutEntry> {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return Vec::new();
    }
    Command::new("hyprctl")
        .args(["binds", "-j"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| parse_hypr_binds_json(&String::from_utf8_lossy(&output.stdout)).ok())
        .unwrap_or_default()
}

pub(crate) fn niri_running() -> bool {
    std::env::var_os("NIRI_SOCKET").is_some()
}

pub(crate) fn parse_hypr_binds_json(text: &str) -> Result<Vec<ShortcutEntry>, serde_json::Error> {
    let binds: Vec<HyprBind> = serde_json::from_str(text)?;
    Ok(binds
        .into_iter()
        // Undescribed Lua bindings only report an opaque callback id.
        .filter(|bind| bind.has_description && !bind.description.is_empty())
        .filter(|bind| bind.submap.is_empty())
        .map(|bind| ShortcutEntry {
            combo: format_combo(bind.modmask, &bind.key),
            description: bind.description,
        })
        .collect())
}

fn format_combo(modmask: u32, key: &str) -> String {
    let mut parts = HYPR_MODIFIERS
        .iter()
        .filter(|(bit, _)| modmask & bit != 0)
        .map(|(_, name)| (*name).to_string())
        .collect::<Vec<_>>();
    parts.push(display_key(key));
    parts.join("+")
}

fn display_key(key: &str) -> String {
    match key {
        "SPACE" | "space" => "Space".to_string(),
        "TAB" | "Tab" => "Tab".to_string(),
        "grave" => "`".to_string(),
        "mouse:272" => "Left drag".to_string(),
        "mouse:273" => "Right drag".to_string(),
        "mouse_down" => "Scroll down".to_string(),
        "mouse_up" => "Scroll up".to_string(),
        key if key.starts_with("XF86") => key.trim_start_matches("XF86").to_string(),
        key if key.chars().count() == 1 => key.to_uppercase(),
        key => {
            let mut chars = key.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

pub(crate) fn shortcut_match_score(shortcut: &ShortcutEntry, query: &str) -> Option<i32> {
    [&shortcut.description, &shortcut.combo]
        .into_iter()
        .filter_map(|text| score_text(text, query))
        .max()
}

pub(crate) fn shortcut_result_item(shortcut: ShortcutEntry, score: i32) -> ResultItem {
    ResultItem {
        prediction_key: None,
        title: shortcut.description,
        subtitle: "Keyboard shortcut".to_string(),
        source: "Shortcuts",
        icon_name: "input-keyboard-symbolic".to_string(),
        score,
        action: Action::None,
        accessory: Some(shortcut.combo),
        ..Default::default()
    }
}

pub(crate) fn niri_overlay_result_item(score: i32) -> ResultItem {
    ResultItem {
        prediction_key: None,
        title: "Show Niri's shortcut overlay".to_string(),
        subtitle: "Niri lists its bindings itself; also on Mod+Shift+/".to_string(),
        source: "Shortcuts",
        icon_name: "input-keyboard-symbolic".to_string(),
        score,
        action: Action::RunCommand {
            command: "niri msg action show-hotkey-overlay".to_string(),
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::{ShortcutEntry, format_combo, parse_hypr_binds_json, shortcut_match_score};

    #[test]
    fn keeps_only_described_top_level_binds() {
        let json = r#"[
            {"modmask": 64, "key": "F", "has_description": true, "description": "Toggle maximized window", "submap": "", "dispatcher": "__lua", "arg": "26"},
            {"modmask": 65, "key": "1", "has_description": false, "description": "", "submap": ""},
            {"modmask": 0, "key": "escape", "has_description": true, "description": "Leave resize", "submap": "resize"}
        ]"#;

        assert_eq!(
            parse_hypr_binds_json(json).unwrap(),
            vec![ShortcutEntry {
                description: "Toggle maximized window".to_string(),
                combo: "Super+F".to_string(),
            }]
        );
    }

    #[test]
    fn formats_modifiers_and_special_keys() {
        assert_eq!(format_combo(64 | 1, "SPACE"), "Super+Shift+Space");
        assert_eq!(format_combo(64 | 4, "left"), "Super+Ctrl+Left");
        assert_eq!(format_combo(0, "XF86AudioRaiseVolume"), "AudioRaiseVolume");
        assert_eq!(format_combo(64, "grave"), "Super+`");
        assert_eq!(format_combo(64, "mouse:272"), "Super+Left drag");
    }

    #[test]
    fn matches_description_or_combo() {
        let shortcut = ShortcutEntry {
            description: "Screenshot an area".to_string(),
            combo: "Print".to_string(),
        };
        assert!(shortcut_match_score(&shortcut, "screen").is_some());
        assert!(shortcut_match_score(&shortcut, "print").is_some());
        assert!(shortcut_match_score(&shortcut, "volume").is_none());
    }
}

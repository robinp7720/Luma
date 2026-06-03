use crate::model::{Action, ResultItem, WindowFocusTarget};
use std::process::Command;

#[derive(Clone, Debug)]
pub struct WindowEntry {
    pub title: String,
    pub app_name: String,
    pub workspace: String,
    pub search_blob: String,
    pub focus_order: i64,
    pub focus_target: WindowFocusTarget,
}

pub(crate) fn load_windows() -> Vec<WindowEntry> {
    if super::command_exists("hyprctl") {
        if let Ok(output) = Command::new("hyprctl").args(["clients", "-j"]).output() {
            if output.status.success() {
                if let Ok(windows) =
                    parse_hypr_windows_json(&String::from_utf8_lossy(&output.stdout))
                {
                    if !windows.is_empty() {
                        return windows;
                    }
                }
            }
        }
    }

    if super::command_exists("niri") {
        if let Ok(output) = Command::new("niri")
            .args(["msg", "windows", "--json"])
            .output()
        {
            if output.status.success() {
                if let Ok(windows) =
                    parse_niri_windows_json(&String::from_utf8_lossy(&output.stdout))
                {
                    return windows;
                }
            }
        }
    }

    Vec::new()
}

pub fn focus_window(target: &WindowFocusTarget) -> std::io::Result<std::process::ExitStatus> {
    let (program, args) = window_focus_command(target);
    Command::new(program).args(args).status()
}

pub fn focused_window_target() -> Option<WindowFocusTarget> {
    if super::command_exists("hyprctl") {
        if let Ok(output) = Command::new("hyprctl")
            .args(["activewindow", "-j"])
            .output()
        {
            if output.status.success() {
                let parsed = serde_json::from_slice::<serde_json::Value>(&output.stdout).ok()?;
                if let Some(address) = string_field(&parsed, "address") {
                    if !address.is_empty() && address != "0x0" {
                        let xwayland = bool_field(&parsed, "xwayland").unwrap_or(false);
                        return Some(WindowFocusTarget::Hyprland { address, xwayland });
                    }
                }
            }
        }
    }

    if super::command_exists("niri") {
        if let Ok(output) = Command::new("niri")
            .args(["msg", "focused-window", "--json"])
            .output()
        {
            if output.status.success() {
                let parsed = serde_json::from_slice::<serde_json::Value>(&output.stdout).ok()?;
                if let Some(id) = parsed.get("id").and_then(serde_json::Value::as_u64) {
                    return Some(WindowFocusTarget::Niri { id });
                }
            }
        }
    }

    if super::command_exists("xdotool") {
        if let Ok(output) = Command::new("xdotool").arg("getactivewindow").output() {
            if output.status.success() {
                let window_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !window_id.is_empty() {
                    return Some(WindowFocusTarget::X11 { window_id });
                }
            }
        }
    }

    None
}

pub fn window_focus_command(target: &WindowFocusTarget) -> (&'static str, Vec<String>) {
    match target {
        WindowFocusTarget::Hyprland { address, .. } => (
            "hyprctl",
            vec![
                "dispatch".to_string(),
                "focuswindow".to_string(),
                format!("address:{address}"),
            ],
        ),
        WindowFocusTarget::Niri { id } => (
            "niri",
            vec![
                "msg".to_string(),
                "action".to_string(),
                "focus-window".to_string(),
                "--id".to_string(),
                id.to_string(),
            ],
        ),
        WindowFocusTarget::X11 { window_id } => (
            "xdotool",
            vec![
                "windowactivate".to_string(),
                "--sync".to_string(),
                window_id.clone(),
            ],
        ),
    }
}

pub fn parse_hypr_windows_json(raw: &str) -> serde_json::Result<Vec<WindowEntry>> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let windows = value.as_array().into_iter().flatten();
    let mut entries = windows
        .filter(|window| bool_field(window, "mapped").unwrap_or(true))
        .filter(|window| !bool_field(window, "hidden").unwrap_or(false))
        .filter_map(|window| {
            let address = string_field(window, "address")?;
            let title = string_field(window, "title")
                .filter(|title| !title.trim().is_empty())
                .or_else(|| string_field(window, "initialTitle"))
                .unwrap_or_else(|| "Untitled window".to_string());
            let app_name = string_field(window, "class")
                .filter(|class| !class.trim().is_empty())
                .or_else(|| string_field(window, "initialClass"))
                .unwrap_or_else(|| "Unknown app".to_string());
            let workspace = window
                .get("workspace")
                .and_then(|workspace| string_field(workspace, "name"))
                .or_else(|| {
                    window
                        .get("workspace")
                        .and_then(|workspace| number_field(workspace, "id"))
                        .map(|id| id.to_string())
                })
                .unwrap_or_else(|| "unknown".to_string());
            let focus_order = number_field(window, "focusHistoryID").unwrap_or(i64::MAX);
            let search_blob =
                format!("{title} {app_name} workspace {workspace}").to_ascii_lowercase();

            Some(WindowEntry {
                title,
                app_name,
                workspace,
                search_blob,
                focus_order,
                focus_target: WindowFocusTarget::Hyprland {
                    address,
                    xwayland: bool_field(window, "xwayland").unwrap_or(false),
                },
            })
        })
        .collect::<Vec<_>>();

    entries.sort_by(|left, right| {
        left.focus_order
            .cmp(&right.focus_order)
            .then_with(|| left.title.cmp(&right.title))
    });
    Ok(entries)
}

pub fn parse_niri_windows_json(raw: &str) -> serde_json::Result<Vec<WindowEntry>> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let windows = value.as_array().into_iter().flatten();
    let mut entries = windows
        .enumerate()
        .filter_map(|(index, window)| {
            let id = unsigned_field(window, "id")?;
            let title = string_field(window, "title")
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| "Untitled window".to_string());
            let app_name = string_field(window, "app_id")
                .filter(|app_id| !app_id.trim().is_empty())
                .or_else(|| string_field(window, "app_id_or_class"))
                .unwrap_or_else(|| "Unknown app".to_string());
            let workspace = string_field(window, "workspace_name")
                .or_else(|| {
                    number_field(window, "workspace_id")
                        .map(|workspace_id| workspace_id.to_string())
                })
                .unwrap_or_else(|| "unknown".to_string());
            let focus_order = number_field(window, "focus_order")
                .or_else(|| number_field(window, "last_focus_time"))
                .unwrap_or(index as i64);
            let search_blob =
                format!("{title} {app_name} workspace {workspace}").to_ascii_lowercase();

            Some(WindowEntry {
                title,
                app_name,
                workspace,
                search_blob,
                focus_order,
                focus_target: WindowFocusTarget::Niri { id },
            })
        })
        .collect::<Vec<_>>();

    entries.sort_by(|left, right| {
        left.focus_order
            .cmp(&right.focus_order)
            .then_with(|| left.title.cmp(&right.title))
    });
    Ok(entries)
}

pub(crate) fn window_result_item(window: WindowEntry) -> ResultItem {
    let score = 760 - window.focus_order.min(200) as i32;
    window_result_item_with_score(window, score)
}

pub(crate) fn window_result_item_with_score(window: WindowEntry, score: i32) -> ResultItem {
    let prediction_key = window_prediction_key(&window);
    ResultItem {
        prediction_key: Some(prediction_key),
        title: window.title,
        subtitle: format!("{} on workspace {}", window.app_name, window.workspace),
        source: "Windows",
        icon_name: "view-grid-symbolic".to_string(),
        score,
        action: Action::FocusWindow {
            target: window.focus_target,
        },
        ..Default::default()
    }
}

pub(crate) fn window_prediction_key(window: &WindowEntry) -> String {
    format!(
        "window:{}:{}:{}",
        window.app_name, window.title, window.workspace
    )
}

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

fn number_field(value: &serde_json::Value, key: &str) -> Option<i64> {
    value.get(key).and_then(serde_json::Value::as_i64)
}

fn unsigned_field(value: &serde_json::Value, key: &str) -> Option<u64> {
    value.get(key).and_then(serde_json::Value::as_u64)
}

fn bool_field(value: &serde_json::Value, key: &str) -> Option<bool> {
    value.get(key).and_then(serde_json::Value::as_bool)
}

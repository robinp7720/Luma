use crate::model::{Action, PowerOperation, ResultItem};
use anyhow::{Context, Result};
use std::process::Command;
use std::thread;
use std::time::Duration;

pub(crate) fn power_confirmation_results(operation: PowerOperation) -> Vec<ResultItem> {
    vec![
        ResultItem {
            prediction_key: None,
            title: format!("Confirm {}", power_operation_title(operation)),
            subtitle: power_operation_confirmation(operation).to_string(),
            source: "Power",
            icon_name: power_operation_icon(operation).to_string(),
            score: 100,
            action: Action::Power {
                operation,
                confirmed: true,
            },
            ..Default::default()
        },
        ResultItem {
            prediction_key: None,
            title: "Cancel".to_string(),
            subtitle: "Keep the current session untouched".to_string(),
            source: "Power",
            icon_name: "process-stop-symbolic".to_string(),
            score: 90,
            action: Action::ReturnToSearch,
            ..Default::default()
        },
    ]
}

pub(crate) fn power_requires_confirmation(operation: PowerOperation) -> bool {
    !matches!(operation, PowerOperation::Lock)
}

fn power_operation_title(operation: PowerOperation) -> &'static str {
    match operation {
        PowerOperation::Lock => "Lock",
        PowerOperation::Suspend => "Suspend",
        PowerOperation::Logout => "Logout",
        PowerOperation::Reboot => "Reboot",
        PowerOperation::Shutdown => "Shutdown",
    }
}

fn power_operation_confirmation(operation: PowerOperation) -> &'static str {
    match operation {
        PowerOperation::Lock => "Blank the screen and keep the session running",
        PowerOperation::Suspend => "Lock first, then suspend the machine",
        PowerOperation::Logout => "Close the current desktop session now",
        PowerOperation::Reboot => "Restart the system now",
        PowerOperation::Shutdown => "Power off the system now",
    }
}

fn power_operation_icon(operation: PowerOperation) -> &'static str {
    match operation {
        PowerOperation::Lock => "system-lock-screen-symbolic",
        PowerOperation::Suspend => "media-playback-pause-symbolic",
        PowerOperation::Logout => "system-log-out-symbolic",
        PowerOperation::Reboot => "system-reboot-symbolic",
        PowerOperation::Shutdown => "system-shutdown-symbolic",
    }
}

pub(crate) fn execute_power_operation(operation: PowerOperation) -> Result<()> {
    match operation {
        PowerOperation::Lock => lock_session(),
        PowerOperation::Suspend => {
            let _ = lock_session();
            thread::sleep(Duration::from_secs(1));
            spawn_system_command("systemctl", &["suspend"], "failed to suspend")
        }
        PowerOperation::Logout => logout_session(),
        PowerOperation::Reboot => {
            spawn_system_command("systemctl", &["reboot"], "failed to reboot")
        }
        PowerOperation::Shutdown => {
            spawn_system_command("systemctl", &["poweroff"], "failed to power off")
        }
    }
}

fn lock_session() -> Result<()> {
    if is_hyprland_session() && !process_running_for_user("hyprlock") {
        if crate::spawn_optional_command("hyprlock", &[])?.is_some() {
            return Ok(());
        }
    }

    if lock_current_logind_session()? {
        return Ok(());
    }

    if !process_running_for_user("hyprlock")
        && crate::spawn_optional_command("hyprlock", &[])?.is_some()
    {
        return Ok(());
    }

    anyhow::bail!("no lock command is available for the current session");
}

fn logout_session() -> Result<()> {
    if is_hyprland_session()
        && crate::spawn_optional_command("hyprctl", &["dispatch", "exit"])?.is_some()
    {
        return Ok(());
    }

    if is_niri_session()
        && crate::spawn_optional_command("niri", &["msg", "action", "quit", "--skip-confirmation"])?
            .is_some()
    {
        return Ok(());
    }

    if is_bspwm_session() && crate::spawn_optional_command("bspc", &["quit"])?.is_some() {
        return Ok(());
    }

    if let Some(session_id) = current_logind_session_id() {
        spawn_system_command(
            "loginctl",
            &["terminate-session", &session_id],
            "failed to terminate current session",
        )?;
        return Ok(());
    }

    anyhow::bail!("no safe logout method found for the current session");
}

fn lock_current_logind_session() -> Result<bool> {
    if let Some(session_id) = current_logind_session_id() {
        return crate::spawn_optional_command("loginctl", &["lock-session", &session_id])
            .map(|child| child.is_some());
    }

    crate::spawn_optional_command("loginctl", &["lock-session"]).map(|child| child.is_some())
}

fn current_logind_session_id() -> Option<String> {
    if let Ok(session_id) = std::env::var("XDG_SESSION_ID") {
        if !session_id.trim().is_empty() {
            return Some(session_id);
        }
    }

    let user = std::env::var("USER").ok()?;
    let output = Command::new("loginctl")
        .args(["show-user", &user, "--property=Display", "--value"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let session_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if session_id.is_empty() || session_id == "n/a" {
        None
    } else {
        Some(session_id)
    }
}

pub(crate) fn is_hyprland_session() -> bool {
    std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() || desktop_matches("hyprland")
}

pub(crate) fn is_niri_session() -> bool {
    std::env::var("NIRI_SOCKET").is_ok() || desktop_matches("niri")
}

fn is_bspwm_session() -> bool {
    std::env::var("BSPWM_SOCKET").is_ok() || desktop_matches("bspwm")
}

fn desktop_matches(wanted: &str) -> bool {
    let raw = std::env::var("XDG_CURRENT_DESKTOP")
        .or_else(|_| std::env::var("XDG_SESSION_DESKTOP"))
        .or_else(|_| std::env::var("DESKTOP_SESSION"))
        .unwrap_or_default();

    raw.split([':', ';'])
        .any(|token| token.eq_ignore_ascii_case(wanted))
}

fn process_running_for_user(process_name: &str) -> bool {
    let Ok(uid) = std::env::var("UID") else {
        return false;
    };
    if uid.trim().is_empty() {
        return false;
    }

    Command::new("pgrep")
        .args(["-u", &uid, "-x", process_name])
        .status()
        .is_ok_and(|status| status.success())
}

fn spawn_system_command(program: &str, args: &[&str], message: &str) -> Result<()> {
    crate::spawn_optional_command(program, args)
        .with_context(|| message.to_string())?
        .with_context(|| format!("{program} is not installed"))?;
    Ok(())
}

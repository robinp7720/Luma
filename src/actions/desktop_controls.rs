use crate::cockpit::{CockpitClient, DesktopContext};
use crate::model::DesktopControlOperation;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DesktopControlCommand {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
}

impl DesktopControlCommand {
    fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.to_string(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
        }
    }
}

/// Screenshots go through the same script as the compositor bindings so the
/// output choice, cancel handling and notification stay in one place.
fn screenshot_command(mode: &str) -> DesktopControlCommand {
    let script = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| screenshot_script_for_home(&home))
        .unwrap_or_else(|| PathBuf::from("screenshot.sh"));
    DesktopControlCommand {
        program: script.to_string_lossy().into_owned(),
        args: vec![mode.to_string()],
    }
}

fn screenshot_script_for_home(home: &Path) -> PathBuf {
    home.join(".dotfiles/scripts/screenshot.sh")
}

/// The Vigil OSD that matches a level change, mirroring the key bindings.
pub(crate) fn osd_kind(operation: &DesktopControlOperation) -> Option<&'static str> {
    match operation {
        DesktopControlOperation::VolumeUp
        | DesktopControlOperation::VolumeDown
        | DesktopControlOperation::VolumeMute => Some("volume"),
        DesktopControlOperation::BrightnessUp | DesktopControlOperation::BrightnessDown => {
            Some("brightness")
        }
        _ => None,
    }
}

pub(crate) fn desktop_control_commands(
    operation: &DesktopControlOperation,
) -> Vec<DesktopControlCommand> {
    match operation {
        DesktopControlOperation::MediaPlayPause => {
            vec![DesktopControlCommand::new("playerctl", &["play-pause"])]
        }
        DesktopControlOperation::MediaNext => {
            vec![DesktopControlCommand::new("playerctl", &["next"])]
        }
        DesktopControlOperation::MediaPrevious => {
            vec![DesktopControlCommand::new("playerctl", &["previous"])]
        }
        DesktopControlOperation::VolumeUp => vec![DesktopControlCommand::new(
            "wpctl",
            &["set-volume", "@DEFAULT_AUDIO_SINK@", "5%+"],
        )],
        DesktopControlOperation::VolumeDown => vec![DesktopControlCommand::new(
            "wpctl",
            &["set-volume", "@DEFAULT_AUDIO_SINK@", "5%-"],
        )],
        DesktopControlOperation::VolumeMute => vec![DesktopControlCommand::new(
            "wpctl",
            &["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"],
        )],
        DesktopControlOperation::BrightnessUp => vec![DesktopControlCommand::new(
            "brightnessctl",
            &["--class=backlight", "set", "+10%"],
        )],
        DesktopControlOperation::BrightnessDown => vec![DesktopControlCommand::new(
            "brightnessctl",
            &["--class=backlight", "set", "10%-"],
        )],
        DesktopControlOperation::AudioSettings => {
            vec![DesktopControlCommand::new("pavucontrol", &[])]
        }
        DesktopControlOperation::BluetoothTogglePower => vec![DesktopControlCommand::new(
            "sh",
            &[
                "-lc",
                "state=$(bluetoothctl show | awk '/Powered:/ {print $2}'); if [ \"$state\" = yes ]; then bluetoothctl power off; else bluetoothctl power on; fi",
            ],
        )],
        DesktopControlOperation::NetworkSettings => {
            vec![DesktopControlCommand::new("nm-connection-editor", &[])]
        }
        DesktopControlOperation::PowerProfileCycle => vec![DesktopControlCommand::new(
            "sh",
            &[
                "-lc",
                "current=$(powerprofilesctl get); case \"$current\" in performance) next=balanced ;; balanced) next=power-saver ;; *) next=performance ;; esac; powerprofilesctl set \"$next\"",
            ],
        )],
        DesktopControlOperation::PowerProfileSet { profile } => vec![DesktopControlCommand {
            program: "powerprofilesctl".to_string(),
            args: vec!["set".to_string(), profile.clone()],
        }],
        DesktopControlOperation::ScreenshotArea => vec![screenshot_command("area")],
        DesktopControlOperation::ScreenshotScreen => vec![screenshot_command("output")],
        DesktopControlOperation::ColorPicker => {
            vec![DesktopControlCommand::new("hyprpicker", &["-a"])]
        }
        DesktopControlOperation::NotificationHistoryPop => Vec::new(),
        DesktopControlOperation::NotificationCloseAll => Vec::new(),
        DesktopControlOperation::NotificationPauseToggle => Vec::new(),
        DesktopControlOperation::TvModeStart => {
            vec![DesktopControlCommand::new("tv-mode", &["start"])]
        }
    }
}

pub(crate) fn execute_desktop_control_operation(operation: &DesktopControlOperation) -> Result<()> {
    match operation {
        DesktopControlOperation::NotificationHistoryPop => {
            return CockpitClient::from_env()?.open_context(DesktopContext::Notifications, None);
        }
        DesktopControlOperation::NotificationCloseAll => {
            return CockpitClient::from_env()?.clear_notifications();
        }
        DesktopControlOperation::NotificationPauseToggle => {
            let client = CockpitClient::from_env()?;
            let enabled = !client.notifications()?.dnd;
            return client.set_notification_dnd(enabled);
        }
        DesktopControlOperation::TvModeStart => {}
        _ => {}
    }
    let osd = osd_kind(operation);
    for command in desktop_control_commands(operation) {
        let mut child = Command::new(&command.program)
            .args(&command.args)
            .spawn()
            .with_context(|| format!("failed to spawn {}", command.program))?;
        if osd.is_some() {
            // The OSD reads the new level, so the change has to land first.
            child
                .wait()
                .with_context(|| format!("failed to wait for {}", command.program))?;
        }
    }
    if let Some(kind) = osd {
        crate::spawn_optional_command("vigil", &["osd", kind])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshots_use_the_shared_script() {
        assert_eq!(
            screenshot_script_for_home(Path::new("/home/test")),
            PathBuf::from("/home/test/.dotfiles/scripts/screenshot.sh")
        );
        let area = desktop_control_commands(&DesktopControlOperation::ScreenshotArea);
        assert!(area[0].program.ends_with("scripts/screenshot.sh"));
        assert_eq!(area[0].args, vec!["area"]);
        let screen = desktop_control_commands(&DesktopControlOperation::ScreenshotScreen);
        assert_eq!(screen[0].args, vec!["output"]);
    }

    #[test]
    fn level_changes_show_the_matching_osd() {
        assert_eq!(
            osd_kind(&DesktopControlOperation::VolumeMute),
            Some("volume")
        );
        assert_eq!(
            osd_kind(&DesktopControlOperation::BrightnessDown),
            Some("brightness")
        );
        assert_eq!(osd_kind(&DesktopControlOperation::MediaNext), None);
    }
}

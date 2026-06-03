use crate::model::DesktopControlOperation;
use anyhow::{Context, Result};
use std::process::Command;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DesktopControlCommand {
    pub(crate) program: &'static str,
    pub(crate) args: Vec<String>,
}

impl DesktopControlCommand {
    fn new(program: &'static str, args: &[&str]) -> Self {
        Self {
            program,
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
        }
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
            program: "powerprofilesctl",
            args: vec!["set".to_string(), profile.clone()],
        }],
        DesktopControlOperation::ScreenshotArea => vec![DesktopControlCommand::new(
            "sh",
            &[
                "-lc",
                "dir=\"$HOME/Pictures/Screenshots\"; mkdir -p \"$dir\"; ts=$(date +'%Y-%m-%d %H-%M-%S'); file=\"$dir/Screenshot from $ts.png\"; region=$(slurp) || exit 1; grim -g \"$region\" \"$file\" && wl-copy -t image/png < \"$file\"; notify-send \"Screenshot\" \"Saved to $file\" >/dev/null 2>&1 || true",
            ],
        )],
        DesktopControlOperation::ScreenshotScreen => vec![DesktopControlCommand::new(
            "sh",
            &[
                "-lc",
                "dir=\"$HOME/Pictures/Screenshots\"; mkdir -p \"$dir\"; ts=$(date +'%Y-%m-%d %H-%M-%S'); file=\"$dir/Screenshot from $ts.png\"; grim \"$file\" && wl-copy -t image/png < \"$file\"; notify-send \"Screenshot\" \"Saved to $file\" >/dev/null 2>&1 || true",
            ],
        )],
        DesktopControlOperation::ColorPicker => {
            vec![DesktopControlCommand::new("hyprpicker", &["-a"])]
        }
        DesktopControlOperation::NotificationHistoryPop => {
            vec![DesktopControlCommand::new("dunstctl", &["history-pop"])]
        }
        DesktopControlOperation::NotificationCloseAll => {
            vec![DesktopControlCommand::new("dunstctl", &["close-all"])]
        }
        DesktopControlOperation::NotificationPauseToggle => vec![DesktopControlCommand::new(
            "sh",
            &[
                "-lc",
                "paused=$(dunstctl is-paused); if [ \"$paused\" = true ]; then dunstctl set-paused false; else dunstctl set-paused true; fi",
            ],
        )],
    }
}

pub(crate) fn execute_desktop_control_operation(operation: &DesktopControlOperation) -> Result<()> {
    for command in desktop_control_commands(operation) {
        Command::new(command.program)
            .args(&command.args)
            .spawn()
            .with_context(|| format!("failed to spawn {}", command.program))?;
    }
    Ok(())
}

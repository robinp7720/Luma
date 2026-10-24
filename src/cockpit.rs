use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::model::{Action, DesktopControlOperation, ResultItem};
use crate::sources::sort_visible_results;

const SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(400);
const ACTION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum DesktopContext {
    #[default]
    Overview,
    Keyboard,
    Resources,
    Network,
    Bluetooth,
    Audio,
    Power,
    Clock,
    Notifications,
}

impl DesktopContext {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Keyboard => "keyboard",
            Self::Resources => "resources",
            Self::Network => "network",
            Self::Bluetooth => "bluetooth",
            Self::Audio => "audio",
            Self::Power => "power",
            Self::Clock => "clock",
            Self::Notifications => "notifications",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextHealth {
    #[default]
    Healthy,
    Degraded,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaControlAction {
    Previous,
    Next,
    PlayPause,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerProfile {
    Performance,
    Balanced,
    PowerSaver,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContextAction {
    SelectKeyboardLayout {
        index: u8,
    },
    SetWifiEnabled {
        enabled: bool,
    },
    SetBluetoothPowered {
        powered: bool,
    },
    ConnectBluetoothDevice {
        address: String,
    },
    DisconnectBluetoothDevice {
        address: String,
    },
    SetVolumePercent {
        percent: u8,
    },
    ToggleMute,
    SetAudioOutput {
        sink_name: String,
    },
    ControlMedia {
        player: String,
        action: MediaControlAction,
    },
    SetBrightnessPercent {
        device: String,
        percent: u8,
    },
    SetPowerProfile {
        profile: PowerProfile,
    },
    PauseTimer {
        id: String,
    },
    ResumeTimer {
        id: String,
    },
    CancelTimer {
        id: String,
    },
}

impl ContextAction {
    /// The same level action aimed at `percent`; `None` for non-level actions.
    pub fn with_level(&self, percent: u8) -> Option<ContextAction> {
        let percent = percent.min(100);
        match self {
            ContextAction::SetVolumePercent { .. } => {
                Some(ContextAction::SetVolumePercent { percent })
            }
            ContextAction::SetBrightnessPercent { device, .. } => {
                Some(ContextAction::SetBrightnessPercent {
                    device: device.clone(),
                    percent,
                })
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextActionSpec {
    pub title: String,
    pub subtitle: String,
    pub icon_name: String,
    pub accessory: Option<String>,
    pub action: ContextAction,
    /// A 0–100 level shown as a slider; the action is resent with each value.
    #[serde(default)]
    pub level: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub context: DesktopContext,
    pub title: String,
    pub icon_name: String,
    pub summary: String,
    pub detail: String,
    pub health: ContextHealth,
    pub actions: Vec<ContextActionSpec>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationState {
    pub items: Vec<NotificationEntry>,
    pub visible_ids: Vec<u32>,
    pub unread_count: u16,
    pub dnd: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationEntry {
    pub id: u32,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    #[serde(default = "default_duplicate_count")]
    pub duplicate_count: u16,
    #[serde(default)]
    pub actions: Vec<NotificationAction>,
    /// Whether the sender still listens for actions.
    #[serde(default)]
    pub actionable: bool,
    #[serde(default)]
    pub received_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationAction {
    pub key: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimerState {
    pub id: String,
    pub label: String,
    pub remaining_seconds: u64,
    /// `None` while paused.
    pub target_epoch: Option<i64>,
    pub completed: bool,
}

impl TimerState {
    pub fn is_paused(&self) -> bool {
        !self.completed && self.target_epoch.is_none()
    }
}

fn default_duplicate_count() -> u16 {
    1
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ControlRequest {
    TimerStart {
        label: String,
        duration_seconds: u64,
    },
    TimerPause {
        id: String,
    },
    TimerResume {
        id: String,
    },
    TimerCancel {
        id: String,
    },
    TimerList,
    ContextGet {
        context: Option<DesktopContext>,
    },
    ContextExecute {
        action: ContextAction,
    },
    ControlCenterOpen {
        context: DesktopContext,
        output: Option<String>,
    },
    ControlCenterClose,
    NotificationsGet,
    NotificationInvoke {
        id: u32,
        action_key: String,
    },
    NotificationDismiss {
        id: u32,
    },
    NotificationsClear,
    NotificationsSetDnd {
        enabled: bool,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ControlResponse {
    Accepted,
    Timers {
        timers: Vec<TimerState>,
    },
    Contexts {
        contexts: Vec<ContextSnapshot>,
    },
    Notifications {
        state: NotificationState,
    },
    ActionResult {
        success: bool,
        message: Option<String>,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Debug)]
pub struct CockpitClient {
    socket_path: PathBuf,
}

impl CockpitClient {
    pub fn from_env() -> Result<Self> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("XDG_RUNTIME_DIR is unavailable"))?;
        Ok(Self {
            socket_path: PathBuf::from(runtime).join("vigil.sock"),
        })
    }

    pub fn contexts(&self, context: Option<DesktopContext>) -> Result<Vec<ContextSnapshot>> {
        match self.send(&ControlRequest::ContextGet { context }, SNAPSHOT_TIMEOUT)? {
            ControlResponse::Contexts { contexts } => Ok(contexts),
            ControlResponse::Error { message } => bail!(message),
            _ => bail!("bar returned an unexpected context response"),
        }
    }

    pub fn execute(&self, action: ContextAction) -> Result<()> {
        match self.send(&ControlRequest::ContextExecute { action }, ACTION_TIMEOUT)? {
            ControlResponse::ActionResult { success: true, .. } => Ok(()),
            ControlResponse::ActionResult {
                success: false,
                message,
            } => bail!(message.unwrap_or_else(|| "bar action failed".to_string())),
            ControlResponse::Error { message } => bail!(message),
            _ => bail!("bar returned an unexpected action response"),
        }
    }

    pub fn open_context(&self, context: DesktopContext, output: Option<String>) -> Result<()> {
        match self.send(
            &ControlRequest::ControlCenterOpen { context, output },
            ACTION_TIMEOUT,
        )? {
            ControlResponse::Accepted => Ok(()),
            ControlResponse::Error { message } => bail!(message),
            _ => bail!("bar returned an unexpected surface response"),
        }
    }

    pub fn close_control_center(&self) -> Result<()> {
        match self.send(&ControlRequest::ControlCenterClose, SNAPSHOT_TIMEOUT)? {
            ControlResponse::Accepted => Ok(()),
            ControlResponse::Error { message } => bail!(message),
            _ => bail!("bar returned an unexpected surface response"),
        }
    }

    pub fn notifications(&self) -> Result<NotificationState> {
        match self.send(&ControlRequest::NotificationsGet, SNAPSHOT_TIMEOUT)? {
            ControlResponse::Notifications { state } => Ok(state),
            ControlResponse::Error { message } => bail!(message),
            _ => bail!("Vigil returned an unexpected notification response"),
        }
    }

    pub fn clear_notifications(&self) -> Result<()> {
        match self.send(&ControlRequest::NotificationsClear, ACTION_TIMEOUT)? {
            ControlResponse::Accepted => Ok(()),
            ControlResponse::Error { message } => bail!(message),
            _ => bail!("Vigil returned an unexpected notification response"),
        }
    }

    pub fn set_notification_dnd(&self, enabled: bool) -> Result<()> {
        match self.send(
            &ControlRequest::NotificationsSetDnd { enabled },
            ACTION_TIMEOUT,
        )? {
            ControlResponse::Accepted => Ok(()),
            ControlResponse::Error { message } => bail!(message),
            _ => bail!("Vigil returned an unexpected notification response"),
        }
    }

    pub fn timers(&self) -> Result<Vec<TimerState>> {
        self.timer_request(&ControlRequest::TimerList, SNAPSHOT_TIMEOUT)
    }

    pub fn start_timer(&self, label: String, duration_seconds: u64) -> Result<()> {
        self.timer_request(
            &ControlRequest::TimerStart {
                label,
                duration_seconds,
            },
            ACTION_TIMEOUT,
        )
        .map(|_| ())
    }

    pub fn pause_timer(&self, id: String) -> Result<()> {
        self.timer_request(&ControlRequest::TimerPause { id }, ACTION_TIMEOUT)
            .map(|_| ())
    }

    pub fn resume_timer(&self, id: String) -> Result<()> {
        self.timer_request(&ControlRequest::TimerResume { id }, ACTION_TIMEOUT)
            .map(|_| ())
    }

    pub fn cancel_timer(&self, id: String) -> Result<()> {
        self.timer_request(&ControlRequest::TimerCancel { id }, ACTION_TIMEOUT)
            .map(|_| ())
    }

    fn timer_request(
        &self,
        request: &ControlRequest,
        timeout: Duration,
    ) -> Result<Vec<TimerState>> {
        match self.send(request, timeout)? {
            ControlResponse::Timers { timers } => Ok(timers),
            ControlResponse::Error { message } => bail!(message),
            _ => bail!("Vigil returned an unexpected timer response"),
        }
    }

    pub fn invoke_notification(&self, id: u32, action_key: String) -> Result<()> {
        match self.send(
            &ControlRequest::NotificationInvoke { id, action_key },
            ACTION_TIMEOUT,
        )? {
            ControlResponse::Accepted => Ok(()),
            ControlResponse::Error { message } => bail!(message),
            _ => bail!("Vigil returned an unexpected notification response"),
        }
    }

    pub fn dismiss_notification(&self, id: u32) -> Result<()> {
        match self.send(&ControlRequest::NotificationDismiss { id }, ACTION_TIMEOUT)? {
            ControlResponse::Accepted => Ok(()),
            ControlResponse::Error { message } => bail!(message),
            _ => bail!("Vigil returned an unexpected notification response"),
        }
    }

    fn send(&self, request: &ControlRequest, timeout: Duration) -> Result<ControlResponse> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .with_context(|| format!("cannot connect to {}", self.socket_path.display()))?;
        stream
            .set_read_timeout(Some(timeout))
            .context("failed to set bar response timeout")?;
        stream
            .set_write_timeout(Some(timeout))
            .context("failed to set bar request timeout")?;
        serde_json::to_writer(&mut stream, request).context("failed to encode bar request")?;
        stream
            .write_all(b"\n")
            .context("failed to write bar request")?;
        stream.flush().context("failed to flush bar request")?;
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .context("failed to read bar response")?;
        if line.trim().is_empty() {
            bail!("bar closed the integration socket without a response");
        }
        serde_json::from_str(&line).context("failed to decode bar response")
    }
}

pub fn context_results(
    contexts: &[ContextSnapshot],
    selected: Option<DesktopContext>,
    query: &str,
) -> Vec<ResultItem> {
    let normalized = query.trim().to_ascii_lowercase();
    let mut results = Vec::new();
    for context in contexts
        .iter()
        .filter(|context| selected.is_none_or(|selected| selected == context.context))
    {
        for spec in &context.actions {
            if !normalized.is_empty()
                && !matches_query(&normalized, [&spec.title, &spec.subtitle, &context.title])
            {
                continue;
            }
            if results.iter().any(|item: &ResultItem| {
                matches!(&item.action, Action::CockpitControl { action } if action == &spec.action)
            }) {
                continue;
            }
            results.push(ResultItem {
                title: spec.title.clone(),
                subtitle: spec.subtitle.clone(),
                source: "Vigil",
                icon_name: spec.icon_name.clone(),
                score: match (selected, spec.level) {
                    // A context's level slider leads, ahead of its toggles.
                    (Some(_), Some(_)) => 1_720,
                    (Some(_), None) => contextual_action_score(&spec.action),
                    (None, _) => 1_050,
                },
                action: Action::CockpitControl {
                    action: spec.action.clone(),
                },
                prediction_key: None,
                accessory: spec.accessory.clone(),
                badges: Vec::new(),
                thumbnail: None,
                level: spec.level,
            });
        }
        if selected.is_none()
            && (normalized.is_empty()
                || matches_query(
                    &normalized,
                    [&context.title, &context.summary, &context.detail],
                ))
        {
            results.push(ResultItem {
                title: format!("Open {}", context.title),
                subtitle: context.summary.clone(),
                source: "Vigil",
                icon_name: context.icon_name.clone(),
                // An empty Controls filter starts with places to inspect, before
                // commands that immediately change volume, connectivity, or power.
                score: if normalized.is_empty() {
                    match context.context {
                        DesktopContext::Overview => 1_200,
                        DesktopContext::Audio => 1_190,
                        DesktopContext::Network => 1_180,
                        DesktopContext::Bluetooth => 1_170,
                        DesktopContext::Notifications => 1_160,
                        DesktopContext::Clock => 1_150,
                        DesktopContext::Power => 1_140,
                        DesktopContext::Keyboard => 1_130,
                        DesktopContext::Resources => 1_120,
                    }
                } else if context.title.to_ascii_lowercase().contains(&normalized) {
                    1_150
                } else {
                    740
                },
                action: Action::OpenCockpitContext {
                    context: context.context,
                },
                prediction_key: None,
                accessory: None,
                badges: Vec::new(),
                thumbnail: None,
                level: None,
            });
        }
    }
    results
}

fn contextual_action_score(action: &ContextAction) -> i32 {
    match action {
        ContextAction::SetAudioOutput { .. }
        | ContextAction::SelectKeyboardLayout { .. }
        | ContextAction::ConnectBluetoothDevice { .. }
        | ContextAction::DisconnectBluetoothDevice { .. }
        | ContextAction::SetPowerProfile { .. }
        | ContextAction::PauseTimer { .. }
        | ContextAction::ResumeTimer { .. }
        | ContextAction::CancelTimer { .. } => 1_620,
        ContextAction::SetBluetoothPowered { .. } => 1_700,
        ContextAction::SetWifiEnabled { .. } | ContextAction::ToggleMute => 1_590,
        ContextAction::SetVolumePercent { .. } | ContextAction::SetBrightnessPercent { .. } => {
            1_560
        }
        ContextAction::ControlMedia { .. } => 1_500,
    }
}

pub fn merge_context_results(
    mut local: Vec<ResultItem>,
    contexts: &[ContextSnapshot],
    selected: Option<DesktopContext>,
    query: &str,
    bar_available: bool,
) -> Vec<ResultItem> {
    if !bar_available {
        if let Some(selected) = selected
            && query.trim().is_empty()
        {
            local.retain(|item| local_action_matches_context(&item.action, selected));
        }
        return sort_visible_results(local);
    }

    local.retain(|item| !duplicates_bar_control(&item.action));
    let cockpit = context_results(contexts, selected, query);
    if selected.is_some() && query.trim().is_empty() {
        return sort_visible_results(cockpit);
    }
    local.extend(cockpit);
    sort_visible_results(local)
}

fn local_action_matches_context(action: &Action, context: DesktopContext) -> bool {
    let Action::DesktopControl { operation } = action else {
        return context == DesktopContext::Overview;
    };
    match context {
        DesktopContext::Overview => true,
        DesktopContext::Audio => matches!(
            operation,
            DesktopControlOperation::MediaPlayPause
                | DesktopControlOperation::MediaNext
                | DesktopControlOperation::MediaPrevious
                | DesktopControlOperation::VolumeUp
                | DesktopControlOperation::VolumeDown
                | DesktopControlOperation::VolumeMute
                | DesktopControlOperation::AudioSettings
        ),
        DesktopContext::Network => {
            matches!(operation, DesktopControlOperation::NetworkSettings)
        }
        DesktopContext::Bluetooth => {
            matches!(operation, DesktopControlOperation::BluetoothTogglePower)
        }
        DesktopContext::Power => matches!(
            operation,
            DesktopControlOperation::PowerProfileCycle
                | DesktopControlOperation::PowerProfileSet { .. }
        ),
        DesktopContext::Notifications => matches!(
            operation,
            DesktopControlOperation::NotificationHistoryPop
                | DesktopControlOperation::NotificationCloseAll
                | DesktopControlOperation::NotificationPauseToggle
        ),
        DesktopContext::Keyboard | DesktopContext::Resources | DesktopContext::Clock => false,
    }
}

fn duplicates_bar_control(action: &Action) -> bool {
    matches!(
        action,
        Action::DesktopControl {
            operation: DesktopControlOperation::MediaPlayPause
                | DesktopControlOperation::MediaNext
                | DesktopControlOperation::MediaPrevious
                | DesktopControlOperation::VolumeUp
                | DesktopControlOperation::VolumeDown
                | DesktopControlOperation::VolumeMute
                | DesktopControlOperation::BluetoothTogglePower
                | DesktopControlOperation::PowerProfileCycle
                | DesktopControlOperation::PowerProfileSet { .. }
        }
    )
}

fn matches_query<'a>(query: &str, values: impl IntoIterator<Item = &'a String>) -> bool {
    values
        .into_iter()
        .any(|value| value.to_ascii_lowercase().contains(query))
}

#[cfg(test)]
mod tests {
    use super::{ContextAction, ContextActionSpec, ContextHealth, ContextSnapshot, DesktopContext};
    use crate::model::{Action, DesktopControlOperation, ResultItem};

    #[test]
    fn contextual_results_include_actions_and_reverse_navigation() {
        let context = ContextSnapshot {
            context: DesktopContext::Audio,
            title: "Audio".to_string(),
            icon_name: "audio-volume-high-symbolic".to_string(),
            summary: "Volume 42%".to_string(),
            detail: "Speakers".to_string(),
            health: ContextHealth::Healthy,
            actions: vec![ContextActionSpec {
                title: "Mute audio".to_string(),
                subtitle: "Current volume 42%".to_string(),
                icon_name: "audio-volume-muted-symbolic".to_string(),
                accessory: None,
                action: ContextAction::ToggleMute,
                level: None,
            }],
        };
        let results = super::context_results(&[context.clone()], Some(DesktopContext::Audio), "");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Mute audio");

        let mut overview = context.clone();
        overview.context = DesktopContext::Overview;
        overview.title = "Quick Settings".to_string();
        let results =
            super::merge_context_results(Vec::new(), &[overview, context], None, "", true);
        assert!(matches!(
            results[0].action,
            Action::OpenCockpitContext {
                context: DesktopContext::Overview
            }
        ));
        assert_eq!(results[0].title, "Open Quick Settings");
        assert_eq!(
            results
                .iter()
                .filter(|item| matches!(
                    item.action,
                    Action::CockpitControl {
                        action: ContextAction::ToggleMute
                    }
                ))
                .count(),
            1
        );
    }

    #[test]
    fn level_actions_retarget_and_lead_their_context() {
        assert_eq!(
            ContextAction::SetVolumePercent { percent: 10 }.with_level(130),
            Some(ContextAction::SetVolumePercent { percent: 100 })
        );
        assert_eq!(
            ContextAction::SetBrightnessPercent {
                device: "intel_backlight".to_string(),
                percent: 10,
            }
            .with_level(42),
            Some(ContextAction::SetBrightnessPercent {
                device: "intel_backlight".to_string(),
                percent: 42,
            })
        );
        assert_eq!(ContextAction::ToggleMute.with_level(42), None);

        let spec = |title: &str, action, level| ContextActionSpec {
            title: title.to_string(),
            subtitle: String::new(),
            icon_name: String::new(),
            accessory: None,
            action,
            level,
        };
        let context = ContextSnapshot {
            context: DesktopContext::Audio,
            title: "Audio".to_string(),
            icon_name: String::new(),
            summary: String::new(),
            detail: String::new(),
            health: ContextHealth::Healthy,
            actions: vec![
                spec("Mute audio", ContextAction::ToggleMute, None),
                spec(
                    "Volume",
                    ContextAction::SetVolumePercent { percent: 42 },
                    Some(42),
                ),
            ],
        };
        let mut results = super::context_results(&[context], Some(DesktopContext::Audio), "");
        results.sort_by(|left, right| right.score.cmp(&left.score));
        assert_eq!(results[0].title, "Volume");
        assert_eq!(results[0].level, Some(42));
        assert_eq!(results[1].level, None);
    }

    #[test]
    fn offline_context_keeps_only_matching_local_controls() {
        let local = vec![
            ResultItem {
                title: "Mute volume".to_string(),
                action: Action::DesktopControl {
                    operation: DesktopControlOperation::VolumeMute,
                },
                ..Default::default()
            },
            ResultItem {
                title: "Toggle Bluetooth".to_string(),
                action: Action::DesktopControl {
                    operation: DesktopControlOperation::BluetoothTogglePower,
                },
                ..Default::default()
            },
        ];
        let results =
            super::merge_context_results(local, &[], Some(DesktopContext::Audio), "", false);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Mute volume");
    }
}

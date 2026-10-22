//! Timers and notification history live in Vigil; these modes list and drive
//! them over its control socket so the launcher and bar never disagree.

use super::instruction_result;
use crate::cockpit::{NotificationState, TimerState};
use crate::model::{Action, ResultItem, VigilCommand, score_text};

const MAX_TIMER_SECONDS: u64 = 7 * 24 * 60 * 60;
const MAX_NOTIFICATIONS: usize = 50;
const MAX_BODY_CHARS: usize = 120;

/// Reads `25m focus`, `1h30m`, `90s tea` or `25` (minutes) into a duration and
/// a label. The duration has to come first so ordinary words never start one.
pub(crate) fn parse_timer_request(text: &str) -> Option<(u64, String)> {
    let text = text.trim();
    let (duration, label) = text
        .split_once(char::is_whitespace)
        .map_or((text, ""), |(duration, label)| (duration, label.trim()));
    let seconds = parse_duration(duration)?;
    let label = if label.is_empty() { "Timer" } else { label };
    Some((seconds, label.to_string()))
}

fn parse_duration(text: &str) -> Option<u64> {
    if let Ok(minutes) = text.parse::<u64>() {
        return minutes
            .checked_mul(60)
            .filter(|seconds| (1..=MAX_TIMER_SECONDS).contains(seconds));
    }
    let mut total = 0_u64;
    let mut digits = String::new();
    for character in text.chars() {
        if character.is_ascii_digit() {
            digits.push(character);
            continue;
        }
        let unit = match character.to_ascii_lowercase() {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
        let value = digits.parse::<u64>().ok()?;
        digits.clear();
        total = total.checked_add(value.checked_mul(unit)?)?;
    }
    if !digits.is_empty() || !(1..=MAX_TIMER_SECONDS).contains(&total) {
        return None;
    }
    Some(total)
}

pub(crate) fn format_duration(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    match (hours, minutes, seconds) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, 0) => format!("{minutes}m"),
        (0, minutes, seconds) => format!("{minutes}m {seconds}s"),
        (hours, 0, _) => format!("{hours}h"),
        (hours, minutes, _) => format!("{hours}h {minutes}m"),
    }
}

fn timer_remaining(timer: &TimerState, now: i64) -> u64 {
    timer
        .target_epoch
        .map_or(timer.remaining_seconds, |target| {
            u64::try_from(target.saturating_sub(now)).unwrap_or(0)
        })
}

fn vigil_unavailable(source: &'static str, icon: &str) -> ResultItem {
    instruction_result(
        "Vigil is not running",
        "Timers and notifications live in Vigil; start vigil.service",
        source,
        icon,
        65,
    )
}

pub(crate) fn timer_results(
    text: &str,
    timers: Option<Vec<TimerState>>,
    now: i64,
) -> Vec<ResultItem> {
    let Some(timers) = timers else {
        return vec![vigil_unavailable("Timers", "alarm-symbolic")];
    };
    let mut results = Vec::new();
    if let Some((seconds, label)) = parse_timer_request(text) {
        results.push(ResultItem {
            title: format!("Start {} timer", format_duration(seconds)),
            subtitle: label.clone(),
            source: "Timers",
            icon_name: "alarm-symbolic".to_string(),
            score: 1000,
            action: Action::Vigil {
                command: VigilCommand::StartTimer {
                    label,
                    duration_seconds: seconds,
                },
            },
            ..Default::default()
        });
    }
    let query = text.trim();
    for timer in timers.into_iter().filter(|timer| !timer.completed) {
        let score = if query.is_empty() || !results.is_empty() {
            Some(500)
        } else {
            score_text(&timer.label, query)
        };
        let Some(score) = score else {
            continue;
        };
        let remaining = format_duration(timer_remaining(&timer, now));
        let paused = timer.is_paused();
        results.push(ResultItem {
            subtitle: if paused {
                format!("Paused · {remaining} left")
            } else {
                format!("{remaining} left")
            },
            title: timer.label.clone(),
            source: "Timers",
            icon_name: if paused {
                "media-playback-pause-symbolic"
            } else {
                "alarm-symbolic"
            }
            .to_string(),
            score,
            action: Action::TimerActions {
                id: timer.id,
                label: timer.label,
                paused,
            },
            ..Default::default()
        });
    }
    if results.is_empty() {
        results.push(instruction_result(
            if query.is_empty() {
                "No timers running"
            } else {
                "Start with a duration"
            },
            "Type a duration and a label, like 25m focus, 1h30m or 90s tea",
            "Timers",
            "alarm-symbolic",
            65,
        ));
    }
    results
}

pub(crate) fn timer_action_results(id: &str, label: &str, paused: bool) -> Vec<ResultItem> {
    let item = |title: &str, icon: &str, command: VigilCommand| ResultItem {
        title: title.to_string(),
        subtitle: label.to_string(),
        source: "Timers",
        icon_name: icon.to_string(),
        action: Action::Vigil { command },
        ..Default::default()
    };
    let id = id.to_string();
    vec![
        if paused {
            item(
                "Resume timer",
                "media-playback-start-symbolic",
                VigilCommand::ResumeTimer { id: id.clone() },
            )
        } else {
            item(
                "Pause timer",
                "media-playback-pause-symbolic",
                VigilCommand::PauseTimer { id: id.clone() },
            )
        },
        item(
            "Cancel timer",
            "process-stop-symbolic",
            VigilCommand::CancelTimer { id },
        ),
        back_result(),
    ]
}

fn back_result() -> ResultItem {
    ResultItem {
        title: "Back".to_string(),
        subtitle: "Return to search".to_string(),
        source: "Status",
        icon_name: "go-previous-symbolic".to_string(),
        action: Action::ReturnToSearch,
        ..Default::default()
    }
}

fn first_line_preview(body: &str) -> String {
    let line = body
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    if line.chars().count() > MAX_BODY_CHARS {
        format!("{}…", line.chars().take(MAX_BODY_CHARS).collect::<String>())
    } else {
        line.to_string()
    }
}

/// History rows never get a prediction key, so notification text stays out
/// of the usage store.
pub(crate) fn notification_results(
    text: &str,
    state: Option<NotificationState>,
) -> Vec<ResultItem> {
    let Some(state) = state else {
        return vec![vigil_unavailable(
            "Notifications",
            "preferences-system-notifications-symbolic",
        )];
    };
    let query = text.trim();
    let mut items = state
        .items
        .into_iter()
        .filter(|notification| notification.duplicate_count != 0)
        .collect::<Vec<_>>();
    items.sort_by(|left, right| right.received_at.cmp(&left.received_at));

    let mut results = Vec::new();
    for (index, notification) in items.into_iter().enumerate() {
        let score = if query.is_empty() {
            Some(1000 - index as i32)
        } else {
            score_text(
                &format!(
                    "{} {} {}",
                    notification.app_name, notification.summary, notification.body
                ),
                query,
            )
        };
        let Some(score) = score else {
            continue;
        };
        let summary = if notification.summary.is_empty() {
            notification.app_name.clone()
        } else {
            notification.summary.clone()
        };
        let preview = first_line_preview(&notification.body);
        results.push(ResultItem {
            subtitle: if preview.is_empty() {
                notification.app_name.clone()
            } else {
                format!("{} · {preview}", notification.app_name)
            },
            title: summary.clone(),
            source: "Notifications",
            icon_name: "preferences-system-notifications-symbolic".to_string(),
            score,
            action: Action::NotificationActions {
                id: notification.id,
                summary,
                actionable: notification.actionable,
                actions: notification
                    .actions
                    .into_iter()
                    .filter(|action| action.key != "default")
                    .map(|action| (action.key, action.label))
                    .collect(),
            },
            ..Default::default()
        });
    }
    results.sort_by(|left, right| right.score.cmp(&left.score));
    results.truncate(MAX_NOTIFICATIONS);
    if results.is_empty() {
        results.push(instruction_result(
            if query.is_empty() {
                "No notifications"
            } else {
                "No matching notifications"
            },
            "Notifications Vigil keeps in its history appear here",
            "Notifications",
            "preferences-system-notifications-symbolic",
            65,
        ));
    }
    results
}

pub(crate) fn notification_action_results(
    id: u32,
    summary: &str,
    actionable: bool,
    actions: &[(String, String)],
) -> Vec<ResultItem> {
    let item = |title: String, icon: &str, command: VigilCommand| ResultItem {
        title,
        subtitle: summary.to_string(),
        source: "Notifications",
        icon_name: icon.to_string(),
        action: Action::Vigil { command },
        ..Default::default()
    };
    let mut results = Vec::new();
    if actionable {
        results.push(item(
            "Open".to_string(),
            "go-next-symbolic",
            VigilCommand::InvokeNotification {
                id,
                action_key: "default".to_string(),
            },
        ));
        results.extend(actions.iter().map(|(key, label)| {
            item(
                label.clone(),
                "media-playback-start-symbolic",
                VigilCommand::InvokeNotification {
                    id,
                    action_key: key.clone(),
                },
            )
        }));
    }
    results.push(item(
        "Dismiss".to_string(),
        "edit-delete-symbolic",
        VigilCommand::DismissNotification { id },
    ));
    results.push(back_result());
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cockpit::{NotificationAction, NotificationEntry};

    #[test]
    fn timer_durations_accept_compound_units_and_bare_minutes() {
        assert_eq!(
            parse_timer_request("25m focus"),
            Some((1500, "focus".to_string()))
        );
        assert_eq!(
            parse_timer_request("1h30m"),
            Some((5400, "Timer".to_string()))
        );
        assert_eq!(
            parse_timer_request("90s  green tea"),
            Some((90, "green tea".to_string()))
        );
        assert_eq!(parse_timer_request("25"), Some((1500, "Timer".to_string())));
        assert_eq!(parse_timer_request("focus 25m"), None);
        assert_eq!(parse_timer_request("10x"), None);
        assert_eq!(parse_timer_request("0m"), None);
        assert_eq!(parse_timer_request("1m30"), None);
        assert_eq!(parse_timer_request("999h"), None);
        assert_eq!(parse_timer_request(""), None);
    }

    #[test]
    fn durations_format_compactly() {
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(1500), "25m");
        assert_eq!(format_duration(90), "1m 30s");
        assert_eq!(format_duration(5400), "1h 30m");
        assert_eq!(format_duration(7200), "2h");
    }

    fn timer(id: &str, label: &str, target_epoch: Option<i64>) -> TimerState {
        TimerState {
            id: id.to_string(),
            label: label.to_string(),
            remaining_seconds: 300,
            target_epoch,
            completed: false,
        }
    }

    #[test]
    fn timer_mode_offers_start_and_lists_running_timers() {
        let results = timer_results(
            "10m tea",
            Some(vec![timer("a", "focus", Some(1_000 + 120))]),
            1_000,
        );
        assert_eq!(results[0].title, "Start 10m timer");
        assert!(matches!(
            &results[0].action,
            Action::Vigil {
                command: VigilCommand::StartTimer { label, duration_seconds: 600 }
            } if label == "tea"
        ));
        assert_eq!(results[1].title, "focus");
        assert_eq!(results[1].subtitle, "2m left");

        let paused = timer_results("", Some(vec![timer("b", "laundry", None)]), 0);
        assert_eq!(paused[0].subtitle, "Paused · 5m left");
        assert!(matches!(
            paused[0].action,
            Action::TimerActions { paused: true, .. }
        ));

        let offline = timer_results("", None, 0);
        assert_eq!(offline[0].title, "Vigil is not running");
    }

    #[test]
    fn timer_actions_toggle_between_pause_and_resume() {
        let running = timer_action_results("a", "focus", false);
        assert_eq!(running[0].title, "Pause timer");
        assert_eq!(running[1].title, "Cancel timer");
        assert!(matches!(running[2].action, Action::ReturnToSearch));
        assert_eq!(
            timer_action_results("a", "focus", true)[0].title,
            "Resume timer"
        );
    }

    fn notification(id: u32, summary: &str, received_at: i64) -> NotificationEntry {
        NotificationEntry {
            id,
            app_name: "Builds".to_string(),
            summary: summary.to_string(),
            body: "line one\nline two".to_string(),
            duplicate_count: 1,
            actions: vec![
                NotificationAction {
                    key: "default".to_string(),
                    label: "Open".to_string(),
                },
                NotificationAction {
                    key: "retry".to_string(),
                    label: "Retry".to_string(),
                },
            ],
            actionable: true,
            received_at,
        }
    }

    #[test]
    fn notification_mode_lists_newest_first_and_filters() {
        let state = NotificationState {
            items: vec![
                notification(1, "older build", 10),
                notification(2, "newer deploy", 20),
            ],
            ..Default::default()
        };
        let all = notification_results("", Some(state.clone()));
        assert_eq!(all[0].title, "newer deploy");
        assert_eq!(all[0].subtitle, "Builds · line one");
        assert!(all.iter().all(|item| item.prediction_key.is_none()));

        let filtered = notification_results("build", Some(state));
        assert_eq!(filtered.len(), 2);
        let filtered = notification_results(
            "deploy",
            Some(NotificationState {
                items: vec![notification(2, "newer deploy", 20), notification(1, "x", 1)],
                ..Default::default()
            }),
        );
        assert_eq!(filtered[0].title, "newer deploy");
        assert!(matches!(
            &filtered[0].action,
            Action::NotificationActions { id: 2, actions, .. } if actions == &vec![("retry".to_string(), "Retry".to_string())]
        ));
    }

    #[test]
    fn notification_actions_offer_open_actions_and_dismiss() {
        let actions = vec![("retry".to_string(), "Retry".to_string())];
        let results = notification_action_results(7, "build failed", true, &actions);
        let titles = results
            .iter()
            .map(|item| item.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(titles, ["Open", "Retry", "Dismiss", "Back"]);
        assert!(matches!(
            &results[1].action,
            Action::Vigil {
                command: VigilCommand::InvokeNotification { id: 7, action_key }
            } if action_key == "retry"
        ));

        let passive = notification_action_results(7, "done", false, &actions);
        let titles = passive
            .iter()
            .map(|item| item.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(titles, ["Dismiss", "Back"]);
    }
}

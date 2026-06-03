use crate::actions::{is_hyprland_session, is_niri_session};
use crate::model::{Action, PasswordOperation, ResultItem, WindowFocusTarget};
use crate::password::{
    Credential, TypeStep, default_login_steps, parse_credential, run_program_input,
    wl_copy_command, wtype_commands_for_steps, xclip_command, xdotool_commands_for_steps,
};
use crate::sources::{focus_window, pass_prediction_key};
use anyhow::{Context, Result};
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use std::process::Command;
use std::thread;
use std::time::Duration;

const AUTOTYPE_AFTER_CLOSE_DELAY_MS: u64 = 180;

pub(crate) fn copy_pass_entry(entry: &str) -> Result<()> {
    let secret = load_pass_secret(entry)?;
    copy_secret(&secret)
}

pub(crate) fn execute_password_operation(
    window: &gtk4::ApplicationWindow,
    entry: &str,
    operation: PasswordOperation,
    previous_focus_target: Option<&WindowFocusTarget>,
) -> Result<()> {
    let credential = load_pass_credential(entry)?;

    match operation {
        PasswordOperation::AutotypeLogin => {
            type_secret_steps(
                window,
                previous_focus_target,
                default_login_steps(&credential),
            )?;
        }
        PasswordOperation::CopyPassword => {
            copy_secret(&credential.password)?;
            window.close();
        }
        PasswordOperation::CopyUsername => {
            copy_secret(&credential.username)?;
            window.close();
        }
        PasswordOperation::TypePassword => {
            type_secret_steps(
                window,
                previous_focus_target,
                vec![TypeStep::Text(credential.password)],
            )?;
        }
        PasswordOperation::TypeUsername => {
            type_secret_steps(
                window,
                previous_focus_target,
                vec![TypeStep::Text(credential.username)],
            )?;
        }
        PasswordOperation::OpenUrl => {
            let url = credential
                .url
                .context("pass entry does not contain a URL")?;
            gio::AppInfo::launch_default_for_uri(&url, gio::AppLaunchContext::NONE)
                .context("failed to open URL")?;
            window.close();
        }
        PasswordOperation::CopyUrl => {
            let url = credential
                .url
                .context("pass entry does not contain a URL")?;
            copy_secret(&url)?;
            window.close();
        }
        PasswordOperation::CopyOtp => {
            let otp = load_pass_otp(entry)?;
            copy_secret(&otp)?;
            window.close();
        }
        PasswordOperation::TypeOtp => {
            let otp = load_pass_otp(entry)?;
            type_secret_steps(window, previous_focus_target, vec![TypeStep::Text(otp)])?;
        }
        PasswordOperation::CustomAutotype => {
            let steps = custom_autotype_steps(entry, &credential)?;
            type_secret_steps(window, previous_focus_target, steps)?;
        }
        PasswordOperation::Inspect => unreachable!("inspect is handled before action execution"),
    }

    Ok(())
}

fn type_secret_steps(
    window: &gtk4::ApplicationWindow,
    previous_focus_target: Option<&WindowFocusTarget>,
    steps: Vec<TypeStep>,
) -> Result<()> {
    let target = previous_focus_target
        .context("no previously focused window was captured")?
        .clone();
    if !type_backend_available() {
        anyhow::bail!("wtype or xdotool is required for password autotype");
    }

    let application_hold = window.application().map(|app| app.hold());
    window.close();

    glib::timeout_add_local_once(
        Duration::from_millis(AUTOTYPE_AFTER_CLOSE_DELAY_MS),
        move || {
            if let Err(error) = run_type_secret_steps(&target, steps) {
                eprintln!("password autotype failed: {error:?}");
            }
            drop(application_hold);
        },
    );

    Ok(())
}

fn run_type_secret_steps(target: &WindowFocusTarget, steps: Vec<TypeStep>) -> Result<()> {
    let status = focus_window(target).context("failed to refocus previous window")?;
    if !status.success() {
        anyhow::bail!("failed to refocus previous window");
    }

    let use_x11_backend = use_x11_type_backend(target) && crate::command_exists("xdotool");
    thread::sleep(Duration::from_millis(if use_x11_backend {
        500
    } else {
        80
    }));

    let commands = if use_x11_backend {
        xdotool_commands_for_steps(&steps)
    } else if wayland_available() && crate::command_exists("wtype") {
        wtype_commands_for_steps(&steps)
    } else {
        xdotool_commands_for_steps(&steps)
    };

    for command in commands {
        run_program_input(command)?;
    }

    Ok(())
}

fn type_backend_available() -> bool {
    (wayland_available() && crate::command_exists("wtype")) || crate::command_exists("xdotool")
}

pub(crate) fn use_x11_type_backend(target: &WindowFocusTarget) -> bool {
    matches!(
        target,
        WindowFocusTarget::Hyprland { xwayland: true, .. } | WindowFocusTarget::X11 { .. }
    )
}

fn copy_secret(text: &str) -> Result<()> {
    if wayland_available() && crate::command_exists("wl-copy") {
        run_program_input(wl_copy_command(text, password_clip_timeout_seconds()))
    } else if crate::command_exists("xclip") {
        run_program_input(xclip_command(text))?;
        clear_xclip_clipboard_after(password_clip_timeout_seconds());
        Ok(())
    } else {
        anyhow::bail!("wl-copy or xclip is required to copy password data");
    }
}

fn clear_xclip_clipboard_after(timeout_seconds: u64) {
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(timeout_seconds));
        let _ = Command::new("xclip")
            .args(["-selection", "clipboard", "-in"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                if let Some(stdin) = child.stdin.take() {
                    drop(stdin);
                }
                child.wait().map(|_| ())
            });
    });
}

fn wayland_available() -> bool {
    wayland_available_for_session(
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        is_hyprland_session() || is_niri_session(),
    )
}

pub(crate) fn wayland_available_for_session(
    session_type: Option<&str>,
    wayland_display_set: bool,
    known_wayland_compositor: bool,
) -> bool {
    wayland_display_set
        && (known_wayland_compositor
            || !session_type.is_some_and(|session| session.eq_ignore_ascii_case("x11")))
}

fn password_clip_timeout_seconds() -> u64 {
    crate::app_config()
        .map(|config| config.current().integrations.password_clip_timeout_seconds)
        .filter(|seconds| *seconds > 0)
        .unwrap_or_else(|| {
            std::env::var("PASSWORD_STORE_CLIP_TIME")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|seconds| *seconds > 0)
                .unwrap_or(15)
        })
}

pub(crate) fn inspected_password_results(credential: &Credential) -> Vec<ResultItem> {
    let mut rows = vec![
        password_action_result(
            &credential.entry,
            "Autotype login",
            "Type username, Tab, and password without submitting",
            PasswordOperation::AutotypeLogin,
            1_000,
            Some(pass_prediction_key(&credential.entry)),
        ),
        password_action_result(
            &credential.entry,
            "Copy password",
            "Copy password and clear it after the password-store timeout",
            PasswordOperation::CopyPassword,
            950,
            None,
        ),
        password_action_result(
            &credential.entry,
            "Copy username",
            "Copy username metadata or the entry basename",
            PasswordOperation::CopyUsername,
            940,
            None,
        ),
        password_action_result(
            &credential.entry,
            "Type password",
            "Type only the password into the focused window",
            PasswordOperation::TypePassword,
            930,
            None,
        ),
        password_action_result(
            &credential.entry,
            "Type username",
            "Type only the username into the focused window",
            PasswordOperation::TypeUsername,
            920,
            None,
        ),
    ];

    if credential.url.is_some() {
        rows.push(password_action_result(
            &credential.entry,
            "Open URL",
            "Open this entry's URL in the default browser",
            PasswordOperation::OpenUrl,
            910,
            None,
        ));
        rows.push(password_action_result(
            &credential.entry,
            "Copy URL",
            "Copy this entry's URL",
            PasswordOperation::CopyUrl,
            900,
            None,
        ));
    }

    if credential.otp_uri.is_some() {
        rows.push(password_action_result(
            &credential.entry,
            "Copy OTP",
            "Generate and copy a one-time password with pass-otp",
            PasswordOperation::CopyOtp,
            890,
            None,
        ));
        rows.push(password_action_result(
            &credential.entry,
            "Type OTP",
            "Generate and type a one-time password with pass-otp",
            PasswordOperation::TypeOtp,
            880,
            None,
        ));
    }

    if credential.autotype.is_some() {
        rows.push(password_action_result(
            &credential.entry,
            "Custom autotype",
            "Run this entry's autotype template",
            PasswordOperation::CustomAutotype,
            870,
            None,
        ));
    }

    rows
}

fn password_action_result(
    entry: &str,
    title: &str,
    subtitle: &str,
    operation: PasswordOperation,
    score: i32,
    prediction_key: Option<String>,
) -> ResultItem {
    ResultItem {
        prediction_key,
        title: format!("{title}: {entry}"),
        subtitle: subtitle.to_string(),
        source: "Passwords",
        icon_name: "dialog-password-symbolic".to_string(),
        score,
        action: Action::Password {
            entry: entry.to_string(),
            operation,
        },
        ..Default::default()
    }
}

fn custom_autotype_steps(entry: &str, credential: &Credential) -> Result<Vec<TypeStep>> {
    let template = credential
        .autotype
        .as_deref()
        .context("pass entry does not contain an autotype template")?;
    let mut steps = Vec::new();

    let mut tokens = template.split_whitespace().peekable();
    while let Some(token) = tokens.next() {
        match token {
            ":tab" => steps.push(TypeStep::Key("Tab")),
            ":space" => steps.push(TypeStep::Text(" ".to_string())),
            ":enter" => steps.push(TypeStep::Key("Return")),
            ":delay" => steps.push(TypeStep::Delay(1_000)),
            "pass" | "password" => steps.push(TypeStep::Text(credential.password.clone())),
            "user" | "username" => steps.push(TypeStep::Text(credential.username.clone())),
            "path" => steps.push(TypeStep::Text(entry.to_string())),
            "basename" | "filename" => {
                steps.push(TypeStep::Text(crate::password::fallback_username(entry)));
            }
            ":otp" => {
                if matches!(tokens.peek(), Some(&"pass") | Some(&"gopass")) {
                    tokens.next();
                }
                steps.push(TypeStep::Text(load_pass_otp(entry)?));
            }
            key => {
                let Some(value) = credential.fields.get(&key.to_ascii_lowercase()) else {
                    anyhow::bail!("unknown autotype token: {key}");
                };
                steps.push(TypeStep::Text(value.clone()));
            }
        }
    }

    Ok(steps)
}

fn load_pass_secret(entry: &str) -> Result<String> {
    parse_credential(entry, &load_pass_output(&["show", entry])?)
        .map(|credential| credential.password)
}

pub(crate) fn load_pass_credential(entry: &str) -> Result<Credential> {
    parse_credential(entry, &load_pass_output(&["show", entry])?)
}

fn load_pass_otp(entry: &str) -> Result<String> {
    load_pass_output(&["otp", entry]).map(|output| output.trim().to_string())
}

fn load_pass_output(args: &[&str]) -> Result<String> {
    let output = Command::new("pass")
        .args(args)
        .output()
        .context("failed to run pass")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!(
            "{}",
            if stderr.is_empty() {
                "pass failed"
            } else {
                stderr.as_str()
            }
        );
    }

    String::from_utf8(output.stdout).context("pass returned non-UTF-8 output")
}

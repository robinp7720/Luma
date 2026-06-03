pub(crate) mod desktop_controls;
pub(crate) mod password;
pub(crate) mod power;

pub(crate) use desktop_controls::execute_desktop_control_operation;
pub(crate) use password::{
    copy_pass_entry, execute_password_operation, inspected_password_results, load_pass_credential,
};
pub(crate) use power::{
    execute_power_operation, is_hyprland_session, is_niri_session, power_confirmation_results,
    power_requires_confirmation,
};

use std::fs;
use std::path::{Path, PathBuf};

use gtk4::gdk;

pub(crate) fn theme_css_path(
    xdg_config_home: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    xdg_config_home
        .map(|path| path.join("Luma/theme.css"))
        .or_else(|| home.map(|path| path.join(".config/Luma/theme.css")))
}

pub(crate) fn compose_theme_css(fallback: &str, theme_override: Option<&str>) -> String {
    match theme_override {
        Some(theme_override) => format!("{fallback}\n\n{theme_override}"),
        None => fallback.to_string(),
    }
}

pub(crate) fn load_theme_override(path: Option<&Path>) -> Option<String> {
    path.and_then(|path| fs::read_to_string(path).ok())
}

pub(crate) fn apply_css() {
    let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let home = dirs::home_dir();
    let path = theme_css_path(xdg_config_home.as_deref(), home.as_deref());
    let theme_override = load_theme_override(path.as_deref());
    let css = compose_theme_css(fallback_css(), theme_override.as_deref());
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(&css);
    gtk4::style_context_add_provider_for_display(
        &gdk::Display::default().expect("display"),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

pub(crate) fn fallback_css() -> &'static str {
    r#"
      window {
        background: transparent;
      }

      .launcher-shell {
        background: linear-gradient(180deg, rgba(19, 23, 33, 0.78), rgba(12, 15, 24, 0.92));
        border: 1px solid rgba(255, 255, 255, 0.10);
        border-radius: 18px;
        box-shadow: 0 18px 44px rgba(0, 0, 0, 0.32);
        padding: 0.8rem;
      }

      @keyframes launcher-context-enter {
        0% { opacity: 0; transform: translate(14px, -12px) scale(0.96); }
        62% { opacity: 1; transform: translate(-2px, 2px) scale(1.008); }
        100% { opacity: 1; transform: none; }
      }

      .launcher-from-bar {
        transform-origin: 100% 0;
      }

      .launcher-from-bar.launcher-entering {
        animation: launcher-context-enter 320ms cubic-bezier(0.16, 1, 0.3, 1) both;
      }

      .launcher-context {
        min-height: 58px;
        padding: 10px 12px;
        border-radius: 14px;
        background: rgba(255, 255, 255, 0.045);
        border: 1px solid rgba(255, 255, 255, 0.06);
      }

      .launcher-context-icon {
        color: rgba(190, 213, 255, 0.94);
      }

      .launcher-context-title {
        font-size: 0.78rem;
        font-weight: 720;
        letter-spacing: 0.08em;
        text-transform: uppercase;
        color: rgba(210, 219, 237, 0.72);
      }

      .launcher-context-summary {
        font-size: 1.02rem;
        font-weight: 650;
        color: rgba(247, 249, 255, 0.98);
      }

      .launcher-context-detail {
        font-size: 0.84rem;
        color: rgba(210, 219, 237, 0.70);
      }

      .launcher-context-health {
        padding: 4px 8px;
        border-radius: 999px;
        background: rgba(120, 168, 255, 0.12);
        color: rgba(190, 213, 255, 0.90);
      }

      .launcher-context-open {
        min-width: 30px;
        min-height: 30px;
        padding: 0;
        border: 0;
        border-radius: 10px;
        background: rgba(255, 255, 255, 0.055);
        color: rgba(247, 249, 255, 0.90);
      }

      .launcher-context-open:hover {
        background: rgba(120, 168, 255, 0.16);
      }

      .launcher-context.is-unavailable .launcher-context-health {
        background: rgba(255, 170, 120, 0.12);
        color: rgba(255, 198, 162, 0.92);
      }

      @media (prefers-reduced-motion: reduce) {
        .launcher-from-bar.launcher-entering { animation: none; }
      }

      .launcher-entry {
        min-height: 54px;
        font-size: 1.08rem;
        padding: 0.35rem 2.55rem 0.35rem 0.82rem;
        border-radius: 12px;
        border: 1px solid rgba(255, 255, 255, 0.09);
        background: rgba(255, 255, 255, 0.07);
        color: rgba(247, 249, 255, 0.98);
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.04);
      }

      .launcher-entry:focus-within {
        border-color: rgba(142, 188, 255, 0.55);
        background: rgba(255, 255, 255, 0.10);
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08),
                    0 0 0 3px rgba(106, 160, 255, 0.14);
      }

      .launcher-search-spinner {
        color: rgba(190, 213, 255, 0.88);
        min-width: 22px;
        min-height: 22px;
      }

      .launcher-results {
        background: transparent;
      }

      .launcher-row {
        margin-bottom: 5px;
        border-radius: 12px;
        border: 1px solid rgba(255, 255, 255, 0.02);
        background: rgba(255, 255, 255, 0.02);
      }

      .launcher-row:selected {
        background: linear-gradient(90deg, rgba(120, 168, 255, 0.16), rgba(255, 255, 255, 0.08));
        border-color: rgba(142, 188, 255, 0.22);
      }

      .launcher-row-status {
        background: rgba(255, 255, 255, 0.04);
        border: 1px dashed rgba(255, 255, 255, 0.08);
      }

      .launcher-row-status:selected {
        background: rgba(255, 255, 255, 0.07);
      }

      .launcher-icon-wrap {
        min-width: 34px;
        border-radius: 10px;
        background: rgba(255, 255, 255, 0.07);
        border: 1px solid rgba(255, 255, 255, 0.04);
        padding: 6px;
      }

      .launcher-icon {
        color: rgba(240, 244, 255, 0.96);
      }

      .launcher-title {
        font-size: 1rem;
        font-weight: 650;
      }

      .launcher-subtitle {
        font-size: 0.86rem;
        color: rgba(210, 219, 237, 0.70);
      }

      .launcher-accessory {
        font-size: 0.82rem;
        color: rgba(190, 213, 255, 0.78);
      }

      .launcher-badge {
        color: rgba(210, 219, 237, 0.80);
      }

      .launcher-badge-unread {
        color: rgba(120, 168, 255, 0.95);
      }

      .settings-shell {
        background: linear-gradient(180deg, rgba(18, 22, 31, 0.94), rgba(10, 13, 21, 0.98));
        border: 1px solid rgba(255, 255, 255, 0.10);
        border-radius: 22px;
        box-shadow: 0 22px 58px rgba(0, 0, 0, 0.40);
      }

      .settings-scroller {
        background: transparent;
      }

      .settings-header {
        padding-bottom: 4px;
      }

      .settings-title {
        font-size: 1.55rem;
        font-weight: 720;
        color: rgba(247, 249, 255, 0.98);
      }

      .settings-subtitle {
        color: rgba(210, 219, 237, 0.78);
      }

      .settings-card {
        background: rgba(255, 255, 255, 0.04);
        border: 1px solid rgba(255, 255, 255, 0.08);
        border-radius: 18px;
        padding: 16px;
      }

      .settings-card-title {
        font-size: 1.05rem;
        font-weight: 680;
        color: rgba(247, 249, 255, 0.98);
      }

      .settings-card-subtitle {
        color: rgba(210, 219, 237, 0.76);
      }

      .settings-row {
        min-height: 46px;
        padding-top: 4px;
        padding-bottom: 4px;
      }

      .settings-row-title {
        font-weight: 600;
        color: rgba(247, 249, 255, 0.96);
      }

      .settings-row-subtitle {
        color: rgba(210, 219, 237, 0.72);
      }

      .settings-status {
        color: rgba(210, 219, 237, 0.76);
      }
    "#
}

#[cfg(test)]
mod tests {
    use super::{compose_theme_css, load_theme_override, theme_css_path};
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn theme_path_prefers_xdg_config_home() {
        assert_eq!(
            theme_css_path(Some(Path::new("/tmp/xdg")), Some(Path::new("/home/test"))),
            Some(PathBuf::from("/tmp/xdg/Luma/theme.css"))
        );
    }

    #[test]
    fn theme_path_falls_back_to_home_config() {
        assert_eq!(
            theme_css_path(None, Some(Path::new("/home/test"))),
            Some(PathBuf::from("/home/test/.config/Luma/theme.css"))
        );
    }

    #[test]
    fn composed_css_keeps_fallback_when_override_is_missing() {
        assert_eq!(compose_theme_css("fallback", None), "fallback");
    }

    #[test]
    fn composed_css_places_override_after_fallback() {
        assert_eq!(
            compose_theme_css("fallback", Some("override")),
            "fallback\n\noverride"
        );
    }

    #[test]
    fn missing_theme_override_is_ignored() {
        let missing = std::env::temp_dir().join(format!(
            "luma-theme-missing-{}-{}.css",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));

        assert_eq!(load_theme_override(Some(&missing)), None);
    }

    #[test]
    fn readable_theme_override_is_loaded() {
        let path = std::env::temp_dir().join(format!(
            "luma-theme-readable-{}-{}.css",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::write(&path, ".launcher-shell { color: red; }").expect("write theme fixture");

        assert_eq!(
            load_theme_override(Some(&path)).as_deref(),
            Some(".launcher-shell { color: red; }")
        );

        fs::remove_file(path).expect("remove theme fixture");
    }
}

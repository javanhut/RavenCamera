//! The look: Raven Glass (`data/raven-glass.css`, the stylesheet shared with
//! Settings, Store and Power, kept identical to theirs), then the classes
//! only a camera has — the viewfinder, the floating control bar, the record
//! button.
//!
//! The accent is the person's, from `~/.config/raven/desktop.toml`, as in
//! every Raven app. The record button is red whatever the accent is: red is
//! what "recording" means, and an accent-coloured one would read as an
//! ordinary primary action.

use gtk4 as gtk;
use libadwaita as adw;

use serde::Deserialize;

const DEFAULT_ACCENT: &str = "#7AA2F7";

pub const CSS: &str = concat!(
    include_str!("../../data/raven-glass.css"),
    include_str!("../../data/camera.css"),
);

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Appearance {
    theme_mode: String,
    accent: String,
    transparency: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Desktop {
    appearance: Appearance,
}

fn desktop() -> Desktop {
    std::fs::read_to_string(crate::paths::config_dir().join("desktop.toml"))
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn is_hex(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Load the stylesheet and the person's accent and light/dark choice.
/// Returns whether the window should be glass.
pub fn load() -> bool {
    let display = gtk::gdk::Display::default().expect("no display");
    let base = gtk::CssProvider::new();
    base.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &base,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let desk = desktop();
    let accent = if is_hex(&desk.appearance.accent) {
        desk.appearance.accent.as_str()
    } else {
        DEFAULT_ACCENT
    };
    let light = desk.appearance.theme_mode == "light";
    adw::StyleManager::default().set_color_scheme(if light {
        adw::ColorScheme::ForceLight
    } else {
        adw::ColorScheme::ForceDark
    });
    let css = format!(
        "@define-color accent_bg_color {accent};\n@define-color accent_color {accent};\n{}",
        if light {
            include_str!("../../data/raven-glass-light.css")
        } else {
            ""
        }
    );
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
    desk.appearance.transparency.unwrap_or(true)
}

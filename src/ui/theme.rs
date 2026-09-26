//! The look: Raven Glass (`data/raven-glass.css`, the stylesheet shared with
//! Settings, Store and Power, kept identical to theirs), then the classes
//! only a camera has — the viewfinder, the floating control bar, the record
//! button.
//!
//! The accent is the person's, from `~/.config/raven/desktop.toml`, as in
//! every Raven app. The record button is red whatever the accent is: red is
//! what "recording" means, and an accent-coloured one would read as an
//! ordinary primary action.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use serde::Deserialize;

const DEFAULT_ACCENT: &str = "#7AA2F7";

pub const CSS: &str = concat!(
    include_str!("../../data/raven-glass.css"),
    include_str!("../../data/camera.css"),
);

/// Laid over [`CSS`] when the desktop is light.
const LIGHT_CSS: &str = concat!(
    include_str!("../../data/raven-glass-light.css"),
    include_str!("../../data/camera-light.css"),
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

impl Desktop {
    fn parse(text: &str) -> Desktop {
        toml::from_str(text).unwrap_or_default()
    }

    fn accent(&self) -> &str {
        if is_hex(&self.appearance.accent) {
            &self.appearance.accent
        } else {
            DEFAULT_ACCENT
        }
    }

    /// Only an explicit "light" is light. "auto" is dark, as in Settings,
    /// Store and the file manager, so the desktop's apps agree.
    fn light(&self) -> bool {
        self.appearance.theme_mode == "light"
    }

    fn color_scheme(&self) -> adw::ColorScheme {
        match self.appearance.theme_mode.as_str() {
            "light" => adw::ColorScheme::ForceLight,
            "auto" => adw::ColorScheme::PreferDark,
            _ => adw::ColorScheme::ForceDark,
        }
    }

    fn glass(&self) -> bool {
        self.appearance.transparency.unwrap_or(true)
    }
}

fn desktop_path() -> std::path::PathBuf {
    crate::paths::config_dir().join("desktop.toml")
}

fn desktop() -> Desktop {
    std::fs::read_to_string(desktop_path())
        .map(|t| Desktop::parse(&t))
        .unwrap_or_default()
}

fn is_hex(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

thread_local! {
    /// The accent and light-mode provider, replaced (never stacked) on every
    /// change to the desktop's appearance.
    static OVERRIDES: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
    /// Kept alive for as long as the app runs; dropping it stops the watch.
    static DESKTOP_MONITOR: RefCell<Option<gio::FileMonitor>> = const { RefCell::new(None) };
}

/// How long `desktop.toml` has to stay quiet before it is read again: one
/// save from Settings arrives as a burst of events.
const DESKTOP_SETTLE: Duration = Duration::from_millis(150);

/// Load the stylesheet and the person's accent and light/dark choice, and
/// follow `desktop.toml` from here on. Returns whether the window should be
/// glass.
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
    apply(&desk);
    watch_desktop();
    desk.glass()
}

/// Light or dark, the accent, and glass on the open windows.
fn apply(desk: &Desktop) {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    adw::StyleManager::default().set_color_scheme(desk.color_scheme());
    let accent = desk.accent();
    let css = format!(
        "@define-color accent_bg_color {accent};\n@define-color accent_color {accent};\n{}",
        if desk.light() { LIGHT_CSS } else { "" }
    );
    OVERRIDES.with(|slot| {
        if let Some(old) = slot.borrow_mut().take() {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&css);
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        *slot.borrow_mut() = Some(provider);
    });

    let glass = desk.glass();
    let toplevels = gtk::Window::toplevels();
    for i in 0..toplevels.n_items() {
        if let Some(window) = toplevels.item(i).and_downcast::<gtk::Window>() {
            if window.has_css_class("raven") && window.transient_for().is_none() {
                if glass {
                    window.add_css_class("glass");
                } else {
                    window.remove_css_class("glass");
                }
            }
        }
    }
}

/// Follow Settings: re-read `desktop.toml` whenever it changes. The directory
/// is watched, not the file, because Settings replaces the file by rename and
/// it may not exist yet.
fn watch_desktop() {
    let path = desktop_path();
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let name = name.to_os_string();
    let Ok(monitor) = gio::File::for_path(dir)
        .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
    else {
        return;
    };
    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    monitor.connect_changed(move |_, file, other, event| {
        if matches!(
            event,
            gio::FileMonitorEvent::AttributeChanged
                | gio::FileMonitorEvent::PreUnmount
                | gio::FileMonitorEvent::Unmounted
        ) {
            return;
        }
        let names_desktop = |f: Option<&gio::File>| {
            f.and_then(|f| f.basename())
                .is_some_and(|b| b.as_os_str() == name.as_os_str())
        };
        if !names_desktop(Some(file)) && !names_desktop(other) {
            return;
        }
        if let Some(id) = pending.borrow_mut().take() {
            id.remove();
        }
        let fired = pending.clone();
        let id = glib::timeout_add_local_once(DESKTOP_SETTLE, move || {
            fired.borrow_mut().take();
            apply(&desktop());
        });
        *pending.borrow_mut() = Some(id);
    });
    DESKTOP_MONITOR.with(|m| *m.borrow_mut() = Some(monitor));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_is_dark_and_bad_values_fall_back() {
        let d = Desktop::parse("[appearance]\ntheme_mode = \"auto\"\naccent = \"red\"\n");
        assert!(!d.light());
        assert_eq!(d.color_scheme(), adw::ColorScheme::PreferDark);
        assert_eq!(d.accent(), DEFAULT_ACCENT);
        assert!(d.glass());

        let d = Desktop::parse(
            "[appearance]\ntheme_mode = \"light\"\naccent = \"#F7768E\"\ntransparency = false\n",
        );
        assert!(d.light());
        assert_eq!(d.accent(), "#F7768E");
        assert!(!d.glass());

        let d = Desktop::parse("not toml at all");
        assert_eq!(d.color_scheme(), adw::ColorScheme::ForceDark);
    }
}

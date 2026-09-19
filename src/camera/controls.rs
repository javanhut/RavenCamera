//! The picture controls a camera offers — brightness, contrast, white
//! balance, exposure, focus, anti-flicker — read from the camera itself, so
//! the Camera page shows exactly the knobs this camera has and no others.
//!
//! Controls are set on a node opened just for that: V4L2 allows it while
//! another handle streams, and the change shows in the preview at once.
//!
//! A camera forgets its controls when it is unplugged, so the ones changed by
//! hand are kept in the settings and put back by [`restore`] whenever it
//! starts, along with the few this app sets for everyone ([`recommended`]).

use std::collections::BTreeMap;
use std::path::Path;

use super::v4l2::{self, Node};

#[derive(Debug, Clone)]
pub enum Kind {
    Slider { min: i32, max: i32, step: i32 },
    Toggle,
    Menu(Vec<(u32, String)>),
}

#[derive(Debug, Clone)]
pub struct Control {
    pub id: u32,
    pub name: String,
    pub kind: Kind,
    pub value: i32,
    pub default: i32,
    /// Greyed out because another control governs it, like manual white
    /// balance while automatic white balance is on.
    pub inactive: bool,
    /// What it is for, for the few whose driver name does not say.
    pub hint: Option<&'static str>,
}

pub const POWER_LINE_FREQUENCY: u32 = 0x0098_0918;
pub const BACKLIGHT_COMPENSATION: u32 = 0x0098_091c;
pub const EXPOSURE_AUTO_PRIORITY: u32 = 0x009a_0903;

/// `V4L2_CID_POWER_LINE_FREQUENCY`'s menu.
const MAINS_50HZ: i32 = 1;
const MAINS_60HZ: i32 = 2;
const MAINS_AUTO: i32 = 3;

/// The controls that change a webcam's picture most and are least likely to
/// be found under their driver names, first in the list, in words.
const FIRST: [(u32, &str, &str); 3] = [
    (
        POWER_LINE_FREQUENCY,
        "Anti-flicker",
        "Stops the picture flickering or banding under indoor lights.",
    ),
    (
        EXPOSURE_AUTO_PRIORITY,
        "Brighter in low light",
        "Lets the camera slow down in dim light for a brighter, cleaner picture.",
    ),
    (
        BACKLIGHT_COMPENSATION,
        "Backlight compensation",
        "Brightens a face in front of a window or a bright lamp.",
    ),
];

/// How a control is remembered in the settings.
pub fn key(id: u32) -> String {
    format!("{id:#x}")
}

/// The "user" and "camera" control classes. Anything else a driver exposes
/// (codec, flash, private) is not a picture control.
fn wanted(id: u32) -> bool {
    matches!(id >> 16, 0x0098 | 0x009a)
}

/// The controls the camera at `path` has now.
pub fn read(path: &Path) -> Vec<Control> {
    let Ok(node) = Node::open(path) else {
        return Vec::new();
    };
    let mut list: Vec<Control> = node
        .controls()
        .into_iter()
        .filter(|(q, _)| wanted(q.id))
        .filter_map(|(q, menu)| {
            let kind = match q.kind {
                v4l2::CTRL_TYPE_INTEGER if q.maximum > q.minimum => Kind::Slider {
                    min: q.minimum,
                    max: q.maximum,
                    step: q.step.max(1),
                },
                v4l2::CTRL_TYPE_BOOLEAN => Kind::Toggle,
                v4l2::CTRL_TYPE_MENU if menu.len() > 1 => Kind::Menu(menu),
                _ => return None,
            };
            let first = FIRST.iter().find(|(id, _, _)| *id == q.id);
            Some(Control {
                id: q.id,
                name: first.map_or_else(|| tidy(&v4l2::cstr(&q.name)), |f| f.1.to_owned()),
                kind,
                value: node.control(q.id).unwrap_or(q.default_value),
                default: q.default_value,
                inactive: q.flags & v4l2::CTRL_FLAG_INACTIVE != 0,
                hint: first.map(|f| f.2),
            })
        })
        .collect();
    // Stable: the rest keep the driver's order.
    list.sort_by_key(|c| {
        FIRST
            .iter()
            .position(|(id, _, _)| *id == c.id)
            .unwrap_or(FIRST.len())
    });
    list
}

pub fn get(path: &Path, id: u32) -> std::io::Result<i32> {
    Node::open(path)?.control(id)
}

pub fn set(path: &Path, id: u32, value: i32) -> std::io::Result<()> {
    Node::open(path)?.set_control(id, value)
}

/// What this app sets on every camera unless the person chose otherwise:
/// anti-flicker matched to the mains, since a camera set for 60 Hz under
/// 50 Hz lights (or the reverse, or off) shows rolling dark bands.
pub fn recommended(controls: &[Control]) -> Vec<(u32, i32)> {
    let mut out = Vec::new();
    if let Some(c) = controls.iter().find(|c| c.id == POWER_LINE_FREQUENCY) {
        if let Kind::Menu(items) = &c.kind {
            let has = |v: i32| items.iter().any(|(i, _)| *i as i32 == v);
            let want = if has(MAINS_AUTO) {
                Some(MAINS_AUTO)
            } else {
                match mains_hz(&time_zone()) {
                    Some(50) if has(MAINS_50HZ) => Some(MAINS_50HZ),
                    Some(60) if has(MAINS_60HZ) => Some(MAINS_60HZ),
                    _ => None,
                }
            };
            if let Some(v) = want {
                out.push((c.id, v));
            }
        }
    }
    out
}

/// Put back the controls in `saved` (keyed by [`key`]) and the
/// [`recommended`] ones not in it. Automatic modes and menus go first: a
/// manual exposure or white balance is refused while its automatic partner
/// is on.
pub fn restore(path: &Path, saved: Option<&BTreeMap<String, i32>>) {
    let controls = read(path);
    let mut wanted: Vec<(u32, i32)> = recommended(&controls)
        .into_iter()
        .filter(|(id, _)| saved.is_none_or(|s| !s.contains_key(&key(*id))))
        .collect();
    if let Some(saved) = saved {
        wanted.extend(
            controls
                .iter()
                .filter_map(|c| saved.get(&key(c.id)).map(|v| (c.id, *v))),
        );
    }
    let is_slider = |id: u32| {
        controls
            .iter()
            .any(|c| c.id == id && matches!(c.kind, Kind::Slider { .. }))
    };
    wanted.sort_by_key(|(id, _)| is_slider(*id));
    let Ok(node) = Node::open(path) else {
        return;
    };
    for (id, value) in wanted {
        let current = controls.iter().find(|c| c.id == id).map(|c| c.value);
        if current != Some(value) {
            if let Err(e) = node.set_control(id, value) {
                tracing::debug!("restoring control {id:#x}: {e}");
            }
        }
    }
}

/// The system's time zone name, like `Europe/Berlin`.
fn time_zone() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        return tz.trim_start_matches(':').to_owned();
    }
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|p| {
            let p = p.to_string_lossy().into_owned();
            p.split_once("zoneinfo/").map(|(_, z)| z.to_owned())
        })
        .or_else(|| {
            std::fs::read_to_string("/etc/timezone")
                .ok()
                .map(|s| s.trim().to_owned())
        })
        .unwrap_or_default()
}

/// The mains frequency where `zone` is, or `None` where it is mixed (Japan)
/// or unknown. The Americas are 60 Hz but for the south of South America and
/// the French territories; a handful of Asian countries are 60 Hz; the rest
/// of the world is 50.
fn mains_hz(zone: &str) -> Option<u32> {
    const AMERICAS_50: [&str; 9] = [
        "Argentina",
        "Buenos_Aires",
        "Santiago",
        "Punta_Arenas",
        "Montevideo",
        "Asuncion",
        "La_Paz",
        "Cayenne",
        "Nuuk",
    ];
    const ASIA_60: [&str; 5] = ["Seoul", "Taipei", "Manila", "Riyadh", "Guam"];
    let zone = zone.trim();
    if zone.is_empty() || zone == "UTC" || zone.starts_with("Etc/") {
        return None;
    }
    if zone == "Asia/Tokyo" || zone == "Japan" {
        return None;
    }
    let american = ["America/", "US/", "Canada/", "Brazil/", "Mexico/"]
        .iter()
        .any(|p| zone.starts_with(p))
        || zone == "Pacific/Honolulu";
    if american {
        let fifty = AMERICAS_50.iter().any(|z| zone.contains(z))
            || zone.ends_with("Martinique")
            || zone.ends_with("Guadeloupe")
            || zone.ends_with("Godthab");
        return Some(if fifty { 50 } else { 60 });
    }
    if ASIA_60.iter().any(|z| zone.ends_with(z)) || zone == "ROK" || zone == "ROC" {
        return Some(60);
    }
    Some(50)
}

/// Driver names are written for engineers — "White Balance Temperature,
/// Auto", "Exposure, Dynamic Framerate" — so the commas are turned into
/// ordinary words.
fn tidy(name: &str) -> String {
    let name = name.trim();
    match name.split_once(", ") {
        Some((what, "Auto")) => format!("Automatic {}", lower_first(what)),
        Some((what, how)) => format!("{what} ({})", lower_first(how)),
        None => name.to_owned(),
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_names_read_as_words() {
        assert_eq!(
            tidy("White Balance Temperature, Auto"),
            "Automatic white Balance Temperature"
        );
        assert_eq!(
            tidy("Exposure, Dynamic Framerate"),
            "Exposure (dynamic Framerate)"
        );
        assert_eq!(tidy("Brightness"), "Brightness");
    }

    #[test]
    fn mains_frequency_by_time_zone() {
        assert_eq!(mains_hz("America/New_York"), Some(60));
        assert_eq!(mains_hz("America/Sao_Paulo"), Some(60));
        assert_eq!(mains_hz("America/Argentina/Buenos_Aires"), Some(50));
        assert_eq!(mains_hz("America/Santiago"), Some(50));
        assert_eq!(mains_hz("Europe/Berlin"), Some(50));
        assert_eq!(mains_hz("Asia/Seoul"), Some(60));
        assert_eq!(mains_hz("Asia/Kolkata"), Some(50));
        assert_eq!(mains_hz("Asia/Tokyo"), None);
        assert_eq!(mains_hz("UTC"), None);
        assert_eq!(mains_hz(""), None);
    }

    #[test]
    fn anti_flicker_prefers_the_cameras_own_auto() {
        let menu = |items: &[u32]| Control {
            id: POWER_LINE_FREQUENCY,
            name: "Anti-flicker".into(),
            kind: Kind::Menu(items.iter().map(|&i| (i, i.to_string())).collect()),
            value: 0,
            default: 0,
            inactive: false,
            hint: None,
        };
        assert_eq!(
            recommended(&[menu(&[0, 1, 2, 3])]),
            vec![(POWER_LINE_FREQUENCY, MAINS_AUTO)]
        );
        assert!(recommended(&[]).is_empty());
        assert_eq!(key(0x0098_0900), "0x980900");
    }

    #[test]
    fn only_picture_controls() {
        assert!(wanted(0x0098_0900)); // brightness
        assert!(wanted(0x009a_0902)); // exposure
        assert!(!wanted(0x0099_0000)); // codec class
    }
}

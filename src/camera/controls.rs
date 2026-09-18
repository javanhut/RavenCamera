//! The picture controls a camera offers — brightness, contrast, white
//! balance, exposure, focus, anti-flicker — read from the camera itself, so
//! the Camera page shows exactly the knobs this camera has and no others.
//!
//! Controls are set on a node opened just for that: V4L2 allows it while
//! another handle streams, and the change shows in the preview at once.

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
    node.controls()
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
            Some(Control {
                id: q.id,
                name: tidy(&v4l2::cstr(&q.name)),
                kind,
                value: node.control(q.id).unwrap_or(q.default_value),
                default: q.default_value,
                inactive: q.flags & v4l2::CTRL_FLAG_INACTIVE != 0,
            })
        })
        .collect()
}

pub fn set(path: &Path, id: u32, value: i32) -> std::io::Result<()> {
    Node::open(path)?.set_control(id, value)
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
    fn only_picture_controls() {
        assert!(wanted(0x0098_0900)); // brightness
        assert!(wanted(0x009a_0902)); // exposure
        assert!(!wanted(0x0099_0000)); // codec class
    }
}

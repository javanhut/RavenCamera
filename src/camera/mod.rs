//! Cameras: finding them, describing them, and streaming from them.
//!
//! Every camera Raven's kernel drives through `uvcvideo` — the one in the
//! lid, a USB webcam, a capture card, a document camera — is a V4L2 node
//! with the same interface, so there is one code path for all of them and
//! the only difference shown is the label: Built-in or USB.
//!
//! A camera exposes more than one node (the second is usually metadata), so
//! a node counts as a camera only if it captures video and offers a pixel
//! format this app can read.

pub mod controls;
pub mod stream;
pub mod v4l2;

use std::path::{Path, PathBuf};

pub use stream::{Frame, Stream};

/// One way a camera can stream: a pixel format at a size, at its fastest
/// frame rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mode {
    pub format: u32,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

impl Mode {
    pub fn label(&self) -> String {
        let name = match self.width.min(self.height) {
            2160 => " (4K)",
            1440 => " (1440p)",
            1080 => " (1080p)",
            720 => " (720p)",
            480 => " (480p)",
            _ => "",
        };
        format!("{}×{}{name} · {:.0} fps", self.width, self.height, self.fps)
    }

    pub fn resolution_key(&self) -> String {
        format!("{}x{}", self.width, self.height)
    }
}

/// A camera.
#[derive(Debug, Clone)]
pub struct Device {
    pub path: PathBuf,
    /// What the camera calls itself.
    pub name: String,
    /// Where it is attached, e.g. `usb-0000:00:15.0-7`.
    pub bus: String,
    pub driver: String,
    /// Plugged in rather than built in, from the USB port's `removable`
    /// attribute; a camera the firmware does not describe counts as external.
    pub external: bool,
    /// Every resolution offered, largest first, each in its best format.
    pub modes: Vec<Mode>,
}

impl Device {
    /// What the settings remember a camera by: its name, not its node or
    /// port, so a USB camera replugged into another port — and given another
    /// `/dev/videoN` — is still the camera that was chosen.
    pub fn key(&self) -> String {
        self.name.clone()
    }

    pub fn kind_label(&self) -> &'static str {
        if self.external {
            "USB"
        } else {
            "Built-in"
        }
    }

    /// The mode for `resolution` (`WxH`) if the camera has it, otherwise the
    /// best one: the largest that still runs at a video frame rate.
    pub fn mode_for(&self, resolution: &str) -> Option<Mode> {
        if let Some(m) = self.modes.iter().find(|m| m.resolution_key() == resolution) {
            return Some(*m);
        }
        self.best_mode()
    }

    pub fn best_mode(&self) -> Option<Mode> {
        self.modes
            .iter()
            .find(|m| m.fps >= 24.0 && m.width.min(m.height) <= 1080)
            .or_else(|| self.modes.iter().find(|m| m.fps >= 24.0))
            .or_else(|| self.modes.first())
            .copied()
    }
}

/// Pixel formats this app reads, in order of preference at equal frame
/// rates: JPEG frames are what a recording stores anyway, so they cost
/// nothing to keep.
const READABLE: [u32; 4] = [
    v4l2::PIX_MJPEG,
    v4l2::PIX_JPEG,
    v4l2::PIX_YUYV,
    v4l2::PIX_NV12,
];

/// Every camera connected now, built-in cameras first.
pub fn devices() -> Vec<Device> {
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev")
        .map(|it| {
            it.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("video") && n[5..].parse::<u32>().is_ok())
                })
                .collect()
        })
        .unwrap_or_default();
    nodes.sort_by_key(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n[5..].parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    });
    let mut out: Vec<Device> = nodes.iter().filter_map(|p| probe(p)).collect();
    out.sort_by_key(|d| d.external);
    out
}

/// Describe the node at `path`, or `None` if it is not a camera this app can
/// use (a metadata node, an output device, a codec, one we cannot open).
pub fn probe(path: &Path) -> Option<Device> {
    let node = v4l2::Node::open(path).ok()?;
    let cap = node.query_cap().ok()?;
    let caps = if cap.capabilities & v4l2::CAP_DEVICE_CAPS != 0 {
        cap.device_caps
    } else {
        cap.capabilities
    };
    if caps & v4l2::CAP_VIDEO_CAPTURE == 0 || caps & v4l2::CAP_STREAMING == 0 {
        return None;
    }
    let formats: Vec<u32> = node
        .formats()
        .into_iter()
        .map(|(f, _)| f)
        .filter(|f| READABLE.contains(f))
        .collect();
    if formats.is_empty() {
        return None;
    }
    let mut modes: Vec<Mode> = Vec::new();
    for &format in &formats {
        for (width, height) in node.frame_sizes(format) {
            let fps = node
                .frame_rates(format, width, height)
                .first()
                .copied()
                .unwrap_or(30.0);
            let candidate = Mode {
                format,
                width,
                height,
                fps,
            };
            match modes
                .iter_mut()
                .find(|m| m.width == width && m.height == height)
            {
                Some(existing) => {
                    if better(&candidate, existing) {
                        *existing = candidate;
                    }
                }
                None => modes.push(candidate),
            }
        }
    }
    if modes.is_empty() {
        return None;
    }
    modes.sort_by(|a, b| {
        (b.width * b.height)
            .cmp(&(a.width * a.height))
            .then(b.fps.total_cmp(&a.fps))
    });
    Some(Device {
        path: path.to_path_buf(),
        name: v4l2::cstr(&cap.card).trim().to_owned(),
        bus: v4l2::cstr(&cap.bus_info),
        driver: v4l2::cstr(&cap.driver),
        external: is_external(path),
        modes,
    })
}

/// Whether `a` is a better way than `b` to stream the same size: faster, or
/// as fast and earlier in [`READABLE`].
fn better(a: &Mode, b: &Mode) -> bool {
    let rank = |m: &Mode| READABLE.iter().position(|&f| f == m.format).unwrap_or(99);
    if (a.fps - b.fps).abs() > 0.5 {
        a.fps > b.fps
    } else {
        rank(a) < rank(b)
    }
}

/// Built in or plugged in. The USB port a camera hangs off says `fixed` when
/// the firmware describes it as part of the machine.
fn is_external(path: &Path) -> bool {
    let Some(name) = path.file_name() else {
        return true;
    };
    let dev = Path::new("/sys/class/video4linux")
        .join(name)
        .join("device");
    // The interface's parent is the USB device, which carries `removable`.
    for dir in [dev.join(".."), dev.clone()] {
        if let Ok(v) = std::fs::read_to_string(dir.join("removable")) {
            return v.trim() != "fixed";
        }
    }
    // No `removable` anywhere: a USB camera the firmware says nothing about
    // is taken to be plugged in, and one not on USB at all (a platform
    // camera) to be part of the machine.
    std::fs::canonicalize(&dev)
        .map(|p| p.to_string_lossy().contains("/usb"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(format: u32, width: u32, height: u32, fps: f64) -> Mode {
        Mode {
            format,
            width,
            height,
            fps,
        }
    }

    #[test]
    fn faster_wins_and_jpeg_breaks_ties() {
        let yuyv30 = mode(v4l2::PIX_YUYV, 640, 480, 30.0);
        let mjpg30 = mode(v4l2::PIX_MJPEG, 640, 480, 30.0);
        let yuyv10 = mode(v4l2::PIX_YUYV, 1280, 720, 10.0);
        let mjpg30hd = mode(v4l2::PIX_MJPEG, 1280, 720, 30.0);
        assert!(better(&mjpg30, &yuyv30));
        assert!(!better(&yuyv30, &mjpg30));
        assert!(better(&mjpg30hd, &yuyv10));
    }

    #[test]
    fn the_best_mode_is_the_largest_at_video_rate() {
        let dev = Device {
            path: "/dev/video0".into(),
            name: "Cam".into(),
            bus: String::new(),
            driver: "uvcvideo".into(),
            external: true,
            modes: vec![
                mode(v4l2::PIX_YUYV, 2592, 1944, 2.0),
                mode(v4l2::PIX_MJPEG, 1920, 1080, 30.0),
                mode(v4l2::PIX_MJPEG, 1280, 720, 30.0),
            ],
        };
        assert_eq!(dev.best_mode().unwrap().width, 1920);
        assert_eq!(dev.mode_for("1280x720").unwrap().width, 1280);
        assert_eq!(dev.mode_for("999x1").unwrap().width, 1920);
    }
}

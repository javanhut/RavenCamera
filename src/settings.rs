//! `~/.config/raven/camera.toml`: what the person chose.
//!
//! The house convention: TOML, every key optional, a file that does not parse
//! is logged and replaced by the defaults rather than stopping the app. A
//! setting added in a later version reads as its default from an older file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    /// Near-transparent; large files.
    High,
    #[default]
    Standard,
    /// For sharing where size matters more than detail.
    Compact,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::High, Quality::Standard, Quality::Compact];

    /// The H.264 quantiser. Raven's encoder codes every macroblock at one
    /// quantiser, so this is the quality knob, and bitrate follows content.
    pub fn qp(self) -> u8 {
        match self {
            Quality::High => 20,
            Quality::Standard => 24,
            Quality::Compact => 29,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Quality::High => "High (H.264)",
            Quality::Standard => "Standard (H.264)",
            Quality::Compact => "Compact (H.264)",
        }
    }

    /// AAC bitrate, per stereo pair.
    pub fn audio_bitrate(self) -> u32 {
        match self {
            Quality::High => 192_000,
            Quality::Standard => 160_000,
            Quality::Compact => 96_000,
        }
    }
}

/// The largest the finished video may be. Recordings are captured at full
/// size either way; this is applied when they are finished, and a smaller
/// video also finishes sooner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OutputSize {
    #[default]
    Original,
    #[serde(rename = "1080p")]
    P1080,
    #[serde(rename = "720p")]
    P720,
}

impl OutputSize {
    pub const ALL: [OutputSize; 3] = [OutputSize::Original, OutputSize::P1080, OutputSize::P720];

    pub fn label(self) -> &'static str {
        match self {
            OutputSize::Original => "Original size",
            OutputSize::P1080 => "At most 1080p",
            OutputSize::P720 => "At most 720p",
        }
    }

    /// The longest the shorter side may be, if there is a limit.
    pub fn limit(self) -> Option<u32> {
        match self {
            OutputSize::Original => None,
            OutputSize::P1080 => Some(1080),
            OutputSize::P720 => Some(720),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PostAction {
    /// Select it on the Media page.
    #[default]
    ShowInMedia,
    /// Open it in the default application for its type.
    OpenFile,
    /// Open the folder it was saved in.
    OpenFolder,
    Nothing,
}

impl PostAction {
    pub const ALL: [PostAction; 4] = [
        PostAction::ShowInMedia,
        PostAction::OpenFile,
        PostAction::OpenFolder,
        PostAction::Nothing,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PostAction::ShowInMedia => "Show in Media",
            PostAction::OpenFile => "Open in Player",
            PostAction::OpenFolder => "Open Folder",
            PostAction::Nothing => "Do Nothing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
}

impl Corner {
    pub const ALL: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomLeft,
        Corner::BottomRight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Corner::TopLeft => "Top left",
            Corner::TopRight => "Top right",
            Corner::BottomLeft => "Bottom left",
            Corner::BottomRight => "Bottom right",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    #[default]
    Png,
    Jpeg,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Recording {
    pub fps: u32,
    pub quality: Quality,
    pub output_size: OutputSize,
    /// Record what the computer plays.
    pub system_audio: bool,
    /// PipeWire node name of the output to record; empty for the default.
    pub system_audio_device: String,
    pub microphone: bool,
    /// PipeWire node name of the input; empty for the default.
    pub microphone_device: String,
    pub show_cursor: bool,
    pub highlight_clicks: bool,
    pub countdown: bool,
    pub countdown_seconds: u32,
    /// A camera picture-in-picture over screen recordings.
    pub webcam_overlay: bool,
    pub overlay_corner: Corner,
    /// Width of the overlay as a percentage of the video's width.
    pub overlay_percent: u32,
    /// Minimise Raven Camera when a screen recording starts, so it is not
    /// in the recording.
    pub hide_window: bool,
}

impl Default for Recording {
    fn default() -> Self {
        Self {
            fps: 30,
            quality: Quality::Standard,
            output_size: OutputSize::Original,
            system_audio: true,
            system_audio_device: String::new(),
            microphone: false,
            microphone_device: String::new(),
            show_cursor: true,
            highlight_clicks: true,
            countdown: true,
            countdown_seconds: 3,
            webcam_overlay: false,
            overlay_corner: Corner::BottomRight,
            overlay_percent: 22,
            hide_window: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Camera {
    /// The camera last used, by its stable identity (see
    /// `camera::Device::key`), so a USB camera plugged into another port is
    /// still recognised.
    pub device: String,
    /// `WIDTHxHEIGHT`, or empty for the camera's best mode.
    pub resolution: String,
    /// Show the preview as a mirror, the way a person expects to see
    /// themselves.
    pub mirror_preview: bool,
    /// Save photos and videos mirrored too.
    pub mirror_saved: bool,
    pub photo_format: ImageFormat,
    pub grid: bool,
    /// Seconds between pressing the shutter and the photo.
    pub photo_timer: u32,
    /// Levels, colour and sharpness finished in software, the way a phone
    /// finishes its camera's pictures.
    pub enhance: bool,
    /// Take photos in the camera's largest mode, which is usually far larger
    /// than the preview's, at the cost of a second's pause.
    pub full_resolution_photos: bool,
    /// Picture controls changed by hand, by camera (its key) and control id
    /// (`0x…`), put back whenever that camera starts — the camera itself
    /// forgets them when it is unplugged.
    pub controls: BTreeMap<String, BTreeMap<String, i32>>,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            device: String::new(),
            resolution: String::new(),
            mirror_preview: true,
            mirror_saved: false,
            photo_format: ImageFormat::Jpeg,
            grid: false,
            photo_timer: 0,
            enhance: true,
            full_resolution_photos: true,
            controls: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Screenshot {
    pub delay_seconds: u32,
    pub format: ImageFormat,
    pub show_cursor: bool,
    pub copy_to_clipboard: bool,
}

impl Default for Screenshot {
    fn default() -> Self {
        Self {
            delay_seconds: 0,
            format: ImageFormat::Png,
            show_cursor: false,
            copy_to_clipboard: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Where videos and audio go; empty for `~/Videos/Raven Camera`.
    pub video_dir: String,
    /// Where photos and screenshots go; empty for `~/Pictures/Raven Camera`.
    pub picture_dir: String,
    pub name_prefix: String,
    pub name_pattern: String,
    pub post_action: PostAction,
    pub recording: Recording,
    pub camera: Camera,
    pub screenshot: Screenshot,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            video_dir: String::new(),
            picture_dir: String::new(),
            name_prefix: "Raven Camera".into(),
            name_pattern: crate::naming::PATTERNS[0].into(),
            post_action: PostAction::ShowInMedia,
            recording: Recording::default(),
            camera: Camera::default(),
            screenshot: Screenshot::default(),
        }
    }
}

pub fn path() -> PathBuf {
    paths::config_dir().join("camera.toml")
}

impl Settings {
    pub fn load() -> Self {
        Self::load_from(&path())
    }

    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(settings) => settings,
                Err(e) => {
                    tracing::warn!("{}: {e}; using defaults", path.display());
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&path())
    }

    /// Written to a temporary file and renamed over the old one, so a crash
    /// mid-write leaves the previous settings rather than half of new ones.
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn video_dir(&self) -> PathBuf {
        if self.video_dir.trim().is_empty() {
            paths::videos_dir().join("Raven Camera")
        } else {
            PathBuf::from(self.video_dir.trim())
        }
    }

    pub fn picture_dir(&self) -> PathBuf {
        if self.picture_dir.trim().is_empty() {
            paths::pictures_dir().join("Raven Camera")
        } else {
            PathBuf::from(self.picture_dir.trim())
        }
    }

    /// The stem for a capture taken now.
    pub fn stem_now(&self) -> String {
        crate::naming::stem(
            &self.name_prefix,
            &self.name_pattern,
            chrono::Local::now().naive_local(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let dir =
            std::env::temp_dir().join(format!("raven-camera-settings-{}", std::process::id()));
        let file = dir.join("camera.toml");
        let mut s = Settings::default();
        s.recording.fps = 60;
        s.recording.output_size = OutputSize::P720;
        s.camera.resolution = "1280x720".into();
        s.camera
            .controls
            .entry("HD Webcam: C270".into())
            .or_default()
            .insert("0x980900".into(), 140);
        s.save_to(&file).unwrap();
        let back = Settings::load_from(&file);
        assert_eq!(back.recording.fps, 60);
        assert_eq!(back.recording.output_size, OutputSize::P720);
        assert_eq!(back.camera.resolution, "1280x720");
        assert_eq!(back.camera.controls["HD Webcam: C270"]["0x980900"], 140);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_keys_are_defaults_and_garbage_is_not_fatal() {
        let s: Settings = toml::from_str("[recording]\nfps = 24\n").unwrap();
        assert_eq!(s.recording.fps, 24);
        assert!(s.recording.show_cursor);
        assert_eq!(s.name_prefix, "Raven Camera");
        assert!(s.camera.enhance && s.camera.full_resolution_photos);
        let dir = std::env::temp_dir().join(format!("raven-camera-garbage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("camera.toml");
        std::fs::write(&file, "this is = = not toml").unwrap();
        assert_eq!(Settings::load_from(&file).recording.fps, 30);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

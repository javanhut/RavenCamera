//! A recording on disk before it is a video: a folder under
//! `~/.local/share/raven-camera/sessions` holding what was captured and a
//! manifest saying what it is and what it should become.
//!
//! The manifest is written when recording starts and again when it stops.
//! One still saying `recording` when the app starts is a recording a crash
//! or a power cut interrupted; everything captured up to that moment is in
//! the folder, and it can be finished like any other.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings::Corner;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Screen,
    Window,
    Region,
    Camera,
    Audio,
}

impl Kind {
    pub fn title(self) -> &'static str {
        match self {
            Kind::Screen => "Screen Recording",
            Kind::Window => "Window Recording",
            Kind::Region => "Region Recording",
            Kind::Camera => "Camera Recording",
            Kind::Audio => "Audio Recording",
        }
    }

    pub fn is_video(self) -> bool {
        self != Kind::Audio
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Being recorded, or interrupted while it was.
    Recording,
    /// Stopped and waiting to be finished.
    Ready,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Overlay {
    pub corner: Corner,
    pub percent: u32,
    pub mirror: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioTrack {
    /// File name in the session folder.
    pub file: String,
    pub offset_us: u64,
    pub frames: u64,
    pub gain: f32,
    /// "system" or "microphone", for the record.
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub kind: Kind,
    pub state: State,
    /// Where the finished file goes.
    pub output: PathBuf,
    /// Local time it was started, RFC 3339.
    pub created: String,
    /// H.264 quantiser.
    pub qp: u8,
    /// Shorter side of the finished video at most this.
    pub output_limit: Option<u32>,
    pub audio_bitrate: u32,
    /// Seconds between key frames in the finished video, where players can
    /// seek; also how finishing is split between threads.
    pub keyframe_seconds: f64,
    /// The screen recording (`.rvr`), if there is one.
    pub screen: Option<String>,
    /// The camera recording (`.rcam`), if there is one.
    pub camera: Option<String>,
    /// For a camera recording: flip it left to right.
    pub mirror_camera: bool,
    /// Finish the camera's picture (see `enhance::Enhancer`), in a camera
    /// recording or an overlay. Absent from recordings made before it was.
    #[serde(default)]
    pub enhance_camera: bool,
    /// For a screen recording with the camera over it.
    pub overlay: Option<Overlay>,
    pub audio: Vec<AudioTrack>,
    /// Length of the recording, paused time excluded. Zero until stopped.
    pub length_us: u64,
    /// Name of what was recorded, for the Media page: a window's title.
    pub subject: String,
}

pub const MANIFEST: &str = "manifest.toml";

impl Manifest {
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(dir.join(MANIFEST))?;
        Ok(toml::from_str(&text)?)
    }

    /// Written beside and renamed over, so a crash never leaves half a
    /// manifest.
    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        let tmp = dir.join("manifest.toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, dir.join(MANIFEST))?;
        Ok(())
    }
}

/// A new, empty session folder named for now.
pub fn create_dir() -> anyhow::Result<PathBuf> {
    let root = crate::paths::sessions_dir();
    std::fs::create_dir_all(&root)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let dir = (0..)
        .map(|n| {
            if n == 0 {
                root.join(&stamp)
            } else {
                root.join(format!("{stamp}-{n}"))
            }
        })
        .find(|p| !p.exists())
        .expect("an unbounded range finds a free name");
    std::fs::create_dir(&dir)?;
    Ok(dir)
}

/// Sessions left over from before: interrupted recordings and ones stopped
/// but never finished. Oldest first.
pub fn leftovers() -> Vec<(PathBuf, Manifest)> {
    let Ok(entries) = std::fs::read_dir(crate::paths::sessions_dir()) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, Manifest)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|p| Manifest::load(&p).ok().map(|m| (p, m)))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Bytes the session's captured files take: what finishing it frees.
pub fn size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|it| {
            it.filter_map(Result::ok)
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

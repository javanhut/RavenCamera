//! What has been captured: the files in the save folders, newest first,
//! with a thumbnail and a length for each.
//!
//! The folders are the truth — a video deleted in the file manager is gone
//! here too, and one copied in shows up. What the folder cannot say (that
//! this file was a window recording, how long it runs, what its first frame
//! looked like) is kept beside it in `library.toml` and the thumbnail
//! cache, written when this app makes the file.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::paths;
use crate::pixels::{self, Rgba};
use crate::settings::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Video,
    Audio,
    Photo,
    Screenshot,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Video => "Videos",
            Kind::Audio => "Audio",
            Kind::Photo => "Photos",
            Kind::Screenshot => "Screenshots",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Kind::Video => "video-x-generic-symbolic",
            Kind::Audio => "audio-x-generic-symbolic",
            Kind::Photo => "camera-photo-symbolic",
            Kind::Screenshot => "image-x-generic-symbolic",
        }
    }
}

/// One captured file.
#[derive(Debug, Clone)]
pub struct Item {
    pub path: PathBuf,
    pub kind: Kind,
    /// What it is, in words: "Screen Recording", "Photo".
    pub title: String,
    pub modified: SystemTime,
    pub length: Option<Duration>,
    pub bytes: u64,
}

impl Item {
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Entry {
    title: String,
    kind: Option<Kind>,
    length_ms: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    files: HashMap<String, Entry>,
}

fn index_path() -> PathBuf {
    paths::data_dir().join("library.toml")
}

fn load_index() -> Index {
    std::fs::read_to_string(index_path())
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_index(index: &Index) {
    let path = index_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("toml.tmp");
    if let Ok(text) = toml::to_string(index) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// Where the thumbnail for `path` is cached.
pub fn thumbnail_path(path: &Path) -> PathBuf {
    // FNV-1a over the path: stable, and no dependency for a cache key.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.as_os_str().as_encoded_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    paths::cache_dir()
        .join("thumbnails")
        .join(format!("{h:016x}.png"))
}

/// Remember a file this app just made.
pub fn remember(
    path: &Path,
    title: &str,
    kind: Kind,
    length: Option<Duration>,
    thumbnail: Option<&Rgba>,
) {
    let mut index = load_index();
    index.files.insert(
        path.to_string_lossy().into_owned(),
        Entry {
            title: title.to_owned(),
            kind: Some(kind),
            length_ms: length.map(|d| d.as_millis() as u64),
        },
    );
    save_index(&index);
    if let Some(thumb) = thumbnail {
        let _ = write_thumbnail(path, thumb);
    }
}

/// Forget a file (it was deleted or renamed away).
pub fn forget(path: &Path) {
    let mut index = load_index();
    if index.files.remove(&*path.to_string_lossy()).is_some() {
        save_index(&index);
    }
    let _ = std::fs::remove_file(thumbnail_path(path));
}

/// Carry what is known about `from` over to `to`.
pub fn renamed(from: &Path, to: &Path) {
    let mut index = load_index();
    if let Some(entry) = index.files.remove(&*from.to_string_lossy()) {
        index.files.insert(to.to_string_lossy().into_owned(), entry);
        save_index(&index);
    }
    let _ = std::fs::rename(thumbnail_path(from), thumbnail_path(to));
}

fn write_thumbnail(path: &Path, image: &Rgba) -> anyhow::Result<()> {
    let (w, h) = pixels::fit_size(image.width, image.height, 480, 270);
    let small = pixels::resize(image, w, h);
    let out = thumbnail_path(path);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(out, pixels::encode_png(&small)?)?;
    Ok(())
}

/// The thumbnail for `item`, made now if it can be (a picture) and not
/// cached yet. Videos made elsewhere have none: that would take a decoder,
/// and this app only has encoders.
pub fn thumbnail(item: &Item) -> Option<Rgba> {
    let cached = thumbnail_path(&item.path);
    let fresh = std::fs::metadata(&cached)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t >= item.modified);
    if fresh {
        if let Ok(img) = std::fs::read(&cached)
            .map_err(anyhow::Error::from)
            .and_then(|b| pixels::decode_png(&b))
        {
            return Some(img);
        }
    }
    if matches!(item.kind, Kind::Photo | Kind::Screenshot) {
        let bytes = std::fs::read(&item.path).ok()?;
        let img = match item
            .path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .as_deref()
        {
            Some("png") => pixels::decode_png(&bytes).ok()?,
            Some("jpg" | "jpeg") => pixels::decode_jpeg(&bytes).ok()?,
            _ => return None,
        };
        let _ = write_thumbnail(&item.path, &img);
        let (w, h) = pixels::fit_size(img.width, img.height, 480, 270);
        return Some(pixels::resize(&img, w, h));
    }
    if fresh {
        return None;
    }
    // A video this app made keeps its thumbnail even if the file is newer
    // (touched by a copy); try the cache regardless.
    std::fs::read(&cached)
        .ok()
        .and_then(|b| pixels::decode_png(&b).ok())
}

fn kind_for(path: &Path, pictures: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    match ext.as_str() {
        "mp4" | "mkv" | "webm" | "mov" => Some(Kind::Video),
        "m4a" | "wav" | "ogg" | "opus" | "mp3" | "flac" => Some(Kind::Audio),
        "png" | "jpg" | "jpeg" => Some(
            if path.starts_with(pictures) && path.to_string_lossy().contains("Screenshot") {
                Kind::Screenshot
            } else {
                Kind::Photo
            },
        ),
        _ => None,
    }
}

/// Everything in the save folders, newest first.
pub fn scan(settings: &Settings) -> Vec<Item> {
    let index = load_index();
    let pictures = settings.picture_dir();
    let mut dirs = vec![settings.video_dir(), pictures.clone()];
    dirs.dedup();
    let mut items = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let path = e.path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
            {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let entry = index.files.get(&*path.to_string_lossy()).cloned();
            let Some(kind) = entry
                .as_ref()
                .and_then(|e| e.kind)
                .or_else(|| kind_for(&path, &pictures))
            else {
                continue;
            };
            let length = entry
                .as_ref()
                .and_then(|e| e.length_ms)
                .map(Duration::from_millis)
                .or_else(|| match kind {
                    Kind::Video | Kind::Audio => mp4_duration(&path),
                    _ => None,
                });
            let title = entry
                .map(|e| e.title)
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| {
                    match kind {
                        Kind::Video => "Video",
                        Kind::Audio => "Audio",
                        Kind::Photo => "Photo",
                        Kind::Screenshot => "Screenshot",
                    }
                    .into()
                });
            items.push(Item {
                path,
                kind,
                title,
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                length,
                bytes: meta.len(),
            });
        }
    }
    items.sort_by_key(|i| std::cmp::Reverse(i.modified));
    items
}

/// An MP4's length from its `mvhd`, walking only the top-level boxes and
/// `moov`'s children — so a `moov` written at the end of a long file is
/// found without reading the video.
pub fn mp4_duration(path: &Path) -> Option<Duration> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut at = 0u64;
    while at + 8 <= len {
        f.seek(SeekFrom::Start(at)).ok()?;
        let (size, kind, header) = box_header(&mut f, len - at)?;
        if &kind == b"moov" {
            let mut child = at + header;
            while child + 8 <= at + size {
                f.seek(SeekFrom::Start(child)).ok()?;
                let (csize, ckind, cheader) = box_header(&mut f, at + size - child)?;
                if &ckind == b"mvhd" {
                    let mut body = [0u8; 32];
                    f.read_exact(&mut body[..]).ok()?;
                    let (timescale, duration) = if body[0] == 1 {
                        (
                            u32::from_be_bytes(body[20..24].try_into().ok()?),
                            u64::from_be_bytes(body[24..32].try_into().ok()?),
                        )
                    } else {
                        (
                            u32::from_be_bytes(body[12..16].try_into().ok()?),
                            u64::from(u32::from_be_bytes(body[16..20].try_into().ok()?)),
                        )
                    };
                    let _ = cheader;
                    return (timescale > 0)
                        .then(|| Duration::from_secs_f64(duration as f64 / f64::from(timescale)));
                }
                child += csize.max(8);
            }
            return None;
        }
        at += size.max(8);
    }
    None
}

/// `(size, type, header length)` of the box at the reader's position.
fn box_header(f: &mut std::fs::File, remaining: u64) -> Option<(u64, [u8; 4], u64)> {
    let mut h = [0u8; 8];
    f.read_exact(&mut h).ok()?;
    let kind: [u8; 4] = h[4..8].try_into().ok()?;
    let size32 = u32::from_be_bytes(h[0..4].try_into().ok()?);
    match size32 {
        0 => Some((remaining, kind, 8)),
        1 => {
            let mut big = [0u8; 8];
            f.read_exact(&mut big).ok()?;
            Some((u64::from_be_bytes(big), kind, 16))
        }
        n => Some((u64::from(n), kind, 8)),
    }
}

/// Save a still — a photo or a screenshot — and remember it.
pub fn save_still(
    image: &Rgba,
    settings: &Settings,
    kind: Kind,
    format: crate::settings::ImageFormat,
) -> anyhow::Result<PathBuf> {
    let dir = settings.picture_dir();
    std::fs::create_dir_all(&dir)?;
    let prefix = match kind {
        Kind::Screenshot => "Screenshot",
        _ => settings.name_prefix.as_str(),
    };
    let stem = crate::naming::stem(
        prefix,
        &settings.name_pattern,
        chrono::Local::now().naive_local(),
    );
    let path = crate::naming::unique_path(&dir, &stem, format.extension());
    let bytes = match format {
        crate::settings::ImageFormat::Png => pixels::encode_png(image)?,
        crate::settings::ImageFormat::Jpeg => pixels::encode_jpeg(image, 93)?,
    };
    // Written beside and renamed, so a watcher never sees half a picture.
    let tmp = dir.join(format!(
        ".{}.part",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)?;
    let title = match kind {
        Kind::Screenshot => "Screenshot",
        _ => "Photo",
    };
    remember(&path, title, kind, None, Some(image));
    Ok(path)
}

/// "1:05", "12:40", "1:02:03".
pub fn format_length(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// "3.4 MB".
pub fn format_bytes(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_read_like_a_player() {
        assert_eq!(format_length(Duration::from_secs(24)), "00:24");
        assert_eq!(format_length(Duration::from_secs(72)), "01:12");
        assert_eq!(format_length(Duration::from_secs(3723)), "1:02:03");
        assert_eq!(format_bytes(3_400_000), "3.4 MB");
    }

    #[test]
    fn mvhd_is_found_after_mdat() {
        // ftyp, a large mdat, then moov with an mvhd of 90000 ticks at 1000/s.
        let mut f = Vec::new();
        let boxed = |kind: &[u8; 4], body: &[u8]| {
            let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
            b.extend_from_slice(kind);
            b.extend_from_slice(body);
            b
        };
        f.extend(boxed(b"ftyp", b"isom\0\0\0\0"));
        f.extend(boxed(b"mdat", &[0u8; 1000]));
        let mut mvhd = vec![0u8; 100];
        mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
        mvhd[16..20].copy_from_slice(&90_000u32.to_be_bytes());
        f.extend(boxed(b"moov", &boxed(b"mvhd", &mvhd)));
        let path =
            std::env::temp_dir().join(format!("raven-camera-mvhd-{}.mp4", std::process::id()));
        std::fs::write(&path, f).unwrap();
        assert_eq!(mp4_duration(&path), Some(Duration::from_secs(90)));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn thumbnails_are_keyed_by_path() {
        assert_ne!(
            thumbnail_path(Path::new("/a.mp4")),
            thumbnail_path(Path::new("/b.mp4"))
        );
        assert_eq!(
            thumbnail_path(Path::new("/a.mp4")),
            thumbnail_path(Path::new("/a.mp4"))
        );
    }
}

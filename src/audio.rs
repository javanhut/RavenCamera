//! Sound: what the computer plays and what the microphone hears.
//!
//! Through PipeWire's own tools rather than a PipeWire client linked into
//! the app — the choice Huginn made for its mixer and Oracle for dictation.
//! `pw-dump` lists the devices; `pw-record` records one, as raw 48 kHz
//! stereo on its standard output. Recording what the computer plays is
//! `pw-record` on the output with `stream.capture.sink`, which PipeWire
//! answers with the output's monitor: every application's sound, mixed, as
//! it goes to the speakers. Neither takes the device from anything else.
//!
//! Each stream is written to a WAV file in the recording's session folder,
//! and mixed only when the recording is finished, so the two can be
//! balanced, and one that failed does not take the other with it.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Context, Result};

pub const RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
/// Bytes per stereo frame of 16-bit samples.
pub const FRAME_BYTES: usize = 4;

/// An audio device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// PipeWire node name; what `pw-record --target` takes.
    pub name: String,
    pub description: String,
    pub is_default: bool,
}

/// Outputs (whose sound can be recorded) and inputs (microphones).
#[derive(Debug, Clone, Default)]
pub struct Devices {
    pub outputs: Vec<Device>,
    pub inputs: Vec<Device>,
    /// `pw-record` is installed and PipeWire answered.
    pub available: bool,
}

/// What PipeWire has now. Blocks for as long as `pw-dump` takes, so call it
/// off the main thread.
pub fn devices() -> Devices {
    let Ok(out) = Command::new("pw-dump")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Devices::default();
    };
    if !out.status.success() {
        return Devices::default();
    }
    let mut devices = parse_dump(&String::from_utf8_lossy(&out.stdout));
    devices.available = which("pw-record");
    devices
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

/// Sinks and sources out of `pw-dump`'s JSON, and which are the defaults
/// (the `default` metadata object names them).
fn parse_dump(json: &str) -> Devices {
    let Ok(serde_json::Value::Array(objects)) = serde_json::from_str::<serde_json::Value>(json)
    else {
        return Devices::default();
    };
    let mut default_sink = String::new();
    let mut default_source = String::new();
    for o in &objects {
        let is_default_meta = o["type"].as_str().is_some_and(|t| t.ends_with("Metadata"))
            && o["props"]["metadata.name"].as_str() == Some("default");
        if !is_default_meta {
            continue;
        }
        for entry in o["metadata"].as_array().into_iter().flatten() {
            let name = entry["value"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            match entry["key"].as_str() {
                Some("default.audio.sink") => default_sink = name,
                Some("default.audio.source") => default_source = name,
                _ => {}
            }
        }
    }
    let mut devices = Devices::default();
    for o in &objects {
        let props = &o["info"]["props"];
        let (Some(class), Some(name)) =
            (props["media.class"].as_str(), props["node.name"].as_str())
        else {
            continue;
        };
        let description = props["node.description"]
            .as_str()
            .or_else(|| props["node.nick"].as_str())
            .unwrap_or(name)
            .to_owned();
        let device = |default: &str| Device {
            name: name.to_owned(),
            description: description.clone(),
            is_default: name == default,
        };
        match class {
            "Audio/Sink" => devices.outputs.push(device(&default_sink)),
            "Audio/Source" => devices.inputs.push(device(&default_source)),
            _ => {}
        }
    }
    for list in [&mut devices.outputs, &mut devices.inputs] {
        list.sort_by_key(|d| !d.is_default);
    }
    devices
}

/// Which way a stream faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// What an output plays.
    System,
    Microphone,
}

/// A running `pw-record`, writing to a WAV file.
#[derive(Debug)]
pub struct Recorder {
    child: Child,
    thread: Option<JoinHandle<Result<u64>>>,
    level: Arc<AtomicU32>,
    /// Of the last block, how many samples in a thousand were at full scale.
    clipped: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    /// When the first sound arrived, as microseconds after `epoch`; the
    /// track's place on the recording's timeline.
    started_us: Arc<AtomicU64>,
    pub path: PathBuf,
}

/// Microseconds not yet known.
const UNKNOWN: u64 = u64::MAX;

impl Recorder {
    /// Start recording `direction` from `device` (a node name, or empty for
    /// the default) into `path`. `epoch` is the recording's time zero.
    pub fn start(direction: Direction, device: &str, path: &Path, epoch: Instant) -> Result<Self> {
        let mut cmd = Command::new("pw-record");
        cmd.args(["--rate", "48000", "--channels", "2", "--format", "s16"]);
        cmd.args([
            "-P",
            "{ media.name = \"Raven Camera\" node.dont-reconnect = false }",
        ]);
        match direction {
            Direction::System => {
                cmd.args(["-P", "{ stream.capture.sink = true }"]);
                if !device.is_empty() {
                    cmd.args(["--target", device]);
                }
            }
            Direction::Microphone => {
                if !device.is_empty() {
                    cmd.args(["--target", device]);
                }
            }
        }
        cmd.arg("-")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd
            .spawn()
            .context("starting pw-record (is PipeWire installed?)")?;
        let stdout = child.stdout.take().expect("piped");
        let file =
            std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;

        let level = Arc::new(AtomicU32::new(0));
        let clipped = Arc::new(AtomicU32::new(0));
        let muted = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let started_us = Arc::new(AtomicU64::new(UNKNOWN));
        let thread = {
            let level = level.clone();
            let clipped = clipped.clone();
            let muted = muted.clone();
            let paused = paused.clone();
            let started_us = started_us.clone();
            std::thread::Builder::new()
                .name("audio".into())
                .spawn(move || {
                    pump(
                        stdout,
                        file,
                        &level,
                        &clipped,
                        &muted,
                        &paused,
                        &started_us,
                        epoch,
                    )
                })?
        };
        Ok(Self {
            child,
            thread: Some(thread),
            level,
            clipped,
            muted,
            paused,
            started_us,
            path: path.to_path_buf(),
        })
    }

    /// The last peak, 0.0 to 1.0, for a meter.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    /// Whether the input looks broken rather than loud: most of what it
    /// sends is pinned at full scale. A floating analogue input — a
    /// laptop whose real microphone is digital, read through the wrong
    /// path — does exactly this, and a recording of it is a roar.
    pub fn broken(&self) -> bool {
        self.clipped.load(Ordering::Relaxed) > 500
    }

    /// Record silence in place of sound, keeping time.
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// While paused, what arrives is dropped: the recording's clock stops
    /// too, so the track stays in step.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// Stop, finish the WAV header, and say where on the timeline the track
    /// starts. `None` if no sound ever arrived.
    pub fn stop(mut self) -> Result<Option<Track>> {
        // SIGINT lets pw-record flush and exit tidily; the pipe then closes
        // and the pump finishes.
        // SAFETY: signalling our own child by its pid.
        unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGINT) };
        let frames = match self.thread.take() {
            Some(t) => t
                .join()
                .map_err(|_| anyhow::anyhow!("the audio writer panicked"))??,
            None => 0,
        };
        let _ = self.child.wait();
        let start = self.started_us.load(Ordering::Relaxed);
        if frames == 0 || start == UNKNOWN {
            return Ok(None);
        }
        Ok(Some(Track {
            path: self.path.clone(),
            offset_us: start,
            frames,
        }))
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// A finished audio track.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Track {
    pub path: PathBuf,
    /// Where it starts on the recording's timeline.
    pub offset_us: u64,
    /// Stereo frames at [`RATE`].
    pub frames: u64,
}

/// Copy `pw-record`'s output into a WAV file until it ends. Returns the
/// number of stereo frames written.
#[allow(clippy::too_many_arguments)]
fn pump(
    mut input: impl Read,
    file: std::fs::File,
    level: &AtomicU32,
    clipped: &AtomicU32,
    muted: &AtomicBool,
    paused: &AtomicBool,
    started_us: &AtomicU64,
    epoch: Instant,
) -> Result<u64> {
    let mut out = io::BufWriter::new(file);
    out.write_all(&wav_header(0))?;
    let mut buf = vec![0u8; 4096 * FRAME_BYTES];
    let mut carry = 0usize;
    let mut frames = 0u64;
    loop {
        let n = match input.read(&mut buf[carry..]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        if started_us.load(Ordering::Relaxed) == UNKNOWN {
            // The first bytes stand for sound that began one buffer ago.
            let buffered = (carry + n) as u64 / FRAME_BYTES as u64 * 1_000_000 / u64::from(RATE);
            let at = epoch.elapsed().as_micros() as u64;
            started_us.store(at.saturating_sub(buffered), Ordering::Relaxed);
        }
        let total = carry + n;
        let whole = total - total % FRAME_BYTES;
        let chunk = &mut buf[..whole];
        if muted.load(Ordering::Relaxed) {
            chunk.fill(0);
        }
        let peak = chunk
            .chunks_exact(2)
            .map(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs())
            .max()
            .unwrap_or(0);
        level.store((f32::from(peak) / 32768.0).to_bits(), Ordering::Relaxed);
        let samples = (whole / 2).max(1);
        let pinned = chunk
            .chunks_exact(2)
            .filter(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs() >= 32_700)
            .count();
        clipped.store((pinned * 1000 / samples) as u32, Ordering::Relaxed);
        if !paused.load(Ordering::Relaxed) {
            out.write_all(chunk)?;
            frames += (whole / FRAME_BYTES) as u64;
        }
        buf.copy_within(whole..total, 0);
        carry = total - whole;
    }
    out.flush()?;
    let mut file = out.into_inner().map_err(|e| e.into_error())?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&wav_header(frames))?;
    file.sync_all()?;
    Ok(frames)
}

/// A canonical 44-byte WAV header for `frames` of 48 kHz 16-bit stereo.
pub fn wav_header(frames: u64) -> [u8; 44] {
    let data = (frames * FRAME_BYTES as u64).min(u64::from(u32::MAX - 36)) as u32;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&CHANNELS.to_le_bytes());
    h[24..28].copy_from_slice(&RATE.to_le_bytes());
    h[28..32].copy_from_slice(&(RATE * FRAME_BYTES as u32).to_le_bytes());
    h[32..34].copy_from_slice(&(FRAME_BYTES as u16).to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

/// Mixes finished tracks onto one timeline, a block at a time, so a long
/// recording is never held in memory whole. Each track is placed at its
/// offset with its gain; where more than one plays, the sum is soft-limited
/// so two loud sources clip gently instead of wrapping.
#[derive(Debug)]
pub struct Mixer {
    sources: Vec<MixSource>,
    /// The next stereo frame to produce.
    position: u64,
    total: u64,
}

#[derive(Debug)]
struct MixSource {
    reader: io::BufReader<std::fs::File>,
    start: u64,
    remaining: u64,
    gain: f32,
}

impl Mixer {
    /// `tracks` with their gains, over `length_us` of timeline.
    pub fn open(tracks: &[(Track, f32)], length_us: u64) -> Result<Self> {
        let mut sources = Vec::new();
        for (track, gain) in tracks {
            let mut file = std::fs::File::open(&track.path)
                .with_context(|| format!("opening {}", track.path.display()))?;
            file.seek(SeekFrom::Start(44))?;
            // A header never finished (a crash) says 0; the file length is
            // the truth either way.
            let on_disk = file.metadata()?.len().saturating_sub(44) / FRAME_BYTES as u64;
            sources.push(MixSource {
                reader: io::BufReader::with_capacity(1 << 16, file),
                start: us_to_frames(track.offset_us),
                remaining: on_disk,
                gain: *gain,
            });
        }
        Ok(Self {
            sources,
            position: 0,
            total: us_to_frames(length_us),
        })
    }

    /// Stereo frames the whole mix will have.
    #[cfg(test)]
    pub fn total_frames(&self) -> u64 {
        self.total
    }

    /// The next `frames` stereo frames, interleaved, or fewer at the end;
    /// `None` when there are no more.
    pub fn next_block(&mut self, frames: usize) -> Result<Option<Vec<i16>>> {
        if self.position >= self.total {
            return Ok(None);
        }
        let n = (frames as u64).min(self.total - self.position) as usize;
        let mut acc = vec![0f32; n * 2];
        let mut playing = 0;
        let mut bytes = Vec::new();
        for src in &mut self.sources {
            // Frames of this block before the track starts are silence.
            let lead = src.start.saturating_sub(self.position).min(n as u64) as usize;
            let want = ((n - lead) as u64).min(src.remaining) as usize;
            if want == 0 {
                continue;
            }
            playing += 1;
            bytes.resize(want * FRAME_BYTES, 0);
            let got = read_up_to(&mut src.reader, &mut bytes)? / FRAME_BYTES;
            src.remaining = if got < want {
                0
            } else {
                src.remaining - got as u64
            };
            for (i, s) in bytes[..got * FRAME_BYTES].chunks_exact(2).enumerate() {
                acc[lead * 2 + i] += f32::from(i16::from_le_bytes([s[0], s[1]])) * src.gain;
            }
        }
        self.position += n as u64;
        let limit = playing > 1;
        Ok(Some(
            acc.into_iter()
                .map(|v| {
                    let x = v / 32768.0;
                    let y = if limit {
                        soft_clip(x)
                    } else {
                        x.clamp(-1.0, 1.0)
                    };
                    (y * 32767.0).round() as i16
                })
                .collect(),
        ))
    }
}

fn us_to_frames(us: u64) -> u64 {
    (u128::from(us) * u128::from(RATE) / 1_000_000) as u64
}

fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Linear below 0.8, then a smooth knee that never exceeds 1.0.
fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= 0.8 {
        x
    } else {
        let over = (a - 0.8) / 0.2;
        x.signum() * (0.8 + 0.2 * (over / (1.0 + over)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = r#"[
      {"id": 30, "type": "PipeWire:Interface:Metadata", "props": {"metadata.name": "default"},
       "metadata": [
         {"subject": 0, "key": "default.audio.sink", "type": "Spa:String:JSON", "value": {"name": "speakers"}},
         {"subject": 0, "key": "default.audio.source", "type": "Spa:String:JSON", "value": {"name": "mic"}}
       ]},
      {"id": 52, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Sink", "node.name": "hdmi", "node.description": "HDMI"}}},
      {"id": 53, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Sink", "node.name": "speakers", "node.description": "Built-in Audio"}}},
      {"id": 54, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Source", "node.name": "mic", "node.description": "Built-in Mic"}}},
      {"id": 55, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Video/Source", "node.name": "cam"}}}
    ]"#;

    #[test]
    fn dump_lists_devices_defaults_first() {
        let d = parse_dump(DUMP);
        assert_eq!(d.outputs.len(), 2);
        assert_eq!(d.outputs[0].name, "speakers");
        assert!(d.outputs[0].is_default);
        assert_eq!(d.inputs.len(), 1);
        assert_eq!(d.inputs[0].description, "Built-in Mic");
    }

    #[test]
    fn garbage_is_no_devices() {
        let d = parse_dump("not json");
        assert!(d.outputs.is_empty() && d.inputs.is_empty());
    }

    #[test]
    fn wav_header_is_canonical() {
        let h = wav_header(48_000);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(h[40..44].try_into().unwrap()), 192_000);
        assert_eq!(u32::from_le_bytes(h[24..28].try_into().unwrap()), 48_000);
    }

    #[test]
    fn pump_writes_and_counts_whole_frames() {
        let dir = std::env::temp_dir().join(format!("raven-camera-pump-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        // Ten stereo frames of a loud sample, plus a stray odd byte.
        let mut input: Vec<u8> = [16000i16, -16000]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .cycle()
            .take(40)
            .collect();
        input.push(7);
        let level = AtomicU32::new(0);
        let started = AtomicU64::new(UNKNOWN);
        let frames = pump(
            &input[..],
            std::fs::File::create(&path).unwrap(),
            &level,
            &AtomicU32::new(0),
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            &started,
            Instant::now(),
        )
        .unwrap();
        assert_eq!(frames, 10);
        assert!(f32::from_bits(level.load(Ordering::Relaxed)) > 0.45);
        assert_ne!(started.load(Ordering::Relaxed), UNKNOWN);
        let track = Track {
            path: path.clone(),
            offset_us: 0,
            frames,
        };
        let mut mixer = Mixer::open(&[(track, 1.0)], 10 * 1_000_000 / 48_000 + 1).unwrap();
        let block = mixer.next_block(100).unwrap().unwrap();
        assert_eq!(block.len(), 20);
        assert_eq!(block[0], 16000);
        assert_eq!(block[1], -16000);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mixing_places_tracks_and_limits() {
        let dir = std::env::temp_dir().join(format!("raven-camera-mix-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 0.1 s of a loud constant, written twice, both starting at 0.25 s.
        let write = |name: &str| {
            let path = dir.join(name);
            let mut bytes = wav_header(4800).to_vec();
            for _ in 0..4800 * 2 {
                bytes.extend_from_slice(&30000i16.to_le_bytes());
            }
            std::fs::write(&path, bytes).unwrap();
            Track {
                path,
                offset_us: 250_000,
                frames: 4800,
            }
        };
        let tracks = [(write("a.wav"), 1.0), (write("b.wav"), 1.0)];
        let mut mixer = Mixer::open(&tracks, 500_000).unwrap();
        assert_eq!(mixer.total_frames(), 24_000);
        let mut out = Vec::new();
        while let Some(block) = mixer.next_block(1000).unwrap() {
            out.extend(block);
        }
        assert_eq!(out.len(), 2 * 24_000);
        assert_eq!(out[0], 0);
        let at = 2 * 12_500;
        assert!(out[at] > 30_000, "{}", out[at]);
        // After both tracks end, silence again.
        assert_eq!(out[2 * 20_000], 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

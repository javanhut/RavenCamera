//! A recording in progress.
//!
//! Every source writes to its own file on its own thread — the screen into
//! Raven's lossless screen format, the camera as the JPEG frames it sent,
//! each sound as a WAV — all against one clock. The clock is what makes
//! pause work: while paused it reads nothing, every writer drops what
//! arrives, and when recording resumes the paused stretch is simply not on
//! the timeline.
//!
//! Nothing here encodes H.264. A Celeron cannot do that in real time; it can
//! do this. The video is made when the recording is finished
//! ([`super::export`]).

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::camfile;
use super::session::{AudioTrack, Kind, Manifest, Overlay, State};
use crate::audio::{self, Direction};
use crate::camera;
use crate::pixels;
use crate::screen;
use crate::settings::Settings;

/// The recording's timeline: time since it started, less time paused.
#[derive(Debug)]
pub struct Clock {
    pub epoch: Instant,
    inner: Mutex<(Duration, Option<Instant>)>,
}

impl Clock {
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            inner: Mutex::new((Duration::ZERO, None)),
        }
    }

    /// Where `t` falls on the timeline, or `None` while paused.
    pub fn at(&self, t: Instant) -> Option<Duration> {
        let (paused_total, paused_at) = *self.inner.lock().unwrap();
        if paused_at.is_some() {
            return None;
        }
        Some(
            t.saturating_duration_since(self.epoch)
                .saturating_sub(paused_total),
        )
    }

    /// The timeline now, paused or not.
    pub fn now(&self) -> Duration {
        let (paused_total, paused_at) = *self.inner.lock().unwrap();
        let end = paused_at.unwrap_or_else(Instant::now);
        end.saturating_duration_since(self.epoch)
            .saturating_sub(paused_total)
    }

    pub fn pause(&self) {
        let mut g = self.inner.lock().unwrap();
        if g.1.is_none() {
            g.1 = Some(Instant::now());
        }
    }

    pub fn resume(&self) {
        let mut g = self.inner.lock().unwrap();
        if let Some(at) = g.1.take() {
            g.0 += at.elapsed();
        }
    }

    pub fn paused(&self) -> bool {
        self.inner.lock().unwrap().1.is_some()
    }
}

/// What to record.
pub struct Plan {
    pub kind: Kind,
    /// The screen capture to record from, already running for the preview.
    pub screen: Option<Arc<screen::Capture>>,
    /// The camera to record from, already streaming for the preview.
    pub camera: Option<Arc<camera::Stream>>,
    pub system_audio: bool,
    pub microphone: bool,
    /// What the window capture is of, for the Media page.
    pub subject: String,
}

/// Counts shown while recording.
#[derive(Debug, Default)]
pub struct Stats {
    pub screen_frames: AtomicU64,
    pub camera_frames: AtomicU64,
    pub dropped: AtomicU64,
}

pub struct Live {
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub clock: Arc<Clock>,
    pub stats: Arc<Stats>,
    stop: Arc<AtomicBool>,
    screen_writer: Option<JoinHandle<Result<u64>>>,
    camera_writer: Option<JoinHandle<Result<u64>>>,
    system: Option<audio::Recorder>,
    microphone: Option<audio::Recorder>,
    _screen: Option<Arc<screen::Capture>>,
    _camera: Option<Arc<camera::Stream>>,
}

impl std::fmt::Debug for Live {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Live")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl Live {
    pub fn start(plan: Plan, settings: &Settings) -> Result<Self> {
        let dir = super::session::create_dir().context("making a folder for the recording")?;
        let rec = &settings.recording;
        let ext = if plan.kind.is_video() { "mp4" } else { "m4a" };
        let out_dir = settings.video_dir();
        std::fs::create_dir_all(&out_dir)
            .with_context(|| format!("creating {}", out_dir.display()))?;
        let output = crate::naming::unique_path(&out_dir, &settings.stem_now(), ext);
        let manifest = Manifest {
            kind: plan.kind,
            state: State::Recording,
            output,
            created: chrono::Local::now().to_rfc3339(),
            qp: rec.quality.qp(),
            output_limit: rec.output_size.limit(),
            audio_bitrate: rec.quality.audio_bitrate(),
            keyframe_seconds: 2.0,
            screen: plan.screen.as_ref().map(|_| "screen.rvr".into()),
            camera: plan.camera.as_ref().map(|_| "camera.rcam".into()),
            mirror_camera: plan.kind == Kind::Camera && settings.camera.mirror_saved,
            overlay: (plan.screen.is_some() && plan.camera.is_some()).then(|| Overlay {
                corner: rec.overlay_corner,
                percent: rec.overlay_percent.clamp(10, 50),
                mirror: settings.camera.mirror_preview,
            }),
            audio: Vec::new(),
            length_us: 0,
            subject: plan.subject.clone(),
        };
        manifest.save(&dir)?;

        let clock = Arc::new(Clock::new());
        let stats = Arc::new(Stats::default());
        let stop = Arc::new(AtomicBool::new(false));

        let screen_writer = match &plan.screen {
            Some(capture) => {
                capture.set_fps(f64::from(rec.fps));
                let frames = capture.subscribe(3);
                let path = dir.join("screen.rvr");
                let (clock, stop, stats) = (clock.clone(), stop.clone(), stats.clone());
                let fps = rec.fps;
                Some(
                    std::thread::Builder::new()
                        .name("record-screen".into())
                        .spawn(move || write_screen(frames, &path, fps, &clock, &stop, &stats))?,
                )
            }
            None => None,
        };
        let camera_writer = match &plan.camera {
            Some(stream) => {
                let frames = stream.subscribe(6);
                let path = dir.join("camera.rcam");
                let (w, h) = (stream.mode.width, stream.mode.height);
                let (clock, stop, stats) = (clock.clone(), stop.clone(), stats.clone());
                Some(
                    std::thread::Builder::new()
                        .name("record-camera".into())
                        .spawn(move || write_camera(frames, &path, w, h, &clock, &stop, &stats))?,
                )
            }
            None => None,
        };
        let start_audio =
            |on: bool, direction, device: &str, file: &str| -> Option<audio::Recorder> {
                if !on {
                    return None;
                }
                match audio::Recorder::start(direction, device, &dir.join(file), clock.epoch) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        tracing::warn!("{file}: {e:#}");
                        None
                    }
                }
            };
        let system = start_audio(
            plan.system_audio,
            Direction::System,
            &rec.system_audio_device,
            "system.wav",
        );
        let microphone = start_audio(
            plan.microphone,
            Direction::Microphone,
            &rec.microphone_device,
            "microphone.wav",
        );
        if plan.kind == Kind::Audio && system.is_none() && microphone.is_none() {
            let _ = std::fs::remove_dir_all(&dir);
            anyhow::bail!("no sound to record: PipeWire's pw-record could not be started");
        }

        Ok(Self {
            dir,
            manifest,
            clock,
            stats,
            stop,
            screen_writer,
            camera_writer,
            system,
            microphone,
            _screen: plan.screen,
            _camera: plan.camera,
        })
    }

    pub fn elapsed(&self) -> Duration {
        self.clock.now()
    }

    pub fn paused(&self) -> bool {
        self.clock.paused()
    }

    pub fn set_paused(&self, paused: bool) {
        if paused {
            self.clock.pause();
        } else {
            self.clock.resume();
        }
        for r in [&self.system, &self.microphone].into_iter().flatten() {
            r.set_paused(paused);
        }
    }

    pub fn set_system_muted(&self, muted: bool) {
        if let Some(r) = &self.system {
            r.set_muted(muted);
        }
    }

    pub fn set_microphone_muted(&self, muted: bool) {
        if let Some(r) = &self.microphone {
            r.set_muted(muted);
        }
    }

    pub fn has_system_audio(&self) -> bool {
        self.system.is_some()
    }

    pub fn has_microphone(&self) -> bool {
        self.microphone.is_some()
    }

    /// The microphone is sending a pinned, broken signal.
    pub fn microphone_broken(&self) -> bool {
        self.microphone.as_ref().is_some_and(|r| r.broken())
    }

    /// Peaks for the meters: (system, microphone).
    pub fn levels(&self) -> (f32, f32) {
        (
            self.system.as_ref().map_or(0.0, |r| r.level()),
            self.microphone.as_ref().map_or(0.0, |r| r.level()),
        )
    }

    /// Stop every writer, and leave the session ready to finish.
    pub fn stop(mut self) -> Result<(PathBuf, Manifest)> {
        self.clock.resume();
        let length = self.clock.now();
        self.stop.store(true, Ordering::Relaxed);
        let mut problems = Vec::new();
        for (name, writer) in [
            ("screen", self.screen_writer.take()),
            ("camera", self.camera_writer.take()),
        ] {
            if let Some(w) = writer {
                match w.join() {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => problems.push(format!("{name}: {e:#}")),
                    Err(_) => problems.push(format!("{name}: the writer panicked")),
                }
            }
        }
        let mut tracks = Vec::new();
        for (source, recorder, gain) in [
            ("system", self.system.take(), 1.0f32),
            ("microphone", self.microphone.take(), 1.0),
        ] {
            let Some(r) = recorder else { continue };
            let file = r
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            match r.stop() {
                Ok(Some(track)) => tracks.push(AudioTrack {
                    file,
                    offset_us: track.offset_us,
                    frames: track.frames,
                    gain,
                    source: source.into(),
                }),
                Ok(None) => tracing::info!("{source}: no sound arrived"),
                Err(e) => problems.push(format!("{source} audio: {e:#}")),
            }
        }
        self.manifest.audio = tracks;
        self.manifest.length_us = length.as_micros() as u64;
        self.manifest.state = State::Ready;
        self.manifest.save(&self.dir)?;
        if !problems.is_empty() {
            tracing::warn!("recording stopped with problems: {}", problems.join("; "));
        }
        Ok((self.dir.clone(), self.manifest.clone()))
    }
}

/// Write screen frames into a Raven screen recording until told to stop.
///
/// The recording is one size for its whole length: the size of the first
/// frame. A window resized mid-recording is fitted into it, letterboxed,
/// rather than stretched or cut off.
fn write_screen(
    frames: Receiver<Arc<screen::Frame>>,
    path: &Path,
    fps: u32,
    clock: &Clock,
    stop: &AtomicBool,
    stats: &Stats,
) -> Result<u64> {
    let mut encoder: Option<raven_rec::Encoder<BufWriter<File>>> = None;
    let mut size = (0usize, 0usize);
    let mut written = 0u64;
    let mut last_pts = Duration::ZERO;
    loop {
        let frame = match frames.recv_timeout(Duration::from_millis(100)) {
            Ok(f) => f,
            Err(RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Some(pts) = clock.at(frame.at) else {
            continue;
        };
        let rgba = pixels::bgra_to_rgba(&frame.bgra, frame.width, frame.height, frame.width * 4);
        if encoder.is_none() {
            // raven-rec's dimensions are the recording's; even, for H.264.
            size = ((frame.width & !1).max(2), (frame.height & !1).max(2));
            let file =
                File::create(path).with_context(|| format!("creating {}", path.display()))?;
            // A key frame every two seconds: finishing splits the work at
            // key frames, so this is also how finely it can be shared out.
            let key_every = (fps * 2).max(2);
            encoder = Some(
                raven_rec::Encoder::new(
                    BufWriter::with_capacity(1 << 20, file),
                    size.0 as u32,
                    size.1 as u32,
                )
                .context("starting the screen recording")?
                .with_key_interval(key_every),
            );
        }
        let rgba = pixels::fit(&rgba, size.0, size.1);
        last_pts = pts.max(last_pts);
        encoder
            .as_mut()
            .expect("created above")
            .push(last_pts, &rgba.data)
            .context("writing a screen frame")?;
        written += 1;
        stats.screen_frames.store(written, Ordering::Relaxed);
    }
    if let Some(encoder) = encoder {
        let end = clock.now().max(last_pts);
        encoder
            .finish(end)
            .context("finishing the screen recording")?;
    }
    Ok(written)
}

/// Write camera frames as JPEG until told to stop.
fn write_camera(
    frames: Receiver<Arc<camera::Frame>>,
    path: &Path,
    width: u32,
    height: u32,
    clock: &Clock,
    stop: &AtomicBool,
    stats: &Stats,
) -> Result<u64> {
    let mut out = camfile::Writer::create(path, width, height)
        .with_context(|| format!("creating {}", path.display()))?;
    loop {
        let frame = match frames.recv_timeout(Duration::from_millis(100)) {
            Ok(f) => f,
            Err(RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Some(pts) = clock.at(frame.at) else {
            continue;
        };
        match frame.to_jpeg(false) {
            Ok(jpeg) => out.push(pts, &jpeg)?,
            Err(e) => {
                tracing::debug!("camera frame: {e:#}");
                stats.dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        }
        stats.camera_frames.store(out.frames, Ordering::Relaxed);
    }
    Ok(out.finish(clock.now())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_leaves_out_paused_time() {
        let clock = Clock::new();
        std::thread::sleep(Duration::from_millis(30));
        clock.pause();
        assert!(clock.paused());
        assert_eq!(clock.at(Instant::now()), None);
        std::thread::sleep(Duration::from_millis(60));
        let during = clock.now();
        clock.resume();
        let after = clock.now();
        assert!(after >= during);
        // Well under the 90 ms that passed: the pause is not on the timeline.
        assert!(after < Duration::from_millis(80), "{after:?}");
        assert!(clock.at(Instant::now()).is_some());
    }
}

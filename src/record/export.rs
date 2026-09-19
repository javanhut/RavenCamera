//! Finishing a recording: the captured files in, one MP4 (or M4A) out.
//!
//! Video is Raven's own H.264 (`raven-h264`), sound Raven's own AAC
//! (`raven-aac`), the file Raven's own MP4 writer (`raven-mp4`). None of it
//! runs while recording — the encoder cannot keep up in real time on the
//! machines Raven is for — so it runs here, afterwards, on every core.
//!
//! # How the work is shared
//!
//! A video is cut into segments at key frames, every couple of seconds, and
//! each segment is encoded by its own H.264 encoder: a segment starts with
//! an IDR frame, which refers to nothing before it, so segments encoded
//! apart join into one valid stream. Worker threads take segments in turn;
//! this thread writes them to the file in order as they come back, and
//! weaves the sound in between so a player never has to seek back and
//! forth.
//!
//! A screen recording can be cut this way because it was written with a key
//! frame every two seconds, and a key frame decodes with no history; a
//! camera recording can be cut anywhere, being JPEG frames. Each worker
//! opens the file itself and reads only its segment.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::camfile;
use super::session::{Kind, Manifest, Overlay};
use crate::audio::{self, Mixer, Track};
use crate::pixels::{self, Rgba, I420};

/// What a finished recording amounted to.
#[derive(Debug, Clone)]
pub struct Finished {
    pub path: PathBuf,
    pub length: Duration,
    /// The first picture, small, for the Media page.
    pub thumbnail: Option<Rgba>,
    pub size: (u32, u32),
}

/// One encoded video frame.
struct Encoded {
    /// Microseconds on the timeline.
    pts: u64,
    nals: Vec<Vec<u8>>,
    key: bool,
}

/// A stretch of video one encoder takes.
#[derive(Debug, Clone)]
struct Segment {
    /// Where its first (key) frame's record starts, for a screen recording;
    /// its first frame's index, for a camera recording.
    start: u64,
    /// Frames in it.
    frames: usize,
}

/// The source of the pictures.
#[derive(Debug, Clone)]
enum Video {
    Screen {
        path: PathBuf,
        header: [u8; raven_rec::HEADER_LEN],
        width: usize,
        height: usize,
    },
    Camera {
        path: PathBuf,
        index: Arc<camfile::Index>,
        mirror: bool,
        enhance: bool,
    },
}

/// A camera picture-in-picture to draw over each screen frame.
#[derive(Debug, Clone)]
struct Pip {
    path: PathBuf,
    index: Arc<camfile::Index>,
    overlay: Overlay,
    enhance: bool,
}

/// Turn the session in `dir` into its output file. `progress` is called
/// with 0.0–1.0 as it goes; setting `cancel` stops it, leaving the session
/// as it was.
pub fn finish(
    dir: &Path,
    manifest: &Manifest,
    progress: &(dyn Fn(f64) + Sync),
    cancel: &AtomicBool,
) -> Result<Finished> {
    let length = Duration::from_micros(manifest.length_us.max(1));
    let tracks: Vec<(Track, f32)> = manifest
        .audio
        .iter()
        .filter(|t| dir.join(&t.file).is_file())
        .map(|t| {
            (
                Track {
                    path: dir.join(&t.file),
                    offset_us: t.offset_us,
                    frames: t.frames,
                },
                t.gain,
            )
        })
        .collect();

    let (video, segments, total_frames, pip) = match manifest.kind {
        Kind::Audio => (None, Vec::new(), 0, None),
        _ => {
            let (video, segments, total) = plan_video(dir, manifest)?;
            let pip = match (&manifest.overlay, &manifest.camera, &video) {
                (Some(overlay), Some(cam), Video::Screen { .. }) => {
                    let path = dir.join(cam);
                    camfile::Index::read(&path)
                        .ok()
                        .filter(|i| !i.frames.is_empty())
                        .map(|index| Pip {
                            path,
                            index: Arc::new(index),
                            overlay: overlay.clone(),
                            enhance: manifest.enhance_camera,
                        })
                }
                _ => None,
            };
            (Some(video), segments, total, pip)
        }
    };
    if manifest.kind == Kind::Audio && tracks.is_empty() {
        bail!("the recording has no sound in it");
    }
    if video.is_some() && total_frames == 0 {
        bail!("the recording has no pictures in it");
    }
    // A recording interrupted by a crash has no length in its manifest; the
    // files know how long they are.
    let length = if manifest.length_us > 0 {
        length
    } else {
        recovered_length(dir, manifest, &tracks)
    };

    let (src_w, src_h) = match &video {
        Some(Video::Screen { width, height, .. }) => (*width, *height),
        Some(Video::Camera { index, .. }) => (index.width as usize, index.height as usize),
        None => (0, 0),
    };
    let (out_w, out_h) = pixels::output_size(src_w, src_h, manifest.output_limit);

    let part = part_path(&manifest.output);
    if let Some(parent) = part.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    // Sound, on its own thread, a block at a time, handed over as AAC
    // frames with their times.
    let audio_frames = AtomicU64::new(0);
    let (audio_tx, audio_rx) = mpsc::sync_channel::<Vec<u8>>(512);
    let (asc_tx, asc_rx) = mpsc::channel::<Vec<u8>>();

    let config = raven_h264::Config {
        width: out_w as u32,
        height: out_h as u32,
        qp: manifest.qp,
        frame_rate: 60,
    };
    let (sps, pps) = match &video {
        Some(_) => {
            let probe = raven_h264::Encoder::new(config)
                .map_err(|e| anyhow::anyhow!("H.264 cannot encode {out_w}×{out_h}: {e}"))?;
            (probe.sps().to_vec(), probe.pps().to_vec())
        }
        None => (Vec::new(), Vec::new()),
    };

    let next = AtomicUsize::new(0);
    let done_frames = AtomicU64::new(0);
    let thumbnail = std::sync::Mutex::new(None::<Rgba>);
    let failed = AtomicBool::new(false);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(1, 8);

    let result = std::thread::scope(|scope| -> Result<Finished> {
        // Audio.
        // With no sound the sender is dropped here, so the writer below sees
        // the channel closed rather than waiting on it.
        let audio_handle = if tracks.is_empty() {
            drop(audio_tx);
            None
        } else {
            let audio_tx = audio_tx;
            Some({
                let tracks = &tracks;
                let audio_frames = &audio_frames;
                let bitrate = manifest.audio_bitrate;
                scope.spawn(move || -> Result<()> {
                    let mut mixer = Mixer::open(tracks, length.as_micros() as u64)?;
                    let config = raven_aac::Config {
                        sample_rate: audio::RATE,
                        channels: 2,
                        bitrate,
                    };
                    let mut aac = raven_aac::Encoder::new(config)
                        .map_err(|e| anyhow::anyhow!("AAC: {e:?}"))?;
                    let _ = asc_tx.send(aac.audio_specific_config());
                    while let Some(block) = mixer.next_block(8192)? {
                        if cancel.load(Ordering::Relaxed) {
                            return Ok(());
                        }
                        audio_frames.fetch_add((block.len() / 2) as u64, Ordering::Relaxed);
                        for au in aac.encode(&block) {
                            if audio_tx.send(au).is_err() {
                                return Ok(());
                            }
                        }
                    }
                    for au in aac.finish() {
                        if audio_tx.send(au).is_err() {
                            return Ok(());
                        }
                    }
                    Ok(())
                })
            })
        };

        let asc = if audio_handle.is_some() {
            asc_rx.recv().ok()
        } else {
            None
        };

        // Video workers.
        let (seg_tx, seg_rx) = mpsc::channel::<(usize, Result<Vec<Encoded>>)>();
        if let Some(video) = &video {
            for _ in 0..workers.min(segments.len().max(1)) {
                let seg_tx = seg_tx.clone();
                let (segments, next, done_frames, thumbnail, failed, pip) =
                    (&segments, &next, &done_frames, &thumbnail, &failed, &pip);
                scope.spawn(move || loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= segments.len()
                        || cancel.load(Ordering::Relaxed)
                        || failed.load(Ordering::Relaxed)
                    {
                        break;
                    }
                    let r = encode_segment(
                        video,
                        &segments[i],
                        i == 0,
                        config,
                        (out_w, out_h),
                        manifest.keyframe_seconds,
                        pip.as_ref(),
                        cancel,
                        done_frames,
                        thumbnail,
                    );
                    if r.is_err() {
                        failed.store(true, Ordering::Relaxed);
                    }
                    if seg_tx.send((i, r)).is_err() {
                        break;
                    }
                });
            }
        }
        drop(seg_tx);

        let mut mux = mux::Muxer::create(
            &part,
            video.as_ref().map(|_| mux::VideoTrack {
                width: out_w as u16,
                height: out_h as u16,
                sps: sps.clone(),
                pps: pps.clone(),
            }),
            asc.map(|asc| mux::AudioTrack {
                sample_rate: audio::RATE,
                channels: 2,
                asc,
            }),
        )
        .with_context(|| format!("creating {}", part.display()))?;

        let report = |extra: f64| {
            let v = if total_frames > 0 {
                done_frames.load(Ordering::Relaxed) as f64 / total_frames as f64
            } else {
                1.0
            };
            let total_audio = (length.as_secs_f64() * f64::from(audio::RATE)).max(1.0);
            let a = if tracks.is_empty() {
                1.0
            } else {
                audio_frames.load(Ordering::Relaxed) as f64 / total_audio
            };
            progress((v.min(a) * 0.98 + extra).min(1.0));
        };

        // Write segments in order, sound woven in up to each frame's time.
        let mut audio_written: u64 = 0;
        let mut audio_open = !tracks.is_empty();
        let mut pending: BTreeMap<usize, Vec<Encoded>> = BTreeMap::new();
        let mut want = 0usize;
        let mut first_pts = None;
        let au_us = |n: u64| n * raven_aac::FRAME_LEN as u64 * 1_000_000 / u64::from(audio::RATE);
        let mut pull_audio_until = |mux: &mut mux::Muxer, until_us: u64| -> Result<()> {
            while audio_open && au_us(audio_written) <= until_us {
                match audio_rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(au) => {
                        mux.audio(&au)?;
                        audio_written += 1;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => audio_open = false,
                }
            }
            Ok(())
        };
        if video.is_some() {
            while want < segments.len() {
                if cancel.load(Ordering::Relaxed) {
                    bail!("cancelled");
                }
                let (i, r) = match seg_rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(x) => x,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        report(0.0);
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        bail!("the encoders stopped early")
                    }
                };
                pending.insert(i, r.with_context(|| format!("encoding part {}", i + 1))?);
                while let Some(frames) = pending.remove(&want) {
                    for f in frames {
                        // The first frame is shown from the very start, so
                        // the picture and the sound share a zero.
                        let pts = if first_pts.is_none() {
                            first_pts = Some(f.pts);
                            0
                        } else {
                            f.pts
                        };
                        pull_audio_until(&mut mux, pts)?;
                        mux.video(pts, &f.nals, f.key)?;
                    }
                    want += 1;
                }
                report(0.0);
            }
        }
        // The rest of the sound.
        pull_audio_until(&mut mux, u64::MAX)?;
        if let Some(h) = audio_handle {
            h.join()
                .map_err(|_| anyhow::anyhow!("the audio encoder panicked"))??;
        }
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        mux.finish(length.as_micros() as u64)?;
        report(0.02);
        Ok(Finished {
            path: manifest.output.clone(),
            length,
            thumbnail: thumbnail.lock().unwrap().take(),
            size: (out_w as u32, out_h as u32),
        })
    });

    match result {
        Ok(finished) => {
            let target = crate::naming::unique_path(
                manifest.output.parent().unwrap_or(Path::new(".")),
                &manifest
                    .output
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Recording".into()),
                manifest
                    .output
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("mp4"),
            );
            std::fs::rename(&part, &target)
                .with_context(|| format!("saving {}", target.display()))?;
            Ok(Finished {
                path: target,
                ..finished
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// Where the file is written while it is being made, so a half-made video
/// never shows up as a finished one.
fn part_path(output: &Path) -> PathBuf {
    let mut name = output.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    output.with_file_name(format!(".{}", name.to_string_lossy()))
}

/// How long an interrupted recording is, from what reached the disk.
fn recovered_length(dir: &Path, manifest: &Manifest, tracks: &[(Track, f32)]) -> Duration {
    let mut longest = Duration::ZERO;
    if let Some(screen) = &manifest.screen {
        if let Ok(index) = rvr_index(&dir.join(screen)) {
            longest = longest.max(index.length());
        }
    }
    if let Some(cam) = &manifest.camera {
        if let Ok(index) = camfile::Index::read(&dir.join(cam)) {
            longest = longest.max(index.length());
        }
    }
    for (t, _) in tracks {
        let frames = std::fs::metadata(&t.path)
            .map(|m| m.len().saturating_sub(44) / audio::FRAME_BYTES as u64)
            .unwrap_or(0);
        longest = longest.max(Duration::from_micros(
            t.offset_us + frames * 1_000_000 / u64::from(audio::RATE),
        ));
    }
    longest.max(Duration::from_millis(1))
}

/// The records of a Raven screen recording, found by reading only their
/// headers.
#[derive(Debug)]
struct RvrIndex {
    header: [u8; raven_rec::HEADER_LEN],
    width: usize,
    height: usize,
    /// `(offset, is_key, pts)` of every frame record.
    records: Vec<(u64, bool, Duration)>,
    end: Option<Duration>,
}

impl RvrIndex {
    fn length(&self) -> Duration {
        self.end.unwrap_or_else(|| {
            self.records
                .last()
                .map(|r| r.2 + Duration::from_millis(33))
                .unwrap_or_default()
        })
    }
}

fn rvr_index(path: &Path) -> Result<RvrIndex> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let len = file.metadata()?.len();
    let mut r = BufReader::new(file);
    let mut header = [0u8; raven_rec::HEADER_LEN];
    r.read_exact(&mut header)?;
    if header[0..6] != raven_rec::MAGIC {
        bail!("{} is not a Raven screen recording", path.display());
    }
    let width = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
    let height = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
    let mut records = Vec::new();
    let mut end = None;
    let mut at = raven_rec::HEADER_LEN as u64;
    let mut head = [0u8; raven_rec::RECORD_HEADER_LEN];
    while at + raven_rec::RECORD_HEADER_LEN as u64 <= len {
        if r.read_exact(&mut head).is_err() {
            break;
        }
        let kind = head[0];
        let pts = Duration::from_micros(u64::from_le_bytes(head[1..9].try_into().unwrap()));
        let size = u64::from(u32::from_le_bytes(head[9..13].try_into().unwrap()));
        let next = at + raven_rec::RECORD_HEADER_LEN as u64 + size;
        if next > len {
            break;
        }
        match kind {
            0 | 1 => records.push((at, kind == 0, pts)),
            2 => {
                end = Some(pts);
                break;
            }
            _ => {}
        }
        r.seek(SeekFrom::Current(size as i64))?;
        at = next;
    }
    Ok(RvrIndex {
        header,
        width,
        height,
        records,
        end,
    })
}

/// Cut the video into segments, each starting where a decoder can.
fn plan_video(dir: &Path, manifest: &Manifest) -> Result<(Video, Vec<Segment>, u64)> {
    let every = Duration::from_secs_f64(manifest.keyframe_seconds.max(0.5));
    if manifest.kind != Kind::Camera {
        let screen = manifest
            .screen
            .as_ref()
            .context("a screen recording with no screen file")?;
        let path = dir.join(screen);
        let index = rvr_index(&path)?;
        // Frames before the first key frame cannot be decoded; there are
        // none unless the file is damaged.
        let first_key = index.records.iter().position(|r| r.1).unwrap_or(0);
        let records = &index.records[first_key..];
        let mut segments: Vec<Segment> = Vec::new();
        let mut seg_start_pts = None::<Duration>;
        for &(offset, key, pts) in records {
            let starts = key
                && match (seg_start_pts, segments.last()) {
                    (None, _) => true,
                    // At least two frames a segment: two IDR frames in a row
                    // must differ in idr_pic_id, which separate encoders
                    // cannot promise.
                    (Some(start), Some(last)) => pts >= start + every && last.frames >= 2,
                    _ => false,
                };
            if starts {
                segments.push(Segment {
                    start: offset,
                    frames: 0,
                });
                seg_start_pts = Some(pts);
            }
            if let Some(last) = segments.last_mut() {
                last.frames += 1;
            }
        }
        let total = records.len() as u64;
        Ok((
            Video::Screen {
                path,
                header: index.header,
                width: index.width,
                height: index.height,
            },
            segments,
            total,
        ))
    } else {
        let cam = manifest
            .camera
            .as_ref()
            .context("a camera recording with no camera file")?;
        let path = dir.join(cam);
        let index = camfile::Index::read(&path)?;
        let mut segments: Vec<Segment> = Vec::new();
        let mut seg_start = None::<Duration>;
        for (i, &(pts, _, _)) in index.frames.iter().enumerate() {
            let starts = match (seg_start, segments.last()) {
                (None, _) => true,
                (Some(start), Some(last)) => pts >= start + every && last.frames >= 2,
                _ => false,
            };
            if starts {
                segments.push(Segment {
                    start: i as u64,
                    frames: 0,
                });
                seg_start = Some(pts);
            }
            if let Some(last) = segments.last_mut() {
                last.frames += 1;
            }
        }
        let total = index.frames.len() as u64;
        Ok((
            Video::Camera {
                path,
                index: Arc::new(index),
                mirror: manifest.mirror_camera,
                enhance: manifest.enhance_camera,
            },
            segments,
            total,
        ))
    }
}

/// Encode one segment, start to finish, with an encoder of its own.
#[allow(clippy::too_many_arguments)]
fn encode_segment(
    video: &Video,
    segment: &Segment,
    first: bool,
    config: raven_h264::Config,
    (out_w, out_h): (usize, usize),
    keyframe_seconds: f64,
    pip: Option<&Pip>,
    cancel: &AtomicBool,
    done_frames: &AtomicU64,
    thumbnail: &std::sync::Mutex<Option<Rgba>>,
) -> Result<Vec<Encoded>> {
    let mut encoder =
        raven_h264::Encoder::new(config).map_err(|e| anyhow::anyhow!("H.264: {e}"))?;
    let mut picture = I420::new(out_w, out_h);
    let mut out = Vec::with_capacity(segment.frames);
    let mut last_key: Option<Duration> = None;
    let every = Duration::from_secs_f64(keyframe_seconds.max(0.5));
    let mut pip_state = pip.map(|p| PipState::open(p, out_w, out_h)).transpose()?;

    let mut encode = |rgba: Rgba, pts: Duration, out: &mut Vec<Encoded>| -> Result<()> {
        let mut frame = if rgba.width == out_w && rgba.height == out_h {
            rgba
        } else {
            pixels::resize(&rgba, out_w, out_h)
        };
        if let Some(pip) = pip_state.as_mut() {
            pip.draw(&mut frame, pts)?;
        }
        if first && out.is_empty() {
            let (tw, th) = pixels::fit_size(frame.width, frame.height, 480, 270);
            *thumbnail.lock().unwrap() = Some(pixels::resize(&frame, tw, th));
        }
        picture.fill(&frame);
        let key = last_key.is_none_or(|k| pts >= k + every);
        let encoded = encoder
            .encode(
                &raven_h264::Picture {
                    y: &picture.y,
                    cb: &picture.cb,
                    cr: &picture.cr,
                },
                key,
            )
            .map_err(|e| anyhow::anyhow!("H.264: {e}"))?;
        if encoded.idr {
            last_key = Some(pts);
        }
        out.push(Encoded {
            pts: pts.as_micros() as u64,
            nals: encoded.nals,
            key: encoded.idr,
        });
        done_frames.fetch_add(1, Ordering::Relaxed);
        Ok(())
    };

    match video {
        Video::Screen {
            path,
            header,
            width,
            height,
        } => {
            let mut file = File::open(path)?;
            file.seek(SeekFrom::Start(segment.start))?;
            let input = Cursor::new(header.to_vec()).chain(BufReader::with_capacity(1 << 20, file));
            let mut decoder = raven_rec::Decoder::new(input)
                .map_err(|e| anyhow::anyhow!("reading the screen recording: {e}"))?;
            let mut n = 0;
            while n < segment.frames {
                if cancel.load(Ordering::Relaxed) {
                    bail!("cancelled");
                }
                let frame = match decoder.next_frame() {
                    Ok(Some(f)) => f,
                    Ok(None) | Err(raven_rec::Error::Truncated) => break,
                    Err(raven_rec::Error::Io(e)) => return Err(e.into()),
                    // A damaged record: skip it; the picture comes back at
                    // the next key frame.
                    Err(_) => {
                        n += 1;
                        done_frames.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                };
                let rgba = Rgba {
                    width: *width,
                    height: *height,
                    data: frame.rgba.to_vec(),
                };
                let pts = frame.pts;
                encode(rgba, pts, &mut out)?;
                n += 1;
            }
        }
        Video::Camera {
            path,
            index,
            mirror,
            enhance,
        } => {
            let mut reader = camfile::Reader::open(path)?;
            // Each segment follows the light on its own; the levels settle
            // within a few frames of a key frame.
            let mut enhancer = enhance.then(crate::enhance::Enhancer::default);
            let start = segment.start as usize;
            for &(pts, offset, len) in &index.frames[start..start + segment.frames] {
                if cancel.load(Ordering::Relaxed) {
                    bail!("cancelled");
                }
                let jpeg = reader.frame(offset, len)?;
                let mut rgba = match pixels::decode_jpeg(&jpeg) {
                    Ok(img) => img,
                    // A frame the camera garbled: repeat the last one by
                    // skipping this, which the next frame's time covers.
                    Err(_) => {
                        done_frames.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                };
                if let Some(e) = enhancer.as_mut() {
                    e.frame(&mut rgba);
                }
                if *mirror {
                    rgba.mirror();
                }
                // A camera that changed its mind about its size mid-stream
                // is fitted to the first size.
                if rgba.width != index.width as usize || rgba.height != index.height as usize {
                    rgba = pixels::fit(&rgba, index.width as usize, index.height as usize);
                }
                encode(rgba, pts, &mut out)?;
            }
        }
    }
    Ok(out)
}

/// The picture-in-picture for one worker: its own reader, and the last
/// camera frame decoded, since consecutive screen frames usually share one.
struct PipState<'a> {
    pip: &'a Pip,
    reader: camfile::Reader,
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    radius: f32,
    last: Option<(usize, Rgba)>,
    enhancer: Option<crate::enhance::Enhancer>,
}

impl<'a> PipState<'a> {
    fn open(pip: &'a Pip, out_w: usize, out_h: usize) -> Result<Self> {
        let w = (out_w * pip.overlay.percent as usize / 100).max(32);
        let h = (w * pip.index.height as usize / pip.index.width.max(1) as usize).max(18);
        let (x, y) = pixels::corner_position(pip.overlay.corner, out_w, out_h, w, h);
        Ok(Self {
            pip,
            reader: camfile::Reader::open(&pip.path)?,
            width: w,
            height: h,
            x,
            y,
            radius: (w.min(h) as f32 * 0.12).max(4.0),
            last: None,
            enhancer: pip.enhance.then(crate::enhance::Enhancer::default),
        })
    }

    fn draw(&mut self, frame: &mut Rgba, pts: Duration) -> Result<()> {
        let Some(i) = self.pip.index.at(pts) else {
            return Ok(());
        };
        if self.last.as_ref().is_none_or(|(at, _)| *at != i) {
            let (_, offset, len) = self.pip.index.frames[i];
            let jpeg = self.reader.frame(offset, len)?;
            if let Ok(mut img) = pixels::decode_jpeg(&jpeg) {
                if self.pip.overlay.mirror {
                    img.mirror();
                }
                // Enhanced at overlay size, where it costs next to nothing.
                let mut small = pixels::resize(&img, self.width, self.height);
                if let Some(e) = self.enhancer.as_mut() {
                    e.frame(&mut small);
                }
                self.last = Some((i, small));
            }
        }
        if let Some((_, img)) = &self.last {
            pixels::overlay_rounded(frame, img, self.x, self.y, self.radius);
        }
        Ok(())
    }
}

/// The MP4 writer, behind the few calls this module makes.
mod mux {
    use std::path::Path;

    use anyhow::Result;

    pub struct VideoTrack {
        pub width: u16,
        pub height: u16,
        pub sps: Vec<u8>,
        pub pps: Vec<u8>,
    }

    pub struct AudioTrack {
        pub sample_rate: u32,
        pub channels: u16,
        pub asc: Vec<u8>,
    }

    pub struct Muxer {
        inner: raven_mp4::Mp4,
    }

    /// Microseconds to the writer's 90 kHz ticks, rounded.
    fn ticks(us: u64) -> u64 {
        (u128::from(us) * u128::from(raven_mp4::TIMESCALE) / 1_000_000) as u64
    }

    impl Muxer {
        pub fn create(
            path: &Path,
            video: Option<VideoTrack>,
            audio: Option<AudioTrack>,
        ) -> Result<Self> {
            let inner = raven_mp4::Mp4::create(
                path,
                video.map(|v| raven_mp4::Video {
                    width: u32::from(v.width),
                    height: u32::from(v.height),
                    sps: v.sps,
                    pps: v.pps,
                }),
                audio.map(|a| raven_mp4::Audio {
                    sample_rate: a.sample_rate,
                    channels: a.channels as u8,
                    config: a.asc,
                    priming: raven_aac::PRIMING,
                }),
            )?;
            Ok(Self { inner })
        }

        /// A video frame shown from `pts_us` on the timeline.
        pub fn video(&mut self, pts_us: u64, nals: &[Vec<u8>], key: bool) -> Result<()> {
            Ok(self.inner.push_video(ticks(pts_us), nals, key)?)
        }

        /// The next AAC frame.
        pub fn audio(&mut self, au: &[u8]) -> Result<()> {
            Ok(self.inner.push_audio(au)?)
        }

        /// Close the file; the last video frame lasts, and the sound is cut,
        /// at `end_us`.
        pub fn finish(self, end_us: u64) -> Result<u64> {
            Ok(self.inner.finish(ticks(end_us))?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_files_are_hidden_beside_the_output() {
        assert_eq!(
            part_path(Path::new("/v/Clip 1.mp4")),
            PathBuf::from("/v/.Clip 1.mp4.part")
        );
    }
}

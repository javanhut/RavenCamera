//! A photo at the camera's full size.
//!
//! The preview streams at a video size, 1080p at most. Most webcams can
//! also send a much larger picture at a few frames a second, so a photo is
//! taken by starting the camera again in its largest mode, letting it
//! settle, and keeping a short burst of frames to merge. The caller must have stopped every other
//! stream from this camera first: a camera streams to one reader.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::controls::{self, EXPOSURE_AUTO_PRIORITY};
use super::{Device, Mode, Stream};
use crate::pixels::Rgba;

/// How long automatic exposure and white balance get to find their feet
/// after the camera starts: the first pictures are dark or tinted.
const SETTLE: Duration = Duration::from_millis(900);
/// Frames thrown away however long they took.
const SETTLE_FRAMES: u32 = 2;
/// Give up on a mode that sends nothing usable in this long.
const DEADLINE: Duration = Duration::from_secs(6);

/// How many frames a photo merges (see `enhance::stack`): as many as the
/// camera sends in about 0.8 s, between two and six, so a camera in a
/// two-frames-a-second mode adds a second and one at 30 fps a fifth.
pub fn burst_len(fps: f64) -> usize {
    (fps * 0.8).round().clamp(2.0, 6.0) as usize
}

/// Frames for a photo from `device`, in the first of `modes` that works: a
/// burst of [`burst_len`] if `burst`, otherwise one.
pub fn capture(device: &Device, modes: &[Mode], burst: bool) -> Result<Vec<Rgba>> {
    // At a few frames a second the camera can afford long exposures, which
    // in dim light means a brighter picture with less noise. Put back as it
    // was afterwards.
    let priority = controls::get(&device.path, EXPOSURE_AUTO_PRIORITY).ok();
    if priority == Some(0) {
        let _ = controls::set(&device.path, EXPOSURE_AUTO_PRIORITY, 1);
    }
    let mut result = Err(anyhow::anyhow!("the camera has no mode for photos"));
    for mode in modes {
        let count = if burst { burst_len(mode.fps) } else { 1 };
        result = Stream::start(device, *mode).and_then(|s| frames(&s, count, true));
        match &result {
            Ok(_) => break,
            Err(e) => tracing::info!("photo at {}: {e:#}", mode.label()),
        }
    }
    if priority == Some(0) {
        let _ = controls::set(&device.path, EXPOSURE_AUTO_PRIORITY, 0);
    }
    result
}

/// `count` frames in a row from `stream`, decoded. `settle` if the camera
/// has only just started, so the first ones are thrown away.
pub fn frames(stream: &Stream, count: usize, settle: bool) -> Result<Vec<Rgba>> {
    let rx = stream.subscribe(count + 2);
    let started = Instant::now();
    let mut seen = 0;
    let mut raw = Vec::with_capacity(count);
    while raw.len() < count {
        let left = DEADLINE
            .checked_sub(started.elapsed())
            .context("the camera sent no picture")?;
        let frame = match rx.recv_timeout(left) {
            Ok(f) => f,
            // A burst cut short still makes a photo.
            Err(_) if !raw.is_empty() => break,
            Err(_) => anyhow::bail!("the camera sent no picture"),
        };
        seen += 1;
        if settle && (seen <= SETTLE_FRAMES || started.elapsed() < SETTLE) {
            continue;
        }
        raw.push(frame);
    }
    // Decoded after, so decoding never makes the burst miss a frame.
    let out: Vec<Rgba> = raw
        .iter()
        .filter_map(|f| match f.to_rgba() {
            Ok(img) => Some(img),
            // A garbled frame: the others will do.
            Err(e) => {
                tracing::debug!("photo frame: {e:#}");
                None
            }
        })
        .collect();
    if out.is_empty() {
        anyhow::bail!("the camera's pictures did not decode");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn bursts_are_short_at_any_frame_rate() {
        assert_eq!(super::burst_len(2.0), 2);
        assert_eq!(super::burst_len(5.0), 4);
        assert_eq!(super::burst_len(30.0), 6);
    }
}

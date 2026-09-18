//! Live pictures in the window: frames from a camera or the screen, turned
//! into textures for a `gtk::Picture`.
//!
//! A preview never makes its source wait. Each has a thread that takes the
//! newest frame, readies it (a camera's JPEG is decoded there, not on the
//! main loop) and offers it to the window through a one-slot channel; a
//! frame the window has not taken yet is simply replaced. A busy main loop
//! shows fewer frames; it never shows old ones, and it never slows a
//! recording.
//!
//! Screen frames reach the texture without a copy: the compositor's BGRA
//! is a format GDK takes as it is.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::gdk;
use gtk4::prelude::*;

use crate::camera;
use crate::pixels::Rgba;
use crate::screen;

/// How often the screen is sampled for the preview when not recording: the
/// preview is a look at what will be recorded, not a second monitor.
pub const SCREEN_FPS: f64 = 12.0;

/// Stops its preview when dropped.
#[derive(Debug)]
pub struct Handle {
    stop: Arc<AtomicBool>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Screen frames, shared rather than copied, as GDK bytes.
struct Shared(Arc<screen::Frame>);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        &self.0.bgra
    }
}

enum Picture {
    Rgba(Rgba),
    Screen(Arc<screen::Frame>),
}

fn texture(p: Picture) -> gdk::Texture {
    match p {
        Picture::Rgba(img) => {
            let stride = img.width * 4;
            let (w, h) = (img.width as i32, img.height as i32);
            gdk::MemoryTexture::new(
                w,
                h,
                gdk::MemoryFormat::R8g8b8a8,
                &glib::Bytes::from_owned(img.data),
                stride,
            )
            .upcast()
        }
        Picture::Screen(frame) => {
            let (w, h) = (frame.width as i32, frame.height as i32);
            gdk::MemoryTexture::new(
                w,
                h,
                gdk::MemoryFormat::B8g8r8a8Premultiplied,
                &glib::Bytes::from_owned(Shared(frame.clone())),
                frame.width * 4,
            )
            .upcast()
        }
    }
}

/// Deliver pictures from `produce` (run on its own thread until the handle
/// drops) to `on_frame` on the main loop.
fn attach(
    name: &str,
    produce: impl FnMut(&AtomicBool) -> Option<Picture> + Send + 'static,
    on_frame: impl Fn(gdk::Texture) + 'static,
) -> Handle {
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = async_channel::bounded::<Picture>(1);
    {
        let stop = stop.clone();
        let mut produce = produce;
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let Some(p) = produce(&stop) else { continue };
                    match tx.try_send(p) {
                        Ok(()) | Err(async_channel::TrySendError::Full(_)) => {}
                        Err(async_channel::TrySendError::Closed(_)) => break,
                    }
                }
            })
            .expect("spawning a preview thread");
    }
    let stop_ui = stop.clone();
    glib::spawn_future_local(async move {
        while let Ok(p) = rx.recv().await {
            if stop_ui.load(Ordering::Relaxed) {
                break;
            }
            on_frame(texture(p));
        }
    });
    Handle { stop }
}

/// Show `stream` through `on_frame`, mirrored if `mirror`.
pub fn camera(
    stream: &camera::Stream,
    mirror: bool,
    on_frame: impl Fn(gdk::Texture) + 'static,
) -> Handle {
    let frames = stream.subscribe(1);
    attach(
        "preview-camera",
        move |stop| match frames.recv_timeout(Duration::from_millis(200)) {
            Ok(frame) => {
                // Only the newest: drop what queued while decoding.
                let frame = frames.try_iter().last().unwrap_or(frame);
                let mut img = frame.to_rgba().ok()?;
                if mirror {
                    img.mirror();
                }
                Some(Picture::Rgba(img))
            }
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                stop.store(true, Ordering::Relaxed);
                None
            }
        },
        on_frame,
    )
}

/// Show `capture` through `on_frame`.
pub fn screen(capture: &screen::Capture, on_frame: impl Fn(gdk::Texture) + 'static) -> Handle {
    let frames = capture.subscribe(1);
    attach(
        "preview-screen",
        move |stop| match frames.recv_timeout(Duration::from_millis(200)) {
            Ok(frame) => Some(Picture::Screen(frames.try_iter().last().unwrap_or(frame))),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                stop.store(true, Ordering::Relaxed);
                None
            }
        },
        on_frame,
    )
}

/// A texture of a still picture.
pub fn still(img: &Rgba) -> gdk::Texture {
    texture(Picture::Rgba(img.clone()))
}

/// Set `picture` to show `texture`.
pub fn show(picture: &gtk::Picture, texture: &gdk::Texture) {
    picture.set_paintable(Some(texture));
}

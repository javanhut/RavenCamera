//! Streaming from a camera on a thread of its own.
//!
//! The thread does as little as possible with each buffer — copy the bytes
//! out, give the buffer straight back to the driver — so the camera never
//! runs out of buffers and drops frames at the source. Everything else
//! (decoding for the preview, writing a recording, taking a photo) happens
//! on the thread of whoever subscribed.
//!
//! Subscribers get frames through a bounded channel of their own and a slow
//! one misses frames rather than holding up the camera or the others.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::v4l2::{self, Node};
use super::{Device, Mode};
use crate::pixels::{self, Rgba};

/// The bytes of one frame, in the format the camera sent.
#[derive(Debug)]
pub enum Payload {
    Jpeg(Vec<u8>),
    Yuyv { data: Vec<u8>, stride: usize },
    Nv12 { data: Vec<u8>, stride: usize },
}

/// One frame from the camera.
#[derive(Debug)]
pub struct Frame {
    /// When it came off the driver's queue.
    pub at: Instant,
    pub width: usize,
    pub height: usize,
    pub payload: Payload,
}

impl Frame {
    /// The frame as RGBA.
    pub fn to_rgba(&self) -> Result<Rgba> {
        let image = match &self.payload {
            Payload::Jpeg(bytes) => pixels::decode_jpeg(bytes)?,
            Payload::Yuyv { data, stride } => {
                pixels::yuyv_to_rgba(data, self.width, self.height, *stride)
            }
            Payload::Nv12 { data, stride } => {
                pixels::nv12_to_rgba(data, self.width, self.height, *stride)
            }
        };
        Ok(image)
    }

    /// The frame as a JPEG: as it came for a JPEG camera, compressed here
    /// for an uncompressed one. Recordings store camera video this way.
    pub fn to_jpeg(&self, mirror: bool) -> Result<Vec<u8>> {
        match &self.payload {
            Payload::Jpeg(bytes) if !mirror => Ok(bytes.clone()),
            _ => {
                let mut image = self.to_rgba()?;
                if mirror {
                    image.mirror();
                }
                pixels::encode_jpeg(&image, 90)
            }
        }
    }
}

/// Why a stream ended on its own.
#[derive(Debug, Clone)]
pub enum Ended {
    /// Unplugged.
    Disconnected,
    Failed(String),
}

type Subscribers = Arc<Mutex<Vec<SyncSender<Arc<Frame>>>>>;

/// A running camera stream. Dropping it stops the camera.
#[derive(Debug)]
pub struct Stream {
    pub device: Device,
    pub mode: Mode,
    stop: Arc<AtomicBool>,
    subscribers: Subscribers,
    ended: Arc<Mutex<Option<Ended>>>,
    thread: Option<JoinHandle<()>>,
}

/// How many buffers the driver cycles through. Four is what the UVC driver
/// is tuned for: enough that a frame being copied out never makes the
/// camera wait, few enough that the preview is not a quarter-second behind.
const BUFFERS: u32 = 4;

impl Stream {
    /// Start `device` streaming in `mode`.
    pub fn start(device: &Device, mode: Mode) -> Result<Self> {
        let node = Node::open(&device.path)
            .with_context(|| format!("opening {}", device.path.display()))?;
        let pix = node
            .set_format(mode.width, mode.height, mode.format)
            .map_err(busy)
            .context("setting the camera's format")?;
        let actual = Mode {
            format: pix.pixelformat,
            width: pix.width,
            height: pix.height,
            fps: mode.fps,
        };
        if !matches!(
            pix.pixelformat,
            v4l2::PIX_MJPEG | v4l2::PIX_JPEG | v4l2::PIX_YUYV | v4l2::PIX_NV12
        ) {
            bail!(
                "the camera answered with a format this app cannot read ({})",
                v4l2::fourcc_name(pix.pixelformat)
            );
        }
        // Not every camera lets the rate be set; it streams at its own.
        let _ = node.set_frame_rate(mode.fps.round() as u32);
        let buffers = Buffers::map(&node)
            .map_err(busy)
            .context("preparing the camera's buffers")?;

        let stop = Arc::new(AtomicBool::new(false));
        let subscribers: Subscribers = Arc::default();
        let ended = Arc::new(Mutex::new(None));
        let thread = {
            let stop = stop.clone();
            let subscribers = subscribers.clone();
            let ended = ended.clone();
            let stride = pix.bytesperline as usize;
            std::thread::Builder::new()
                .name("camera".into())
                .spawn(move || {
                    let result = run(&node, &buffers, actual, stride, &stop, &subscribers);
                    // STREAMOFF before the buffers are unmapped by drop.
                    let mut kind = v4l2::BUF_TYPE_VIDEO_CAPTURE as libc::c_int;
                    // SAFETY: VIDIOC_STREAMOFF takes an int buffer type.
                    let _ = unsafe { v4l2::ioctl(node.fd(), v4l2::VIDIOC_STREAMOFF, &mut kind) };
                    drop(buffers);
                    if let Err(e) = result {
                        if let Ended::Failed(why) = &e {
                            tracing::warn!("camera stream ended: {why}");
                        }
                        *ended.lock().unwrap() = Some(e);
                    }
                    // Wake subscribers so they notice the end.
                    subscribers.lock().unwrap().clear();
                })?
        };
        Ok(Self {
            device: device.clone(),
            mode: actual,
            stop,
            subscribers,
            ended,
            thread: Some(thread),
        })
    }

    /// Frames from now on. At most `depth` wait for the receiver; the rest
    /// are dropped.
    pub fn subscribe(&self, depth: usize) -> Receiver<Arc<Frame>> {
        let (tx, rx) = mpsc::sync_channel(depth.max(1));
        self.subscribers.lock().unwrap().push(tx);
        rx
    }

    /// Why the stream stopped by itself, if it did.
    pub fn ended(&self) -> Option<Ended> {
        self.ended.lock().unwrap().clone()
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// EBUSY means another program has the camera, which deserves saying in
/// words.
fn busy(e: std::io::Error) -> anyhow::Error {
    if e.raw_os_error() == Some(libc::EBUSY) {
        anyhow::anyhow!("the camera is in use by another application")
    } else {
        e.into()
    }
}

/// The driver's buffers, mapped into this process.
struct Buffers {
    maps: Vec<(*mut libc::c_void, usize)>,
}

// SAFETY: the mappings are only read, by the camera thread that owns this
// value after it is moved there; nothing else holds the pointers.
unsafe impl Send for Buffers {}

impl Buffers {
    fn map(node: &Node) -> std::io::Result<Self> {
        let mut req = v4l2::RequestBuffers {
            count: BUFFERS,
            kind: v4l2::BUF_TYPE_VIDEO_CAPTURE,
            memory: v4l2::MEMORY_MMAP,
            ..Default::default()
        };
        // SAFETY: VIDIOC_REQBUFS takes a v4l2_requestbuffers.
        unsafe { v4l2::ioctl(node.fd(), v4l2::VIDIOC_REQBUFS, &mut req)? };
        // Built up in place so an error part-way unmaps what was mapped.
        let mut this = Self { maps: Vec::new() };
        for index in 0..req.count {
            let mut buf = v4l2::Buffer::mmap(index);
            // SAFETY: VIDIOC_QUERYBUF takes a v4l2_buffer.
            unsafe { v4l2::ioctl(node.fd(), v4l2::VIDIOC_QUERYBUF, &mut buf)? };
            // SAFETY: mapping the driver buffer at the offset and length the
            // driver just reported, read-only and shared, as V4L2 specifies.
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    buf.length as usize,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    node.fd(),
                    libc::off_t::from(buf.offset()),
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            this.maps.push((ptr, buf.length as usize));
            // SAFETY: VIDIOC_QBUF takes a v4l2_buffer.
            unsafe { v4l2::ioctl(node.fd(), v4l2::VIDIOC_QBUF, &mut buf)? };
        }
        let mut kind = v4l2::BUF_TYPE_VIDEO_CAPTURE as libc::c_int;
        // SAFETY: VIDIOC_STREAMON takes an int buffer type.
        unsafe { v4l2::ioctl(node.fd(), v4l2::VIDIOC_STREAMON, &mut kind)? };
        Ok(this)
    }

    fn bytes(&self, index: usize, used: usize) -> &[u8] {
        let (ptr, len) = self.maps[index];
        // SAFETY: the mapping is `len` bytes long and lives as long as self;
        // the driver does not write a buffer while it is dequeued.
        unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), used.min(len)) }
    }
}

impl Drop for Buffers {
    fn drop(&mut self) {
        for &(ptr, len) in &self.maps {
            // SAFETY: each pair is a mapping made in `map` and not unmapped
            // since.
            unsafe { libc::munmap(ptr, len) };
        }
    }
}

fn run(
    node: &Node,
    buffers: &Buffers,
    mode: Mode,
    stride: usize,
    stop: &AtomicBool,
    subscribers: &Subscribers,
) -> Result<(), Ended> {
    let mut quiet_since = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let mut pfd = libc::pollfd {
            fd: node.fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd, count 1.
        let r = unsafe { libc::poll(&mut pfd, 1, 100) };
        if r < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(Ended::Failed(e.to_string()));
        }
        if pfd.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            return Err(Ended::Disconnected);
        }
        if r == 0 {
            // A camera that has sent nothing for five seconds has hung.
            if quiet_since.elapsed() > Duration::from_secs(5) {
                return Err(Ended::Failed("the camera stopped sending pictures".into()));
            }
            continue;
        }
        let mut buf = v4l2::Buffer::mmap(0);
        // SAFETY: VIDIOC_DQBUF takes a v4l2_buffer.
        match unsafe { v4l2::ioctl(node.fd(), v4l2::VIDIOC_DQBUF, &mut buf) } {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) if e.raw_os_error() == Some(libc::ENODEV) => return Err(Ended::Disconnected),
            Err(e) => return Err(Ended::Failed(e.to_string())),
        }
        quiet_since = Instant::now();
        let at = Instant::now();
        let bytes = buffers
            .bytes(buf.index as usize, buf.bytesused as usize)
            .to_vec();
        // Straight back to the driver before anything else is done.
        // SAFETY: VIDIOC_QBUF takes a v4l2_buffer.
        if let Err(e) = unsafe { v4l2::ioctl(node.fd(), v4l2::VIDIOC_QBUF, &mut buf) } {
            if e.raw_os_error() == Some(libc::ENODEV) {
                return Err(Ended::Disconnected);
            }
        }
        // A buffer the driver flagged as bad, or an empty JPEG, is skipped.
        const BUF_FLAG_ERROR: u32 = 0x40;
        if buf.flags & BUF_FLAG_ERROR != 0 || bytes.len() < 16 {
            continue;
        }
        let payload = match mode.format {
            v4l2::PIX_MJPEG | v4l2::PIX_JPEG => Payload::Jpeg(bytes),
            v4l2::PIX_YUYV => Payload::Yuyv {
                data: bytes,
                stride,
            },
            _ => Payload::Nv12 {
                data: bytes,
                stride,
            },
        };
        let frame = Arc::new(Frame {
            at,
            width: mode.width as usize,
            height: mode.height as usize,
            payload,
        });
        subscribers.lock().unwrap().retain(|tx| {
            !matches!(
                tx.try_send(frame.clone()),
                Err(TrySendError::Disconnected(_))
            )
        });
    }
    Ok(())
}

//! The screen, through Huginn: which screens and windows there are, frames
//! of any of them, and a rectangle the person drags out.
//!
//! Huginn has no standard capture protocol (no `wlr-screencopy`, no
//! `ext-image-copy-capture`). What it has is `raven_capture_v1` in its own
//! `raven_shell_v1`, version 4, which this module speaks on a Wayland
//! connection of its own on a thread of its own — GTK's connection is GTK's
//! business, and a capture must keep running while the main loop is busy
//! laying out a page.
//!
//! The client supplies the memory: one `wl_shm` buffer per capture, sized by
//! the compositor's `buffer_size`, handed over with each `frame` request. A
//! capture delivers nothing it was not asked for, so the frame rate is set
//! here, by how often this thread asks.

use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wayland_client::protocol::{wl_buffer, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};

pub mod protocol {
    #![allow(dead_code, non_upper_case_globals, non_camel_case_types, clippy::all)]
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/raven-shell-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/raven-shell-v1.xml");
}

use protocol::raven_capture_v1::{self, RavenCaptureV1};
use protocol::raven_output_layout_v1::{self, RavenOutputLayoutV1};
use protocol::raven_region_selection_v1::{self, RavenRegionSelectionV1};
use protocol::raven_shell_manager_v1::RavenShellManagerV1;

/// The manager version that has capture.
const CAPTURE_VERSION: u32 = 4;

/// A screen, as Huginn arranges them.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    /// Connector name: `eDP-1`, `HDMI-A-1`.
    pub name: String,
    pub x: i32,
    pub y: i32,
    /// Logical size.
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    /// Pixels: what a capture of the whole screen delivers.
    pub physical_width: i32,
    pub physical_height: i32,
    pub focused: bool,
}

impl Output {
    pub fn label(&self, index: usize) -> String {
        format!(
            "Display {} ({} × {})",
            index + 1,
            self.physical_width,
            self.physical_height
        )
    }
}

/// A window, from the compositor's window list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// Stable for the window's life; what `capture_window` takes.
    pub identifier: String,
    pub title: String,
    pub app_id: String,
}

/// A rectangle of one screen, logical pixels from its top-left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub output: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Output(String),
    Window(String),
    Region(Region),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    pub cursor: bool,
    pub clicks: bool,
}

/// What the screen thread reports to the window.
#[derive(Debug, Clone)]
pub enum Event {
    /// Connected. `capture` is false when the compositor is a Huginn older
    /// than capture, or not Huginn at all.
    Ready {
        capture: bool,
    },
    /// No compositor to talk to.
    Unavailable(String),
    Outputs(Vec<Output>),
    Windows(Vec<Window>),
    /// `None` when the selection was cancelled.
    Region(Option<Region>),
    /// A capture's source went away: the window closed, the screen was
    /// unplugged.
    Stopped(u64),
}

/// One frame of a capture: tightly packed BGRA, as the compositor drew it.
#[derive(Debug)]
pub struct Frame {
    pub at: Instant,
    pub width: usize,
    pub height: usize,
    pub bgra: Vec<u8>,
}

type Subscribers = Arc<Mutex<Vec<SyncSender<Arc<Frame>>>>>;

enum Command {
    Start {
        id: u64,
        source: Source,
        options: Options,
        interval: Duration,
        subscribers: Subscribers,
    },
    Interval(u64, Duration),
    Stop(u64),
    SelectRegion,
}

/// The handle the window holds.
#[derive(Debug, Clone)]
pub struct Screen {
    commands: Sender<Command>,
    wake: Arc<EventFd>,
    next_id: Arc<std::sync::atomic::AtomicU64>,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Command")
    }
}

/// Frames of one source for as long as this lives.
#[derive(Debug)]
pub struct Capture {
    pub id: u64,
    subscribers: Subscribers,
    screen: Screen,
}

impl Capture {
    /// Frames from now on; at most `depth` wait, the rest are dropped.
    pub fn subscribe(&self, depth: usize) -> Receiver<Arc<Frame>> {
        let (tx, rx) = mpsc::sync_channel(depth.max(1));
        self.subscribers.lock().unwrap().push(tx);
        rx
    }

    /// Ask for frames this often from now on.
    pub fn set_fps(&self, fps: f64) {
        self.screen.send(Command::Interval(self.id, interval(fps)));
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.screen.send(Command::Stop(self.id));
    }
}

fn interval(fps: f64) -> Duration {
    Duration::from_secs_f64(1.0 / fps.clamp(0.5, 144.0))
}

impl Screen {
    /// Start the screen thread. Whether it connected arrives as an
    /// [`Event`] on `events`.
    pub fn connect(events: async_channel::Sender<Event>) -> Self {
        let (commands, inbox) = mpsc::channel();
        let wake = Arc::new(EventFd::new());
        let thread_wake = wake.clone();
        std::thread::Builder::new()
            .name("screen".into())
            .spawn(move || {
                if let Err(e) = run(inbox, &thread_wake, &events) {
                    let _ = events.send_blocking(Event::Unavailable(format!("{e:#}")));
                }
            })
            .expect("spawning the screen thread");
        Self {
            commands,
            wake,
            next_id: Arc::default(),
        }
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            self.wake.signal();
        }
    }

    /// Capture `source` at `fps` until the returned handle is dropped.
    pub fn capture(&self, source: Source, options: Options, fps: f64) -> Capture {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let subscribers: Subscribers = Arc::default();
        self.send(Command::Start {
            id,
            source,
            options,
            interval: interval(fps),
            subscribers: subscribers.clone(),
        });
        Capture {
            id,
            subscribers,
            screen: self.clone(),
        }
    }

    /// Put up the compositor's region selection; the answer arrives as
    /// [`Event::Region`].
    pub fn select_region(&self) {
        self.send(Command::SelectRegion);
    }
}

/// An eventfd: how the window wakes the screen thread out of `poll`.
#[derive(Debug)]
struct EventFd(OwnedFd);

impl EventFd {
    fn new() -> Self {
        // SAFETY: eventfd returns a new descriptor or -1.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        assert!(fd >= 0, "eventfd: {}", std::io::Error::last_os_error());
        // SAFETY: fd is a fresh descriptor this value now owns.
        Self(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn signal(&self) {
        let one: u64 = 1;
        // SAFETY: writing eight bytes from a live u64 to our eventfd.
        unsafe { libc::write(self.0.as_raw_fd(), (&one as *const u64).cast(), 8) };
    }

    fn drain(&self) {
        let mut n: u64 = 0;
        // SAFETY: reading eight bytes into a live u64 from our eventfd.
        unsafe { libc::read(self.0.as_raw_fd(), (&mut n as *mut u64).cast(), 8) };
    }

    fn raw(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

/// A `wl_shm` buffer: a memfd mapped into this process and shared with the
/// compositor.
struct ShmBuffer {
    _fd: OwnedFd,
    map: *mut u8,
    len: usize,
    pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
    width: usize,
    height: usize,
    stride: usize,
}

impl ShmBuffer {
    fn new(
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<State>,
        width: usize,
        height: usize,
    ) -> anyhow::Result<Self> {
        let stride = width * 4;
        let len = stride * height;
        anyhow::ensure!(len > 0, "a capture of no size");
        // SAFETY: memfd_create with a static NUL-terminated name.
        let raw =
            unsafe { libc::memfd_create(c"raven-camera-capture".as_ptr(), libc::MFD_CLOEXEC) };
        anyhow::ensure!(
            raw >= 0,
            "memfd_create: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: raw is a fresh descriptor this value now owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: sizing our own memfd.
        anyhow::ensure!(
            unsafe { libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) } == 0,
            "ftruncate: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: mapping `len` bytes of the memfd just sized to `len`.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        anyhow::ensure!(
            map != libc::MAP_FAILED,
            "mmap: {}",
            std::io::Error::last_os_error()
        );
        let pool = shm.create_pool(fd.as_fd(), len as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            qh,
            (),
        );
        Ok(Self {
            _fd: fd,
            map: map.cast(),
            len,
            pool,
            buffer,
            width,
            height,
            stride,
        })
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the mapping is `len` bytes and lives as long as self; the
        // compositor only writes it between a frame request and its answer,
        // and this is read after the answer.
        unsafe { std::slice::from_raw_parts(self.map, self.len) }
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
        // SAFETY: unmapping the mapping made in `new`.
        unsafe { libc::munmap(self.map.cast(), self.len) };
    }
}

struct CaptureState {
    proxy: RavenCaptureV1,
    size: Option<(usize, usize)>,
    buffer: Option<ShmBuffer>,
    pending: bool,
    due: Instant,
    interval: Duration,
    subscribers: Subscribers,
}

#[derive(Default)]
struct ToplevelState {
    identifier: String,
    title: String,
    app_id: String,
}

struct State {
    manager: Option<RavenShellManagerV1>,
    manager_version: u32,
    shm: Option<wl_shm::WlShm>,
    toplevel_list: Option<ExtForeignToplevelListV1>,
    outputs: Vec<Output>,
    staged_outputs: Vec<Output>,
    toplevels: HashMap<u32, ToplevelState>,
    captures: HashMap<u64, CaptureState>,
    selection: Option<RavenRegionSelectionV1>,
    events: async_channel::Sender<Event>,
}

impl State {
    fn emit(&self, event: Event) {
        let _ = self.events.send_blocking(event);
    }

    fn emit_windows(&self) {
        let mut windows: Vec<Window> = self
            .toplevels
            .values()
            .filter(|t| !t.identifier.is_empty())
            .map(|t| Window {
                identifier: t.identifier.clone(),
                title: t.title.clone(),
                app_id: t.app_id.clone(),
            })
            .collect();
        windows.sort_by_key(|w| w.title.to_lowercase());
        self.emit(Event::Windows(windows));
    }
}

fn run(
    inbox: Receiver<Command>,
    wake: &EventFd,
    events: &async_channel::Sender<Event>,
) -> anyhow::Result<()> {
    let conn = Connection::connect_to_env()
        .map_err(|e| anyhow::anyhow!("no Wayland compositor to capture from: {e}"))?;
    let mut queue: EventQueue<State> = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut state = State {
        manager: None,
        manager_version: 0,
        shm: None,
        toplevel_list: None,
        outputs: Vec::new(),
        staged_outputs: Vec::new(),
        toplevels: HashMap::new(),
        captures: HashMap::new(),
        selection: None,
        events: events.clone(),
    };
    queue.roundtrip(&mut state)?;
    let capture = state.manager_version >= CAPTURE_VERSION && state.shm.is_some();
    match &state.manager {
        Some(manager) if state.manager_version >= 3 => {
            manager.get_output_layout(&qh, ());
        }
        _ => {}
    }
    state.emit(Event::Ready { capture });
    queue.roundtrip(&mut state)?;

    loop {
        queue.dispatch_pending(&mut state)?;
        conn.flush()?;

        // Ask for every frame that is due, and work out how long until the
        // next one will be.
        let now = Instant::now();
        let mut timeout = Duration::from_secs(1);
        for cap in state.captures.values_mut() {
            let Some(buffer) = &cap.buffer else { continue };
            if cap.pending {
                continue;
            }
            if cap.due <= now {
                cap.proxy.frame(&buffer.buffer);
                cap.pending = true;
                // Paced from when it was asked for, not when it arrived: a
                // still screen answers late, and the next frame should not
                // then be late as well.
                cap.due = (cap.due + cap.interval).max(now);
            } else {
                timeout = timeout.min(cap.due - now);
            }
        }
        conn.flush()?;

        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let mut fds = [
            libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake.raw(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ms = timeout.as_millis().clamp(1, 1000) as i32;
        // SAFETY: two valid pollfds.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
        if r > 0 && fds[0].revents & libc::POLLIN != 0 {
            guard.read()?;
        } else {
            drop(guard);
        }
        if fds[0].revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            anyhow::bail!("the compositor closed the connection");
        }
        if fds[1].revents & libc::POLLIN != 0 {
            wake.drain();
        }
        loop {
            match inbox.try_recv() {
                Ok(command) => handle(&mut state, &qh, command),
                Err(mpsc::TryRecvError::Empty) => break,
                // The window is gone: so is this thread.
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }
    }
}

fn handle(state: &mut State, qh: &QueueHandle<State>, command: Command) {
    match command {
        Command::Start {
            id,
            source,
            options,
            interval,
            subscribers,
        } => {
            let Some(manager) = &state.manager else {
                state.emit(Event::Stopped(id));
                return;
            };
            if state.manager_version < CAPTURE_VERSION {
                state.emit(Event::Stopped(id));
                return;
            }
            let mut bits = raven_capture_v1::Options::empty();
            if options.cursor {
                bits |= raven_capture_v1::Options::Cursor;
            }
            if options.clicks {
                bits |= raven_capture_v1::Options::Clicks;
            }
            let proxy = match source {
                Source::Output(name) => manager.capture_output(name, bits, qh, id),
                Source::Window(identifier) => manager.capture_window(identifier, bits, qh, id),
                Source::Region(r) => {
                    manager.capture_region(r.output, r.x, r.y, r.width, r.height, bits, qh, id)
                }
            };
            state.captures.insert(
                id,
                CaptureState {
                    proxy,
                    size: None,
                    buffer: None,
                    pending: false,
                    due: Instant::now(),
                    interval,
                    subscribers,
                },
            );
        }
        Command::Interval(id, interval) => {
            if let Some(cap) = state.captures.get_mut(&id) {
                cap.interval = interval;
                cap.due = cap.due.min(Instant::now() + interval);
            }
        }
        Command::Stop(id) => {
            if let Some(cap) = state.captures.remove(&id) {
                cap.proxy.destroy();
            }
        }
        Command::SelectRegion => match &state.manager {
            Some(manager) if state.manager_version >= CAPTURE_VERSION => {
                if let Some(old) = state.selection.take() {
                    old.destroy();
                }
                state.selection = Some(manager.select_region(qh, ()));
            }
            _ => state.emit(Event::Region(None)),
        },
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "raven_shell_manager_v1" => {
                    let v = version.min(CAPTURE_VERSION);
                    state.manager = Some(registry.bind(name, v, qh, ()));
                    state.manager_version = v;
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "ext_foreign_toplevel_list_v1" => {
                    state.toplevel_list = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<RavenShellManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &RavenShellManagerV1,
        _: <RavenShellManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<RavenOutputLayoutV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &RavenOutputLayoutV1,
        event: raven_output_layout_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            raven_output_layout_v1::Event::Output {
                name,
                x,
                y,
                width,
                height,
                scale,
                physical_width,
                physical_height,
                focused,
                ..
            } => state.staged_outputs.push(Output {
                name,
                x,
                y,
                width,
                height,
                scale,
                physical_width,
                physical_height,
                focused: focused != 0,
            }),
            raven_output_layout_v1::Event::Done => {
                let mut outputs = std::mem::take(&mut state.staged_outputs);
                outputs.sort_by_key(|o| (o.x, o.y));
                if outputs != state.outputs {
                    state.outputs = outputs.clone();
                    state.emit(Event::Outputs(outputs));
                }
            }
        }
    }
}

impl Dispatch<RavenCaptureV1, u64> for State {
    fn event(
        state: &mut Self,
        _: &RavenCaptureV1,
        event: raven_capture_v1::Event,
        id: &u64,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let shm = state.shm.clone();
        let Some(cap) = state.captures.get_mut(id) else {
            return;
        };
        match event {
            raven_capture_v1::Event::BufferSize { width, height } => {
                let size = (width as usize, height as usize);
                cap.size = Some(size);
                // A pending frame is about to be answered with failed; the
                // buffer it holds must not be freed under the compositor
                // until then, so the old one is kept until that answer.
                if !cap.pending {
                    cap.buffer = None;
                    if let Some(shm) = &shm {
                        match ShmBuffer::new(shm, qh, size.0, size.1) {
                            Ok(b) => cap.buffer = Some(b),
                            Err(e) => tracing::warn!("capture buffer: {e:#}"),
                        }
                    }
                }
            }
            raven_capture_v1::Event::Ready {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
            } => {
                cap.pending = false;
                let secs = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                let at = monotonic_to_instant(secs, tv_nsec);
                if let Some(buffer) = &cap.buffer {
                    let bytes = buffer.bytes();
                    let packed = if buffer.stride == buffer.width * 4 {
                        bytes.to_vec()
                    } else {
                        bytes
                            .chunks(buffer.stride)
                            .flat_map(|row| &row[..buffer.width * 4])
                            .copied()
                            .collect()
                    };
                    let frame = Arc::new(Frame {
                        at,
                        width: buffer.width,
                        height: buffer.height,
                        bgra: packed,
                    });
                    cap.subscribers.lock().unwrap().retain(|tx| {
                        !matches!(
                            tx.try_send(frame.clone()),
                            Err(TrySendError::Disconnected(_))
                        )
                    });
                }
            }
            raven_capture_v1::Event::Failed => {
                cap.pending = false;
                // After a size change, now the old buffer is free.
                let stale = match (&cap.buffer, cap.size) {
                    (Some(b), Some((w, h))) => b.width != w || b.height != h,
                    (None, Some(_)) => true,
                    _ => false,
                };
                if stale {
                    cap.buffer = None;
                    if let (Some(shm), Some((w, h))) = (&shm, cap.size) {
                        match ShmBuffer::new(shm, qh, w, h) {
                            Ok(b) => cap.buffer = Some(b),
                            Err(e) => tracing::warn!("capture buffer: {e:#}"),
                        }
                    }
                }
                cap.due = Instant::now();
            }
            raven_capture_v1::Event::Stopped => {
                if let Some(cap) = state.captures.remove(id) {
                    cap.proxy.destroy();
                }
                state.emit(Event::Stopped(*id));
            }
        }
    }
}

impl Dispatch<RavenRegionSelectionV1, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &RavenRegionSelectionV1,
        event: raven_region_selection_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let answer = match event {
            raven_region_selection_v1::Event::Selected {
                output,
                x,
                y,
                width,
                height,
            } => Some(Region {
                output,
                x,
                y,
                width,
                height,
            }),
            _ => None,
        };
        proxy.destroy();
        if state.selection.as_ref() == Some(proxy) {
            state.selection = None;
        }
        state.emit(Event::Region(answer));
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ExtForeignToplevelListV1,
        _: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    wayland_client::event_created_child!(State, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let key = handle.id().protocol_id();
        match event {
            ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } => {
                state.toplevels.entry(key).or_default().identifier = identifier;
            }
            ext_foreign_toplevel_handle_v1::Event::Title { title } => {
                state.toplevels.entry(key).or_default().title = title;
            }
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                state.toplevels.entry(key).or_default().app_id = app_id;
            }
            ext_foreign_toplevel_handle_v1::Event::Done => state.emit_windows(),
            ext_foreign_toplevel_handle_v1::Event::Closed => {
                state.toplevels.remove(&key);
                handle.destroy();
                state.emit_windows();
            }
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($ty:ty),*) => {$(
        impl Dispatch<$ty, ()> for State {
            fn event(
                _: &mut Self,
                _: &$ty,
                _: <$ty as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
}
ignore_events!(wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_buffer::WlBuffer);

/// A CLOCK_MONOTONIC time from the compositor as an [`Instant`]. On Linux
/// both are CLOCK_MONOTONIC, but std offers no way to build an Instant from
/// a timespec, so the offset from now is measured instead.
fn monotonic_to_instant(secs: u64, nsec: u32) -> Instant {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime into a live timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    let now_ns = now.tv_sec as i128 * 1_000_000_000 + now.tv_nsec as i128;
    let then_ns = secs as i128 * 1_000_000_000 + i128::from(nsec);
    let ago = now_ns - then_ns;
    // A timestamp from the future, or implausibly old, is a compositor bug;
    // "now" is the honest answer.
    if (0..5_000_000_000).contains(&ago) {
        Instant::now() - Duration::from_nanos(ago as u64)
    } else {
        Instant::now()
    }
}

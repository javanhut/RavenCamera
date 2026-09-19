//! The GTK side. Everything here runs on the main thread; cameras, the
//! screen, sound and the encoders run on threads of their own and report
//! back through channels polled by futures on the main loop.
//!
//! [`App`] owns what more than one page shares: the settings, the camera
//! and screen sources (so the Capture page and the Camera page do not open
//! the same camera twice), the recording in progress, and the recordings
//! being finished. Pages listen for the [`Topic`]s they draw.

pub mod pages;
pub mod preview;
pub mod theme;
pub mod widgets;
pub mod window;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::gdk::prelude::PaintableExt;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::audio;
use crate::camera;
use crate::library;
use crate::record::export;
use crate::record::live::{Live, Plan};
use crate::record::session::{self, Kind, Manifest, State};
use crate::screen::{self, Screen};
use crate::settings::{PostAction, Settings};

pub const APP_ID: &str = "com.ravencamera.Raven";

/// What changed, for the pages that draw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    /// Screens, windows, or whether capture is possible at all.
    Screen,
    Cameras,
    Audio,
    /// A file was added, removed or renamed.
    Library,
    /// Recording started, stopped, paused or resumed.
    Recording,
    /// Finishing jobs came, progressed or went.
    Jobs,
    Settings,
}

/// What the screen thread has told us.
#[derive(Debug, Clone, Default)]
pub struct ScreenState {
    /// `None` until the screen thread has connected (or failed to).
    pub capture: Option<bool>,
    pub unavailable: Option<String>,
    pub outputs: Vec<screen::Output>,
    pub windows: Vec<screen::Window>,
}

/// A recording being turned into a video.
#[derive(Debug)]
pub struct Job {
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub progress: Cell<f64>,
    pub cancel: Arc<AtomicBool>,
    pub failed: RefCell<Option<String>>,
    pub running: Cell<bool>,
}

type Listener = (Topic, Rc<dyn Fn()>);
type RegionCallback = Box<dyn FnOnce(Option<screen::Region>)>;
type Navigate = Box<dyn Fn(&str)>;

pub struct App {
    pub gtk_app: adw::Application,
    pub settings: RefCell<Settings>,
    pub toasts: adw::ToastOverlay,
    pub screen: Screen,
    pub screen_state: RefCell<ScreenState>,
    pub cameras: RefCell<Vec<camera::Device>>,
    pub audio: RefCell<audio::Devices>,
    pub recording: RefCell<Option<Live>>,
    pub jobs: RefCell<Vec<Rc<Job>>>,
    /// Interrupted recordings found at startup, waiting for a decision.
    pub leftovers: RefCell<Vec<(PathBuf, Manifest)>>,
    camera: RefCell<Option<Arc<camera::Stream>>>,
    camera_users: Cell<u32>,
    /// The camera is stopped while a full-size photo is taken.
    camera_busy: Cell<bool>,
    /// A settings save is due shortly (see [`App::save_settings_soon`]).
    save_due: Cell<bool>,
    capture: RefCell<Option<(screen::Source, screen::Options, Arc<screen::Capture>)>>,
    window: RefCell<Option<adw::ApplicationWindow>>,
    listeners: RefCell<Vec<Listener>>,
    navigate: RefCell<Option<Navigate>>,
    /// A file the Media page should select when it next draws.
    pub highlight: RefCell<Option<PathBuf>>,
    region_callback: RefCell<Option<RegionCallback>>,
}

impl App {
    pub fn window(&self) -> Option<adw::ApplicationWindow> {
        self.window.borrow().clone()
    }

    pub fn on(&self, topic: Topic, f: impl Fn() + 'static) {
        self.listeners.borrow_mut().push((topic, Rc::new(f)));
    }

    pub fn notify(&self, topic: Topic) {
        // A snapshot, not a held borrow: a listener may register another
        // listener or notify in turn.
        let matching: Vec<Rc<dyn Fn()>> = self
            .listeners
            .borrow()
            .iter()
            .filter(|(t, _)| *t == topic)
            .map(|(_, f)| f.clone())
            .collect();
        for f in matching {
            f();
        }
    }

    pub fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    pub fn error(&self, context: &str, err: &anyhow::Error) {
        tracing::warn!("{context}: {err:#}");
        let t = adw::Toast::new(&format!("{context}: {err}"));
        t.set_timeout(6);
        self.toasts.add_toast(t);
    }

    /// Change the settings, save them, and tell the pages.
    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) {
        f(&mut self.settings.borrow_mut());
        if let Err(e) = self.settings.borrow().save() {
            self.error("Could not save settings", &e);
        }
        self.notify(Topic::Settings);
    }

    pub fn navigate(&self, page: &str) {
        if let Some(f) = self.navigate.borrow().as_ref() {
            f(page);
        }
    }

    // ── Cameras ───────────────────────────────────────────────────────

    /// Look for cameras again: at startup, and whenever `/dev` changes.
    pub fn rescan_cameras(self: &Rc<Self>) {
        let app = self.clone();
        spawn(camera::devices, move |devices| {
            let before: Vec<String> = app
                .cameras
                .borrow()
                .iter()
                .map(|d| d.path.display().to_string())
                .collect();
            let after: Vec<String> = devices
                .iter()
                .map(|d| d.path.display().to_string())
                .collect();
            let added = devices
                .iter()
                .filter(|d| !before.contains(&d.path.display().to_string()))
                .map(|d| d.name.clone())
                .collect::<Vec<_>>();
            *app.cameras.borrow_mut() = devices;
            if before != after {
                if !before.is_empty() {
                    for name in added {
                        app.toast(&format!("{name} connected"));
                    }
                }
                // The camera in use was unplugged: let it go.
                let gone = app
                    .camera
                    .borrow()
                    .as_ref()
                    .is_some_and(|s| !after.contains(&s.device.path.display().to_string()));
                if gone && app.recording.borrow().is_none() {
                    app.camera.borrow_mut().take();
                }
                app.notify(Topic::Cameras);
            }
        });
    }

    /// The camera the settings name, or the first there is.
    pub fn chosen_camera(&self) -> Option<camera::Device> {
        let key = self.settings.borrow().camera.device.clone();
        let cams = self.cameras.borrow();
        cams.iter()
            .find(|d| d.key() == key)
            .or_else(|| cams.first())
            .cloned()
    }

    /// The running camera stream, started (or restarted, if the settings
    /// now name another camera or size) if need be. Pages call
    /// [`App::camera_acquire`] while they show it.
    pub fn camera_stream(&self) -> anyhow::Result<Arc<camera::Stream>> {
        if self.camera_busy.get() {
            anyhow::bail!("the camera is taking a photo");
        }
        let device = self
            .chosen_camera()
            .ok_or_else(|| anyhow::anyhow!("no camera is connected"))?;
        let resolution = self.settings.borrow().camera.resolution.clone();
        let mode = device
            .mode_for(&resolution)
            .ok_or_else(|| anyhow::anyhow!("the camera offers no format this app can read"))?;
        if let Some(s) = self.camera.borrow().as_ref() {
            if s.device.path == device.path
                && s.mode.width == mode.width
                && s.mode.height == mode.height
                && s.ended().is_none()
            {
                return Ok(s.clone());
            }
        }
        // Drop the old stream first: the same camera cannot stream twice.
        self.camera.borrow_mut().take();
        camera::controls::restore(
            &device.path,
            self.settings.borrow().camera.controls.get(&device.key()),
        );
        let stream = Arc::new(camera::Stream::start(&device, mode)?);
        *self.camera.borrow_mut() = Some(stream.clone());
        Ok(stream)
    }

    /// A page is showing the camera.
    pub fn camera_acquire(&self) {
        self.camera_users.set(self.camera_users.get() + 1);
    }

    /// A page stopped showing the camera. When none shows it and nothing is
    /// recording it, it is closed, and its light goes out.
    pub fn camera_release(&self) {
        let n = self.camera_users.get().saturating_sub(1);
        self.camera_users.set(n);
        if n == 0 {
            self.camera.borrow_mut().take();
        }
    }

    /// Whether the camera is stopped for a full-size photo; pages leave
    /// their previews as they are until [`Topic::Cameras`] says it is back.
    pub fn camera_busy(&self) -> bool {
        self.camera_busy.get()
    }

    /// Stop the camera so a photo can be taken in another mode. `false` if
    /// it cannot be stopped now, because a recording is using it.
    pub fn camera_pause(&self) -> bool {
        if self.camera_busy.get() || self.recording.borrow().is_some() {
            return false;
        }
        self.camera_busy.set(true);
        self.camera.borrow_mut().take();
        true
    }

    /// The photo is taken: the pages start their previews again.
    pub fn camera_resume(&self) {
        self.camera_busy.set(false);
        self.notify(Topic::Cameras);
    }

    /// Remember a picture control changed by hand on `device`. A slider
    /// sends a stream of these, so the file is written once it settles
    /// rather than for each, and no page is told: none shows them but the
    /// Camera page, which already does.
    pub fn remember_control(self: &Rc<Self>, device: &camera::Device, id: u32, value: i32) {
        self.settings
            .borrow_mut()
            .camera
            .controls
            .entry(device.key())
            .or_default()
            .insert(camera::controls::key(id), value);
        self.save_settings_soon();
    }

    /// Forget the controls changed by hand on `device`.
    pub fn forget_controls(self: &Rc<Self>, device: &camera::Device) {
        self.settings
            .borrow_mut()
            .camera
            .controls
            .remove(&device.key());
        self.save_settings_soon();
    }

    fn save_settings_soon(self: &Rc<Self>) {
        if self.save_due.replace(true) {
            return;
        }
        let app = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(600), move || {
            app.save_due.set(false);
            if let Err(e) = app.settings.borrow().save() {
                app.error("Could not save settings", &e);
            }
        });
    }

    /// Restart the camera, for a new device or size.
    pub fn camera_restart(&self) {
        self.camera.borrow_mut().take();
        self.notify(Topic::Cameras);
    }

    // ── The screen ────────────────────────────────────────────────────

    /// A capture of `source`, shared: asking again for the same source and
    /// options returns the one already running.
    pub fn screen_capture(
        &self,
        source: screen::Source,
        options: screen::Options,
        fps: f64,
    ) -> Arc<screen::Capture> {
        if let Some((s, o, c)) = self.capture.borrow().as_ref() {
            if *s == source && *o == options {
                c.set_fps(fps);
                return c.clone();
            }
        }
        let capture = Arc::new(self.screen.capture(source.clone(), options, fps));
        *self.capture.borrow_mut() = Some((source, options, capture.clone()));
        capture
    }

    /// Stop the shared screen capture, unless a recording holds it.
    pub fn screen_release(&self) {
        self.capture.borrow_mut().take();
    }

    fn on_screen_event(self: &Rc<Self>, event: screen::Event) {
        let mut st = self.screen_state.borrow_mut();
        match event {
            screen::Event::Ready { capture } => {
                st.capture = Some(capture);
                st.unavailable = None;
            }
            screen::Event::Unavailable(why) => {
                st.capture = Some(false);
                st.unavailable = Some(why);
            }
            screen::Event::Outputs(o) => st.outputs = o,
            screen::Event::Windows(w) => st.windows = w,
            screen::Event::Region(r) => {
                drop(st);
                if let Some(cb) = self.region_callback.borrow_mut().take() {
                    cb(r);
                }
                return;
            }
            screen::Event::Stopped(id) => {
                let ours = self
                    .capture
                    .borrow()
                    .as_ref()
                    .is_some_and(|(_, _, c)| c.id == id);
                if ours {
                    self.capture.borrow_mut().take();
                }
                drop(st);
                if self.recording.borrow().as_ref().is_some() && ours {
                    self.toast("The window being recorded closed; the recording has stopped");
                    self.stop_recording();
                }
                self.notify(Topic::Screen);
                return;
            }
        }
        drop(st);
        self.notify(Topic::Screen);
    }

    /// Ask the compositor for a rectangle; `done` gets it, or `None`.
    pub fn select_region(&self, done: impl FnOnce(Option<screen::Region>) + 'static) {
        *self.region_callback.borrow_mut() = Some(Box::new(done));
        self.screen.select_region();
    }

    // ── Sound ─────────────────────────────────────────────────────────

    pub fn rescan_audio(self: &Rc<Self>) {
        let app = self.clone();
        spawn(audio::devices, move |devices| {
            *app.audio.borrow_mut() = devices;
            app.notify(Topic::Audio);
        });
    }

    // ── Recording ─────────────────────────────────────────────────────

    pub fn is_recording(&self) -> bool {
        self.recording.borrow().is_some()
    }

    pub fn start_recording(self: &Rc<Self>, plan: Plan) {
        if self.is_recording() {
            return;
        }
        let hides = plan.kind != Kind::Camera
            && plan.kind != Kind::Audio
            && self.settings.borrow().recording.hide_window;
        let result = Live::start(plan, &self.settings.borrow());
        match result {
            Ok(live) => {
                *self.recording.borrow_mut() = Some(live);
                self.notify(Topic::Recording);
                if hides {
                    if let Some(w) = self.window() {
                        w.minimize();
                    }
                }
                let app = self.clone();
                // Keep the timer and meters moving.
                glib::timeout_add_local(Duration::from_millis(200), move || {
                    if app.is_recording() {
                        app.notify(Topic::Recording);
                        glib::ControlFlow::Continue
                    } else {
                        glib::ControlFlow::Break
                    }
                });
            }
            Err(e) => self.error("Could not start recording", &e),
        }
    }

    pub fn set_paused(&self, paused: bool) {
        if let Some(live) = self.recording.borrow().as_ref() {
            live.set_paused(paused);
        }
        self.notify(Topic::Recording);
    }

    /// Stop recording and start finishing it.
    pub fn stop_recording(self: &Rc<Self>) {
        let Some(live) = self.recording.borrow_mut().take() else {
            return;
        };
        // The preview capture goes back to its own pace.
        if let Some((_, _, c)) = self.capture.borrow().as_ref() {
            c.set_fps(preview::SCREEN_FPS);
        }
        match live.stop() {
            Ok((dir, manifest)) => self.finish(dir, manifest),
            Err(e) => self.error("The recording could not be stopped cleanly", &e),
        }
        if self.camera_users.get() == 0 {
            self.camera.borrow_mut().take();
        }
        self.notify(Topic::Recording);
        if let Some(w) = self.window() {
            w.present();
        }
    }

    /// Turn a stopped recording into its file, in the background.
    pub fn finish(self: &Rc<Self>, dir: PathBuf, manifest: Manifest) {
        if self.jobs.borrow().iter().any(|j| j.dir == dir) {
            return;
        }
        let job = Rc::new(Job {
            dir: dir.clone(),
            manifest: manifest.clone(),
            progress: Cell::new(0.0),
            cancel: Arc::new(AtomicBool::new(false)),
            failed: RefCell::new(None),
            running: Cell::new(true),
        });
        self.jobs.borrow_mut().push(job.clone());
        self.leftovers.borrow_mut().retain(|(d, _)| *d != dir);
        self.notify(Topic::Jobs);

        let (tx, rx) = async_channel::unbounded::<Result<f64, Result<export::Finished, String>>>();
        let cancel = job.cancel.clone();
        std::thread::Builder::new()
            .name("finish".into())
            .spawn(move || {
                let progress_tx = tx.clone();
                let progress = move |p: f64| {
                    let _ = progress_tx.try_send(Ok(p));
                };
                let result = export::finish(&dir, &manifest, &progress, &cancel)
                    .map_err(|e| format!("{e:#}"));
                if result.is_ok() {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                let _ = tx.send_blocking(Err(result));
            })
            .expect("spawning the finishing thread");

        let app = self.clone();
        // The app stays alive until the file is written, even with its
        // window closed.
        let hold = self.gtk_app.hold();
        glib::spawn_future_local(async move {
            let _hold = hold;
            while let Ok(msg) = rx.recv().await {
                match msg {
                    Ok(p) => {
                        job.progress.set(p);
                        app.notify(Topic::Jobs);
                    }
                    Err(result) => {
                        job.running.set(false);
                        app.finished(&job, result);
                        break;
                    }
                }
            }
        });
    }

    fn finished(self: &Rc<Self>, job: &Rc<Job>, result: Result<export::Finished, String>) {
        match result {
            Ok(done) => {
                self.jobs.borrow_mut().retain(|j| !Rc::ptr_eq(j, job));
                let title = match (&job.manifest.kind, job.manifest.subject.is_empty()) {
                    (Kind::Window, false) => format!("Window · {}", job.manifest.subject),
                    (kind, _) => kind.title().to_owned(),
                };
                let kind = if job.manifest.kind == Kind::Audio {
                    library::Kind::Audio
                } else {
                    library::Kind::Video
                };
                library::remember(
                    &done.path,
                    &title,
                    kind,
                    Some(done.length),
                    done.thumbnail.as_ref(),
                );
                self.notify(Topic::Jobs);
                self.notify(Topic::Library);
                self.after_capture(&done.path, "Recording saved");
            }
            Err(e) if job.cancel.load(Ordering::Relaxed) => {
                self.jobs.borrow_mut().retain(|j| !Rc::ptr_eq(j, job));
                let _ = e;
                self.notify(Topic::Jobs);
            }
            Err(e) => {
                tracing::warn!("finishing {}: {e}", job.dir.display());
                *job.failed.borrow_mut() = Some(e.clone());
                self.notify(Topic::Jobs);
                let t = adw::Toast::new(&format!("A recording could not be finished: {e}"));
                t.set_timeout(0);
                self.toasts.add_toast(t);
            }
        }
    }

    /// Do what the settings say to after a capture is saved.
    pub fn after_capture(self: &Rc<Self>, path: &Path, what: &str) {
        let action = self.settings.borrow().post_action;
        match action {
            PostAction::ShowInMedia => {
                *self.highlight.borrow_mut() = Some(path.to_path_buf());
                self.notify(Topic::Library);
                let toast = adw::Toast::builder()
                    .title(format!(
                        "{what}: {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ))
                    .button_label("Show")
                    .timeout(5)
                    .build();
                let app = self.clone();
                toast.connect_button_clicked(move |_| app.navigate("media"));
                self.toasts.add_toast(toast);
            }
            PostAction::OpenFile => open_path(path),
            PostAction::OpenFolder => {
                if let Some(dir) = path.parent() {
                    open_path(dir);
                }
            }
            PostAction::Nothing => {
                self.toast(what);
            }
        }
        let window_active = self.window().is_some_and(|w| w.is_active());
        if !window_active {
            let n = gio::Notification::new(what);
            n.set_body(Some(&path.display().to_string()));
            self.gtk_app.send_notification(None, &n);
        }
    }

    /// Discard an unfinished recording for good.
    pub fn discard(&self, dir: &Path) {
        if let Err(e) = std::fs::remove_dir_all(dir) {
            self.error("Could not delete the recording", &e.into());
        }
        self.leftovers.borrow_mut().retain(|(d, _)| d != dir);
        self.jobs.borrow_mut().retain(|j| j.dir != dir);
        self.notify(Topic::Jobs);
    }
}

/// Run `work` on a thread and `done` with its result on the main loop.
pub fn spawn<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    glib::spawn_future_local(async move {
        match gio::spawn_blocking(work).await {
            Ok(v) => done(v),
            Err(_) => tracing::error!("background task panicked"),
        }
    });
}

/// Open a file or folder in the application the desktop says handles it.
pub fn open_path(path: &Path) {
    let uri = gio::File::for_path(path).uri();
    if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>) {
        tracing::warn!("opening {}: {e}", path.display());
    }
}

/// Show a file selected in the file manager.
pub fn show_in_folder(path: &Path) {
    let uri = gio::File::for_path(path).uri();
    let shown = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .and_then(|bus| {
            bus.call_sync(
                Some("org.freedesktop.FileManager1"),
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1",
                "ShowItems",
                Some(&(vec![uri.to_string()], "").to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                2000,
                gio::Cancellable::NONE,
            )
        })
        .is_ok();
    if !shown {
        if let Some(dir) = path.parent() {
            open_path(dir);
        }
    }
}

pub fn run() -> glib::ExitCode {
    let gtk_app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    let state: Rc<RefCell<Option<Rc<App>>>> = Rc::default();
    {
        let state = state.clone();
        gtk_app.connect_command_line(move |gtk_app, cmd| {
            let args: Vec<String> = cmd
                .arguments()
                .iter()
                .skip(1)
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let app = ensure(gtk_app, &state);
            let has = |f: &str| args.iter().any(|a| a == f);
            if has("--stop") {
                if app.is_recording() {
                    app.stop_recording();
                } else if let Some(w) = app.window() {
                    w.present();
                }
            } else if has("--screenshot") {
                app.navigate("screenshot");
                if let Some(w) = app.window() {
                    w.present();
                }
            } else {
                for page in [
                    "capture",
                    "record",
                    "screenshot",
                    "camera",
                    "media",
                    "settings",
                ] {
                    if has(&format!("--{page}")) {
                        app.navigate(page);
                    }
                }
                if let Some(w) = app.window() {
                    w.present();
                }
            }
            glib::ExitCode::SUCCESS
        });
    }
    gtk_app.run()
}

/// The one [`App`] of this process, built on first use.
fn ensure(gtk_app: &adw::Application, state: &Rc<RefCell<Option<Rc<App>>>>) -> Rc<App> {
    if let Some(app) = state.borrow().as_ref() {
        return app.clone();
    }
    let glass = theme::load();
    let (tx, rx) = async_channel::unbounded();
    let app = Rc::new(App {
        gtk_app: gtk_app.clone(),
        settings: RefCell::new(Settings::load()),
        toasts: adw::ToastOverlay::new(),
        screen: Screen::connect(tx),
        screen_state: RefCell::default(),
        cameras: RefCell::default(),
        audio: RefCell::default(),
        recording: RefCell::new(None),
        jobs: RefCell::default(),
        leftovers: RefCell::default(),
        camera: RefCell::new(None),
        camera_users: Cell::new(0),
        camera_busy: Cell::new(false),
        save_due: Cell::new(false),
        capture: RefCell::new(None),
        window: RefCell::new(None),
        listeners: RefCell::default(),
        navigate: RefCell::new(None),
        highlight: RefCell::new(None),
        region_callback: RefCell::new(None),
    });
    {
        let app = app.clone();
        glib::spawn_future_local(async move {
            while let Ok(event) = rx.recv().await {
                app.on_screen_event(event);
            }
        });
    }
    let (window, navigate) = window::build(&app, glass);
    *app.window.borrow_mut() = Some(window.clone());
    *app.navigate.borrow_mut() = Some(Box::new(navigate));
    *state.borrow_mut() = Some(app.clone());

    app.rescan_cameras();
    app.rescan_audio();
    watch_devices(&app);
    if let Some(out) = std::env::var_os("RAVEN_CAMERA_SNAPSHOT") {
        snapshot_later(&window, std::path::PathBuf::from(out));
    }

    // Recordings left from last time: one stopped but not finished is
    // finished now; one a crash interrupted waits for the person to say.
    for (dir, manifest) in session::leftovers() {
        match manifest.state {
            State::Ready => app.finish(dir, manifest),
            State::Recording => app.leftovers.borrow_mut().push((dir, manifest)),
        }
    }
    if !app.leftovers.borrow().is_empty() {
        let n = app.leftovers.borrow().len();
        let toast = adw::Toast::builder()
            .title(if n == 1 {
                "A recording was interrupted. It can still be saved.".to_owned()
            } else {
                format!("{n} recordings were interrupted. They can still be saved.")
            })
            .button_label("Review")
            .timeout(0)
            .build();
        let a = app.clone();
        toast.connect_button_clicked(move |_| a.navigate("record"));
        app.toasts.add_toast(toast);
    }
    app
}

/// Watch `/dev` for cameras coming and going. The node appears a moment
/// before udev gives it its permissions, so the rescan waits for things to
/// settle.
fn watch_devices(app: &Rc<App>) {
    let dev = gio::File::for_path("/dev");
    let Ok(monitor) = dev.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
    else {
        return;
    };
    let pending: Rc<Cell<Option<glib::SourceId>>> = Rc::default();
    let weak = Rc::downgrade(app);
    monitor.connect_changed(move |_, file, _, _| {
        let is_video = file
            .basename()
            .and_then(|n| n.to_str().map(|s| s.starts_with("video")))
            .unwrap_or(false);
        if !is_video {
            return;
        }
        if let Some(id) = pending.take() {
            id.remove();
        }
        let weak = weak.clone();
        let pending2 = pending.clone();
        pending.set(Some(glib::timeout_add_local_once(
            Duration::from_millis(700),
            move || {
                pending2.set(None);
                if let Some(app) = weak.upgrade() {
                    app.rescan_cameras();
                }
            },
        )));
    });
    // Kept alive for the life of the app.
    std::mem::forget(monitor);
}

/// For development: render the window to a PNG a few seconds after it
/// opens, then quit. `RAVEN_CAMERA_SNAPSHOT=out.png raven-camera --media`.
fn snapshot_later(window: &adw::ApplicationWindow, out: PathBuf) {
    let window = window.clone();
    glib::timeout_add_local_once(Duration::from_secs(4), move || {
        // The content, not the toplevel: a toplevel's paintable draws
        // nothing of its own surface.
        let content = window.content();
        let paintable = gtk::WidgetPaintable::new(content.as_ref());
        let (w, h) = (window.width() as f64, window.height() as f64);
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, w, h);
        match (snapshot.to_node(), window.renderer()) {
            (Some(node), Some(renderer)) => {
                let texture = renderer.render_texture(node, None);
                if let Err(e) = texture.save_to_png(&out) {
                    tracing::warn!("snapshot: {e}");
                }
            }
            (node, renderer) => tracing::warn!(
                "snapshot: node {} renderer {} mapped {} size {}x{} suspended {}",
                node.is_some(),
                renderer.is_some(),
                window.is_mapped(),
                window.width(),
                window.height(),
                window.is_suspended()
            ),
        }
        if let Some(a) = window.application() {
            a.quit();
        }
    });
}

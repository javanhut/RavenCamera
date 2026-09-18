//! The sections of the window, and what more than one of them does: take a
//! still of the screen or the camera, count down before something starts.

pub mod camera;
pub mod capture;
pub mod media;
pub mod record;
pub mod screenshot;
pub mod settings;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::prelude::*;

use super::App;
use crate::library;
use crate::pixels;
use crate::screen;

pub struct Page {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
    pub widget: gtk::Widget,
    /// The page came into view: start its previews.
    pub on_show: Box<dyn Fn()>,
    /// It went out of view: stop them, and let go of the camera.
    pub on_hide: Box<dyn Fn()>,
}

pub fn all(app: &Rc<App>) -> Vec<Page> {
    vec![
        capture::page(app),
        record::page(app),
        screenshot::page(app),
        camera::page(app),
        media::page(app),
        settings::page(app),
    ]
}

/// Take one frame of `capture` and save it as a screenshot.
pub fn save_screen_still(app: &Rc<App>, capture: Arc<screen::Capture>, copy: bool) {
    let frames = capture.subscribe(1);
    let settings = app.settings.borrow().clone();
    let app = app.clone();
    super::spawn(
        move || -> anyhow::Result<(std::path::PathBuf, pixels::Rgba)> {
            // Held until a frame arrives, so the capture is still running.
            let _keep = capture;
            let frame = frames
                .recv_timeout(Duration::from_secs(3))
                .map_err(|_| anyhow::anyhow!("the compositor sent no picture"))?;
            let img = pixels::bgra_to_rgba(&frame.bgra, frame.width, frame.height, frame.width * 4);
            let path = library::save_still(
                &img,
                &settings,
                library::Kind::Screenshot,
                settings.screenshot.format,
            )?;
            Ok((path, img))
        },
        move |r| match r {
            Ok((path, img)) => {
                if copy {
                    copy_image(&img);
                }
                app.notify(super::Topic::Library);
                app.after_capture(
                    &path,
                    if copy {
                        "Screenshot saved and copied"
                    } else {
                        "Screenshot saved"
                    },
                );
            }
            Err(e) => app.error("Could not take the screenshot", &e),
        },
    );
}

/// Take the next camera frame and save it as a photo.
pub fn save_photo(app: &Rc<App>, stream: Arc<crate::camera::Stream>) {
    let settings = app.settings.borrow().clone();
    let app = app.clone();
    super::spawn(
        move || -> anyhow::Result<std::path::PathBuf> {
            let frame = stream.next_frame(Duration::from_secs(3))?;
            let mut img = frame.to_rgba()?;
            if settings.camera.mirror_saved {
                img.mirror();
            }
            library::save_still(
                &img,
                &settings,
                library::Kind::Photo,
                settings.camera.photo_format,
            )
        },
        move |r| match r {
            Ok(path) => {
                app.notify(super::Topic::Library);
                app.after_capture(&path, "Photo saved");
            }
            Err(e) => app.error("Could not take the photo", &e),
        },
    );
}

/// Put an image on the clipboard.
pub fn copy_image(img: &pixels::Rgba) {
    if let Some(display) = gtk::gdk::Display::default() {
        display.clipboard().set_texture(&super::preview::still(img));
    }
}

/// A countdown shown on `label`, then `done`. Cancelled by dropping or
/// calling [`Countdown::cancel`].
#[derive(Default)]
pub struct Countdown {
    source: RefCell<Option<glib::SourceId>>,
    left: Cell<u32>,
}

impl Countdown {
    pub fn running(&self) -> bool {
        self.source.borrow().is_some()
    }

    pub fn start(
        self: &Rc<Self>,
        seconds: u32,
        label: &gtk::Label,
        tick: impl Fn(u32) + 'static,
        done: impl Fn() + 'static,
    ) {
        self.cancel();
        if seconds == 0 {
            done();
            return;
        }
        self.left.set(seconds);
        label.set_text(&seconds.to_string());
        label.set_visible(true);
        tick(seconds);
        let (me, label) = (self.clone(), label.clone());
        let id = glib::timeout_add_local(Duration::from_secs(1), move || {
            let n = me.left.get().saturating_sub(1);
            me.left.set(n);
            if n == 0 {
                label.set_visible(false);
                me.source.borrow_mut().take();
                done();
                return glib::ControlFlow::Break;
            }
            label.set_text(&n.to_string());
            tick(n);
            glib::ControlFlow::Continue
        });
        *self.source.borrow_mut() = Some(id);
    }

    pub fn cancel(&self) {
        if let Some(id) = self.source.borrow_mut().take() {
            id.remove();
        }
    }
}

/// A white flash over `overlay`'s child: the shutter.
pub fn flash(overlay: &gtk::Overlay) {
    let f = gtk::Box::new(gtk::Orientation::Vertical, 0);
    f.add_css_class("flash");
    f.set_can_target(false);
    overlay.add_overlay(&f);
    let (overlay, f2) = (overlay.clone(), f.clone());
    let opacity = Rc::new(Cell::new(0.9f64));
    f.set_opacity(0.9);
    glib::timeout_add_local(Duration::from_millis(16), move || {
        let o = opacity.get() - 0.08;
        opacity.set(o);
        if o <= 0.0 {
            overlay.remove_overlay(&f2);
            return glib::ControlFlow::Break;
        }
        f2.set_opacity(o);
        glib::ControlFlow::Continue
    });
}

/// A title for a page, as the other Raven apps set theirs.
pub fn heading(title: &str, subtitle: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let t = gtk::Label::new(Some(title));
    t.add_css_class("page-title");
    t.set_xalign(0.0);
    b.append(&t);
    if !subtitle.is_empty() {
        let s = gtk::Label::new(Some(subtitle));
        s.add_css_class("page-subtitle");
        s.add_css_class("dim");
        s.set_xalign(0.0);
        s.set_wrap(true);
        b.append(&s);
    }
    b
}

/// A scrolled page body with the usual margins.
pub fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let s = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build();
    s.add_css_class("page-scroll");
    s
}

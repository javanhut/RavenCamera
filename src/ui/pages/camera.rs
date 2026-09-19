//! Camera: the viewfinder on its own. Photos and video from the built-in
//! camera or any USB one, with a timer, a grid, a mirror, and the camera's
//! own picture controls — brightness, white balance, exposure, whatever
//! this camera has — read from the camera itself.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::{Countdown, Page};
use crate::camera::controls::{self, Kind};
use crate::record::live::Plan;
use crate::record::session;
use crate::ui::widgets::{self, set_active_quietly, set_items};
use crate::ui::{preview, App, Topic};

const TIMERS: [u32; 3] = [0, 3, 10];

struct State {
    app: Rc<App>,
    visible: Cell<bool>,
    holding: Cell<bool>,
    video: Cell<bool>,
    preview: RefCell<Option<preview::Handle>>,
    overlay: gtk::Overlay,
    frame: gtk::Box,
    stack: gtk::Stack,
    picture: gtk::Picture,
    empty: adw::StatusPage,
    grid: gtk::DrawingArea,
    countdown_label: gtk::Label,
    countdown: Rc<Countdown>,
    shutter: gtk::Button,
    timer_label: gtk::Label,
    device_dd: gtk::DropDown,
    resolution_dd: gtk::DropDown,
    mirror: gtk::Switch,
    grid_switch: gtk::Switch,
    mic: gtk::Switch,
    enhance: gtk::Switch,
    full_size: gtk::Switch,
    photo_size: gtk::Label,
    controls: gtk::Box,
}

pub fn page(app: &Rc<App>) -> Page {
    let s = app.settings.borrow().clone();
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    root.set_margin_top(6);
    root.set_margin_bottom(18);
    root.set_margin_start(18);
    root.set_margin_end(18);

    let main = gtk::Box::new(gtk::Orientation::Vertical, 14);
    main.set_hexpand(true);
    let overlay = gtk::Overlay::new();
    overlay.set_vexpand(true);
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("preview-frame");
    frame.set_overflow(gtk::Overflow::Hidden);
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    let picture = gtk::Picture::new();
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.set_can_shrink(true);
    let empty = widgets::empty("camera-disabled-symbolic", "No camera connected", "");
    stack.add_named(&picture, Some("picture"));
    stack.add_named(&empty, Some("empty"));
    frame.append(&stack);
    overlay.set_child(Some(&frame));

    let grid = gtk::DrawingArea::new();
    grid.set_can_target(false);
    grid.set_visible(s.camera.grid);
    {
        let picture = picture.clone();
        grid.set_draw_func(move |_, cr, w, h| {
            // Thirds of the picture, not of the frame around it.
            let (pw, ph) = picture
                .paintable()
                .map(|p| {
                    (
                        p.intrinsic_width().max(1) as f64,
                        p.intrinsic_height().max(1) as f64,
                    )
                })
                .unwrap_or((16.0, 9.0));
            let scale = (w as f64 / pw).min(h as f64 / ph);
            let (dw, dh) = (pw * scale, ph * scale);
            let (x0, y0) = ((w as f64 - dw) / 2.0, (h as f64 - dh) / 2.0);
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.35);
            cr.set_line_width(1.0);
            for i in 1..3 {
                let x = x0 + dw * f64::from(i) / 3.0;
                let y = y0 + dh * f64::from(i) / 3.0;
                cr.move_to(x.round() + 0.5, y0);
                cr.line_to(x.round() + 0.5, y0 + dh);
                cr.move_to(x0, y.round() + 0.5);
                cr.line_to(x0 + dw, y.round() + 0.5);
            }
            let _ = cr.stroke();
        });
    }
    overlay.add_overlay(&grid);
    let countdown_label = gtk::Label::new(None);
    countdown_label.add_css_class("countdown");
    countdown_label.set_visible(false);
    countdown_label.set_can_target(false);
    overlay.add_overlay(&countdown_label);

    // The bar: mode, shutter, timer.
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    bar.add_css_class("control-bar");
    bar.set_halign(gtk::Align::Center);
    bar.set_valign(gtk::Align::End);
    bar.set_margin_bottom(14);
    let modes = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    modes.add_css_class("segmented");
    modes.add_css_class("mode-switch");
    let photo_b = gtk::ToggleButton::with_label("Photo");
    let video_b = gtk::ToggleButton::with_label("Video");
    video_b.set_group(Some(&photo_b));
    photo_b.set_active(true);
    modes.append(&photo_b);
    modes.append(&video_b);
    modes.set_valign(gtk::Align::Center);
    bar.append(&modes);
    let shutter = gtk::Button::from_icon_name("camera-photo-symbolic");
    shutter.add_css_class("shutter");
    shutter.set_tooltip_text(Some("Take a photo (Space)"));
    bar.append(&shutter);
    let timer_button = widgets::round_button("alarm-symbolic", "Self-timer");
    let timer_label = gtk::Label::new(None);
    timer_label.add_css_class("dim");
    bar.append(&timer_button);
    bar.append(&timer_label);
    overlay.add_overlay(&bar);
    main.append(&overlay);
    root.append(&main);

    // The side panel.
    let side = gtk::Box::new(gtk::Orientation::Vertical, 16);
    side.set_size_request(320, -1);
    let card = widgets::card("Camera");
    let device_dd = gtk::DropDown::from_strings(&[]);
    device_dd.set_hexpand(true);
    card.append(&widgets::field("Device", &device_dd));
    let resolution_dd = gtk::DropDown::from_strings(&[]);
    resolution_dd.set_hexpand(true);
    card.append(&widgets::field("Resolution", &resolution_dd));
    let formats = vec!["JPEG".to_owned(), "PNG".to_owned()];
    let fmt = widgets::dropdown(
        &formats,
        u32::from(s.camera.photo_format == crate::settings::ImageFormat::Png),
        {
            let app = app.clone();
            move |i| {
                app.update_settings(|x| {
                    x.camera.photo_format = if i == 1 {
                        crate::settings::ImageFormat::Png
                    } else {
                        crate::settings::ImageFormat::Jpeg
                    }
                })
            }
        },
    );
    card.append(&widgets::field("Photo format", &fmt));
    let gap = gtk::Box::new(gtk::Orientation::Vertical, 0);
    gap.set_size_request(-1, 6);
    card.append(&gap);
    let (r1, mirror) = widgets::switch_row("Mirror the preview", s.camera.mirror_preview, |_| {});
    let (r2, mirror_saved) =
        widgets::switch_row("Mirror photos and video", s.camera.mirror_saved, {
            let app = app.clone();
            move |on| app.update_settings(|x| x.camera.mirror_saved = on)
        });
    let (r3, grid_switch) = widgets::switch_row("Grid", s.camera.grid, |_| {});
    let (r4, mic) = widgets::switch_row("Record the microphone", s.recording.microphone, {
        let app = app.clone();
        move |on| app.update_settings(|x| x.recording.microphone = on)
    });
    let _ = mirror_saved;
    for r in [&r1, &r2, &r3, &r4] {
        card.append(r);
    }
    side.append(&card);

    let look = widgets::card("Look");
    let (r5, enhance) = widgets::switch_row("Enhance the picture", s.camera.enhance, |_| {});
    look.append(&r5);
    look.append(&caption(
        "Less noise, fuller contrast, richer colour and crisper detail — only as much as \
         this camera needs. In the preview, photos and video.",
    ));
    let (r6, full_size) = widgets::switch_row(
        "Photos at full resolution",
        s.camera.full_resolution_photos,
        {
            let app = app.clone();
            move |on| app.update_settings(|x| x.camera.full_resolution_photos = on)
        },
    );
    look.append(&r6);
    let photo_size = caption("");
    look.append(&photo_size);
    side.append(&look);
    let controls_card = widgets::card("Picture");
    let controls = gtk::Box::new(gtk::Orientation::Vertical, 6);
    controls_card.append(&controls);
    let reset = gtk::Button::with_label("Reset to Defaults");
    reset.set_halign(gtk::Align::Start);
    reset.set_margin_top(6);
    controls_card.append(&reset);
    side.append(&controls_card);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&side)
        .propagate_natural_width(true)
        .build();
    root.append(&scroll);

    let st = Rc::new(State {
        app: app.clone(),
        visible: Cell::new(false),
        holding: Cell::new(false),
        video: Cell::new(false),
        preview: RefCell::new(None),
        overlay,
        frame,
        stack,
        picture,
        empty,
        grid,
        countdown_label,
        countdown: Rc::default(),
        shutter,
        timer_label,
        device_dd,
        resolution_dd,
        mirror,
        grid_switch,
        mic,
        enhance,
        full_size,
        photo_size,
        controls,
    });
    set_timer_label(&st);

    {
        let s = st.clone();
        video_b.connect_toggled(move |b| {
            s.video.set(b.is_active());
            sync_recording(&s);
        });
    }
    {
        let s = st.clone();
        st.shutter.connect_clicked(move |_| press_shutter(&s));
    }
    {
        let s = st.clone();
        timer_button.connect_clicked(move |_| {
            let cur = s.app.settings.borrow().camera.photo_timer;
            let next =
                TIMERS[(TIMERS.iter().position(|&t| t == cur).unwrap_or(0) + 1) % TIMERS.len()];
            s.app.update_settings(|x| x.camera.photo_timer = next);
            set_timer_label(&s);
        });
    }
    {
        let s = st.clone();
        st.mirror.connect_active_notify(move |sw| {
            if sw.widget_name() != "quiet" {
                let on = sw.is_active();
                s.app.update_settings(|x| x.camera.mirror_preview = on);
                restart(&s);
            }
        });
    }
    {
        let s = st.clone();
        st.enhance.connect_active_notify(move |sw| {
            if sw.widget_name() != "quiet" {
                let on = sw.is_active();
                s.app.update_settings(|x| x.camera.enhance = on);
                restart(&s);
            }
        });
    }
    {
        let s = st.clone();
        st.grid_switch.connect_active_notify(move |sw| {
            let on = sw.is_active();
            s.grid.set_visible(on);
            if sw.widget_name() != "quiet" {
                s.app.update_settings(|x| x.camera.grid = on);
            }
        });
    }
    {
        let s = st.clone();
        st.device_dd.connect_selected_notify(move |dd| {
            if dd.widget_name() == "quiet" {
                return;
            }
            let key = s
                .app
                .cameras
                .borrow()
                .get(dd.selected() as usize)
                .map(|d| d.key());
            if let Some(key) = key {
                s.app.update_settings(|x| {
                    x.camera.device = key;
                    x.camera.resolution.clear();
                });
                s.app.camera_restart();
            }
        });
    }
    {
        let s = st.clone();
        st.resolution_dd.connect_selected_notify(move |dd| {
            if dd.widget_name() == "quiet" {
                return;
            }
            let Some(dev) = s.app.chosen_camera() else {
                return;
            };
            if let Some(m) = dev.modes.get(dd.selected() as usize) {
                let key = m.resolution_key();
                s.app.update_settings(|x| x.camera.resolution = key);
                s.app.camera_restart();
            }
        });
    }
    {
        let s = st.clone();
        reset.connect_clicked(move |_| {
            let Some(dev) = s.app.chosen_camera() else {
                return;
            };
            s.app.forget_controls(&dev);
            for c in controls::read(&dev.path) {
                let _ = controls::set(&dev.path, c.id, c.default);
            }
            // The defaults, except anti-flicker matched to the mains.
            controls::restore(&dev.path, None);
            draw_controls(&s);
        });
    }
    {
        let s = st.clone();
        app.on(Topic::Cameras, move || {
            sync_devices(&s);
            if s.visible.get() && !s.app.is_recording() {
                restart(&s);
            }
        });
    }
    {
        let s = st.clone();
        app.on(Topic::Recording, move || sync_recording(&s));
    }
    {
        let s = st.clone();
        app.on(Topic::Settings, move || {
            let x = s.app.settings.borrow().clone();
            set_active_quietly(&s.mic, x.recording.microphone);
            set_active_quietly(&s.mirror, x.camera.mirror_preview);
            set_active_quietly(&s.grid_switch, x.camera.grid);
            set_active_quietly(&s.enhance, x.camera.enhance);
            set_active_quietly(&s.full_size, x.camera.full_resolution_photos);
        });
    }
    // Space takes the picture.
    {
        let keys = gtk::EventControllerKey::new();
        let s = st.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::space && s.visible.get() {
                press_shutter(&s);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        st.overlay.add_controller(keys);
        st.overlay.set_focusable(true);
    }
    sync_devices(&st);

    let (show, hide) = (st.clone(), st.clone());
    Page {
        id: "camera",
        title: "Camera",
        icon: "camera-web-symbolic",
        widget: root.upcast(),
        on_show: Box::new(move || {
            show.visible.set(true);
            sync_devices(&show);
            restart(&show);
            show.overlay.grab_focus();
        }),
        on_hide: Box::new(move || {
            hide.visible.set(false);
            hide.countdown.cancel();
            hide.countdown_label.set_visible(false);
            hide.preview.borrow_mut().take();
            if hide.holding.replace(false) {
                hide.app.camera_release();
            }
        }),
    }
}

/// A line of small print under a setting.
fn caption(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("dim");
    l.add_css_class("caption");
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.set_max_width_chars(36);
    l
}

fn set_timer_label(st: &State) {
    let t = st.app.settings.borrow().camera.photo_timer;
    st.timer_label.set_text(&if t == 0 {
        "Off".into()
    } else {
        format!("{t} s")
    });
}

fn sync_devices(st: &Rc<State>) {
    let cams = st.app.cameras.borrow().clone();
    let chosen = st.app.chosen_camera();
    let labels: Vec<String> = cams
        .iter()
        .map(|d| format!("{} ({})", d.name, d.kind_label()))
        .collect();
    let sel = chosen
        .as_ref()
        .and_then(|c| cams.iter().position(|d| d.path == c.path))
        .unwrap_or(0);
    set_items(&st.device_dd, &labels, sel as u32);
    if let Some(dev) = chosen {
        let want = st.app.settings.borrow().camera.resolution.clone();
        let best = dev.mode_for(&want);
        let labels: Vec<String> = dev.modes.iter().map(|m| m.label()).collect();
        let sel = best
            .and_then(|b| {
                dev.modes
                    .iter()
                    .position(|m| m.width == b.width && m.height == b.height)
            })
            .unwrap_or(0);
        set_items(&st.resolution_dd, &labels, sel as u32);
        st.photo_size.set_text(&match dev.still_modes.first() {
            Some(m) => format!(
                "This camera takes photos at {} × {}; the preview pauses for a moment \
                 while it does.",
                m.width, m.height
            ),
            None => String::new(),
        });
    } else {
        set_items(&st.resolution_dd, &[], 0);
        st.photo_size.set_text("");
    }
}

fn restart(st: &Rc<State>) {
    // While a full-size photo is taken the last picture stays; the camera
    // coming back says so with Topic::Cameras.
    if !st.visible.get() || st.app.camera_busy() {
        return;
    }
    st.preview.borrow_mut().take();
    if st.app.cameras.borrow().is_empty() {
        st.empty.set_title("No camera connected");
        st.empty.set_description(Some(
            "Plug in a USB webcam, or check the built-in camera's privacy switch or shutter. \
             Cameras are picked up the moment they appear.",
        ));
        st.stack.set_visible_child_name("empty");
        st.shutter.set_sensitive(false);
        widgets::clear(&st.controls);
        return;
    }
    if !st.holding.replace(true) {
        st.app.camera_acquire();
    }
    match st.app.camera_stream() {
        Ok(stream) => {
            st.stack.set_visible_child_name("picture");
            st.shutter.set_sensitive(true);
            let (picture, grid) = (st.picture.clone(), st.grid.clone());
            let (mirror, enhance) = {
                let x = st.app.settings.borrow();
                (x.camera.mirror_preview, x.camera.enhance)
            };
            *st.preview.borrow_mut() = Some(preview::camera(&stream, mirror, enhance, move |t| {
                preview::show(&picture, &t);
                if grid.is_visible() {
                    grid.queue_draw();
                }
            }));
            draw_controls(st);
        }
        Err(e) => {
            st.empty.set_title("The camera could not be started");
            st.empty.set_description(Some(&format!("{e:#}")));
            st.stack.set_visible_child_name("empty");
            st.shutter.set_sensitive(false);
        }
    }
}

/// One slider, switch or list per control the camera has.
fn draw_controls(st: &Rc<State>) {
    widgets::clear(&st.controls);
    let Some(dev) = st.app.chosen_camera() else {
        return;
    };
    let list = controls::read(&dev.path);
    if list.is_empty() {
        let l = gtk::Label::new(Some("This camera has no adjustable controls."));
        l.add_css_class("dim");
        st.controls.append(&l);
        return;
    }
    for c in list {
        let id = c.id;
        let app = st.app.clone();
        let dev = dev.clone();
        let set = move |v: i32| match controls::set(&dev.path, id, v) {
            Ok(()) => app.remember_control(&dev, id, v),
            Err(e) => tracing::debug!("control {id:#x}: {e}"),
        };
        match c.kind {
            Kind::Slider { min, max, step } => {
                let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
                let l = gtk::Label::new(Some(&c.name));
                l.add_css_class("control-slider-label");
                l.set_xalign(0.0);
                let scale = gtk::Scale::with_range(
                    gtk::Orientation::Horizontal,
                    f64::from(min),
                    f64::from(max),
                    f64::from(step),
                );
                scale.set_value(f64::from(c.value));
                scale.set_sensitive(!c.inactive);
                scale.set_draw_value(false);
                scale.add_mark(f64::from(c.default), gtk::PositionType::Bottom, None);
                scale.connect_value_changed(move |s| set(s.value().round() as i32));
                row.append(&l);
                row.append(&scale);
                st.controls.append(&row);
            }
            Kind::Toggle => {
                // Turning an automatic control on or off makes its manual
                // partner active or inactive: draw them again.
                let s = st.clone();
                let (row, _) = widgets::switch_row(&c.name, c.value != 0, move |on| {
                    set(i32::from(on));
                    let s = s.clone();
                    glib::idle_add_local_once(move || draw_controls(&s));
                });
                st.controls.append(&row);
            }
            Kind::Menu(items) => {
                let labels: Vec<String> = items.iter().map(|(_, n)| n.clone()).collect();
                let sel = items
                    .iter()
                    .position(|(i, _)| *i as i32 == c.value)
                    .unwrap_or(0) as u32;
                let s = st.clone();
                let ids: Vec<u32> = items.iter().map(|(i, _)| *i).collect();
                let dd = widgets::dropdown(&labels, sel, move |i| {
                    set(ids[i as usize] as i32);
                    let s = s.clone();
                    glib::idle_add_local_once(move || draw_controls(&s));
                });
                st.controls.append(&widgets::field(&c.name, &dd));
            }
        }
        if let Some(hint) = c.hint {
            st.controls.append(&caption(hint));
        }
    }
}

fn sync_recording(st: &Rc<State>) {
    let recording = st
        .app
        .recording
        .borrow()
        .as_ref()
        .map(|l| (l.manifest.kind, l.elapsed()));
    match (st.video.get(), recording) {
        (true, Some((session::Kind::Camera, t))) => {
            st.shutter.set_icon_name("media-playback-stop-symbolic");
            st.shutter.set_tooltip_text(Some("Stop and save"));
            st.frame.add_css_class("recording");
            let s = t.as_secs();
            st.timer_label
                .set_text(&format!("● {:02}:{:02}", s / 60, s % 60));
        }
        (true, _) => {
            st.shutter.set_icon_name("media-record-symbolic");
            st.shutter.set_tooltip_text(Some("Record video (Space)"));
            st.frame.remove_css_class("recording");
            set_timer_label(st);
        }
        (false, _) => {
            st.shutter.set_icon_name("camera-photo-symbolic");
            st.shutter.set_tooltip_text(Some("Take a photo (Space)"));
            st.frame.remove_css_class("recording");
            set_timer_label(st);
        }
    }
}

fn press_shutter(st: &Rc<State>) {
    if st.app.is_recording() {
        st.app.stop_recording();
        return;
    }
    if st.countdown.running() {
        st.countdown.cancel();
        st.countdown_label.set_visible(false);
        return;
    }
    let seconds = st.app.settings.borrow().camera.photo_timer;
    let s = st.clone();
    st.countdown
        .start(seconds, &st.countdown_label, |_| {}, move || fire(&s));
}

fn fire(st: &Rc<State>) {
    let stream = match st.app.camera_stream() {
        Ok(s) => s,
        Err(e) => {
            st.app.error("The camera is not available", &e);
            return;
        }
    };
    if st.video.get() {
        let microphone = st.app.settings.borrow().recording.microphone;
        st.app.start_recording(Plan {
            kind: session::Kind::Camera,
            screen: None,
            camera: Some(stream),
            system_audio: false,
            microphone,
            subject: String::new(),
        });
    } else {
        super::flash(&st.overlay);
        super::save_photo(&st.app, stream);
    }
}

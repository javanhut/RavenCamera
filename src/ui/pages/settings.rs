//! Settings: where captures go, what they are called, how recordings are
//! made, and what this machine can do — cameras found, whether the desktop
//! offers screen capture, sound.

use std::rc::Rc;

use gtk4 as gtk;
use libadwaita::prelude::*;

use super::Page;
use crate::library;
use crate::record::session;
use crate::settings::{Corner, OutputSize, PostAction, Quality};
use crate::ui::widgets::{self, card, field};
use crate::ui::{App, Topic};

pub fn page(app: &Rc<App>) -> Page {
    let s = app.settings.borrow().clone();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
    body.set_margin_top(8);
    body.set_margin_bottom(24);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.append(&super::heading("Settings", ""));
    let columns = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    columns.set_homogeneous(true);
    let left = gtk::Box::new(gtk::Orientation::Vertical, 16);
    let right = gtk::Box::new(gtk::Orientation::Vertical, 16);
    columns.append(&left);
    columns.append(&right);
    body.append(&columns);

    // Saving.
    let saving = card("Saving");
    let folder_row = |label: &str, videos: bool| {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let path = gtk::Label::new(None);
        path.add_css_class("path-box");
        path.set_hexpand(true);
        path.set_xalign(0.0);
        path.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        path.set_max_width_chars(1);
        let b = gtk::Button::with_label("Browse");
        let a = app.clone();
        b.connect_clicked(move |_| super::capture::choose_folder(&a, videos));
        row.append(&path);
        row.append(&b);
        (field(label, &row), path)
    };
    let (f1, videos_path) = folder_row("Recordings and audio", true);
    let (f2, pictures_path) = folder_row("Photos and screenshots", false);
    saving.append(&f1);
    saving.append(&f2);
    let post: Vec<String> = PostAction::ALL
        .iter()
        .map(|p| p.label().to_owned())
        .collect();
    saving.append(&field(
        "After a capture",
        &widgets::dropdown(
            &post,
            PostAction::ALL
                .iter()
                .position(|&p| p == s.post_action)
                .unwrap_or(0) as u32,
            {
                let app = app.clone();
                move |i| app.update_settings(|x| x.post_action = PostAction::ALL[i as usize])
            },
        ),
    ));
    left.append(&saving);

    // Recording.
    let rec = card("Recording");
    let q: Vec<String> = Quality::ALL.iter().map(|q| q.label().to_owned()).collect();
    rec.append(&field(
        "Quality",
        &widgets::dropdown(
            &q,
            Quality::ALL
                .iter()
                .position(|&x| x == s.recording.quality)
                .unwrap_or(1) as u32,
            {
                let app = app.clone();
                move |i| app.update_settings(|x| x.recording.quality = Quality::ALL[i as usize])
            },
        ),
    ));
    let sizes: Vec<String> = OutputSize::ALL
        .iter()
        .map(|o| o.label().to_owned())
        .collect();
    rec.append(&field(
        "Video size",
        &widgets::dropdown(
            &sizes,
            OutputSize::ALL
                .iter()
                .position(|&x| x == s.recording.output_size)
                .unwrap_or(0) as u32,
            {
                let app = app.clone();
                move |i| {
                    app.update_settings(|x| x.recording.output_size = OutputSize::ALL[i as usize])
                }
            },
        ),
    ));
    let note = gtk::Label::new(Some(
        "Recordings are captured at full size; a smaller video size is applied when saving, and also saves faster.",
    ));
    note.add_css_class("note");
    note.set_wrap(true);
    note.set_xalign(0.0);
    rec.append(&note);
    let seconds: Vec<String> = [3u32, 5, 10]
        .iter()
        .map(|n| format!("{n} seconds"))
        .collect();
    rec.append(&field(
        "Countdown",
        &widgets::dropdown(
            &seconds,
            [3u32, 5, 10]
                .iter()
                .position(|&n| n == s.recording.countdown_seconds)
                .unwrap_or(0) as u32,
            {
                let app = app.clone();
                move |i| {
                    app.update_settings(|x| x.recording.countdown_seconds = [3, 5, 10][i as usize])
                }
            },
        ),
    ));
    let corners: Vec<String> = Corner::ALL.iter().map(|c| c.label().to_owned()).collect();
    rec.append(&field(
        "Webcam overlay corner",
        &widgets::dropdown(
            &corners,
            Corner::ALL
                .iter()
                .position(|&c| c == s.recording.overlay_corner)
                .unwrap_or(3) as u32,
            {
                let app = app.clone();
                move |i| {
                    app.update_settings(|x| x.recording.overlay_corner = Corner::ALL[i as usize])
                }
            },
        ),
    ));
    let size = gtk::Scale::with_range(gtk::Orientation::Horizontal, 10.0, 50.0, 1.0);
    size.set_value(f64::from(s.recording.overlay_percent));
    size.set_draw_value(true);
    size.set_value_pos(gtk::PositionType::Right);
    {
        let app = app.clone();
        size.connect_value_changed(move |sc| {
            let v = sc.value().round() as u32;
            app.update_settings(|x| x.recording.overlay_percent = v);
        });
    }
    rec.append(&field("Webcam overlay size (% of width)", &size));
    let (r, _) = widgets::switch_row(
        "Minimise this window while recording the screen",
        s.recording.hide_window,
        {
            let app = app.clone();
            move |on| app.update_settings(|x| x.recording.hide_window = on)
        },
    );
    rec.append(&r);
    left.append(&rec);

    // This computer.
    let system = card("This computer");
    let facts = gtk::Box::new(gtk::Orientation::Vertical, 6);
    system.append(&facts);
    right.append(&system);

    // Storage.
    let storage = card("Unfinished recordings");
    let storage_text = gtk::Label::new(None);
    storage_text.set_xalign(0.0);
    storage_text.set_wrap(true);
    storage.append(&storage_text);
    let manage = gtk::Button::with_label("Review on the Record Page");
    manage.set_halign(gtk::Align::Start);
    {
        let app = app.clone();
        manage.connect_clicked(move |_| app.navigate("record"));
    }
    storage.append(&manage);
    right.append(&storage);

    let about = card("About");
    let text = gtk::Label::new(Some(
        "Raven Camera 0.1\n\nVideo is encoded with Raven's own H.264 encoder, sound with Raven's own AAC encoder, \
         and files are written by Raven's own MP4 writer. Screen capture comes from the Huginn compositor.",
    ));
    text.set_wrap(true);
    text.set_xalign(0.0);
    text.add_css_class("dim");
    about.append(&text);
    right.append(&about);

    let draw = {
        let app = app.clone();
        Rc::new(move || {
            let s = app.settings.borrow().clone();
            videos_path.set_text(&crate::paths::display(&s.video_dir()));
            pictures_path.set_text(&crate::paths::display(&s.picture_dir()));
            widgets::clear(&facts);
            let fact = |k: &str, v: &str| {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                let a = gtk::Label::new(Some(k));
                a.add_css_class("dim");
                a.set_xalign(0.0);
                a.set_width_chars(16);
                let b = gtk::Label::new(Some(v));
                b.set_xalign(0.0);
                b.set_wrap(true);
                b.set_hexpand(true);
                row.append(&a);
                row.append(&b);
                row
            };
            let screen = app.screen_state.borrow().clone();
            facts.append(&fact(
                "Screen capture",
                match screen.capture {
                    Some(true) => "Available (Huginn raven_capture_v1)",
                    Some(false) => "Not available from this compositor",
                    None => "Checking…",
                },
            ));
            facts.append(&fact("Screens", &screen.outputs.len().to_string()));
            let cams = app.cameras.borrow();
            let cam_text = if cams.is_empty() {
                "None found".to_owned()
            } else {
                cams.iter()
                    .map(|d| {
                        let best = d
                            .best_mode()
                            .map(|m| format!(", up to {}×{}", m.width, m.height))
                            .unwrap_or_default();
                        format!("{} ({}{best})", d.name, d.kind_label())
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            facts.append(&fact("Cameras", &cam_text));
            let audio = app.audio.borrow();
            facts.append(&fact(
                "Sound",
                &if audio.available {
                    format!(
                        "PipeWire: {} outputs, {} inputs",
                        audio.outputs.len(),
                        audio.inputs.len()
                    )
                } else {
                    "PipeWire's pw-record was not found; recordings will be silent".into()
                },
            ));
            facts.append(&fact(
                "Saving uses",
                &format!(
                    "{} threads",
                    std::thread::available_parallelism()
                        .map(|n| n.get())
                        .unwrap_or(1)
                ),
            ));
            let left = session::leftovers();
            let bytes: u64 = left.iter().map(|(d, _)| session::size(d)).sum();
            storage_text.set_text(&if left.is_empty() {
                "None. Recordings in progress are kept in ~/.local/share/raven-camera until they are saved.".into()
            } else {
                format!("{} waiting, using {}.", left.len(), library::format_bytes(bytes))
            });
        })
    };
    for t in [
        Topic::Screen,
        Topic::Cameras,
        Topic::Audio,
        Topic::Settings,
        Topic::Jobs,
    ] {
        let d = draw.clone();
        app.on(t, move || d());
    }
    draw();
    let show = draw.clone();
    Page {
        id: "settings",
        title: "Settings",
        icon: "emblem-system-symbolic",
        widget: super::scrolled(&body).upcast(),
        on_show: Box::new(move || show()),
        on_hide: Box::new(|| {}),
    }
}

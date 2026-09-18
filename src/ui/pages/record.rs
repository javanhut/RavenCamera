//! Record: the recording in progress — its clock, its sound levels, pause
//! and stop — then everything being saved, and recordings a crash left
//! behind. When nothing is happening, a quick way to start each kind of
//! recording.

use std::rc::Rc;

use gtk4 as gtk;
use libadwaita::prelude::*;

use super::Page;
use crate::library;
use crate::record::session;
use crate::ui::widgets;
use crate::ui::{App, Topic};

struct Ui {
    live: gtk::Box,
    timer: gtk::Label,
    what: gtk::Label,
    system: gtk::LevelBar,
    mic: gtk::LevelBar,
    system_row: gtk::Box,
    mic_row: gtk::Box,
    mic_warning: gtk::Label,
    pause: gtk::Button,
    idle: gtk::Box,
    jobs: gtk::Box,
    jobs_card: gtk::Box,
    leftovers: gtk::Box,
    leftovers_card: gtk::Box,
}

pub fn page(app: &Rc<App>) -> Page {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 18);
    body.set_margin_top(8);
    body.set_margin_bottom(24);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.append(&super::heading(
        "Record",
        "What is being recorded, and what is being saved. Recordings are captured losslessly and turned into MP4 once they stop, using every core.",
    ));

    // The live card.
    let live = widgets::card("");
    let timer = gtk::Label::new(Some("00:00:00"));
    timer.add_css_class("big-timer");
    timer.add_css_class("live");
    timer.set_xalign(0.0);
    let what = gtk::Label::new(None);
    what.add_css_class("dim");
    what.set_xalign(0.0);
    live.append(&timer);
    live.append(&what);
    let meter = |name: &str| {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let l = gtk::Label::new(Some(name));
        l.set_width_chars(16);
        l.set_xalign(0.0);
        let m = gtk::LevelBar::for_interval(0.0, 1.0);
        m.add_css_class("meter");
        m.set_hexpand(true);
        m.set_valign(gtk::Align::Center);
        row.append(&l);
        row.append(&m);
        (row, m)
    };
    let (system_row, system) = meter("System sound");
    let (mic_row, mic) = meter("Microphone");
    live.append(&system_row);
    live.append(&mic_row);
    let mic_warning = gtk::Label::new(Some(
        "The microphone is sending a signal pinned at full volume — it sounds like a roar, not like a room. \
         On many laptops the real microphone is digital and PipeWire's default input is an unconnected analogue one: \
         choose another input in Capture → Audio Input, or check Settings → Sound.",
    ));
    mic_warning.add_css_class("warning");
    mic_warning.set_wrap(true);
    mic_warning.set_xalign(0.0);
    mic_warning.set_visible(false);
    live.append(&mic_warning);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    buttons.set_margin_top(8);
    let pause = gtk::Button::with_label("Pause");
    pause.add_css_class("pill");
    let stop = gtk::Button::with_label("Stop and Save");
    stop.add_css_class("pill");
    stop.add_css_class("destructive-action");
    buttons.append(&pause);
    buttons.append(&stop);
    live.append(&buttons);
    body.append(&live);
    {
        let app = app.clone();
        stop.connect_clicked(move |_| app.stop_recording());
    }
    {
        let app = app.clone();
        pause.connect_clicked(move |_| {
            let paused = app.recording.borrow().as_ref().is_some_and(|l| l.paused());
            app.set_paused(!paused);
        });
    }

    // Quick starts.
    let idle = gtk::Box::new(gtk::Orientation::Vertical, 10);
    let t = gtk::Label::new(Some("Start a recording"));
    t.add_css_class("section-head");
    t.set_xalign(0.0);
    idle.append(&t);
    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_max_children_per_line(5);
    flow.set_column_spacing(12);
    flow.set_row_spacing(12);
    for (icon, title, text, tab) in [
        ("video-display-symbolic", "Screen", "A whole display", 0),
        ("window-new-symbolic", "Window", "One window, on its own", 1),
        (
            "selection-mode-symbolic",
            "Region",
            "A part you drag out",
            2,
        ),
        ("camera-web-symbolic", "Camera", "Built-in or USB", 3),
        (
            "audio-input-microphone-symbolic",
            "Audio",
            "Sound only, as M4A",
            4,
        ),
    ] {
        let b = gtk::Button::new();
        b.add_css_class("quick-card");
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let i = gtk::Image::from_icon_name(icon);
        i.set_halign(gtk::Align::Start);
        inner.append(&i);
        let l = gtk::Label::new(Some(title));
        l.add_css_class("title");
        l.set_xalign(0.0);
        inner.append(&l);
        let d = gtk::Label::new(Some(text));
        d.add_css_class("dim");
        d.set_xalign(0.0);
        inner.append(&d);
        b.set_child(Some(&inner));
        let app = app.clone();
        b.connect_clicked(move |_| {
            app.navigate("capture");
            crate::ui::pages::capture::select_tab(&app, tab);
        });
        flow.insert(&b, -1);
    }
    idle.append(&flow);
    body.append(&idle);

    // Saving.
    let jobs_card = widgets::card("Saving");
    let jobs = gtk::Box::new(gtk::Orientation::Vertical, 0);
    jobs_card.append(&jobs);
    body.append(&jobs_card);

    // Interrupted.
    let leftovers_card = widgets::card("Interrupted recordings");
    let note = gtk::Label::new(Some(
        "These stopped without being saved — the app or the computer went down while they were recording. Everything captured up to that moment is kept.",
    ));
    note.add_css_class("note");
    note.set_wrap(true);
    note.set_xalign(0.0);
    leftovers_card.append(&note);
    let leftovers = gtk::Box::new(gtk::Orientation::Vertical, 0);
    leftovers_card.append(&leftovers);
    body.append(&leftovers_card);

    let ui = Rc::new(Ui {
        live,
        timer,
        what,
        system,
        mic,
        system_row,
        mic_row,
        mic_warning,
        pause,
        idle,
        jobs,
        jobs_card,
        leftovers,
        leftovers_card,
    });
    let sync_live = {
        let (app, ui) = (app.clone(), ui.clone());
        Rc::new(move || draw_live(&app, &ui))
    };
    let sync_jobs = {
        let (app, ui) = (app.clone(), ui.clone());
        Rc::new(move || draw_jobs(&app, &ui))
    };
    {
        let f = sync_live.clone();
        app.on(Topic::Recording, move || f());
    }
    {
        let f = sync_jobs.clone();
        app.on(Topic::Jobs, move || f());
    }
    sync_live();
    sync_jobs();

    let (s1, s2) = (sync_live.clone(), sync_jobs.clone());
    Page {
        id: "record",
        title: "Record",
        icon: "media-record-symbolic",
        widget: super::scrolled(&body).upcast(),
        on_show: Box::new(move || {
            s1();
            s2();
        }),
        on_hide: Box::new(|| {}),
    }
}

fn draw_live(app: &Rc<App>, ui: &Ui) {
    let rec = app.recording.borrow();
    ui.live.set_visible(rec.is_some());
    ui.idle.set_visible(rec.is_none());
    let Some(live) = rec.as_ref() else { return };
    let t = live.elapsed().as_secs();
    ui.timer
        .set_text(&format!("{:02}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60));
    let m = &live.manifest;
    let mut what = m.kind.title().to_owned();
    if !m.subject.is_empty() {
        what.push_str(&format!(" · {}", m.subject));
    }
    if live.paused() {
        what.push_str(" · paused");
    }
    let frames = live
        .stats
        .screen_frames
        .load(std::sync::atomic::Ordering::Relaxed)
        + live
            .stats
            .camera_frames
            .load(std::sync::atomic::Ordering::Relaxed);
    if frames > 0 {
        what.push_str(&format!(" · {frames} frames"));
    }
    what.push_str(&format!(
        " → {}",
        m.output.file_name().unwrap_or_default().to_string_lossy()
    ));
    ui.what.set_text(&what);
    let (sys, mic) = live.levels();
    ui.system_row.set_visible(live.has_system_audio());
    ui.mic_row.set_visible(live.has_microphone());
    ui.system.set_value(f64::from(sys));
    ui.mic.set_value(f64::from(mic));
    ui.mic_warning.set_visible(live.microphone_broken());
    ui.pause
        .set_label(if live.paused() { "Resume" } else { "Pause" });
    if live.paused() {
        ui.timer.remove_css_class("live");
    } else {
        ui.timer.add_css_class("live");
    }
}

fn draw_jobs(app: &Rc<App>, ui: &Ui) {
    widgets::clear(&ui.jobs);
    let jobs = app.jobs.borrow().clone();
    ui.jobs_card.set_visible(!jobs.is_empty());
    for job in jobs {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 6);
        row.add_css_class("job-row");
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let name = gtk::Label::new(Some(&format!(
            "{} → {}",
            job.manifest.kind.title(),
            job.manifest
                .output
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        )));
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        top.append(&name);
        let failed = job.failed.borrow().clone();
        if let Some(err) = &failed {
            let retry = gtk::Button::with_label("Try Again");
            let discard = gtk::Button::with_label("Discard");
            discard.add_css_class("destructive-action");
            {
                let (app, job) = (app.clone(), job.clone());
                retry.connect_clicked(move |_| {
                    app.jobs.borrow_mut().retain(|j| !Rc::ptr_eq(j, &job));
                    app.finish(job.dir.clone(), job.manifest.clone());
                });
            }
            {
                let (app, dir) = (app.clone(), job.dir.clone());
                discard.connect_clicked(move |_| app.discard(&dir));
            }
            top.append(&retry);
            top.append(&discard);
            row.append(&top);
            let e = gtk::Label::new(Some(err));
            e.add_css_class("error");
            e.set_wrap(true);
            e.set_xalign(0.0);
            row.append(&e);
        } else {
            let pct = gtk::Label::new(Some(&format!("{:.0}%", job.progress.get() * 100.0)));
            pct.add_css_class("dim");
            top.append(&pct);
            let cancel = gtk::Button::from_icon_name("process-stop-symbolic");
            cancel.add_css_class("flat");
            cancel.set_tooltip_text(Some("Stop saving; the recording is kept to save later"));
            {
                let job = job.clone();
                cancel.connect_clicked(move |_| {
                    job.cancel.store(true, std::sync::atomic::Ordering::Relaxed)
                });
            }
            top.append(&cancel);
            row.append(&top);
            let bar = gtk::ProgressBar::new();
            bar.set_fraction(job.progress.get());
            row.append(&bar);
        }
        ui.jobs.append(&row);
    }

    widgets::clear(&ui.leftovers);
    let leftovers = app.leftovers.borrow().clone();
    ui.leftovers_card.set_visible(!leftovers.is_empty());
    for (dir, manifest) in leftovers {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.add_css_class("job-row");
        let when = chrono::DateTime::parse_from_rfc3339(&manifest.created)
            .map(|d| d.format("%b %-d, %H:%M").to_string())
            .unwrap_or_default();
        let name = gtk::Label::new(Some(&format!(
            "{} · {} · {}",
            manifest.kind.title(),
            when,
            library::format_bytes(session::size(&dir))
        )));
        name.set_xalign(0.0);
        name.set_hexpand(true);
        row.append(&name);
        let save = gtk::Button::with_label("Save");
        save.add_css_class("suggested-action");
        let discard = gtk::Button::with_label("Discard");
        {
            let (app, dir, m) = (app.clone(), dir.clone(), manifest.clone());
            save.connect_clicked(move |_| app.finish(dir.clone(), m.clone()));
        }
        {
            let (app, dir) = (app.clone(), dir.clone());
            discard.connect_clicked(move |_| app.discard(&dir));
        }
        row.append(&save);
        row.append(&discard);
        ui.leftovers.append(&row);
    }
}

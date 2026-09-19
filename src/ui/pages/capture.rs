//! Capture: the studio from the mockup. Choose what to capture — a screen,
//! a window, a region, the camera, or sound alone — see it live, and record
//! it, with the recording settings beside it and the latest captures under
//! it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::{Countdown, Page};
use crate::library;
use crate::record::live::Plan;
use crate::record::session::Kind;
use crate::screen::{self, Region, Source};
use crate::settings::{Corner, Quality};
use crate::ui::widgets::{self, set_active_quietly, set_items, set_selected_quietly};
use crate::ui::{preview, App, Topic};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Screen,
    Window,
    Region,
    Camera,
    Audio,
}

impl Mode {
    const ALL: [Mode; 5] = [
        Mode::Screen,
        Mode::Window,
        Mode::Region,
        Mode::Camera,
        Mode::Audio,
    ];

    fn label(self) -> &'static str {
        match self {
            Mode::Screen => "Screen",
            Mode::Window => "Window",
            Mode::Region => "Region",
            Mode::Camera => "Camera",
            Mode::Audio => "Audio Only",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Mode::Screen => "video-display-symbolic",
            Mode::Window => "window-new-symbolic",
            Mode::Region => "selection-mode-symbolic",
            Mode::Camera => "camera-web-symbolic",
            Mode::Audio => "audio-input-microphone-symbolic",
        }
    }

    fn kind(self) -> Kind {
        match self {
            Mode::Screen => Kind::Screen,
            Mode::Window => Kind::Window,
            Mode::Region => Kind::Region,
            Mode::Camera => Kind::Camera,
            Mode::Audio => Kind::Audio,
        }
    }

    fn is_screen(self) -> bool {
        matches!(self, Mode::Screen | Mode::Window | Mode::Region)
    }
}

const FPS: [u32; 4] = [15, 24, 30, 60];

struct Ui {
    tabs: Vec<gtk::ToggleButton>,
    overlay: gtk::Overlay,
    frame: gtk::Box,
    stack: gtk::Stack,
    picture: gtk::Picture,
    empty: adw::StatusPage,
    empty_button: gtk::Button,
    pip: gtk::Picture,
    size_badge: gtk::Label,
    live_badge: gtk::Label,
    picker: gtk::DropDown,
    region_button: gtk::Button,
    countdown_label: gtk::Label,
    source_button: gtk::Button,
    system: gtk::ToggleButton,
    mic: gtk::ToggleButton,
    rec: gtk::Button,
    pause: gtk::ToggleButton,
    timer: gtk::Label,
    still: gtk::Button,
    overlay_toggle: gtk::ToggleButton,
    audio_meters: (gtk::LevelBar, gtk::LevelBar),
    audio_status: gtk::Label,
    recent: gtk::Box,
    // Side panel.
    source_dd: gtk::DropDown,
    source_field: gtk::Box,
    fps_field: gtk::Box,
    fps_dd: gtk::DropDown,
    resolution_field: gtk::Box,
    resolution_dd: gtk::DropDown,
    quality_field: gtk::Box,
    quality_dd: gtk::DropDown,
    system_dd: gtk::DropDown,
    mic_switch: gtk::Switch,
    mic_dd: gtk::DropDown,
    cursor_row: gtk::Box,
    cursor_switch: gtk::Switch,
    clicks_row: gtk::Box,
    clicks_switch: gtk::Switch,
    countdown_switch: gtk::Switch,
    overlay_row: gtk::Box,
    overlay_switch: gtk::Switch,
    path_label: gtk::Label,
    prefix_entry: gtk::Entry,
    pattern_dd: gtk::DropDown,
    post_dd: gtk::DropDown,
}

struct State {
    app: Rc<App>,
    ui: Ui,
    mode: Cell<Mode>,
    output: RefCell<String>,
    window: RefCell<String>,
    region: RefCell<Option<Region>>,
    visible: Cell<bool>,
    holding_camera: Cell<bool>,
    preview: RefCell<Option<preview::Handle>>,
    pip_preview: RefCell<Option<preview::Handle>>,
    capture: RefCell<Option<Arc<screen::Capture>>>,
    countdown: Rc<Countdown>,
}

thread_local! {
    /// The page, for the Record page's quick-start cards to switch its tab.
    static PAGE: RefCell<std::rc::Weak<State>> = const { RefCell::new(std::rc::Weak::new()) };
}

/// Switch the Capture page to tab `i` (Screen, Window, Region, Camera,
/// Audio Only).
pub fn select_tab(_app: &Rc<App>, i: usize) {
    PAGE.with(|p| {
        if let Some(st) = p.borrow().upgrade() {
            if let Some(t) = st.ui.tabs.get(i) {
                t.set_active(true);
            }
        }
    });
}

pub fn page(app: &Rc<App>) -> Page {
    let (widget, ui) = build(app);
    let st = Rc::new(State {
        app: app.clone(),
        ui,
        mode: Cell::new(Mode::Screen),
        output: RefCell::default(),
        window: RefCell::default(),
        region: RefCell::new(None),
        visible: Cell::new(false),
        holding_camera: Cell::new(false),
        preview: RefCell::new(None),
        pip_preview: RefCell::new(None),
        capture: RefCell::new(None),
        countdown: Rc::default(),
    });
    PAGE.with(|p| *p.borrow_mut() = Rc::downgrade(&st));
    wire(&st);
    sync_settings(&st);
    sync_recording(&st);
    for topic in [Topic::Screen, Topic::Cameras] {
        let s = st.clone();
        app.on(topic, move || {
            sync_sources(&s);
            if s.visible.get() && !s.app.is_recording() {
                refresh(&s);
            }
        });
    }
    {
        let s = st.clone();
        app.on(Topic::Audio, move || sync_audio(&s));
    }
    {
        let s = st.clone();
        app.on(Topic::Recording, move || sync_recording(&s));
    }
    {
        let s = st.clone();
        app.on(Topic::Library, move || load_recent(&s));
    }
    {
        let s = st.clone();
        app.on(Topic::Settings, move || sync_settings(&s));
    }
    load_recent(&st);
    let (show, hide) = (st.clone(), st.clone());
    Page {
        id: "capture",
        title: "Capture",
        icon: "video-display-symbolic",
        widget,
        on_show: Box::new(move || {
            show.visible.set(true);
            sync_sources(&show);
            refresh(&show);
        }),
        on_hide: Box::new(move || {
            hide.visible.set(false);
            stop_previews(&hide);
        }),
    }
}

fn build(app: &Rc<App>) -> (gtk::Widget, Ui) {
    let s = app.settings.borrow().clone();
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    root.set_margin_top(6);
    root.set_margin_bottom(18);
    root.set_margin_start(18);
    root.set_margin_end(18);

    // ── The main column ──────────────────────────────────────────────
    let main = gtk::Box::new(gtk::Orientation::Vertical, 16);
    main.set_hexpand(true);

    let tab_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tab_row.add_css_class("source-tabs");
    tab_row.set_homogeneous(true);
    let mut tabs: Vec<gtk::ToggleButton> = Vec::new();
    for m in Mode::ALL {
        let b = gtk::ToggleButton::new();
        let inner = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        inner.set_halign(gtk::Align::Center);
        inner.append(&gtk::Image::from_icon_name(m.icon()));
        // Ellipsized, so five tabs do not set a floor on the window's width;
        // the icon and the tooltip still say which is which.
        let label = gtk::Label::new(Some(m.label()));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        inner.append(&label);
        b.set_child(Some(&inner));
        b.set_tooltip_text(Some(m.label()));
        if let Some(first) = tabs.first() {
            b.set_group(Some(first));
        }
        tab_row.append(&b);
        tabs.push(b);
    }
    tabs[0].set_active(true);
    main.append(&tab_row);

    // The viewfinder.
    let overlay = gtk::Overlay::new();
    overlay.set_vexpand(true);
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("preview-frame");
    frame.set_overflow(gtk::Overflow::Hidden);
    // Enough for the badges and the control bar; the rest is vexpand. Any
    // more and the page is taller than the window's smallest height.
    frame.set_size_request(-1, 200);
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    let picture = gtk::Picture::new();
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.set_can_shrink(true);
    stack.add_named(&picture, Some("picture"));
    let empty = widgets::empty("video-display-symbolic", "", "");
    let empty_button = gtk::Button::with_label("Select Region");
    empty_button.add_css_class("suggested-action");
    empty_button.add_css_class("pill");
    empty_button.set_halign(gtk::Align::Center);
    empty.set_child(Some(&empty_button));
    stack.add_named(&empty, Some("empty"));
    let (audio_view, audio_meters, audio_status) = audio_panel();
    stack.add_named(&audio_view, Some("audio"));
    frame.append(&stack);
    overlay.set_child(Some(&frame));

    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.set_valign(gtk::Align::Start);
    top.set_margin_top(16);
    top.set_margin_start(16);
    top.set_margin_end(16);
    let size_badge = gtk::Label::new(None);
    size_badge.add_css_class("preview-badge");
    let live_badge = gtk::Label::new(Some("● REC"));
    live_badge.add_css_class("preview-badge");
    live_badge.add_css_class("live");
    live_badge.set_visible(false);
    top.append(&live_badge);
    top.append(&size_badge);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    top.append(&spacer);
    let picker = gtk::DropDown::from_strings(&[]);
    picker.add_css_class("preview-picker");
    let region_button = gtk::Button::with_label("Select Region");
    region_button.add_css_class("preview-picker");
    top.append(&region_button);
    top.append(&picker);
    overlay.add_overlay(&top);

    let pip = gtk::Picture::new();
    pip.set_content_fit(gtk::ContentFit::Cover);
    pip.set_size_request(200, 112);
    pip.add_css_class("thumb-frame");
    pip.set_overflow(gtk::Overflow::Hidden);
    pip.set_halign(gtk::Align::End);
    pip.set_valign(gtk::Align::End);
    pip.set_margin_end(20);
    pip.set_margin_bottom(100);
    pip.set_visible(false);
    pip.set_can_target(false);
    overlay.add_overlay(&pip);

    let countdown_label = gtk::Label::new(None);
    countdown_label.add_css_class("countdown");
    countdown_label.set_visible(false);
    countdown_label.set_can_target(false);
    overlay.add_overlay(&countdown_label);

    // The floating control bar.
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("control-bar");
    bar.set_halign(gtk::Align::Center);
    bar.set_valign(gtk::Align::End);
    bar.set_margin_bottom(14);
    let source_button = widgets::round_button("video-display-symbolic", "Choose what to capture");
    let system = widgets::round_toggle("audio-volume-high-symbolic", "Record system sound");
    let mic = widgets::round_toggle("audio-input-microphone-symbolic", "Record the microphone");
    let rec = gtk::Button::with_label("REC");
    rec.add_css_class("rec-button");
    rec.set_tooltip_text(Some("Start recording (Ctrl+R)"));
    let pause = widgets::round_toggle("media-playback-pause-symbolic", "Pause");
    pause.set_visible(false);
    let timer = gtk::Label::new(Some("00:00:00"));
    timer.add_css_class("timer");
    let still = widgets::round_button("camera-photo-symbolic", "Take a screenshot");
    let overlay_toggle = widgets::round_toggle("camera-web-symbolic", "Camera overlay");
    bar.append(&source_button);
    bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    bar.append(&system);
    bar.append(&mic);
    bar.append(&rec);
    bar.append(&pause);
    bar.append(&timer);
    bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    bar.append(&still);
    bar.append(&overlay_toggle);
    overlay.add_overlay(&bar);
    main.append(&overlay);

    // Recent captures.
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let rt = gtk::Label::new(Some("Recent Captures"));
    rt.add_css_class("section-head");
    rt.set_xalign(0.0);
    rt.set_hexpand(true);
    head.append(&rt);
    let view_all = gtk::Button::new();
    let va = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    va.append(&gtk::Label::new(Some("View All")));
    va.append(&gtk::Image::from_icon_name("go-next-symbolic"));
    view_all.set_child(Some(&va));
    view_all.add_css_class("link-button");
    {
        let app = app.clone();
        view_all.connect_clicked(move |_| app.navigate("media"));
    }
    head.append(&view_all);
    main.append(&head);
    let recent = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    recent.set_homogeneous(true);
    recent.set_size_request(-1, 170);
    // Four fixed-width tiles are wider than a narrow window; they scroll
    // sideways rather than hold the window open.
    let recent_scroll = gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&recent)
        .build();
    main.append(&recent_scroll);
    root.append(&main);

    // ── The side panel ───────────────────────────────────────────────
    let side = gtk::Box::new(gtk::Orientation::Vertical, 16);
    side.add_css_class("side-panel");
    side.set_size_request(340, -1);
    let settings_card = widgets::card("Recording Settings");
    let source_dd = gtk::DropDown::from_strings(&[]);
    source_dd.set_hexpand(true);
    let source_field = widgets::field("Video Source", &source_dd);
    settings_card.append(&source_field);
    let fps_labels: Vec<String> = FPS.iter().map(|f| format!("{f} FPS")).collect();
    let fps_dd = widgets::dropdown(
        &fps_labels,
        FPS.iter().position(|&f| f == s.recording.fps).unwrap_or(2) as u32,
        |_| {},
    );
    let fps_field = widgets::field("Frame Rate", &fps_dd);
    settings_card.append(&fps_field);
    let resolution_dd = gtk::DropDown::from_strings(&[]);
    resolution_dd.set_hexpand(true);
    let resolution_field = widgets::field("Resolution", &resolution_dd);
    settings_card.append(&resolution_field);
    let q_labels: Vec<String> = Quality::ALL.iter().map(|q| q.label().to_owned()).collect();
    let quality_dd = widgets::dropdown(
        &q_labels,
        Quality::ALL
            .iter()
            .position(|&q| q == s.recording.quality)
            .unwrap_or(1) as u32,
        |_| {},
    );
    let quality_field = widgets::field("Video Quality", &quality_dd);
    settings_card.append(&quality_field);
    let system_dd = gtk::DropDown::from_strings(&["Default (System Audio)"]);
    system_dd.set_hexpand(true);
    settings_card.append(&widgets::field("Audio Input", &system_dd));
    let mic_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    mic_row.add_css_class("inset-row");
    mic_row.set_margin_top(8);
    mic_row.append(&gtk::Image::from_icon_name(
        "audio-input-microphone-symbolic",
    ));
    let ml = gtk::Label::new(Some("Microphone (Optional)"));
    ml.add_css_class("dim");
    ml.set_hexpand(true);
    ml.set_xalign(0.0);
    mic_row.append(&ml);
    let mic_switch = gtk::Switch::new();
    mic_switch.set_valign(gtk::Align::Center);
    mic_switch.set_active(s.recording.microphone);
    mic_row.append(&mic_switch);
    settings_card.append(&mic_row);
    let mic_dd = gtk::DropDown::from_strings(&["Default Microphone"]);
    mic_dd.set_hexpand(true);
    mic_dd.set_visible(s.recording.microphone);
    settings_card.append(&mic_dd);
    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.set_size_request(-1, 8);
    settings_card.append(&spacer);
    let (cursor_row, cursor_switch) =
        widgets::switch_row("Show Cursor", s.recording.show_cursor, |_| {});
    let (clicks_row, clicks_switch) =
        widgets::switch_row("Highlight Clicks", s.recording.highlight_clicks, |_| {});
    let (cd_row, countdown_switch) =
        widgets::switch_row("Enable Countdown", s.recording.countdown, |_| {});
    let (overlay_row, overlay_switch) =
        widgets::switch_row("Show Webcam Overlay", s.recording.webcam_overlay, |_| {});
    for r in [&cursor_row, &clicks_row, &cd_row, &overlay_row] {
        settings_card.append(r);
    }
    side.append(&settings_card);

    let save_card = widgets::card("");
    let path_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let path_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    path_box.add_css_class("path-box");
    path_box.set_hexpand(true);
    path_box.append(&gtk::Image::from_icon_name("folder-symbolic"));
    let path_label = gtk::Label::new(None);
    path_label.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    path_label.set_xalign(0.0);
    path_label.set_hexpand(true);
    path_label.set_max_width_chars(1);
    path_box.append(&path_label);
    path_row.append(&path_box);
    let browse = gtk::Button::with_label("Browse");
    path_row.append(&browse);
    save_card.append(&widgets::field("Save Location", &path_row));
    {
        let app = app.clone();
        browse.connect_clicked(move |_| choose_folder(&app, true));
    }
    let naming = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let prefix_entry = gtk::Entry::new();
    prefix_entry.set_text(&s.name_prefix);
    prefix_entry.set_width_chars(10);
    prefix_entry.set_hexpand(true);
    let patterns: Vec<String> = crate::naming::PATTERNS
        .iter()
        .map(|p| (*p).to_owned())
        .collect();
    let pattern_dd = widgets::dropdown(
        &patterns,
        crate::naming::PATTERNS
            .iter()
            .position(|p| *p == s.name_pattern)
            .unwrap_or(0) as u32,
        |_| {},
    );
    pattern_dd.set_hexpand(true);
    naming.append(&prefix_entry);
    naming.append(&pattern_dd);
    save_card.append(&widgets::field("File Naming", &naming));
    let post_labels: Vec<String> = crate::settings::PostAction::ALL
        .iter()
        .map(|p| p.label().to_owned())
        .collect();
    let post_dd = widgets::dropdown(
        &post_labels,
        crate::settings::PostAction::ALL
            .iter()
            .position(|&p| p == s.post_action)
            .unwrap_or(0) as u32,
        |_| {},
    );
    save_card.append(&widgets::field("Post Recording", &post_dd));
    side.append(&save_card);

    let side_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&side)
        .propagate_natural_width(true)
        .build();
    root.append(&side_scroll);

    let ui = Ui {
        tabs,
        overlay,
        frame,
        stack,
        picture,
        empty,
        empty_button,
        pip,
        size_badge,
        live_badge,
        picker,
        region_button,
        countdown_label,
        source_button,
        system,
        mic,
        rec,
        pause,
        timer,
        still,
        overlay_toggle,
        audio_meters,
        audio_status,
        recent,
        source_dd,
        source_field,
        fps_field,
        fps_dd,
        resolution_field,
        resolution_dd,
        quality_field,
        quality_dd,
        system_dd,
        mic_switch,
        mic_dd,
        cursor_row,
        cursor_switch,
        clicks_row,
        clicks_switch,
        countdown_switch,
        overlay_row,
        overlay_switch,
        path_label,
        prefix_entry,
        pattern_dd,
        post_dd,
    };
    (root.upcast(), ui)
}

fn audio_panel() -> (gtk::Box, (gtk::LevelBar, gtk::LevelBar), gtk::Label) {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 14);
    b.set_valign(gtk::Align::Center);
    b.set_halign(gtk::Align::Center);
    b.set_size_request(420, -1);
    let icon = gtk::Image::from_icon_name("audio-input-microphone-symbolic");
    icon.set_pixel_size(64);
    icon.add_css_class("dim");
    b.append(&icon);
    let status = gtk::Label::new(Some("Ready to record sound"));
    status.add_css_class("section-head");
    b.append(&status);
    let meter = |name: &str| {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let l = gtk::Label::new(Some(name));
        l.set_xalign(0.0);
        l.add_css_class("dim");
        let m = gtk::LevelBar::for_interval(0.0, 1.0);
        m.add_css_class("meter");
        m.set_hexpand(true);
        row.append(&l);
        row.append(&m);
        (row, m)
    };
    let (r1, m1) = meter("System sound");
    let (r2, m2) = meter("Microphone");
    b.append(&r1);
    b.append(&r2);
    (b, (m1, m2), status)
}

fn wire(st: &Rc<State>) {
    let ui = &st.ui;
    for (i, tab) in ui.tabs.iter().enumerate() {
        let s = st.clone();
        tab.connect_toggled(move |t| {
            if t.is_active() {
                s.mode.set(Mode::ALL[i]);
                sync_sources(&s);
                sync_mode(&s);
                refresh(&s);
            }
        });
    }
    {
        let s = st.clone();
        ui.rec.connect_clicked(move |_| rec_clicked(&s));
    }
    {
        let s = st.clone();
        ui.pause.connect_toggled(move |p| {
            if s.app.is_recording() {
                s.app.set_paused(p.is_active());
            }
        });
    }
    {
        let s = st.clone();
        ui.still.connect_clicked(move |_| take_still(&s));
    }
    {
        let s = st.clone();
        let select = move || {
            let s2 = s.clone();
            s.app.select_region(move |r| {
                if let Some(r) = r {
                    *s2.region.borrow_mut() = Some(r);
                    sync_sources(&s2);
                    refresh(&s2);
                }
            });
        };
        let select = Rc::new(select);
        let a = select.clone();
        ui.region_button.connect_clicked(move |_| a());
        let b = select.clone();
        ui.empty_button.connect_clicked(move |_| b());
        let c = select.clone();
        let s = st.clone();
        ui.source_button.connect_clicked(move |_| {
            if s.mode.get() == Mode::Region {
                c();
            } else {
                s.ui.picker.emit_by_name::<()>("activate", &[]);
            }
        });
    }
    // The picker in the viewfinder and the Video Source field are one
    // choice shown twice.
    for dd in [&ui.picker, &ui.source_dd] {
        let s = st.clone();
        dd.connect_selected_notify(move |dd| {
            if dd.widget_name() == "quiet" {
                return;
            }
            pick_source(&s, dd.selected() as usize);
        });
    }
    {
        let s = st.clone();
        ui.system.connect_toggled(move |t| {
            let on = t.is_active();
            if let Some(live) = s.app.recording.borrow().as_ref() {
                live.set_system_muted(!on);
            }
            if !s.app.is_recording() {
                s.app.update_settings(|x| x.recording.system_audio = on);
            }
            style_toggle(
                t,
                on,
                "audio-volume-high-symbolic",
                "audio-volume-muted-symbolic",
            );
        });
    }
    {
        let s = st.clone();
        ui.mic.connect_toggled(move |t| {
            let on = t.is_active();
            if let Some(live) = s.app.recording.borrow().as_ref() {
                live.set_microphone_muted(!on);
            }
            if !s.app.is_recording() {
                s.app.update_settings(|x| x.recording.microphone = on);
            }
            style_toggle(
                t,
                on,
                "audio-input-microphone-symbolic",
                "microphone-disabled-symbolic",
            );
        });
    }
    {
        let s = st.clone();
        ui.overlay_toggle.connect_toggled(move |t| {
            let on = t.is_active();
            if s.app.settings.borrow().recording.webcam_overlay != on {
                s.app.update_settings(|x| x.recording.webcam_overlay = on);
                refresh(&s);
            }
        });
    }
    // Side panel.
    {
        let s = st.clone();
        ui.fps_dd.connect_selected_notify(move |dd| {
            if dd.widget_name() != "quiet" {
                let fps = FPS[dd.selected() as usize];
                s.app.update_settings(|x| x.recording.fps = fps);
            }
        });
    }
    {
        let s = st.clone();
        ui.quality_dd.connect_selected_notify(move |dd| {
            if dd.widget_name() != "quiet" {
                let q = Quality::ALL[dd.selected() as usize];
                s.app.update_settings(|x| x.recording.quality = q);
            }
        });
    }
    {
        let s = st.clone();
        ui.resolution_dd.connect_selected_notify(move |dd| {
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
        ui.system_dd.connect_selected_notify(move |dd| {
            if dd.widget_name() == "quiet" {
                return;
            }
            let i = dd.selected() as usize;
            let outputs = s.app.audio.borrow().outputs.clone();
            // 0 default, 1..=n a device, n+1 none.
            let (on, name) = if i == 0 {
                (true, String::new())
            } else if i <= outputs.len() {
                (true, outputs[i - 1].name.clone())
            } else {
                (false, String::new())
            };
            s.app.update_settings(|x| {
                x.recording.system_audio = on;
                x.recording.system_audio_device = name;
            });
        });
    }
    {
        let s = st.clone();
        ui.mic_switch.connect_active_notify(move |sw| {
            if sw.widget_name() != "quiet" {
                let on = sw.is_active();
                s.app.update_settings(|x| x.recording.microphone = on);
            }
        });
    }
    {
        let s = st.clone();
        ui.mic_dd.connect_selected_notify(move |dd| {
            if dd.widget_name() == "quiet" {
                return;
            }
            let i = dd.selected() as usize;
            let inputs = s.app.audio.borrow().inputs.clone();
            let name = if i == 0 {
                String::new()
            } else {
                inputs
                    .get(i - 1)
                    .map(|d| d.name.clone())
                    .unwrap_or_default()
            };
            s.app
                .update_settings(|x| x.recording.microphone_device = name);
        });
    }
    let switch = |sw: &gtk::Switch, f: fn(&mut crate::settings::Settings, bool), restart: bool| {
        let s = st.clone();
        sw.connect_active_notify(move |sw| {
            if sw.widget_name() == "quiet" {
                return;
            }
            let on = sw.is_active();
            s.app.update_settings(|x| f(x, on));
            if restart && !s.app.is_recording() {
                refresh(&s);
            }
        });
    };
    switch(
        &ui.cursor_switch,
        |x, on| x.recording.show_cursor = on,
        true,
    );
    switch(
        &ui.clicks_switch,
        |x, on| x.recording.highlight_clicks = on,
        true,
    );
    switch(
        &ui.countdown_switch,
        |x, on| x.recording.countdown = on,
        false,
    );
    switch(
        &ui.overlay_switch,
        |x, on| x.recording.webcam_overlay = on,
        true,
    );
    {
        let s = st.clone();
        ui.prefix_entry.connect_changed(move |e| {
            let text = e.text().to_string();
            if s.app.settings.borrow().name_prefix != text {
                s.app.update_settings(|x| x.name_prefix = text);
            }
        });
    }
    {
        let s = st.clone();
        ui.pattern_dd.connect_selected_notify(move |dd| {
            if dd.widget_name() != "quiet" {
                let p = crate::naming::PATTERNS[dd.selected() as usize].to_owned();
                s.app.update_settings(|x| x.name_pattern = p);
            }
        });
    }
    {
        let s = st.clone();
        ui.post_dd.connect_selected_notify(move |dd| {
            if dd.widget_name() != "quiet" {
                let p = crate::settings::PostAction::ALL[dd.selected() as usize];
                s.app.update_settings(|x| x.post_action = p);
            }
        });
    }
    // Ctrl+R records from anywhere on this page.
    {
        let keys = gtk::EventControllerKey::new();
        let s = st.clone();
        keys.connect_key_pressed(move |_, key, _, mods| {
            if mods.contains(gtk::gdk::ModifierType::CONTROL_MASK) && key == gtk::gdk::Key::r {
                rec_clicked(&s);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        ui.overlay.add_controller(keys);
    }
    sync_mode(st);
}

fn style_toggle(t: &gtk::ToggleButton, on: bool, on_icon: &str, off_icon: &str) {
    t.set_icon_name(if on { on_icon } else { off_icon });
    if on {
        t.remove_css_class("off");
    } else {
        t.add_css_class("off");
    }
}

/// Reflect the settings in every control, without triggering handlers.
fn sync_settings(st: &Rc<State>) {
    let s = st.app.settings.borrow().clone();
    let ui = &st.ui;
    set_selected_quietly(
        &ui.fps_dd,
        FPS.iter().position(|&f| f == s.recording.fps).unwrap_or(2) as u32,
    );
    set_selected_quietly(
        &ui.quality_dd,
        Quality::ALL
            .iter()
            .position(|&q| q == s.recording.quality)
            .unwrap_or(1) as u32,
    );
    set_active_quietly(&ui.mic_switch, s.recording.microphone);
    ui.mic_dd.set_visible(s.recording.microphone);
    set_active_quietly(&ui.cursor_switch, s.recording.show_cursor);
    set_active_quietly(&ui.clicks_switch, s.recording.highlight_clicks);
    set_active_quietly(&ui.countdown_switch, s.recording.countdown);
    set_active_quietly(&ui.overlay_switch, s.recording.webcam_overlay);
    if !st.app.is_recording() {
        if ui.system.is_active() != s.recording.system_audio {
            ui.system.set_active(s.recording.system_audio);
        }
        style_toggle(
            &ui.system,
            s.recording.system_audio,
            "audio-volume-high-symbolic",
            "audio-volume-muted-symbolic",
        );
        if ui.mic.is_active() != s.recording.microphone {
            ui.mic.set_active(s.recording.microphone);
        }
        style_toggle(
            &ui.mic,
            s.recording.microphone,
            "audio-input-microphone-symbolic",
            "microphone-disabled-symbolic",
        );
    }
    if ui.overlay_toggle.is_active() != s.recording.webcam_overlay {
        ui.overlay_toggle.set_active(s.recording.webcam_overlay);
    }
    ui.path_label
        .set_text(&crate::paths::display(&s.video_dir()));
    if ui.prefix_entry.text() != s.name_prefix {
        ui.prefix_entry.set_text(&s.name_prefix);
    }
    set_selected_quietly(
        &ui.pattern_dd,
        crate::naming::PATTERNS
            .iter()
            .position(|p| *p == s.name_pattern)
            .unwrap_or(0) as u32,
    );
    set_selected_quietly(
        &ui.post_dd,
        crate::settings::PostAction::ALL
            .iter()
            .position(|&p| p == s.post_action)
            .unwrap_or(0) as u32,
    );
    sync_audio(st);
}

fn sync_audio(st: &Rc<State>) {
    let devices = st.app.audio.borrow().clone();
    let s = st.app.settings.borrow().clone();
    let mut outs = vec!["Default (System Audio)".to_owned()];
    outs.extend(devices.outputs.iter().map(|d| d.description.clone()));
    outs.push("None".into());
    let sel = if !s.recording.system_audio {
        outs.len() - 1
    } else if s.recording.system_audio_device.is_empty() {
        0
    } else {
        devices
            .outputs
            .iter()
            .position(|d| d.name == s.recording.system_audio_device)
            .map_or(0, |i| i + 1)
    };
    set_items(&st.ui.system_dd, &outs, sel as u32);
    let mut ins = vec!["Default Microphone".to_owned()];
    ins.extend(devices.inputs.iter().map(|d| d.description.clone()));
    let sel = devices
        .inputs
        .iter()
        .position(|d| d.name == s.recording.microphone_device)
        .map_or(0, |i| i + 1);
    set_items(&st.ui.mic_dd, &ins, sel as u32);
    let sound = devices.available || devices.outputs.is_empty() && devices.inputs.is_empty();
    st.ui.system.set_sensitive(sound);
    st.ui.mic.set_sensitive(sound);
}

/// Fill the source pickers for the current mode.
fn sync_sources(st: &Rc<State>) {
    let mode = st.mode.get();
    let screen = st.app.screen_state.borrow().clone();
    let (labels, selected): (Vec<String>, usize) = match mode {
        Mode::Screen => {
            if st.output.borrow().is_empty()
                || !screen.outputs.iter().any(|o| o.name == *st.output.borrow())
            {
                let pick = screen
                    .outputs
                    .iter()
                    .find(|o| o.focused)
                    .or(screen.outputs.first())
                    .map(|o| o.name.clone())
                    .unwrap_or_default();
                *st.output.borrow_mut() = pick;
            }
            (
                screen
                    .outputs
                    .iter()
                    .enumerate()
                    .map(|(i, o)| o.label(i))
                    .collect(),
                screen
                    .outputs
                    .iter()
                    .position(|o| o.name == *st.output.borrow())
                    .unwrap_or(0),
            )
        }
        Mode::Window => {
            let own = "com.ravencamera.Raven";
            let windows: Vec<&screen::Window> =
                screen.windows.iter().filter(|w| w.app_id != own).collect();
            if !windows.iter().any(|w| w.identifier == *st.window.borrow()) {
                st.window.borrow_mut().clear();
            }
            let mut labels = vec!["Choose a window…".to_owned()];
            labels.extend(windows.iter().map(|w| {
                if w.title.is_empty() {
                    w.app_id.clone()
                } else {
                    w.title.clone()
                }
            }));
            let sel = windows
                .iter()
                .position(|w| w.identifier == *st.window.borrow())
                .map_or(0, |i| i + 1);
            (labels, sel)
        }
        Mode::Region => match st.region.borrow().as_ref() {
            Some(r) => (
                vec![format!("{} × {} on {}", r.width, r.height, r.output)],
                0,
            ),
            None => (vec!["No region yet".into()], 0),
        },
        Mode::Camera => {
            let cams = st.app.cameras.borrow();
            let chosen = st.app.chosen_camera();
            (
                cams.iter()
                    .map(|d| format!("{} ({})", d.name, d.kind_label()))
                    .collect(),
                chosen
                    .and_then(|c| cams.iter().position(|d| d.path == c.path))
                    .unwrap_or(0),
            )
        }
        Mode::Audio => (Vec::new(), 0),
    };
    set_items(&st.ui.picker, &labels, selected as u32);
    set_items(&st.ui.source_dd, &labels, selected as u32);
    // Camera resolutions.
    if let Some(dev) = st.app.chosen_camera() {
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
        set_items(&st.ui.resolution_dd, &labels, sel as u32);
    }
}

fn pick_source(st: &Rc<State>, i: usize) {
    match st.mode.get() {
        Mode::Screen => {
            let outputs = st.app.screen_state.borrow().outputs.clone();
            if let Some(o) = outputs.get(i) {
                *st.output.borrow_mut() = o.name.clone();
            }
        }
        Mode::Window => {
            let own = "com.ravencamera.Raven";
            let windows: Vec<screen::Window> = st
                .app
                .screen_state
                .borrow()
                .windows
                .iter()
                .filter(|w| w.app_id != own)
                .cloned()
                .collect();
            *st.window.borrow_mut() = if i == 0 {
                String::new()
            } else {
                windows
                    .get(i - 1)
                    .map(|w| w.identifier.clone())
                    .unwrap_or_default()
            };
        }
        Mode::Camera => {
            let key = st.app.cameras.borrow().get(i).map(|d| d.key());
            if let Some(key) = key {
                st.app.update_settings(|x| {
                    x.camera.device = key;
                    x.camera.resolution.clear();
                });
                st.app.camera_restart();
            }
        }
        _ => {}
    }
    sync_sources(st);
    refresh(st);
}

/// Show and hide what each mode has.
fn sync_mode(st: &Rc<State>) {
    let mode = st.mode.get();
    let ui = &st.ui;
    ui.picker
        .set_visible(matches!(mode, Mode::Screen | Mode::Window | Mode::Camera));
    ui.region_button.set_visible(mode == Mode::Region);
    ui.source_field.set_visible(mode != Mode::Audio);
    ui.fps_field.set_visible(mode.is_screen());
    ui.resolution_field.set_visible(mode == Mode::Camera);
    ui.quality_field.set_visible(mode != Mode::Audio);
    ui.cursor_row.set_visible(mode.is_screen());
    ui.clicks_row.set_visible(mode.is_screen());
    ui.overlay_row.set_visible(mode.is_screen());
    ui.overlay_toggle.set_visible(mode.is_screen());
    ui.still.set_visible(mode != Mode::Audio);
    ui.still.set_tooltip_text(Some(if mode == Mode::Camera {
        "Take a photo"
    } else {
        "Take a screenshot"
    }));
    ui.source_button.set_icon_name(mode.icon());
    ui.size_badge.set_visible(mode != Mode::Audio);
}

fn options(st: &State) -> screen::Options {
    let s = st.app.settings.borrow();
    screen::Options {
        cursor: s.recording.show_cursor,
        clicks: s.recording.highlight_clicks,
    }
}

/// The screen source the current mode names, if it names one.
fn screen_source(st: &State) -> Option<Source> {
    match st.mode.get() {
        Mode::Screen => {
            let o = st.output.borrow().clone();
            (!o.is_empty()).then_some(Source::Output(o))
        }
        Mode::Window => {
            let w = st.window.borrow().clone();
            (!w.is_empty()).then_some(Source::Window(w))
        }
        Mode::Region => st.region.borrow().clone().map(Source::Region),
        _ => None,
    }
}

fn show_empty(st: &State, icon: &str, title: &str, text: &str, button: Option<&str>) {
    st.ui.empty.set_icon_name(Some(icon));
    st.ui.empty.set_title(title);
    st.ui.empty.set_description(Some(text));
    st.ui.empty_button.set_visible(button.is_some());
    if let Some(b) = button {
        st.ui.empty_button.set_label(b);
    }
    st.ui.stack.set_visible_child_name("empty");
    st.ui.size_badge.set_visible(false);
}

fn stop_previews(st: &State) {
    st.preview.borrow_mut().take();
    st.pip_preview.borrow_mut().take();
    st.ui.pip.set_visible(false);
    if !st.app.is_recording() {
        st.capture.borrow_mut().take();
        st.app.screen_release();
    }
    if st.holding_camera.replace(false) {
        st.app.camera_release();
    }
}

/// Start the preview for what is chosen now.
fn refresh(st: &Rc<State>) {
    if !st.visible.get() {
        return;
    }
    // While a full-size photo is taken the last picture stays; the camera
    // coming back says so with Topic::Cameras.
    if st.app.is_recording() || st.app.camera_busy() {
        return;
    }
    stop_previews(st);
    let mode = st.mode.get();
    let settings = st.app.settings.borrow().clone();
    let screen = st.app.screen_state.borrow().clone();
    st.ui.rec.set_sensitive(true);
    st.ui.size_badge.set_visible(mode != Mode::Audio);

    if mode.is_screen() {
        match screen.capture {
            None => {
                show_empty(
                    st,
                    "video-display-symbolic",
                    "Connecting to the desktop…",
                    "",
                    None,
                );
                return;
            }
            Some(false) => {
                let why = screen.unavailable.unwrap_or_else(|| {
                    "This desktop's compositor does not offer screen capture to applications. \
                     Raven Camera needs Huginn with raven_shell_v1 version 4."
                        .into()
                });
                show_empty(
                    st,
                    "dialog-warning-symbolic",
                    "Screen capture is not available",
                    &why,
                    None,
                );
                st.ui.rec.set_sensitive(false);
                return;
            }
            Some(true) => {}
        }
        let Some(source) = screen_source(st) else {
            match mode {
                Mode::Window => show_empty(
                    st,
                    "window-new-symbolic",
                    "Choose a window",
                    "Pick a window from the list at the top right to record it on its own.",
                    None,
                ),
                Mode::Region => show_empty(
                    st,
                    "selection-mode-symbolic",
                    "Choose a region",
                    "Drag out the part of the screen to record.",
                    Some("Select Region"),
                ),
                _ => show_empty(
                    st,
                    "video-display-symbolic",
                    "No screens",
                    "The compositor reported no screens.",
                    None,
                ),
            }
            st.ui.rec.set_sensitive(false);
            return;
        };
        let capture = st
            .app
            .screen_capture(source, options(st), preview::SCREEN_FPS);
        *st.capture.borrow_mut() = Some(capture.clone());
        st.ui.stack.set_visible_child_name("picture");
        let (picture, badge) = (st.ui.picture.clone(), st.ui.size_badge.clone());
        *st.preview.borrow_mut() = Some(preview::screen(&capture, move |t| {
            badge.set_text(&format!("{} × {}", t.width(), t.height()));
            preview::show(&picture, &t);
        }));
        if settings.recording.webcam_overlay {
            start_pip(st, settings.recording.overlay_corner);
        }
        return;
    }
    match mode {
        Mode::Camera => {
            if st.app.cameras.borrow().is_empty() {
                show_empty(
                    st,
                    "camera-disabled-symbolic",
                    "No camera connected",
                    "Plug in a USB webcam, or check the built-in camera's privacy switch. \
                     Cameras are picked up the moment they appear.",
                    None,
                );
                st.ui.rec.set_sensitive(false);
                return;
            }
            st.app.camera_acquire();
            st.holding_camera.set(true);
            match st.app.camera_stream() {
                Ok(stream) => {
                    st.ui.stack.set_visible_child_name("picture");
                    st.ui
                        .size_badge
                        .set_text(&format!("{} × {}", stream.mode.width, stream.mode.height));
                    let picture = st.ui.picture.clone();
                    *st.preview.borrow_mut() = Some(preview::camera(
                        &stream,
                        settings.camera.mirror_preview,
                        settings.camera.enhance,
                        move |t| {
                            preview::show(&picture, &t);
                        },
                    ));
                }
                Err(e) => {
                    show_empty(
                        st,
                        "camera-disabled-symbolic",
                        "The camera could not be started",
                        &format!("{e:#}"),
                        None,
                    );
                    st.ui.rec.set_sensitive(false);
                }
            }
        }
        Mode::Audio => {
            st.ui.stack.set_visible_child_name("audio");
            let (sys, mic) = {
                let s = st.app.settings.borrow();
                (s.recording.system_audio, s.recording.microphone)
            };
            st.ui.audio_status.set_text(match (sys, mic) {
                (false, false) => "Turn on system sound or the microphone to record",
                (true, false) => "Ready to record system sound",
                (false, true) => "Ready to record the microphone",
                (true, true) => "Ready to record system sound and the microphone",
            });
            st.ui.rec.set_sensitive(sys || mic);
        }
        _ => {}
    }
}

fn start_pip(st: &Rc<State>, corner: Corner) {
    if st.app.cameras.borrow().is_empty() {
        return;
    }
    if !st.holding_camera.get() {
        st.app.camera_acquire();
        st.holding_camera.set(true);
    }
    let Ok(stream) = st.app.camera_stream() else {
        return;
    };
    let pip = st.ui.pip.clone();
    let (h, v) = match corner {
        Corner::TopLeft => (gtk::Align::Start, gtk::Align::Start),
        Corner::TopRight => (gtk::Align::End, gtk::Align::Start),
        Corner::BottomLeft => (gtk::Align::Start, gtk::Align::End),
        Corner::BottomRight => (gtk::Align::End, gtk::Align::End),
    };
    pip.set_halign(h);
    pip.set_valign(v);
    pip.set_margin_start(20);
    pip.set_margin_top(if v == gtk::Align::Start { 60 } else { 0 });
    pip.set_visible(true);
    let (mirror, enhance) = {
        let s = st.app.settings.borrow();
        (s.camera.mirror_preview, s.camera.enhance)
    };
    *st.pip_preview.borrow_mut() = Some(preview::camera(&stream, mirror, enhance, move |t| {
        preview::show(&pip, &t)
    }));
}

fn rec_clicked(st: &Rc<State>) {
    if st.app.is_recording() {
        st.app.stop_recording();
        return;
    }
    if st.countdown.running() {
        st.countdown.cancel();
        st.ui.countdown_label.set_visible(false);
        sync_recording(st);
        return;
    }
    let seconds = {
        let s = st.app.settings.borrow();
        if s.recording.countdown {
            s.recording.countdown_seconds.clamp(1, 10)
        } else {
            0
        }
    };
    st.ui.rec.add_css_class("counting");
    st.ui.rec.set_tooltip_text(Some("Cancel"));
    let (s1, s2) = (st.clone(), st.clone());
    st.countdown.start(
        seconds,
        &st.ui.countdown_label,
        move |n| s1.ui.rec.set_label(&n.to_string()),
        move || begin(&s2),
    );
}

/// Start recording what is chosen now.
fn begin(st: &Rc<State>) {
    st.ui.rec.remove_css_class("counting");
    let mode = st.mode.get();
    let settings = st.app.settings.borrow().clone();
    let screen = if mode.is_screen() {
        match screen_source(st) {
            Some(source) => Some(st.app.screen_capture(
                source,
                options(st),
                f64::from(settings.recording.fps),
            )),
            None => {
                st.app.toast("Choose what to record first");
                sync_recording(st);
                return;
            }
        }
    } else {
        None
    };
    let wants_camera =
        mode == Mode::Camera || (mode.is_screen() && settings.recording.webcam_overlay);
    let camera = if wants_camera && !st.app.cameras.borrow().is_empty() {
        match st.app.camera_stream() {
            Ok(s) => Some(s),
            Err(e) => {
                if mode == Mode::Camera {
                    st.app.error("The camera could not be started", &e);
                    sync_recording(st);
                    return;
                }
                None
            }
        }
    } else {
        None
    };
    if mode == Mode::Camera && camera.is_none() {
        st.app.toast("No camera is connected");
        sync_recording(st);
        return;
    }
    let subject = if mode == Mode::Window {
        let id = st.window.borrow().clone();
        st.app
            .screen_state
            .borrow()
            .windows
            .iter()
            .find(|w| w.identifier == id)
            .map(|w| w.title.clone())
            .unwrap_or_default()
    } else {
        String::new()
    };
    st.app.start_recording(Plan {
        kind: mode.kind(),
        screen,
        camera,
        system_audio: settings.recording.system_audio,
        microphone: settings.recording.microphone,
        subject,
    });
}

/// Reflect the recording state: the button, the timer, the meters.
fn sync_recording(st: &Rc<State>) {
    let ui = &st.ui;
    let rec = st.app.recording.borrow();
    match rec.as_ref() {
        Some(live) => {
            ui.rec.remove_css_class("counting");
            ui.rec.add_css_class("recording");
            ui.rec.set_label("");
            ui.rec.set_icon_name("media-playback-stop-symbolic");
            ui.rec.set_tooltip_text(Some("Stop and save (Ctrl+R)"));
            ui.rec.set_sensitive(true);
            let t = live.elapsed().as_secs();
            ui.timer
                .set_text(&format!("{:02}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60));
            ui.timer.add_css_class("live");
            ui.pause.set_visible(true);
            if ui.pause.is_active() != live.paused() {
                ui.pause.set_active(live.paused());
            }
            ui.live_badge.set_visible(true);
            ui.live_badge.set_text(if live.paused() {
                "❚❚ PAUSED"
            } else {
                "● REC"
            });
            ui.frame.add_css_class("recording");
            for t in &ui.tabs {
                t.set_sensitive(false);
            }
            ui.picker.set_sensitive(false);
            ui.source_dd.set_sensitive(false);
            let (sys, mic) = live.levels();
            ui.audio_meters.0.set_value(f64::from(sys));
            ui.audio_meters.1.set_value(f64::from(mic));
            if st.mode.get() == Mode::Audio {
                ui.audio_status.set_text(if live.paused() {
                    "Paused"
                } else {
                    "Recording sound"
                });
            }
        }
        None => {
            if st.countdown.running() {
                return;
            }
            ui.rec.remove_css_class("recording");
            ui.rec.remove_css_class("counting");
            ui.rec.set_label("REC");
            ui.rec.set_tooltip_text(Some("Start recording (Ctrl+R)"));
            ui.timer.set_text("00:00:00");
            ui.timer.remove_css_class("live");
            ui.pause.set_visible(false);
            ui.pause.set_active(false);
            ui.live_badge.set_visible(false);
            ui.frame.remove_css_class("recording");
            for t in &ui.tabs {
                t.set_sensitive(true);
            }
            ui.picker.set_sensitive(true);
            ui.source_dd.set_sensitive(true);
            ui.audio_meters.0.set_value(0.0);
            ui.audio_meters.1.set_value(0.0);
        }
    }
    let was = rec.is_some();
    drop(rec);
    // After a recording stops, the preview goes back to its own pace and
    // any source that changed while recording is picked up.
    if !was && st.visible.get() && st.preview.borrow().is_none() {
        refresh(st);
    }
}

fn take_still(st: &Rc<State>) {
    let mode = st.mode.get();
    if mode == Mode::Camera {
        match st.app.camera_stream() {
            Ok(stream) => {
                super::flash(&st.ui.overlay);
                super::save_photo(&st.app, stream);
            }
            Err(e) => st.app.error("Could not take the photo", &e),
        }
        return;
    }
    let Some(capture) = st.capture.borrow().clone() else {
        st.app.toast("Choose what to capture first");
        return;
    };
    super::flash(&st.ui.overlay);
    let copy = st.app.settings.borrow().screenshot.copy_to_clipboard;
    super::save_screen_still(&st.app, capture, copy);
}

fn load_recent(st: &Rc<State>) {
    let settings = st.app.settings.borrow().clone();
    let s = st.clone();
    crate::ui::spawn(
        move || {
            library::scan(&settings)
                .into_iter()
                .take(4)
                .collect::<Vec<_>>()
        },
        move |items| {
            widgets::clear(&s.ui.recent);
            if items.is_empty() {
                let l = gtk::Label::new(Some(
                    "Nothing captured yet. Recordings, photos and screenshots show up here.",
                ));
                l.add_css_class("dim");
                l.set_xalign(0.0);
                s.ui.recent.append(&l);
                return;
            }
            for item in &items {
                s.ui.recent.append(&widgets::tile(&s.app, item, 200));
            }
            // Keep four columns even with fewer items.
            for _ in items.len()..4 {
                s.ui.recent
                    .append(&gtk::Box::new(gtk::Orientation::Vertical, 0));
            }
        },
    );
}

/// Pick a folder for videos (`videos`) or pictures.
pub fn choose_folder(app: &Rc<App>, videos: bool) {
    let dialog = gtk::FileDialog::builder()
        .title(if videos {
            "Save Recordings In"
        } else {
            "Save Photos and Screenshots In"
        })
        .modal(true)
        .build();
    let current = if videos {
        app.settings.borrow().video_dir()
    } else {
        app.settings.borrow().picture_dir()
    };
    let _ = std::fs::create_dir_all(&current);
    dialog.set_initial_folder(Some(&gio::File::for_path(&current)));
    let app2 = app.clone();
    dialog.select_folder(app.window().as_ref(), gio::Cancellable::NONE, move |r| {
        if let Ok(folder) = r {
            if let Some(path) = folder.path() {
                let p = path.display().to_string();
                app2.update_settings(|x| {
                    if videos {
                        x.video_dir = p;
                    } else {
                        x.picture_dir = p;
                    }
                });
                app2.notify(Topic::Library);
            }
        }
    });
}

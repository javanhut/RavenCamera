//! Screenshot: a screen, a window or a region, now or after a delay, with
//! or without the pointer, saved and copied. The window gets out of the way
//! first, so it is not in the picture.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk4 as gtk;
use libadwaita::prelude::*;

use super::Page;
use crate::library;
use crate::screen::{self, Source};
use crate::settings::ImageFormat;
use crate::ui::widgets::{self, set_items};
use crate::ui::{preview, App, Topic};

const DELAYS: [u32; 5] = [0, 1, 3, 5, 10];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Area {
    Screen,
    Window,
    Region,
}

struct State {
    app: Rc<App>,
    area: std::cell::Cell<Area>,
    target: gtk::DropDown,
    target_field: gtk::Box,
    last: gtk::Picture,
    last_stack: gtk::Stack,
    last_path: RefCell<Option<std::path::PathBuf>>,
    last_actions: gtk::Box,
    take: gtk::Button,
    hide_window: gtk::Switch,
}

pub fn page(app: &Rc<App>) -> Page {
    let s = app.settings.borrow().clone();
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    root.set_margin_top(8);
    root.set_margin_bottom(24);
    root.set_margin_start(24);
    root.set_margin_end(24);

    let left = gtk::Box::new(gtk::Orientation::Vertical, 16);
    left.set_size_request(360, -1);
    left.append(&super::heading(
        "Screenshot",
        "Take a picture of the screen, a window, or a part of it.",
    ));

    let card = widgets::card("");
    let areas = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    areas.add_css_class("segmented");
    areas.set_homogeneous(true);
    let mut area_buttons: Vec<gtk::ToggleButton> = Vec::new();
    for label in ["Screen", "Window", "Region"] {
        let b = gtk::ToggleButton::with_label(label);
        if let Some(f) = area_buttons.first() {
            b.set_group(Some(f));
        }
        areas.append(&b);
        area_buttons.push(b);
    }
    area_buttons[0].set_active(true);
    card.append(&widgets::field("Capture", &areas));
    let target = gtk::DropDown::from_strings(&[]);
    target.set_hexpand(true);
    let target_field = widgets::field("Which", &target);
    card.append(&target_field);
    let delay_labels: Vec<String> = DELAYS
        .iter()
        .map(|d| {
            if *d == 0 {
                "No delay".into()
            } else {
                format!("{d} seconds")
            }
        })
        .collect();
    let delay = widgets::dropdown(
        &delay_labels,
        DELAYS
            .iter()
            .position(|&d| d == s.screenshot.delay_seconds)
            .unwrap_or(0) as u32,
        {
            let app = app.clone();
            move |i| app.update_settings(|x| x.screenshot.delay_seconds = DELAYS[i as usize])
        },
    );
    card.append(&widgets::field("Delay", &delay));
    let formats = vec!["PNG (lossless)".to_owned(), "JPEG (smaller)".to_owned()];
    let format = widgets::dropdown(
        &formats,
        u32::from(s.screenshot.format == ImageFormat::Jpeg),
        {
            let app = app.clone();
            move |i| {
                app.update_settings(|x| {
                    x.screenshot.format = if i == 1 {
                        ImageFormat::Jpeg
                    } else {
                        ImageFormat::Png
                    }
                })
            }
        },
    );
    card.append(&widgets::field("Format", &format));
    let gap = gtk::Box::new(gtk::Orientation::Vertical, 0);
    gap.set_size_request(-1, 6);
    card.append(&gap);
    let (r1, _) = widgets::switch_row("Include the pointer", s.screenshot.show_cursor, {
        let app = app.clone();
        move |on| app.update_settings(|x| x.screenshot.show_cursor = on)
    });
    let (r2, _) = widgets::switch_row("Copy to the clipboard", s.screenshot.copy_to_clipboard, {
        let app = app.clone();
        move |on| app.update_settings(|x| x.screenshot.copy_to_clipboard = on)
    });
    let (r3, hide_window) = widgets::switch_row("Hide this window first", true, |_| {});
    card.append(&r1);
    card.append(&r2);
    card.append(&r3);
    left.append(&card);

    let take = gtk::Button::with_label("Take Screenshot");
    take.add_css_class("suggested-action");
    take.add_css_class("pill");
    take.set_size_request(-1, 44);
    left.append(&take);
    let hint = gtk::Label::new(Some(
        "Print, Ctrl+Print and Shift+Print still work anywhere: the desktop takes those itself.",
    ));
    hint.add_css_class("note");
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    left.append(&hint);
    root.append(&left);

    // The last screenshot.
    let right = gtk::Box::new(gtk::Orientation::Vertical, 12);
    right.set_hexpand(true);
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("preview-frame");
    frame.set_overflow(gtk::Overflow::Hidden);
    frame.set_vexpand(true);
    let last_stack = gtk::Stack::new();
    let last = gtk::Picture::new();
    last.set_content_fit(gtk::ContentFit::Contain);
    last_stack.add_named(
        &widgets::empty(
            "image-x-generic-symbolic",
            "No screenshot yet",
            "The last one you take shows here.",
        ),
        Some("empty"),
    );
    last_stack.add_named(&last, Some("picture"));
    frame.append(&last_stack);
    right.append(&frame);
    let last_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    last_actions.set_halign(gtk::Align::End);
    last_actions.set_visible(false);
    right.append(&last_actions);
    root.append(&right);

    let st = Rc::new(State {
        app: app.clone(),
        area: std::cell::Cell::new(Area::Screen),
        target,
        target_field,
        last,
        last_stack,
        last_path: RefCell::new(None),
        last_actions,
        take,
        hide_window,
    });
    for (i, b) in area_buttons.iter().enumerate() {
        let s = st.clone();
        b.connect_toggled(move |b| {
            if b.is_active() {
                s.area.set([Area::Screen, Area::Window, Area::Region][i]);
                sync_targets(&s);
            }
        });
    }
    {
        let s = st.clone();
        st.take.connect_clicked(move |_| take_shot(&s));
    }
    for (label, icon, f) in [
        ("Open", "document-open-symbolic", 0),
        ("Copy", "edit-copy-symbolic", 1),
        ("Show in Folder", "folder-open-symbolic", 2),
    ] {
        let b = gtk::Button::new();
        let inner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        inner.append(&gtk::Image::from_icon_name(icon));
        inner.append(&gtk::Label::new(Some(label)));
        b.set_child(Some(&inner));
        let s = st.clone();
        b.connect_clicked(move |_| {
            let Some(p) = s.last_path.borrow().clone() else {
                return;
            };
            match f {
                0 => crate::ui::open_path(&p),
                1 => {
                    if let (Ok(t), Some(d)) = (
                        gtk::gdk::Texture::from_filename(&p),
                        gtk::gdk::Display::default(),
                    ) {
                        d.clipboard().set_texture(&t);
                        s.app.toast("Copied");
                    }
                }
                _ => crate::ui::show_in_folder(&p),
            }
        });
        st.last_actions.append(&b);
    }
    {
        let s = st.clone();
        app.on(Topic::Screen, move || sync_targets(&s));
    }
    sync_targets(&st);
    let show = st.clone();
    Page {
        id: "screenshot",
        title: "Screenshot",
        icon: "camera-photo-symbolic",
        widget: root.upcast(),
        on_show: Box::new(move || sync_targets(&show)),
        on_hide: Box::new(|| {}),
    }
}

fn sync_targets(st: &Rc<State>) {
    let screen = st.app.screen_state.borrow().clone();
    let (labels, visible): (Vec<String>, bool) = match st.area.get() {
        Area::Screen => (
            screen
                .outputs
                .iter()
                .enumerate()
                .map(|(i, o)| o.label(i))
                .collect(),
            screen.outputs.len() > 1,
        ),
        Area::Window => (
            screen
                .windows
                .iter()
                .filter(|w| w.app_id != crate::ui::APP_ID)
                .map(|w| {
                    if w.title.is_empty() {
                        w.app_id.clone()
                    } else {
                        w.title.clone()
                    }
                })
                .collect(),
            true,
        ),
        Area::Region => (Vec::new(), false),
    };
    let sel = if st.area.get() == Area::Screen {
        screen.outputs.iter().position(|o| o.focused).unwrap_or(0)
    } else {
        0
    };
    set_items(&st.target, &labels, sel as u32);
    st.target_field.set_visible(visible);
    let can = screen.capture == Some(true);
    st.take.set_sensitive(can);
    st.take.set_tooltip_text(
        (!can).then_some("This desktop does not offer screen capture to applications"),
    );
}

fn take_shot(st: &Rc<State>) {
    let settings = st.app.settings.borrow().clone();
    let screen = st.app.screen_state.borrow().clone();
    let i = st.target.selected() as usize;
    let source = match st.area.get() {
        Area::Screen => screen
            .outputs
            .get(i)
            .map(|o| Source::Output(o.name.clone())),
        Area::Window => screen
            .windows
            .iter()
            .filter(|w| w.app_id != crate::ui::APP_ID)
            .nth(i)
            .map(|w| Source::Window(w.identifier.clone())),
        Area::Region => {
            let s = st.clone();
            // The compositor's own selection; it draws over this window, so
            // there is nothing to hide first.
            st.app.select_region(move |r| {
                if let Some(r) = r {
                    shoot(
                        &s,
                        Source::Region(r),
                        s.app.settings.borrow().screenshot.delay_seconds,
                        false,
                    );
                }
            });
            return;
        }
    };
    let Some(source) = source else {
        st.app.toast("Nothing to capture");
        return;
    };
    let hide = st.hide_window.is_active() && st.area.get() == Area::Screen;
    shoot(st, source, settings.screenshot.delay_seconds, hide);
}

fn shoot(st: &Rc<State>, source: Source, delay: u32, hide: bool) {
    let window = st.app.window();
    if hide {
        if let Some(w) = &window {
            w.minimize();
        }
    }
    // Give a minimising window time to leave the screen.
    let wait = Duration::from_secs(u64::from(delay))
        + if hide {
            Duration::from_millis(600)
        } else {
            Duration::ZERO
        };
    let s = st.clone();
    glib::timeout_add_local_once(wait, move || {
        let settings = s.app.settings.borrow().clone();
        let capture = Arc::new(s.app.screen.capture(
            source,
            screen::Options {
                cursor: settings.screenshot.show_cursor,
                clicks: false,
            },
            4.0,
        ));
        let frames = capture.subscribe(1);
        let s2 = s.clone();
        crate::ui::spawn(
            move || -> anyhow::Result<(std::path::PathBuf, crate::pixels::Rgba)> {
                let _keep = capture;
                let frame = frames
                    .recv_timeout(Duration::from_secs(3))
                    .map_err(|_| anyhow::anyhow!("the compositor sent no picture"))?;
                let img = crate::pixels::bgra_to_rgba(
                    &frame.bgra,
                    frame.width,
                    frame.height,
                    frame.width * 4,
                );
                let path = library::save_still(
                    &img,
                    &settings,
                    library::Kind::Screenshot,
                    settings.screenshot.format,
                )?;
                Ok((path, img))
            },
            move |r| {
                if hide {
                    if let Some(w) = s2.app.window() {
                        w.present();
                    }
                }
                match r {
                    Ok((path, img)) => {
                        let copy = s2.app.settings.borrow().screenshot.copy_to_clipboard;
                        if copy {
                            super::copy_image(&img);
                        }
                        preview::show(&s2.last, &preview::still(&img));
                        s2.last_stack.set_visible_child_name("picture");
                        s2.last_actions.set_visible(true);
                        *s2.last_path.borrow_mut() = Some(path.clone());
                        s2.app.notify(Topic::Library);
                        s2.app.after_capture(
                            &path,
                            if copy {
                                "Screenshot saved and copied"
                            } else {
                                "Screenshot saved"
                            },
                        );
                    }
                    Err(e) => s2.app.error("Could not take the screenshot", &e),
                }
            },
        );
    });
}

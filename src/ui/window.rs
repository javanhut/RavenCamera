//! The shell of the window, laid out as the design mockup has it: the brand
//! and the tagline across the top, the section list down the left, the
//! section on the right.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::gdk;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::pages::{self, Page};
use super::{App, Topic};

/// The size the window opens at on a screen with room for it.
const DEFAULT_SIZE: (i32, i32) = (1480, 960);

/// The part of a screen the window opens at most, leaving room for the
/// shell's panel and a glimpse of the desktop around it.
const SCREEN_SHARE: f64 = 0.9;

/// The opening size for `monitor`: the default, or less on a smaller
/// screen. Monitor geometry is in logical pixels, as window sizes are, so
/// the scale factor needs no handling here.
fn fitted_size(monitor: &gdk::Monitor) -> (i32, i32) {
    let g = monitor.geometry();
    let fit = |len: i32, max: i32| ((f64::from(len) * SCREEN_SHARE) as i32).min(max);
    (
        fit(g.width(), DEFAULT_SIZE.0),
        fit(g.height(), DEFAULT_SIZE.1),
    )
}

/// Open at a size that fits the screen. Before the window is shown, which
/// screen it will land on is the compositor's to decide, so the guess is
/// the first monitor; once the window enters its real one, it is refitted
/// to that, unless it has been resized or maximised in between.
fn fit_to_screen(window: &adw::ApplicationWindow) {
    let first = gdk::Display::default()
        .and_then(|d| d.monitors().item(0))
        .and_downcast::<gdk::Monitor>();
    let guess = first.as_ref().map_or(DEFAULT_SIZE, fitted_size);
    window.set_default_size(guess.0, guess.1);

    window.connect_realize(move |window| {
        let Some(surface) = window.surface() else {
            return;
        };
        let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
        let (window, h) = (window.downgrade(), handler.clone());
        let id = surface.connect_enter_monitor(move |surface, monitor| {
            let id = h.borrow_mut().take();
            if let Some(id) = id {
                surface.disconnect(id);
            }
            let Some(window) = window.upgrade() else {
                return;
            };
            let untouched =
                window.default_size() == guess && !window.is_maximized() && !window.is_fullscreen();
            let size = fitted_size(monitor);
            if untouched && size != guess {
                window.set_default_size(size.0, size.1);
            }
        });
        handler.replace(Some(id));
    });
}

pub fn build(app: &Rc<App>, glass: bool) -> (adw::ApplicationWindow, impl Fn(&str) + 'static) {
    let window = adw::ApplicationWindow::builder()
        .application(&app.gtk_app)
        .title("Raven Camera")
        .width_request(900)
        .height_request(620)
        .build();
    fit_to_screen(&window);
    window.add_css_class("raven");
    window.add_css_class("camera");
    if glass {
        window.add_css_class("glass");
    }

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("camera-root");
    root.append(&topbar());

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.set_vexpand(true);
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(160)
        .hexpand(true)
        .vexpand(true)
        .build();

    let pages: Rc<Vec<Page>> = Rc::new(pages::all(app));
    let nav = gtk::ListBox::new();
    nav.add_css_class("navigation-sidebar");
    nav.set_selection_mode(gtk::SelectionMode::Single);
    let mut badges = Vec::new();
    for page in pages.iter() {
        stack.add_named(&page.widget, Some(page.id));
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        row.append(&gtk::Image::from_icon_name(page.icon));
        let l = gtk::Label::new(Some(page.title));
        l.set_xalign(0.0);
        l.set_hexpand(true);
        row.append(&l);
        let badge = gtk::Image::from_icon_name("media-record-symbolic");
        badge.add_css_class("recording-dot");
        badge.set_visible(false);
        row.append(&badge);
        badges.push((page.id, badge));
        nav.append(&row);
    }

    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar.add_css_class("sidebar");
    nav.set_vexpand(true);
    sidebar.append(&nav);
    let rule = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    rule.add_css_class("sidebar-footer-rule");
    rule.set_halign(gtk::Align::Start);
    sidebar.append(&rule);
    let footer = gtk::Label::new(Some("CREATE\nRECORD\nSHARE\nYOUR WAY."));
    footer.add_css_class("sidebar-footer");
    footer.set_xalign(0.0);
    sidebar.append(&footer);

    body.append(&sidebar);
    body.append(&stack);
    root.append(&body);
    app.toasts.set_child(Some(&root));
    window.set_content(Some(&app.toasts));

    // The Record entry wears a red dot while recording or finishing, so it
    // is findable from any page.
    {
        let app2 = app.clone();
        let badges = badges.clone();
        let update = move || {
            let busy = app2.is_recording() || app2.jobs.borrow().iter().any(|j| j.running.get());
            for (id, b) in &badges {
                b.set_visible(*id == "record" && busy);
            }
        };
        let u = Rc::new(update);
        let u1 = u.clone();
        app.on(Topic::Recording, move || u1());
        app.on(Topic::Jobs, move || u());
    }

    let current: Rc<RefCell<Option<usize>>> = Rc::default();
    {
        let (stack, pages, current) = (stack.clone(), pages.clone(), current.clone());
        nav.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let i = row.index() as usize;
            let prev = current.borrow_mut().replace(i);
            if prev == Some(i) {
                return;
            }
            if let Some(p) = prev.and_then(|p| pages.get(p)) {
                (p.on_hide)();
            }
            stack.set_visible_child_name(pages[i].id);
            (pages[i].on_show)();
        });
    }
    // Pages stop their previews while the window is minimised or hidden,
    // and start them again when it comes back.
    {
        let (pages, current) = (pages.clone(), current.clone());
        window.connect_suspended_notify(move |w| {
            let Some(i) = *current.borrow() else { return };
            if w.is_suspended() {
                (pages[i].on_hide)();
            } else {
                (pages[i].on_show)();
            }
        });
    }
    // Closing while recording asks first; closing while finishing hides the
    // window and lets the app finish the file before it exits.
    {
        let app2 = app.clone();
        window.connect_close_request(move |w| {
            if app2.is_recording() {
                let dialog = adw::AlertDialog::builder()
                    .heading("Stop recording?")
                    .body("Raven Camera is recording. Stop and save the recording, or keep recording.")
                    .close_response("keep")
                    .default_response("stop")
                    .build();
                dialog.add_responses(&[("keep", "Keep Recording"), ("stop", "Stop and Save")]);
                dialog.set_response_appearance("stop", adw::ResponseAppearance::Suggested);
                let (app3, w2) = (app2.clone(), w.clone());
                dialog.connect_response(None, move |_, r| {
                    if r == "stop" {
                        app3.stop_recording();
                        w2.close();
                    }
                });
                dialog.present(Some(w));
                return glib::Propagation::Stop;
            }
            let finishing = app2.jobs.borrow().iter().any(|j| j.running.get());
            if finishing {
                app2.toast("Saving continues in the background");
            }
            glib::Propagation::Proceed
        });
    }
    keyboard(app, &window);

    nav.select_row(nav.row_at_index(0).as_ref());
    window.present();

    let navigate = {
        let nav = nav.clone();
        let ids: Vec<&'static str> = pages.iter().map(|p| p.id).collect();
        move |id: &str| {
            if let Some(i) = ids.iter().position(|p| *p == id) {
                nav.select_row(nav.row_at_index(i as i32).as_ref());
            }
        }
    };
    (window, navigate)
}

fn topbar() -> gtk::Widget {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    bar.add_css_class("topbar");
    let lens = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    lens.add_css_class("brand-lens");
    lens.set_valign(gtk::Align::Center);
    bar.append(&lens);
    let names = gtk::Box::new(gtk::Orientation::Vertical, 2);
    names.set_valign(gtk::Align::Center);
    let title = gtk::Label::new(Some("Raven Camera"));
    title.add_css_class("brand-title");
    title.set_xalign(0.0);
    let sub = gtk::Label::new(Some("Capture what matters."));
    sub.add_css_class("brand-subtitle");
    sub.set_xalign(0.0);
    names.append(&title);
    names.append(&sub);
    bar.append(&names);

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);

    let right = gtk::Box::new(gtk::Orientation::Vertical, 6);
    right.set_valign(gtk::Align::Start);
    let controls = gtk::WindowControls::new(gtk::PackType::End);
    controls.set_halign(gtk::Align::End);
    right.append(&controls);
    let tagline = gtk::Label::new(Some("SIMPLE TO CAPTURE.\nBUILT FOR CREATORS."));
    tagline.add_css_class("tagline");
    tagline.set_justify(gtk::Justification::Right);
    tagline.set_xalign(1.0);
    right.append(&tagline);
    bar.append(&right);

    // Dragging the bar moves the window, as a header bar would.
    let handle = gtk::WindowHandle::new();
    handle.set_child(Some(&bar));
    handle.upcast()
}

/// Shortcuts that work on every page.
fn keyboard(app: &Rc<App>, window: &adw::ApplicationWindow) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let app = app.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        use gtk::gdk::{Key, ModifierType};
        let ctrl = mods.contains(ModifierType::CONTROL_MASK);
        match key {
            Key::_1 | Key::_2 | Key::_3 | Key::_4 | Key::_5 | Key::_6 if ctrl => {
                let ids = [
                    "capture",
                    "record",
                    "screenshot",
                    "camera",
                    "media",
                    "settings",
                ];
                let i = (key.to_unicode().and_then(|c| c.to_digit(10)).unwrap_or(1) - 1) as usize;
                app.navigate(ids[i.min(5)]);
                glib::Propagation::Stop
            }
            Key::period if ctrl && app.is_recording() => {
                app.stop_recording();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    window.add_controller(keys);
}

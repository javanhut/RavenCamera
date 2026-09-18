//! Building blocks every page uses: labelled fields, drop-downs, switch
//! rows, cards, and the thumbnail tile that stands for a capture.

use std::path::Path;
use std::rc::Rc;

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::{preview, App, Topic};
use crate::library::{self, Item};

/// A card with a title.
pub fn card(title: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
    b.add_css_class("panel-card");
    if !title.is_empty() {
        let t = gtk::Label::new(Some(title));
        t.add_css_class("card-title");
        t.set_xalign(0.0);
        b.append(&t);
    }
    b
}

/// `label` above `widget`.
pub fn field(label: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    b.add_css_class("field");
    let l = gtk::Label::new(Some(label));
    l.add_css_class("field-label");
    l.set_xalign(0.0);
    b.append(&l);
    b.append(widget);
    b
}

/// A drop-down of `labels` with `selected` chosen; `on_change` gets the new
/// index when the person picks one (not when it is set from code).
pub fn dropdown(
    labels: &[String],
    selected: u32,
    on_change: impl Fn(u32) + 'static,
) -> gtk::DropDown {
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let dd = gtk::DropDown::from_strings(&refs);
    dd.set_selected(selected.min(labels.len().saturating_sub(1) as u32));
    dd.set_hexpand(true);
    dd.connect_selected_notify(move |dd| {
        if dd.widget_name() != "quiet" {
            on_change(dd.selected());
        }
    });
    dd
}

/// Replace a drop-down's items without telling its handler.
pub fn set_items(dd: &gtk::DropDown, labels: &[String], selected: u32) {
    dd.set_widget_name("quiet");
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    dd.set_model(Some(&gtk::StringList::new(&refs)));
    dd.set_selected(selected.min(labels.len().saturating_sub(1) as u32));
    dd.set_widget_name("");
}

/// Select without telling the handler.
pub fn set_selected_quietly(dd: &gtk::DropDown, selected: u32) {
    if dd.selected() != selected {
        dd.set_widget_name("quiet");
        dd.set_selected(selected);
        dd.set_widget_name("");
    }
}

/// `label` with a switch at the end.
pub fn switch_row(
    label: &str,
    active: bool,
    on_toggle: impl Fn(bool) + 'static,
) -> (gtk::Box, gtk::Switch) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("switch-row");
    let l = gtk::Label::new(Some(label));
    l.set_xalign(0.0);
    l.set_hexpand(true);
    let s = gtk::Switch::new();
    s.set_active(active);
    s.set_valign(gtk::Align::Center);
    s.connect_active_notify(move |s| {
        if s.widget_name() != "quiet" {
            on_toggle(s.is_active());
        }
    });
    row.append(&l);
    row.append(&s);
    (row, s)
}

pub fn set_active_quietly(s: &gtk::Switch, active: bool) {
    if s.is_active() != active {
        s.set_widget_name("quiet");
        s.set_active(active);
        s.set_widget_name("");
    }
}

/// A flat round button with an icon.
pub fn round_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("round");
    b.set_tooltip_text(Some(tooltip));
    b.set_valign(gtk::Align::Center);
    b
}

pub fn round_toggle(icon: &str, tooltip: &str) -> gtk::ToggleButton {
    let b = gtk::ToggleButton::new();
    b.set_icon_name(icon);
    b.add_css_class("round");
    b.set_tooltip_text(Some(tooltip));
    b.set_valign(gtk::Align::Center);
    b
}

/// An empty state: icon, title, a line of explanation.
pub fn empty(icon: &str, title: &str, text: &str) -> adw::StatusPage {
    let p = adw::StatusPage::builder()
        .icon_name(icon)
        .title(title)
        .description(text)
        .build();
    p.add_css_class("preview-empty");
    p
}

/// A capture's thumbnail tile: picture with its length and a menu, title
/// and file name under it. Clicking opens it.
pub fn tile(app: &Rc<App>, item: &Item, width: i32) -> gtk::Widget {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let overlay = gtk::Overlay::new();
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("thumb-frame");
    frame.set_overflow(gtk::Overflow::Hidden);
    let height = width * 9 / 16;
    frame.set_size_request(width, height);
    let picture = gtk::Picture::new();
    picture.set_content_fit(gtk::ContentFit::Cover);
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    let placeholder = gtk::Image::from_icon_name(item.kind.icon());
    placeholder.add_css_class("placeholder");
    placeholder.set_vexpand(true);
    let stack = gtk::Stack::new();
    stack.add_named(&placeholder, Some("icon"));
    stack.add_named(&picture, Some("picture"));
    stack.set_visible_child_name("icon");
    frame.append(&stack);
    overlay.set_child(Some(&frame));

    {
        let item = item.clone();
        let picture = picture.clone();
        let stack = stack.clone();
        super::spawn(
            move || library::thumbnail(&item),
            move |thumb| {
                if let Some(img) = thumb {
                    preview::show(&picture, &preview::still(&img));
                    stack.set_visible_child_name("picture");
                }
            },
        );
    }

    if let Some(len) = item.length {
        let badge = gtk::Label::new(Some(&library::format_length(len)));
        badge.add_css_class("duration-badge");
        badge.set_halign(gtk::Align::End);
        badge.set_valign(gtk::Align::End);
        badge.set_margin_end(8);
        badge.set_margin_bottom(8);
        overlay.add_overlay(&badge);
    } else {
        let kind = gtk::Image::from_icon_name(item.kind.icon());
        kind.add_css_class("kind-badge");
        kind.set_halign(gtk::Align::End);
        kind.set_valign(gtk::Align::End);
        kind.set_margin_end(8);
        kind.set_margin_bottom(8);
        overlay.add_overlay(&kind);
    }
    let menu = item_menu(app, item);
    menu.add_css_class("tile-menu");
    menu.set_halign(gtk::Align::End);
    menu.set_valign(gtk::Align::Start);
    menu.set_margin_top(8);
    menu.set_margin_end(8);
    overlay.add_overlay(&menu);

    let button = gtk::Button::new();
    button.add_css_class("tile");
    button.set_child(Some(&overlay));
    button.set_tooltip_text(Some(&item.path.display().to_string()));
    {
        let path = item.path.clone();
        button.connect_clicked(move |_| super::open_path(&path));
    }
    outer.append(&button);

    let title = gtk::Label::new(Some(&item.title));
    title.add_css_class("tile-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_max_width_chars(1);
    title.set_hexpand(true);
    let name = gtk::Label::new(Some(&item.file_name()));
    name.add_css_class("tile-subtitle");
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    name.set_max_width_chars(1);
    outer.append(&title);
    outer.append(&name);
    outer.set_size_request(width, -1);
    // A tile keeps its size; a grid or row gives it more room than that
    // when there are few of them, and a stretched thumbnail reads as a bug.
    outer.set_halign(gtk::Align::Start);
    outer.set_valign(gtk::Align::Start);
    outer.set_hexpand(false);
    outer.upcast()
}

/// The ⋮ menu for a capture: open, show, copy, rename, trash.
pub fn item_menu(app: &Rc<App>, item: &Item) -> gtk::MenuButton {
    let mb = gtk::MenuButton::new();
    mb.set_icon_name("view-more-symbolic");
    mb.set_tooltip_text(Some("More"));
    let pop = gtk::Popover::new();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    list.set_margin_top(4);
    list.set_margin_bottom(4);
    let entry = |label: &str, icon: &str| {
        let b = gtk::Button::new();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.append(&gtk::Image::from_icon_name(icon));
        let l = gtk::Label::new(Some(label));
        l.set_xalign(0.0);
        row.append(&l);
        b.set_child(Some(&row));
        b.add_css_class("flat");
        list.append(&b);
        b
    };
    let open = entry("Open", "document-open-symbolic");
    let show = entry("Show in Folder", "folder-open-symbolic");
    let copy = matches!(item.kind, library::Kind::Photo | library::Kind::Screenshot)
        .then(|| entry("Copy Image", "edit-copy-symbolic"));
    let rename = entry("Rename…", "document-edit-symbolic");
    let trash = entry("Move to Trash", "user-trash-symbolic");
    pop.set_child(Some(&list));
    mb.set_popover(Some(&pop));

    let path = item.path.clone();
    {
        let (pop, path) = (pop.clone(), path.clone());
        open.connect_clicked(move |_| {
            pop.popdown();
            super::open_path(&path);
        });
    }
    {
        let (pop, path) = (pop.clone(), path.clone());
        show.connect_clicked(move |_| {
            pop.popdown();
            super::show_in_folder(&path);
        });
    }
    if let Some(copy) = copy {
        let (pop, path, app) = (pop.clone(), path.clone(), app.clone());
        copy.connect_clicked(move |_| {
            pop.popdown();
            match gtk::gdk::Texture::from_filename(&path) {
                Ok(t) => {
                    if let Some(display) = gtk::gdk::Display::default() {
                        display.clipboard().set_texture(&t);
                        app.toast("Copied");
                    }
                }
                Err(e) => app.error("Could not copy", &anyhow::anyhow!("{e}")),
            }
        });
    }
    {
        let (pop, path, app) = (pop.clone(), path.clone(), app.clone());
        rename.connect_clicked(move |_| {
            pop.popdown();
            rename_dialog(&app, &path);
        });
    }
    {
        let (pop, app) = (pop.clone(), app.clone());
        trash.connect_clicked(move |_| {
            pop.popdown();
            match gio::File::for_path(&path).trash(gio::Cancellable::NONE) {
                Ok(()) => {
                    library::forget(&path);
                    let toast = adw::Toast::new("Moved to the trash");
                    app.toasts.add_toast(toast);
                    app.notify(Topic::Library);
                }
                Err(e) => app.error("Could not move it to the trash", &anyhow::anyhow!("{e}")),
            }
        });
    }
    mb
}

fn rename_dialog(app: &Rc<App>, path: &Path) {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let entry = gtk::Entry::builder()
        .text(&stem)
        .activates_default(true)
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading("Rename")
        .extra_child(&entry)
        .default_response("rename")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("rename", "Rename")]);
    dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
    let (app2, path) = (app.clone(), path.to_path_buf());
    dialog.connect_response(None, move |_, response| {
        if response != "rename" {
            return;
        }
        let name = crate::naming::sanitize(&entry.text());
        let Some(dir) = path.parent() else { return };
        let target = dir.join(if ext.is_empty() {
            name.clone()
        } else {
            format!("{name}.{ext}")
        });
        if target == path {
            return;
        }
        if target.exists() {
            app2.toast("A file with that name already exists");
            return;
        }
        match std::fs::rename(&path, &target) {
            Ok(()) => {
                library::renamed(&path, &target);
                app2.notify(Topic::Library);
            }
            Err(e) => app2.error("Could not rename", &e.into()),
        }
    });
    if let Some(w) = app.window() {
        dialog.present(Some(&w));
    }
}

/// Remove every child of a box.
pub fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

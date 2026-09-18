//! Media: everything captured, newest first, filtered by kind. A click
//! opens it; each tile's menu shows, copies, renames or trashes it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use libadwaita::prelude::*;

use super::Page;
use crate::library::{self, Item, Kind};
use crate::ui::widgets;
use crate::ui::{App, Topic};

const FILTERS: [Option<Kind>; 5] = [
    None,
    Some(Kind::Video),
    Some(Kind::Photo),
    Some(Kind::Screenshot),
    Some(Kind::Audio),
];

pub fn page(app: &Rc<App>) -> Page {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
    body.set_margin_top(8);
    body.set_margin_bottom(24);
    body.set_margin_start(24);
    body.set_margin_end(24);
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let heading = super::heading("Media", "");
    heading.set_hexpand(true);
    head.append(&heading);
    let summary = gtk::Label::new(None);
    summary.add_css_class("dim");
    head.append(&summary);
    let open_videos = gtk::Button::from_icon_name("folder-open-symbolic");
    open_videos.set_tooltip_text(Some("Open the recordings folder"));
    {
        let app = app.clone();
        open_videos.connect_clicked(move |_| {
            let dir = app.settings.borrow().video_dir();
            let _ = std::fs::create_dir_all(&dir);
            crate::ui::open_path(&dir);
        });
    }
    head.append(&open_videos);
    body.append(&head);

    let chips = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    chips.add_css_class("filter-chips");
    let mut buttons: Vec<gtk::ToggleButton> = Vec::new();
    for f in FILTERS {
        let b = gtk::ToggleButton::with_label(f.map_or("All", Kind::label));
        if let Some(first) = buttons.first() {
            b.set_group(Some(first));
        }
        chips.append(&b);
        buttons.push(b);
    }
    buttons[0].set_active(true);
    body.append(&chips);

    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_min_children_per_line(1);
    flow.set_max_children_per_line(8);
    flow.set_column_spacing(18);
    flow.set_row_spacing(18);
    flow.set_valign(gtk::Align::Start);
    let empty = widgets::empty(
        "folder-pictures-symbolic",
        "Nothing here yet",
        "Recordings, photos and screenshots you take show up here.",
    );
    empty.set_visible(false);
    body.append(&flow);
    body.append(&empty);

    let items: Rc<RefCell<Vec<Item>>> = Rc::default();
    let filter = Rc::new(Cell::new(0usize));
    let draw = {
        let (app, flow, items, filter, empty, summary) = (
            app.clone(),
            flow.clone(),
            items.clone(),
            filter.clone(),
            empty.clone(),
            summary.clone(),
        );
        Rc::new(move || {
            while let Some(c) = flow.first_child() {
                flow.remove(&c);
            }
            let want = FILTERS[filter.get()];
            let highlight = app.highlight.borrow_mut().take();
            let list = items.borrow();
            let shown: Vec<&Item> = list
                .iter()
                .filter(|i| want.is_none_or(|k| i.kind == k))
                .collect();
            let total: u64 = list.iter().map(|i| i.bytes).sum();
            summary.set_text(&format!(
                "{} items · {}",
                list.len(),
                library::format_bytes(total)
            ));
            empty.set_visible(shown.is_empty());
            for item in shown {
                let tile = widgets::tile(&app, item, 240);
                if highlight.as_ref() == Some(&item.path) {
                    tile.grab_focus();
                    tile.add_css_class("highlight");
                }
                flow.insert(&tile, -1);
            }
        })
    };
    let reload = {
        let (app, items, draw) = (app.clone(), items.clone(), draw.clone());
        Rc::new(move || {
            let settings = app.settings.borrow().clone();
            let (items, draw) = (items.clone(), draw.clone());
            crate::ui::spawn(
                move || library::scan(&settings),
                move |list| {
                    *items.borrow_mut() = list;
                    draw();
                },
            );
        })
    };
    for (i, b) in buttons.iter().enumerate() {
        let (filter, draw) = (filter.clone(), draw.clone());
        b.connect_toggled(move |b| {
            if b.is_active() {
                filter.set(i);
                draw();
            }
        });
    }
    let visible = Rc::new(Cell::new(false));
    {
        let (reload, visible) = (reload.clone(), visible.clone());
        let pending = Rc::new(Cell::new(false));
        app.on(Topic::Library, move || {
            if visible.get() {
                reload();
            } else {
                pending.set(true);
            }
        });
    }
    {
        let reload = reload.clone();
        app.on(Topic::Settings, move || reload());
    }
    let (show, hide) = (reload.clone(), visible.clone());
    let vis = visible.clone();
    Page {
        id: "media",
        title: "Media",
        icon: "media-playback-start-symbolic",
        widget: super::scrolled(&body).upcast(),
        on_show: Box::new(move || {
            vis.set(true);
            show();
        }),
        on_hide: Box::new(move || hide.set(false)),
    }
}

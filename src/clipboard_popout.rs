//! Clipboard popout (Super+V), dropping from the middle of the bar: a search
//! field over the clipboard history, newest first, images with a thumbnail.
//! Typing filters; Up/Down or Tab move the selection; Enter or a click copies
//! the entry back and closes; Ctrl+X or a row's ✕ deletes. The pointer's
//! highlight is separate from the keyboard's selection.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{cairo, gdk, glib, pango};
use gtk4 as gtk;

use crate::clipboard::{self, Clip};
use crate::popout::{self, on_click, on_hover, Anchor, Popout};
use crate::theme::{self, TextStyle, CREAM, DIM, FAINT, ICON_DIM, SUBTLE};
use crate::toasts::claim_click;

const NAMESPACE: &str = "eclipse-shell-clipboard";
const WIDTH: f64 = 460.0;
/// Rows built at most; searching reaches the rest.
const SHOWN: usize = 200;
const THUMB: f64 = 44.0;
/// How long "Clear all" waits for its confirming second click.
const CONFIRM: Duration = Duration::from_secs(3);

struct Row {
    widget: gtk::Widget,
    back: gtk::DrawingArea,
}

/// The widgets of one opening; made afresh each time.
struct Parts {
    list: gtk::Box,
    scroller: gtk::ScrolledWindow,
    search: gtk::Text,
    count: gtk::Label,
    clear: gtk::Label,
}

pub struct ClipboardPopout {
    popout: Rc<Popout>,
    clips: RefCell<Vec<Clip>>,
    /// What the search leaves, in order.
    shown: RefCell<Vec<Clip>>,
    rows: RefCell<Vec<Row>>,
    parts: RefCell<Option<Rc<Parts>>>,
    selected: Rc<Cell<usize>>,
    /// "Clear all" has been clicked once and waits for a second.
    confirming: Cell<bool>,
    /// Bumped on every open, so thumbnails from an earlier one stop loading.
    generation: Rc<Cell<u32>>,
}

impl ClipboardPopout {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        Rc::new(Self {
            popout: Popout::new(app, NAMESPACE),
            clips: RefCell::default(),
            shown: RefCell::default(),
            rows: RefCell::default(),
            parts: RefCell::default(),
            selected: Rc::new(Cell::new(0)),
            confirming: Cell::new(false),
            generation: Rc::new(Cell::new(0)),
        })
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        self.popout.set_monitor(monitor);
    }

    /// Opens under the middle of the bar.
    pub fn toggle(self: &Rc<Self>) {
        if self.popout.is_open() {
            return self.popout.hide();
        }
        let clipboard = self.clone();
        glib::spawn_future_local(async move {
            *clipboard.clips.borrow_mut() = clipboard::list().await;
            clipboard.build();
            let x = f64::from(clipboard.popout.monitor_width()) / 2.0;
            clipboard.popout.show(Anchor::Under(x));
            if let Some(parts) = clipboard.parts() {
                parts.search.grab_focus();
            }
        });
    }

    fn parts(&self) -> Option<Rc<Parts>> {
        self.parts.borrow().clone()
    }

    /// Makes the card; the list inside it follows the search.
    fn build(self: &Rc<Self>) {
        let s = self.popout.scale();
        let px = |v: f64| (v * s).round() as i32;
        self.generation.set(self.generation.get() + 1);
        self.confirming.set(false);

        let search = gtk::Text::builder().hexpand(true).placeholder_text("Search the clipboard").build();
        search.set_attributes(Some(&TextStyle::new(12.0 * s, pango::Weight::Medium, CREAM).attrs()));
        let weak = Rc::downgrade(self);
        search.connect_changed(move |_| weak.upgrade().into_iter().for_each(|c| c.filter()));
        let weak = Rc::downgrade(self);
        search.connect_activate(move |_| weak.upgrade().into_iter().for_each(|c| c.copy_selected()));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(px(440.0))
            .child(&list)
            .build();
        let count = TextStyle::new(9.5 * s, pango::Weight::Medium, FAINT).label("");
        count.set_hexpand(true);
        count.set_xalign(0.0);
        let clear = gtk::Label::new(None);
        let weak = Rc::downgrade(self);
        on_click(&clear, move || weak.upgrade().into_iter().for_each(|c| c.clear_clicked()));
        let weak = Rc::downgrade(self);
        on_hover(&clear, move |inside| {
            if let Some(c) = weak.upgrade() {
                c.paint_clear(c.popout.scale(), inside);
            }
        });

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(px(10.0))
            .width_request(px(WIDTH) - 2 * px(16.0))
            .margin_top(px(13.0))
            .margin_bottom(px(12.0))
            .margin_start(px(16.0))
            .margin_end(px(16.0))
            .build();
        let head = gtk::Box::builder().spacing(px(10.0)).height_request(px(28.0)).build();
        head.append(&theme::filled_icon_glyph("content_paste", 16.0 * s, theme::palette().accent));
        head.append(&search);
        column.append(&head);
        column.append(&popout::divider());
        column.append(&scroller);
        column.append(&popout::divider());
        let foot = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
        foot.append(&count);
        foot.append(&TextStyle::new(9.5 * s, pango::Weight::Medium, FAINT).label("Enter copies  \u{b7}  Ctrl+X deletes"));
        foot.append(&clear);
        column.append(&foot);

        let card = popout::card(&column);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(c) = weak.upgrade() else { return glib::Propagation::Proceed };
            let ctrl = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
            match key {
                gdk::Key::Down => c.step(1),
                gdk::Key::Up => c.step(-1),
                gdk::Key::Tab if !shift => c.step(1),
                gdk::Key::Tab | gdk::Key::ISO_Left_Tab => c.step(-1),
                gdk::Key::x | gdk::Key::X if ctrl => c.delete_selected(),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        card.add_controller(keys);
        *self.parts.borrow_mut() = Some(Rc::new(Parts { list, scroller, search, count, clear }));
        self.paint_clear(s, false);
        self.popout.set_content(&card);
        self.filter();
    }

    fn paint_clear(&self, s: f64, hovered: bool) {
        let (text, color) = match () {
            _ if self.confirming.get() => ("Click again to clear", theme::palette().accent_lit),
            _ if hovered => ("Clear all", CREAM),
            _ => ("Clear all", DIM),
        };
        let Some(parts) = self.parts() else { return };
        parts.clear.set_text(text);
        TextStyle::new(9.5 * s, pango::Weight::Semibold, color).apply(&parts.clear);
        parts.clear.set_visible(!self.clips.borrow().is_empty());
    }

    fn clear_clicked(self: &Rc<Self>) {
        let s = self.popout.scale();
        if !self.confirming.replace(true) {
            self.paint_clear(s, true);
            let weak = Rc::downgrade(self);
            glib::timeout_add_local_once(CONFIRM, move || {
                if let Some(c) = weak.upgrade().filter(|c| c.confirming.replace(false)) {
                    c.paint_clear(c.popout.scale(), false);
                }
            });
            return;
        }
        self.confirming.set(false);
        let clipboard = self.clone();
        glib::spawn_future_local(async move {
            clipboard::wipe().await;
            clipboard.clips.borrow_mut().clear();
            clipboard.filter();
            clipboard.paint_clear(clipboard.popout.scale(), false);
        });
    }

    /// Lays the list out again for the search.
    fn filter(self: &Rc<Self>) {
        let Some(parts) = self.parts() else { return };
        let s = self.popout.scale();
        let query = parts.search.text().to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        let clips = self.clips.borrow();
        let matching: Vec<Clip> = clips.iter().filter(|c| words.iter().all(|w| c.preview.to_lowercase().contains(w))).cloned().collect();
        let total = matching.len();
        *self.shown.borrow_mut() = matching.into_iter().take(SHOWN).collect();
        drop(clips);
        parts.count.set_text(&match (total, query.is_empty()) {
            (0, true) => "Nothing copied yet".to_owned(),
            (0, false) => "No matches".to_owned(),
            (1, _) => "1 entry".to_owned(),
            (n, _) => format!("{n} entries"),
        });

        while let Some(child) = parts.list.first_child() {
            parts.list.remove(&child);
        }
        self.rows.borrow_mut().clear();
        self.selected.set(0);
        let shown = self.shown.borrow().clone();
        for (i, clip) in shown.iter().enumerate() {
            let row = self.row(s, i, clip);
            parts.list.append(&row.widget);
            self.rows.borrow_mut().push(row);
        }
        parts.scroller.vadjustment().set_value(0.0);
        self.load_thumbnails(&parts.list, &shown);
    }

    fn row(self: &Rc<Self>, s: f64, index: usize, clip: &Clip) -> Row {
        let px = |v: f64| (v * s).round() as i32;
        let hovered = Rc::new(Cell::new(false));
        let back = gtk::DrawingArea::new();
        back.set_draw_func({
            let (hovered, selected) = (hovered.clone(), self.selected.clone());
            move |_, cr, w, h| {
                let palette = theme::palette();
                let (w, h) = (f64::from(w), f64::from(h));
                let picked = selected.get() == index;
                if picked || hovered.get() {
                    theme::rounded_rect(cr, 0.0, 0.0, w, h, 8.0 * s);
                    theme::set_source(cr, if picked { palette.raised } else { palette.raised.alpha(0.5) });
                    let _ = cr.fill();
                }
                if picked {
                    theme::rounded_rect(cr, 0.0, 6.0 * s, 2.5 * s, h - 12.0 * s, 1.25 * s);
                    theme::set_source(cr, palette.accent_lit);
                    let _ = cr.fill();
                }
            }
        });

        let content = gtk::Box::builder().spacing(px(10.0)).margin_start(px(10.0)).margin_end(px(8.0)).margin_top(px(6.0)).margin_bottom(px(6.0)).build();
        match &clip.image {
            Some(info) => {
                content.append(&thumbnail(s, &clip.id));
                let label = TextStyle::new(10.5 * s, pango::Weight::Medium, SUBTLE).label(&format!("{} \u{b7} {} \u{b7} {}", info.format.to_uppercase(), info.dimensions, info.size));
                popout::fill(&label);
                content.append(&label);
            }
            None => {
                let label = TextStyle::new(11.0 * s, pango::Weight::Normal, CREAM).label(clip.preview.trim());
                popout::fill(&label);
                label.set_wrap(true);
                label.set_wrap_mode(pango::WrapMode::WordChar);
                label.set_lines(2);
                content.append(&label);
            }
        }
        let delete = theme::icon_glyph("close", 14.0 * s, ICON_DIM);
        delete.set_opacity(0.0);
        delete.set_valign(gtk::Align::Center);
        claim_click(&delete, {
            let (weak, id) = (Rc::downgrade(self), clip.id.clone());
            move || weak.upgrade().into_iter().for_each(|c| c.delete(&id))
        });
        content.append(&delete);

        let row = gtk::Overlay::new();
        row.set_child(Some(&back));
        row.add_overlay(&content);
        row.set_measure_overlay(&content, true);
        on_hover(&row, {
            let (back, delete) = (back.downgrade(), delete.downgrade());
            move |inside| {
                hovered.set(inside);
                back.upgrade().into_iter().for_each(|b| b.queue_draw());
                delete.upgrade().into_iter().for_each(|d| d.set_opacity(if inside { 1.0 } else { 0.0 }));
            }
        });
        let (weak, id) = (Rc::downgrade(self), clip.id.clone());
        on_click(&row, move || weak.upgrade().into_iter().for_each(|c| c.copy(&id)));
        Row { widget: row.upcast(), back }
    }

    /// Decodes the shown images' thumbnails, one after another.
    fn load_thumbnails(&self, list: &gtk::Box, shown: &[Clip]) {
        let ids: Vec<String> = shown.iter().filter(|c| c.image.is_some()).map(|c| c.id.clone()).collect();
        let generation = self.generation.clone();
        let opened = generation.get();
        let list = list.downgrade();
        glib::spawn_future_local(async move {
            for id in ids {
                if generation.get() != opened || list.upgrade().is_none() {
                    return;
                }
                if clipboard::thumbnail(&id).await.is_some() {
                    list.upgrade().into_iter().for_each(|l| queue_thumbs(l.upcast_ref()));
                }
            }
        });
    }

    // ---- actions ---------------------------------------------------------

    fn step(&self, by: isize) {
        let count = self.rows.borrow().len() as isize;
        if count == 0 {
            return;
        }
        let next = (self.selected.get() as isize + by).rem_euclid(count) as usize;
        self.selected.set(next);
        self.rows.borrow().iter().for_each(|r| r.back.queue_draw());
        // Keep the selection in view.
        let Some(parts) = self.parts() else { return };
        if let Some(row) = self.rows.borrow().get(next) {
            if let Some(bounds) = row.widget.compute_bounds(&parts.list) {
                let adjustment = parts.scroller.vadjustment();
                let (top, bottom) = (f64::from(bounds.y()), f64::from(bounds.y() + bounds.height()));
                if top < adjustment.value() {
                    adjustment.set_value(top);
                } else if bottom > adjustment.value() + adjustment.page_size() {
                    adjustment.set_value(bottom - adjustment.page_size());
                }
            }
        }
    }

    fn copy(&self, id: &str) {
        clipboard::copy(id);
        self.popout.hide();
    }

    fn copy_selected(&self) {
        let id = self.shown.borrow().get(self.selected.get()).map(|c| c.id.clone());
        if let Some(id) = id {
            self.copy(&id);
        }
    }

    fn delete_selected(self: &Rc<Self>) {
        let id = self.shown.borrow().get(self.selected.get()).map(|c| c.id.clone());
        if let Some(id) = id {
            let keep = self.selected.get();
            self.delete(&id);
            self.selected.set(keep.min(self.rows.borrow().len().saturating_sub(1)));
            self.rows.borrow().iter().for_each(|r| r.back.queue_draw());
        }
    }

    fn delete(self: &Rc<Self>, id: &str) {
        clipboard::delete(id);
        self.clips.borrow_mut().retain(|c| c.id != id);
        self.filter();
    }
}

/// Redraws every thumbnail in the list, as one has just loaded.
fn queue_thumbs(widget: &gtk::Widget) {
    let mut child = widget.first_child();
    while let Some(c) = child {
        if c.widget_name() == "thumb" {
            c.queue_draw();
        }
        queue_thumbs(&c);
        child = c.next_sibling();
    }
}

/// An image entry's picture, cropped square with rounded corners; a frame
/// until it loads.
fn thumbnail(s: f64, id: &str) -> gtk::DrawingArea {
    let size = (THUMB * s).round() as i32;
    let area = gtk::DrawingArea::builder().content_width(size).content_height(size).valign(gtk::Align::Center).name("thumb").build();
    let id = id.to_owned();
    let surface: RefCell<Option<cairo::ImageSurface>> = RefCell::default();
    area.set_draw_func(move |_, cr, w, h| {
        let (w, h) = (f64::from(w), f64::from(h));
        if surface.borrow().is_none() {
            if let Some(texture) = clipboard::cached_thumbnail(&id) {
                *surface.borrow_mut() = theme::texture_surface(&texture);
            }
        }
        theme::rounded_rect(cr, 0.0, 0.0, w, h, 6.0 * s);
        match &*surface.borrow() {
            Some(image) => {
                let _ = cr.save();
                cr.clip_preserve();
                let (iw, ih) = (f64::from(image.width()), f64::from(image.height()));
                let scale = (w / iw).max(h / ih);
                cr.translate((w - iw * scale) / 2.0, (h - ih * scale) / 2.0);
                cr.scale(scale, scale);
                let _ = cr.set_source_surface(image, 0.0, 0.0);
                cr.source().set_filter(cairo::Filter::Good);
                let _ = cr.paint();
                let _ = cr.restore();
            }
            None => {
                theme::set_source(cr, theme::palette().raised);
                let _ = cr.fill_preserve();
            }
        }
        cr.set_line_width(1.0);
        theme::set_source(cr, theme::HAIR);
        let _ = cr.stroke();
    });
    area
}

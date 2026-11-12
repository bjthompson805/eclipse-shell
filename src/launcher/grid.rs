//! The all-apps grid (Ctrl+A, or the grid button): every app as an icon
//! tile, most used first. The arrows move the keyboard selection, which
//! stays separate from the pointer's hover, as in the result list.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, pango};
use gtk4 as gtk;

use crate::theme::{self, TextStyle, BRIGHT, CHIP_EDGE, CHIP_FILL, CREAM, ROW_HOVER};

pub const COLUMNS: usize = 6;
const VISIBLE_ROWS: f64 = 4.0;

pub struct Grid {
    pub widget: gtk::ScrolledWindow,
    grid: gtk::Grid,
    s: f64,
    apps: RefCell<Vec<gio::AppInfo>>,
    tiles: RefCell<Vec<gtk::Overlay>>,
    selected: Cell<usize>,
    hovered: Cell<Option<usize>>,
    run: RefCell<Option<Box<dyn Fn(gio::AppInfo)>>>,
}

impl Grid {
    pub fn new(s: f64) -> Rc<Self> {
        let grid = gtk::Grid::builder().row_spacing((4.0 * s).round() as i32).column_spacing((4.0 * s).round() as i32).column_homogeneous(true).build();
        let widget = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&grid).min_content_height(Self::height(s).round() as i32).max_content_height(Self::height(s).round() as i32).build();
        Rc::new(Self { widget, grid, s, apps: RefCell::default(), tiles: RefCell::default(), selected: Cell::new(0), hovered: Cell::new(None), run: RefCell::default() })
    }

    pub fn height(s: f64) -> f64 {
        VISIBLE_ROWS * 84.0 * s + (VISIBLE_ROWS - 1.0) * 4.0 * s
    }

    pub fn connect_run(&self, f: impl Fn(gio::AppInfo) + 'static) {
        *self.run.borrow_mut() = Some(Box::new(f));
    }

    pub fn set_apps(self: &Rc<Self>, apps: Vec<gio::AppInfo>) {
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
        let mut tiles = Vec::new();
        for (i, app) in apps.iter().enumerate() {
            let tile = self.tile(i, app);
            self.grid.attach(&tile, (i % COLUMNS) as i32, (i / COLUMNS) as i32, 1, 1);
            tiles.push(tile);
        }
        *self.apps.borrow_mut() = apps;
        *self.tiles.borrow_mut() = tiles;
        self.selected.set(0);
        self.hovered.set(None);
        self.widget.vadjustment().set_value(0.0);
        self.redraw();
    }

    fn tile(self: &Rc<Self>, index: usize, app: &gio::AppInfo) -> gtk::Overlay {
        let s = self.s;
        let px = |v: f64| (v * s).round() as i32;
        let back = gtk::DrawingArea::new();
        back.set_size_request(-1, px(84.0));
        back.set_draw_func({
            let grid = Rc::downgrade(self);
            move |_, cr, w, h| {
                let Some(g) = grid.upgrade() else { return };
                let selected = g.selected.get() == index;
                if !selected && g.hovered.get() != Some(index) {
                    return;
                }
                theme::rounded_rect(cr, 0.5, 0.5, f64::from(w) - 1.0, f64::from(h) - 1.0, 10.0 * s);
                theme::set_source(cr, if selected { CHIP_FILL } else { ROW_HOVER });
                let _ = cr.fill_preserve();
                if selected {
                    cr.set_line_width(1.0);
                    theme::set_source(cr, CHIP_EDGE);
                    let _ = cr.stroke();
                }
                cr.new_path();
            }
        });
        let column = gtk::Box::new(gtk::Orientation::Vertical, px(6.0));
        column.set_valign(gtk::Align::Center);
        let icon = gtk::Image::builder().pixel_size(px(34.0)).build();
        match app.icon() {
            Some(gicon) => icon.set_from_gicon(&gicon),
            None => icon.set_icon_name(Some("application-x-executable")),
        }
        column.append(&icon);
        let name = TextStyle::new(10.0 * s, pango::Weight::Normal, CREAM).label(&app.display_name());
        name.set_ellipsize(pango::EllipsizeMode::End);
        name.set_max_width_chars(1);
        name.set_hexpand(true);
        name.set_margin_start(px(4.0));
        name.set_margin_end(px(4.0));
        column.append(&name);
        let tile = gtk::Overlay::new();
        tile.set_child(Some(&back));
        tile.add_overlay(&column);
        tile.set_cursor_from_name(Some("pointer"));
        let grid = Rc::downgrade(self);
        crate::popout::on_hover(&tile, move |inside| {
            if let Some(g) = grid.upgrade() {
                if inside {
                    g.hovered.set(Some(index));
                } else if g.hovered.get() == Some(index) {
                    g.hovered.set(None);
                }
                g.redraw();
            }
        });
        let grid = Rc::downgrade(self);
        crate::popout::on_click(&tile, move || {
            if let Some(g) = grid.upgrade() {
                g.activate(index);
            }
        });
        tile
    }

    fn redraw(&self) {
        let s = self.s;
        for (i, tile) in self.tiles.borrow().iter().enumerate() {
            if let Some(back) = tile.child() {
                back.queue_draw();
            }
            if let Some(label) = tile.last_child().and_then(|c| c.last_child()).and_downcast::<gtk::Label>() {
                let on = i == self.selected.get();
                TextStyle::new(10.0 * s, if on { pango::Weight::Semibold } else { pango::Weight::Normal }, if on { BRIGHT } else { CREAM }).apply(&label);
            }
        }
    }

    /// Moves the selection by `dx` columns and `dy` rows, keeping it in view.
    pub fn step(&self, dx: isize, dy: isize) {
        let count = self.apps.borrow().len();
        if count == 0 {
            return;
        }
        let at = self.selected.get() as isize + dx + dy * COLUMNS as isize;
        self.selected.set(at.clamp(0, count as isize - 1) as usize);
        self.redraw();
        let row = (self.selected.get() / COLUMNS) as f64;
        let row_h = 88.0 * self.s;
        let adj = self.widget.vadjustment();
        let (top, bottom) = (row * row_h, (row + 1.0) * row_h);
        if top < adj.value() {
            adj.set_value(top);
        } else if bottom > adj.value() + adj.page_size() {
            adj.set_value(bottom - adj.page_size());
        }
    }

    pub fn activate_selected(&self) {
        self.activate(self.selected.get());
    }

    fn activate(&self, index: usize) {
        let Some(app) = self.apps.borrow().get(index).cloned() else { return };
        if let Some(run) = self.run.borrow().as_ref() {
            run(app);
        }
    }
}

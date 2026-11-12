//! Calendar popout, dropping from the clock: the month with today ringed in
//! the accent, arrows to page through months, a click to pick a day, and
//! today's weather along the bottom.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, gdk, glib, pango};
use gtk4 as gtk;

use crate::popout::{self, Anchor, Popout};
use crate::theme::{self, BRIGHT, CREAM, DIM, FAINT, HAIR, ICON_DIM, NIGHT_GHOST, Rgba, SUBTLE, TextStyle};
use crate::weather;

const NAMESPACE: &str = "eclipse-shell-calendar";
const CELL_RADIUS: f64 = 10.0;

/// A month: (year, month 1-12).
#[derive(Clone, Copy, PartialEq)]
struct Month(i32, u32);

impl Month {
    fn of(date: &glib::DateTime) -> Self {
        Month(date.year(), date.month() as u32)
    }

    fn shift(self, delta: i32) -> Self {
        let index = self.0 * 12 + self.1 as i32 - 1 + delta;
        Month(index.div_euclid(12), index.rem_euclid(12) as u32 + 1)
    }

    fn first(self) -> Option<glib::DateTime> {
        glib::DateTime::from_local(self.0, self.1 as i32, 1, 0, 0, 0.0).ok()
    }

    fn days(self) -> u32 {
        // The day before next month's 1st is this month's last.
        let last = self.shift(1).first().and_then(|next| next.add_days(-1).ok());
        last.map_or(30, |d| d.day_of_month() as u32)
    }

    fn rows(self) -> u32 {
        (self.lead() + self.days()).div_ceil(7)
    }

    /// Blank cells before the 1st in a Monday-first week.
    fn lead(self) -> u32 {
        self.first().map_or(0, |d| d.day_of_week() as u32 - 1)
    }
}

/// What the grid shows and which cells are lit.
struct View {
    month: Cell<Month>,
    /// Day of the shown month picked by a click.
    selected: Cell<Option<(Month, u32)>>,
    hovered: Cell<Option<usize>>,
}

pub struct Calendar {
    popout: Rc<Popout>,
    view: Rc<View>,
}

impl Calendar {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let today = Month::of(&now());
        Rc::new(Self { popout: Popout::new(app, NAMESPACE), view: Rc::new(View { month: Cell::new(today), selected: Cell::new(None), hovered: Cell::new(None) }) })
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        self.popout.set_monitor(monitor);
    }

    /// Opens under the middle of the screen, roughly where the clock sits.
    pub fn toggle_centred(self: &Rc<Self>) {
        let width = self.popout.monitor_width();
        self.toggle(f64::from(width) / 2.0);
    }

    /// Opens centred under `x`, back on the current month with today picked.
    pub fn toggle(self: &Rc<Self>, x: f64) {
        if self.popout.is_open() {
            return self.popout.hide();
        }
        let today = now();
        self.view.month.set(Month::of(&today));
        self.view.selected.set(Some((Month::of(&today), today.day_of_month() as u32)));
        let s = self.popout.scale();
        // Rebuilt on every open so the weather footer and month are current.
        self.popout.set_content(&self.build(s));
        self.popout.show(Anchor::Under(x));
    }

    fn build(&self, s: f64) -> gtk::Overlay {
        let width = 318.0 * s;
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .width_request(width.round() as i32)
            .margin_top((16.0 * s).round() as i32)
            .margin_bottom((16.0 * s).round() as i32)
            .margin_start((18.0 * s).round() as i32)
            .margin_end((18.0 * s).round() as i32)
            .build();

        // Header: eyebrow and month title, with the paging arrows.
        let title = TextStyle::new(21.0 * s, pango::Weight::Semibold, BRIGHT).tracking(-0.4 * s).label("");
        title.set_xalign(0.0);
        let titles = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing((5.0 * s).round() as i32).hexpand(true).build();
        titles.append(&popout::eyebrow(s, "Calendar"));
        titles.append(&title);
        let header = gtk::Box::builder().height_request((46.0 * s).round() as i32).build();
        header.append(&titles);

        let grid = gtk::DrawingArea::new();
        let set_title = {
            let (title, view) = (title.clone(), self.view.clone());
            move || {
                let month = view.month.get();
                let name = month.first().and_then(|d| d.format("%B %Y").ok()).map(String::from).unwrap_or_default();
                title.set_text(&name);
            }
        };
        set_title();
        let arrows = gtk::Box::builder().spacing((4.0 * s).round() as i32).valign(gtk::Align::End).margin_bottom((2.0 * s).round() as i32).build();
        for delta in [-1, 1] {
            let (view, grid, set_title) = (self.view.clone(), grid.clone(), set_title.clone());
            arrows.append(&arrow(s, delta, move || {
                view.month.set(view.month.get().shift(delta));
                set_title();
                grid.set_content_height(grid_height(view.month.get(), s));
                grid.queue_draw();
            }));
        }
        header.append(&arrows);
        column.append(&header);

        column.append(&rule(s, 9.0, 8.0));
        self.grid(&grid, s);
        column.append(&grid);
        if let Some(footer) = weather_footer(s) {
            column.append(&rule(s, 10.0, 0.0));
            column.append(&footer);
        }

        popout::card(&column)
    }

    /// Weekday initials over the day cells, all drawn in one area that
    /// tracks the pointer for hover and clicks.
    fn grid(&self, area: &gtk::DrawingArea, s: f64) {
        let (head_h, cell_h, gap) = grid_metrics(s);
        area.set_content_height(grid_height(self.view.month.get(), s));

        let cell_at = {
            let view = self.view.clone();
            move |area: &gtk::DrawingArea, x: f64, y: f64| -> Option<usize> {
                let month = view.month.get();
                let col = (x / (f64::from(area.width()) / 7.0)).floor();
                let row = ((y - head_h) / (cell_h + gap)).floor();
                if y < head_h || !(0.0..7.0).contains(&col) || row < 0.0 || row >= f64::from(month.rows()) {
                    return None;
                }
                let index = row as usize * 7 + col as usize;
                let day = index as i64 - i64::from(month.lead()) + 1;
                (1..=i64::from(month.days())).contains(&day).then_some(index)
            }
        };

        area.set_draw_func({
            let view = self.view.clone();
            move |_, cr, w, _| draw_grid(cr, f64::from(w), s, head_h, cell_h, gap, &view)
        });

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let (view, cell_at) = (self.view.clone(), cell_at.clone());
            move |m, x, y| {
                let Some(area) = m.widget().and_downcast::<gtk::DrawingArea>() else {
                    return;
                };
                let cell = cell_at(&area, x, y);
                area.set_cursor_from_name(Some(if cell.is_some() { "pointer" } else { "default" }));
                if view.hovered.replace(cell) != cell {
                    area.queue_draw();
                }
            }
        });
        motion.connect_leave({
            let view = self.view.clone();
            move |m| {
                view.hovered.set(None);
                m.widget().into_iter().for_each(|w| w.queue_draw());
            }
        });
        area.add_controller(motion);

        let click = gtk::GestureClick::new();
        click.connect_released({
            let view = self.view.clone();
            move |gesture, _, x, y| {
                let Some(area) = gesture.widget().and_downcast::<gtk::DrawingArea>() else {
                    return;
                };
                if let Some(index) = cell_at(&area, x, y) {
                    let month = view.month.get();
                    view.selected.set(Some((month, index as u32 + 1 - month.lead())));
                    area.queue_draw();
                }
            }
        });
        area.add_controller(click);
    }
}

/// (weekday header height, cell height, gap between rows).
fn grid_metrics(s: f64) -> (f64, f64, f64) {
    (20.0 * s, 24.0 * s, 2.0 * s)
}

fn grid_height(month: Month, s: f64) -> i32 {
    let (head_h, cell_h, gap) = grid_metrics(s);
    (head_h + f64::from(month.rows()) * (cell_h + gap) - gap).ceil() as i32
}

fn now() -> glib::DateTime {
    glib::DateTime::now_local().expect("local time")
}

fn draw_grid(cr: &cairo::Context, w: f64, s: f64, head_h: f64, cell_h: f64, gap: f64, view: &View) {
    let palette = theme::palette();
    let col_w = w / 7.0;
    let text = |cr: &cairo::Context, label: &str, style: TextStyle, cx: f64, cy: f64| {
        let layout = pangocairo::functions::create_layout(cr);
        layout.set_text(label);
        layout.set_attributes(Some(&style.attrs()));
        let (_, r) = layout.pixel_extents();
        cr.move_to(cx - f64::from(r.width()) / 2.0, cy - f64::from(r.height()) / 2.0);
        pangocairo::functions::show_layout(cr, &layout);
    };

    for (i, day) in ["M", "T", "W", "T", "F", "S", "S"].iter().enumerate() {
        let color = if i >= 5 { FAINT } else { DIM };
        text(cr, day, TextStyle::new(9.0 * s, pango::Weight::Medium, color).tracking(0.5 * s), col_w * (i as f64 + 0.5), 8.0 * s);
    }

    let month = view.month.get();
    let today = now();
    let (lead, days) = (month.lead() as i64, month.days() as i64);
    let previous_days = month.shift(-1).days() as i64;
    let rows = (lead + days + 6) / 7;
    for index in 0..(rows * 7) {
        let (row, col) = (index / 7, index % 7);
        let cx = col_w * (col as f64 + 0.5);
        let cy = head_h + row as f64 * (cell_h + gap) + cell_h / 2.0;
        let day = index - lead + 1;
        let in_month = (1..=days).contains(&day);
        let shown = if day < 1 {
            previous_days + day
        } else if day > days {
            day - days
        } else {
            day
        };
        let is_today = in_month && Month::of(&today) == month && today.day_of_month() as i64 == day;
        let selected = in_month && view.selected.get() == Some((month, day as u32));
        let weekend = col >= 5;
        let box_at = |cr: &cairo::Context, size: f64| theme::rounded_rect(cr, cx - size / 2.0, cy - size / 2.0, size, size, CELL_RADIUS.min(size / 2.0));

        if in_month && !is_today && view.hovered.get() == Some(index as usize) {
            box_at(cr, 22.0 * s);
            theme::set_source(cr, Rgba { r: 0.94, g: 0.88, b: 0.84, a: 0.04 });
            let _ = cr.fill();
        }
        if is_today || selected {
            box_at(cr, 24.0 * s - 1.0);
            if is_today {
                theme::set_source(cr, palette.accent.alpha(0.16));
                let _ = cr.fill_preserve();
            }
            cr.set_line_width(1.0);
            theme::set_source(cr, if is_today { palette.accent.alpha(0.55) } else { CREAM.alpha(0.22) });
            let _ = cr.stroke();
        }

        let color = match () {
            _ if !in_month => NIGHT_GHOST,
            _ if is_today => palette.accent,
            _ if weekend => SUBTLE,
            _ => CREAM.alpha(0.85),
        };
        let weight = if is_today { pango::Weight::Semibold } else { pango::Weight::Normal };
        text(cr, &shown.to_string(), TextStyle::new(11.0 * s, weight, color), cx, cy);
    }
}

/// A square paging button with a chevron, outlined on hover.
fn arrow(s: f64, delta: i32, pressed: impl Fn() + 'static) -> gtk::DrawingArea {
    let px = (24.0 * s).round() as i32;
    let area = gtk::DrawingArea::builder().content_width(px).content_height(px).build();
    area.set_cursor_from_name(Some("pointer"));
    let hovered = Rc::new(Cell::new(false));
    area.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            let (w, h) = (f64::from(w), f64::from(h));
            let palette = theme::palette();
            if hovered.get() {
                theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, CELL_RADIUS);
                theme::set_source(cr, palette.raised);
                let _ = cr.fill_preserve();
                cr.set_line_width(1.0);
                theme::set_source(cr, palette.edge);
                let _ = cr.stroke();
            }
            // A chevron 16px wide, pointing the way it pages.
            let (cx, cy, arm) = (w / 2.0, h / 2.0, 3.5 * s);
            let dir = f64::from(delta);
            cr.move_to(cx - dir * arm / 2.0, cy - arm);
            cr.line_to(cx + dir * arm / 2.0, cy);
            cr.line_to(cx - dir * arm / 2.0, cy + arm);
            cr.set_line_width(1.8 * s * 0.75);
            cr.set_line_cap(cairo::LineCap::Round);
            cr.set_line_join(cairo::LineJoin::Round);
            theme::set_source(cr, if hovered.get() { CREAM } else { ICON_DIM });
            let _ = cr.stroke();
        }
    });
    let motion = gtk::EventControllerMotion::new();
    for entering in [true, false] {
        let (area, hovered) = (area.downgrade(), hovered.clone());
        let update = move || {
            hovered.set(entering);
            area.upgrade().into_iter().for_each(|a| a.queue_draw());
        };
        if entering {
            motion.connect_enter(move |_, _, _| update());
        } else {
            motion.connect_leave(move |_| update());
        }
    }
    area.add_controller(motion);
    let click = gtk::GestureClick::new();
    click.connect_released(move |_, _, _, _| pressed());
    area.add_controller(click);
    area
}

/// A hairline across the card with space above and below.
fn rule(s: f64, above: f64, below: f64) -> gtk::DrawingArea {
    let line = gtk::DrawingArea::builder().content_height(1).margin_top((above * s).round() as i32).margin_bottom((below * s).round() as i32).build();
    line.set_draw_func(|_, cr, w, h| {
        theme::set_source(cr, HAIR);
        cr.rectangle(0.0, 0.0, f64::from(w), f64::from(h));
        let _ = cr.fill();
    });
    line
}

/// Today's weather: glyph, temperature and high/low on the left, the
/// condition on the right.
fn weather_footer(s: f64) -> Option<gtk::Box> {
    let now = weather::current()?;
    let footer = gtk::Box::builder().height_request((24.0 * s).round() as i32).valign(gtk::Align::End).build();
    let left = gtk::Box::builder().spacing((8.0 * s).round() as i32).valign(gtk::Align::End).hexpand(true).build();
    left.append(&theme::icon_glyph(now.glyph.symbol(), 16.0 * s, ICON_DIM));
    left.append(&TextStyle::new(13.0 * s, pango::Weight::Semibold, CREAM).label(&now.temp));
    let range = format!("\u{2191}{}\u{b0}  \u{2193}{}\u{b0}", now.high, now.low);
    left.append(&TextStyle::new(10.0 * s, pango::Weight::Medium, FAINT).label(&range));
    footer.append(&left);
    let condition = TextStyle::new(10.0 * s, pango::Weight::Medium, SUBTLE).label(now.condition);
    condition.set_valign(gtk::Align::End);
    footer.append(&condition);
    Some(footer)
}

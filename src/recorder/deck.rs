//! The recorder's popout, dropping from its bar module: recording control and
//! the latest recordings under one roof. While recording, a pulsing REC tag,
//! the elapsed time and stop; otherwise a Record button that closes the deck
//! and opens the HUD's chooser, so the deck is never in the frame. Below,
//! the four most recent recordings, each with play, folder and trash.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use gtk4 as gtk;

use super::{active, clock_label, elapsed, elapsed_text, icon_button, pulse_dot, recordings, recordings_dir, RED};
use crate::popout::{self, Anchor, Popout};
use crate::theme::{self, TextStyle, CREAM, DIM, FAINT, SUBTLE};

const NAMESPACE: &str = "eclipse-shell-recorder-deck";
const WIDTH: f64 = 320.0;
/// Recordings the deck lists; Eclipse Recorder has them all.
const SHOWN: usize = 4;

pub struct Deck {
    popout: Rc<Popout>,
    /// Whether the card shows the running controls, to rebuild on a change.
    was_active: Cell<bool>,
    clock: RefCell<Option<gtk::Label>>,
}

impl Deck {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let deck = Rc::new(Self { popout: Popout::new(app, NAMESPACE), was_active: Cell::new(false), clock: RefCell::default() });
        super::on_change({
            let deck = Rc::downgrade(&deck);
            move || {
                let Some(deck) = deck.upgrade().filter(|d| d.popout.is_open()) else { return };
                let now = active();
                if now != deck.was_active.get() {
                    deck.rebuild();
                    // The new file shows up a moment after the recorder stops.
                    if !now {
                        let deck = Rc::downgrade(&deck);
                        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
                            if let Some(deck) = deck.upgrade().filter(|d| d.popout.is_open()) {
                                deck.rebuild();
                            }
                        });
                    }
                } else if let Some(clock) = &*deck.clock.borrow() {
                    clock.set_text(&elapsed_text(elapsed()));
                }
            }
        });
        deck
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        if !self.popout.is_open() {
            self.popout.set_monitor(monitor);
        }
    }

    pub fn hide(self: &Rc<Self>) {
        self.popout.hide();
    }

    /// Opens under `x` on its monitor, or against the bar's right end.
    pub fn toggle(self: &Rc<Self>, under: Option<(f64, Option<gdk::Monitor>)>) {
        if self.popout.is_open() {
            return self.popout.hide();
        }
        let anchor = match under {
            Some((x, monitor)) => {
                if monitor.is_some() {
                    self.popout.set_monitor(monitor);
                }
                Anchor::Under(x)
            }
            None => Anchor::Right,
        };
        self.rebuild();
        self.popout.show(anchor);
    }

    fn rebuild(self: &Rc<Self>) {
        let s = self.popout.scale();
        let px = |v: f64| (v * s).round() as i32;
        let palette = theme::palette();
        let on = active();
        self.was_active.set(on);
        *self.clock.borrow_mut() = None;
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(px(8.0))
            .width_request(px(WIDTH) - 2 * px(16.0))
            .margin_top(px(13.0))
            .margin_bottom(px(14.0))
            .margin_start(px(16.0))
            .margin_end(px(16.0))
            .build();

        let all = recordings();
        let head = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
        head.append(&theme::filled_icon_glyph("fiber_manual_record", 16.0 * s, palette.accent));
        let title = TextStyle::new(10.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.6 * s).label("RECORD");
        title.set_hexpand(true);
        title.set_xalign(0.0);
        head.append(&title);
        if !all.is_empty() {
            head.append(&TextStyle::new(10.0 * s, pango::Weight::Medium, DIM).label(&all.len().to_string()));
        }
        column.append(&head);

        if on {
            // Running: the pulsing REC tag and the time, stop on the right.
            let row = gtk::Box::new(gtk::Orientation::Horizontal, px(9.0));
            row.set_size_request(-1, px(30.0));
            let tag = TextStyle::new(9.5 * s, pango::Weight::Bold, CREAM).mono().tracking(1.2 * s).label("REC");
            tag.set_margin_start(px(7.0));
            tag.set_margin_end(px(7.0));
            let tag_back = pulse_dot(1.0, RED);
            tag_back.set_halign(gtk::Align::Fill);
            tag_back.set_valign(gtk::Align::Fill);
            tag_back.set_draw_func(move |area, cr, w, h| {
                let t = area.frame_clock().map_or(0.0, |c| c.frame_time() as f64 / 1e6);
                let alpha = 0.18 + 0.82 * (0.5 + 0.5 * (t * std::f64::consts::TAU / 1.24).cos());
                cr.rectangle(0.0, 0.0, f64::from(w), f64::from(h));
                theme::set_source(cr, RED.alpha(alpha));
                let _ = cr.fill();
            });
            let pill = gtk::Overlay::new();
            pill.set_child(Some(&tag_back));
            pill.add_overlay(&tag);
            pill.set_measure_overlay(&tag, true);
            pill.set_valign(gtk::Align::Center);
            row.append(&pill);
            let clock = clock_label(13.0 * s);
            clock.set_hexpand(true);
            clock.set_xalign(0.0);
            row.append(&clock);
            *self.clock.borrow_mut() = Some(clock);
            row.append(&icon_button(s, "stop", RED, "Stop recording", super::stop));
            column.append(&row);
        } else {
            let deck = Rc::downgrade(self);
            let button = crate::walls::ui::button(s, Some("fiber_manual_record"), "RECORD", false, true, move || {
                if let Some(deck) = deck.upgrade() {
                    deck.popout.hide();
                }
                super::open_chooser();
            });
            button.set_hexpand(true);
            column.append(&button);
        }

        if all.is_empty() && !on {
            let none = TextStyle::new(11.0 * s, pango::Weight::Normal, FAINT).label("No recordings yet");
            none.set_xalign(0.0);
            column.append(&none);
        }
        if !all.is_empty() {
            let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
            for rec in all.iter().take(SHOWN) {
                list.append(&self.recording_row(s, rec));
            }
            column.append(&list);
        }
        self.popout.set_content(&popout::card(&column));
    }

    fn recording_row(self: &Rc<Self>, s: f64, rec: &super::Recording) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let row = gtk::Box::new(gtk::Orientation::Horizontal, px(6.0));
        row.set_size_request(-1, px(30.0));
        let label = TextStyle::new(11.0 * s, pango::Weight::Medium, SUBTLE).label(&rec.label);
        label.set_hexpand(true);
        label.set_xalign(0.0);
        label.set_ellipsize(pango::EllipsizeMode::End);
        label.set_margin_start(px(4.0));
        row.append(&label);
        row.append(&TextStyle::new(9.5 * s, pango::Weight::Medium, FAINT).mono().label(&rec.size));
        let path = rec.path.clone();
        let deck = Rc::downgrade(self);
        row.append(&icon_button(s, "play_arrow", CREAM, "Play", {
            let (path, deck) = (path.clone(), deck.clone());
            move || {
                super::open(&path);
                deck.upgrade().into_iter().for_each(|d| d.popout.hide());
            }
        }));
        row.append(&icon_button(s, "folder", theme::ICON_DIM, "Open the folder", {
            let deck = deck.clone();
            move || {
                super::open(&recordings_dir());
                deck.upgrade().into_iter().for_each(|d| d.popout.hide());
            }
        }));
        row.append(&icon_button(s, "delete", RED, "Move to the trash", move || {
            super::trash(&path);
            if let Some(deck) = deck.upgrade() {
                deck.rebuild();
            }
        }));
        row
    }
}

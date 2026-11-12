//! Inbox popout, dropping from the bell: Do Not Disturb, Clear all, and every
//! notification of the session grouped by app, newest first. A group shows
//! its newest notification and opens to the rest; repeats fold into one with
//! a count. Clicking one runs it as its toast would; hovering shows a button
//! that dismisses it. Opening marks everything read after a moment.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use gtk4 as gtk;

use crate::notifications::{self, Notification, Urgency};
use crate::popout::{self, on_click, on_hover, Anchor, Popout};
use crate::theme::{self, TextStyle, CREAM, DIM, FAINT, ICON_DIM, SUBTLE};
use crate::toasts::{self, claim_click};

const NAMESPACE: &str = "eclipse-shell-inbox";
const WIDTH: f64 = 360.0;
/// How long the inbox has to be open before what's in it counts as read.
const READ_AFTER: Duration = Duration::from_millis(700);

/// Notifications that read the same, folded into one entry.
struct Entry {
    newest: Notification,
    /// Every id folded in, newest first.
    ids: Vec<u32>,
}

struct Group {
    app: String,
    entries: Vec<Entry>,
}

pub struct InboxPopout {
    popout: Rc<Popout>,
    /// Bumped on every open, so a stale "mark read" doesn't land later.
    generation: Cell<u32>,
    /// Apps whose groups are opened out.
    expanded: RefCell<HashSet<String>>,
    /// What was unread when the inbox opened, still marked while it's open.
    unread_at_open: RefCell<HashSet<u32>>,
}

impl InboxPopout {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        Self::with_popout(Popout::new(app, NAMESPACE))
    }

    /// Shown on `host`, e.g. a [`Popout::pane`] in a sidebar.
    pub fn with_popout(host: Rc<Popout>) -> Rc<Self> {
        let inbox = Rc::new(Self { popout: host, generation: Cell::new(0), expanded: RefCell::default(), unread_at_open: RefCell::default() });
        let weak = Rc::downgrade(&inbox);
        notifications::on_change(move |_| {
            if let Some(i) = weak.upgrade().filter(|i| i.popout.is_open()) {
                i.rebuild();
            }
        });
        inbox
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        self.popout.set_monitor(monitor);
    }

    /// Opens centred under `x`.
    pub fn toggle(self: &Rc<Self>, x: f64) {
        if self.popout.is_open() {
            return self.popout.hide();
        }
        *self.unread_at_open.borrow_mut() = notifications::all().iter().filter(|n| !n.read).map(|n| n.id).collect();
        self.rebuild();
        self.popout.show(Anchor::Under(x));

        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let inbox = Rc::downgrade(self);
        glib::timeout_add_local_once(READ_AFTER, move || {
            if inbox.upgrade().is_some_and(|i| i.popout.is_open() && i.generation.get() == generation) {
                notifications::mark_all_read();
            }
        });
    }

    fn rebuild(self: &Rc<Self>) {
        let s = self.popout.scale();
        self.popout.set_content(&popout::card(&self.body(s)));
    }

    fn body(self: &Rc<Self>, s: f64) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(px(10.0))
            .width_request(px(WIDTH) - 2 * px(16.0))
            .margin_top(px(13.0))
            .margin_bottom(px(14.0))
            .margin_start(px(16.0))
            .margin_end(px(16.0))
            .build();
        let all = notifications::all();

        let head = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
        head.append(&theme::filled_icon_glyph("notifications", 16.0 * s, theme::palette().accent));
        let title = TextStyle::new(10.0 * s, pango::Weight::Semibold, SUBTLE).tracking(1.6 * s).label("NOTIFICATIONS");
        title.set_hexpand(true);
        title.set_xalign(0.0);
        head.append(&title);
        if !all.is_empty() {
            head.append(&text_button(s, "Clear all", notifications::clear));
        }
        column.append(&head);

        let quiet = gtk::Box::new(gtk::Orientation::Horizontal, px(8.0));
        quiet.append(&theme::icon_glyph("do_not_disturb_on", 14.0 * s, ICON_DIM));
        let label = TextStyle::new(11.0 * s, pango::Weight::Medium, CREAM).label("Do not disturb");
        label.set_hexpand(true);
        label.set_xalign(0.0);
        quiet.append(&label);
        let on = notifications::dnd();
        quiet.append(&popout::switch(s, on, true, move || notifications::set_dnd(!on)));
        column.append(&quiet);
        if on {
            let note = TextStyle::new(9.5 * s, pango::Weight::Normal, FAINT).label("Only critical notifications pop up; the rest wait here.");
            note.set_xalign(0.0);
            note.set_wrap(true);
            column.append(&note);
        }
        column.append(&popout::divider());

        if all.is_empty() {
            let empty = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(px(6.0)).margin_top(px(10.0)).margin_bottom(px(10.0)).build();
            empty.append(&theme::icon_glyph("notifications_none", 26.0 * s, FAINT));
            empty.append(&TextStyle::new(11.0 * s, pango::Weight::Medium, DIM).label("All caught up"));
            column.append(&empty);
            return column;
        }

        let list = gtk::Box::new(gtk::Orientation::Vertical, px(12.0));
        for group in groups(&all) {
            list.append(&self.group_view(s, group));
        }
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(px(460.0))
            .child(&list)
            .build();
        column.append(&scroller);
        column
    }

    fn group_view(self: &Rc<Self>, s: f64, group: Group) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let view = gtk::Box::new(gtk::Orientation::Vertical, px(2.0));
        let expanded = self.expanded.borrow().contains(&group.app);
        let more = group.entries.len() > 1;

        let head = gtk::Box::builder().spacing(px(8.0)).height_request(px(22.0)).build();
        let icon = toasts::picture(s, group.entries[0].newest.picture.as_ref(), 18.0);
        icon.set_valign(gtk::Align::Center);
        head.append(&icon);
        let name = TextStyle::new(10.5 * s, pango::Weight::Semibold, SUBTLE).label(&group.app);
        popout::fill(&name);
        head.append(&name);
        if more {
            let count: usize = group.entries.iter().map(|e| e.ids.len()).sum();
            head.append(&TextStyle::new(9.5 * s, pango::Weight::Semibold, FAINT).label(&count.to_string()));
            head.append(&theme::icon_glyph(if expanded { "expand_less" } else { "expand_more" }, 14.0 * s, ICON_DIM));
            let (inbox, app) = (Rc::downgrade(self), group.app.clone());
            on_click(&head, move || {
                if let Some(i) = inbox.upgrade() {
                    let mut open = i.expanded.borrow_mut();
                    if !open.remove(&app) {
                        open.insert(app.clone());
                    }
                    drop(open);
                    i.rebuild();
                }
            });
        }
        view.append(&head);

        let shown = if expanded { group.entries.len() } else { 1 };
        for entry in group.entries.into_iter().take(shown) {
            view.append(&self.entry_view(s, entry));
        }
        view
    }

    fn entry_view(self: &Rc<Self>, s: f64, entry: Entry) -> gtk::Overlay {
        let px = |v: f64| (v * s).round() as i32;
        let n = &entry.newest;
        let critical = n.urgency == Urgency::Critical;
        let unread = entry.ids.iter().any(|id| self.unread_at_open.borrow().contains(id));
        let hovered = Rc::new(Cell::new(false));
        let back = gtk::DrawingArea::new();
        back.set_draw_func({
            let hovered = hovered.clone();
            move |_, cr, w, h| {
                let palette = theme::palette();
                let (w, h) = (f64::from(w), f64::from(h));
                if hovered.get() {
                    theme::rounded_rect(cr, 0.0, 0.0, w, h, 8.0 * s);
                    theme::set_source(cr, palette.raised);
                    let _ = cr.fill();
                }
                if critical {
                    theme::rounded_rect(cr, 0.0, 6.0 * s, 2.5 * s, h - 12.0 * s, 1.25 * s);
                    theme::set_source(cr, palette.accent_lit);
                    let _ = cr.fill();
                }
            }
        });

        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(px(2.0)).margin_start(px(26.0)).margin_end(px(8.0)).margin_top(px(6.0)).margin_bottom(px(7.0)).build();
        let top = gtk::Box::new(gtk::Orientation::Horizontal, px(6.0));
        if unread {
            let dot = gtk::DrawingArea::builder().content_width(px(6.0)).content_height(px(6.0)).valign(gtk::Align::Center).build();
            dot.set_draw_func(|_, cr, w, h| {
                let (w, h) = (f64::from(w), f64::from(h));
                cr.arc(w / 2.0, h / 2.0, w.min(h) / 2.0, 0.0, std::f64::consts::TAU);
                theme::set_source(cr, theme::palette().accent_lit);
                let _ = cr.fill();
            });
            top.append(&dot);
        }
        let summary_text = if n.summary.is_empty() { n.app.clone() } else { n.summary.clone() };
        let summary = TextStyle::new(11.5 * s, pango::Weight::Semibold, if critical { CREAM } else { theme::BRIGHT.alpha(0.92) }).label(&summary_text);
        popout::fill(&summary);
        top.append(&summary);
        if entry.ids.len() > 1 {
            top.append(&TextStyle::new(9.5 * s, pango::Weight::Bold, theme::palette().accent_lit).label(&format!("\u{d7}{}", entry.ids.len())));
        }
        // The age, giving way to a dismiss button on hover.
        let age = TextStyle::new(9.5 * s, pango::Weight::Medium, FAINT).mono().label(&toasts::age(&n.arrived));
        top.append(&age);
        let dismiss = theme::icon_glyph("close", 14.0 * s, ICON_DIM);
        dismiss.set_visible(false);
        claim_click(&dismiss, {
            let ids = entry.ids.clone();
            move || ids.iter().for_each(|id| notifications::dismiss(*id))
        });
        top.append(&dismiss);
        content.append(&top);

        if !n.body.is_empty() {
            let body = TextStyle::new(10.5 * s, pango::Weight::Normal, SUBTLE).label(&n.body);
            body.set_xalign(0.0);
            body.set_wrap(true);
            body.set_wrap_mode(pango::WrapMode::WordChar);
            body.set_lines(3);
            body.set_ellipsize(pango::EllipsizeMode::End);
            body.set_max_width_chars(1);
            body.set_hexpand(true);
            content.append(&body);
        }
        let buttons: Vec<&(String, String)> = n.buttons().collect();
        if !buttons.is_empty() {
            let row = gtk::Box::builder().spacing(px(6.0)).margin_top(px(4.0)).build();
            for (key, label) in buttons {
                let (id, key) = (n.id, key.clone());
                row.append(&toasts::action_button(s, label, move || notifications::invoke(id, &key)));
            }
            content.append(&row);
        }

        let view = gtk::Overlay::new();
        view.set_child(Some(&back));
        view.add_overlay(&content);
        view.set_measure_overlay(&content, true);
        let id = n.id;
        let inbox = Rc::downgrade(self);
        on_click(&view, move || {
            notifications::activate(id);
            inbox.upgrade().into_iter().for_each(|i| i.popout.hide());
        });
        let (back, age, dismiss) = (back.downgrade(), age.downgrade(), dismiss.downgrade());
        on_hover(&view, move |inside| {
            hovered.set(inside);
            back.upgrade().into_iter().for_each(|b| b.queue_draw());
            age.upgrade().into_iter().for_each(|a| a.set_visible(!inside));
            dismiss.upgrade().into_iter().for_each(|d| d.set_visible(inside));
        });
        view
    }
}

/// Notifications by app, newest app first; within one, newest first, with
/// neighbours that read the same folded together.
fn groups(all: &[Notification]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for n in all.iter().rev() {
        let group = match groups.iter_mut().position(|g| g.app == n.app) {
            Some(i) => &mut groups[i],
            None => {
                groups.push(Group { app: n.app.clone(), entries: Vec::new() });
                groups.last_mut().expect("just pushed")
            }
        };
        match group.entries.last_mut() {
            Some(last) if last.newest.summary == n.summary && last.newest.body == n.body => last.ids.push(n.id),
            _ => group.entries.push(Entry { newest: n.clone(), ids: vec![n.id] }),
        }
    }
    groups
}

/// A word that brightens on hover and does `pressed` on a click.
fn text_button(s: f64, text: &str, pressed: impl Fn() + 'static) -> gtk::Label {
    let label = TextStyle::new(10.0 * s, pango::Weight::Semibold, DIM).label(text);
    on_hover(&label, {
        let label = label.downgrade();
        move |inside| {
            if let Some(l) = label.upgrade() {
                TextStyle::new(10.0 * s, pango::Weight::Semibold, if inside { CREAM } else { DIM }).apply(&l);
            }
        }
    });
    on_click(&label, pressed);
    label
}

#[cfg(test)]
mod tests {
    use super::groups;
    use crate::notifications::{Notification, Urgency};
    use gtk4::glib;

    fn note(id: u32, app: &str, summary: &str) -> Notification {
        Notification {
            id,
            app: app.into(),
            summary: summary.into(),
            body: String::new(),
            actions: Vec::new(),
            urgency: Urgency::Normal,
            picture: None,
            desktop_entry: String::new(),
            arrived: glib::DateTime::now_utc().expect("a clock"),
            timeout: None,
            transient: false,
            resident: false,
            read: false,
        }
    }

    #[test]
    fn groups_by_app_and_folds_repeats() {
        // Oldest first, as the server keeps them.
        let all = [note(1, "Firefox", "Download complete"), note(2, "Calendar", "Standup"), note(3, "Firefox", "Download complete"), note(4, "Firefox", "Download complete"), note(5, "Firefox", "Page loaded")];
        let groups = groups(&all);
        let apps: Vec<&str> = groups.iter().map(|g| g.app.as_str()).collect();
        assert_eq!(apps, ["Firefox", "Calendar"]);
        let firefox: Vec<Vec<u32>> = groups[0].entries.iter().map(|e| e.ids.clone()).collect();
        // Newest first; the three downloads fold into one, newest id first.
        assert_eq!(firefox, [vec![5], vec![4, 3, 1]]);
    }
}

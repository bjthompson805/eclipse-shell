//! Focused-window title with its app icon. A new title cross-fades in over the
//! old one, and the width eases to fit instead of snapping.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::pango;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::anim::{self, Easing, Tween};
use crate::hyprland::Window;
use crate::icons;
use crate::theme::{TextStyle, DIM};

const RESIZE_MS: f64 = 500.0;
const FADE_MS: f64 = 200.0;

pub struct Title {
    pub widget: gtk::Overlay,
    sizer: gtk::DrawingArea,
    icon: gtk::Image,
    live: gtk::Label,
    ghost: gtk::Label,
    style: TextStyle,
    lead: f64,
    icon_size: f64,
    icon_gap: f64,
    max_width: f64,
    current: RefCell<Option<Window>>,
    width: Rc<RefCell<Tween>>,
    fade: Rc<RefCell<Tween>>,
}

impl Title {
    pub fn new(s: f64) -> Rc<Self> {
        let style = TextStyle::new(10.5 * s, pango::Weight::Medium, DIM);
        let sizer = gtk::DrawingArea::new();
        let icon = gtk::Image::builder().halign(gtk::Align::Start).valign(gtk::Align::Center).build();
        let label = || {
            let label = style.label("");
            label.set_halign(gtk::Align::Start);
            label.set_valign(gtk::Align::Center);
            label.set_xalign(0.0);
            label.set_ellipsize(pango::EllipsizeMode::End);
            label.set_single_line_mode(true);
            label
        };
        let (live, ghost) = (label(), label());

        let widget = gtk::Overlay::builder().valign(gtk::Align::Center).overflow(gtk::Overflow::Hidden).build();
        widget.set_child(Some(&sizer));
        widget.add_overlay(&icon);
        widget.add_overlay(&ghost);
        widget.add_overlay(&live);

        let title = Rc::new(Self {
            widget,
            sizer,
            icon,
            live,
            ghost,
            style,
            lead: 6.0 * s,
            icon_size: 12.0 * s,
            icon_gap: 5.0 * s,
            max_width: 340.0 * s,
            current: RefCell::default(),
            width: Tween::shared(RESIZE_MS, Easing::OutCubic),
            fade: Tween::shared(FADE_MS, Easing::OutCubic),
        });
        title.icon.set_pixel_size(title.icon_size.round() as i32);
        title.icon.set_margin_start(title.lead.round() as i32);
        let text_lead = (title.lead + title.icon_size + title.icon_gap).round() as i32;
        title.live.set_margin_start(text_lead);
        title.ghost.set_margin_start(text_lead);
        title.sizer.set_size_request(0, (style.px * 1.4).round() as i32);
        title
    }

    fn text_width(&self, text: &str) -> f64 {
        let layout = self.live.create_pango_layout(Some(text));
        layout.set_attributes(Some(&self.style.attrs()));
        f64::from(layout.pixel_size().0)
    }

    pub fn update(self: &Rc<Self>, focused: Option<&Window>) {
        if self.current.borrow().as_ref() == focused {
            return;
        }
        let previous = self.current.replace(focused.cloned());
        let text = focused.map(|w| w.title.as_str()).unwrap_or_default();
        let text_lead = self.lead + self.icon_size + self.icon_gap;
        let text_room = self.max_width - text_lead;

        match focused.filter(|w| !w.class.is_empty()) {
            Some(window) => self.icon.set_from_gicon(&icons::for_class(&window.class)),
            None => self.icon.clear(),
        }

        // Pin each label to its own final width so the text never re-elides
        // while the container eases; the container's clip does the reveal.
        let fit = |text: &str| self.text_width(text).min(text_room).ceil() as i32;
        let old_text = self.live.text();
        self.ghost.set_text(&old_text);
        self.ghost.set_size_request(fit(&old_text), -1);
        self.live.set_text(text);
        self.live.set_size_request(fit(text), -1);

        let target = if text.is_empty() { 0.0 } else { self.text_width(text).min(text_room) + text_lead };
        anim::animate_to(&self.sizer, &self.width, target, |sizer, w| {
            sizer.set_size_request(w.round() as i32, sizer.height_request());
        });

        // A plain title change (same window, e.g. a terminal updating its
        // title) cross-fades like a focus change does.
        if previous.is_some() {
            self.fade.borrow_mut().jump(0.0);
        }
        let title = self.clone();
        anim::animate_to(&self.live, &self.fade, 1.0, move |_, t| {
            title.live.set_opacity(t);
            title.ghost.set_opacity(1.0 - t);
        });
    }
}

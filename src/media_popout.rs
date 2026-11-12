//! Media popout, dropping from the now-playing pill: the cover art beside the
//! track, a seek line, the transport, and the player's own volume, on a card
//! washed with the art's colour. With several players, chips switch between
//! them.
//!
//! Track details come from src/media.rs as players announce them. Players
//! don't announce their position, so it's asked for on open and every couple
//! of seconds after, and runs on a local clock in between. The card is only
//! rebuilt when what it shows changes; the seek line and volume move in place.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::cairo::{self, ImageSurface};
use gtk::{gdk, gio, glib, pango};
use gtk4 as gtk;

use crate::audio::{self, Node};
use crate::config;
use crate::fader::Fader;
use crate::media::{self, Player, Repeat};
use crate::popout::{self, on_click, on_hover, Anchor, Popout};
use crate::theme::{self, Rgba, TextStyle, BRIGHT, CREAM, DIM, FAINT, ICON_DIM, SUBTLE};

const NAMESPACE: &str = "eclipse-shell-media";
const TICK: Duration = Duration::from_millis(250);
/// Ticks between asking the player where it is (and reading the volume).
const SYNC_TICKS: u32 = 8;
const WIDTH: f64 = 340.0;
const ART: f64 = 80.0;
/// How long a player may go without a track before the card says so.
const GONE_AFTER: Duration = Duration::from_millis(1500);

/// Where playback was at a moment, to run on from.
#[derive(Clone, Copy)]
struct Clock {
    at: Duration,
    since: Instant,
    running: bool,
}

impl Clock {
    fn now(&self, length: Option<Duration>) -> Duration {
        let at = if self.running { self.at + self.since.elapsed() } else { self.at };
        length.map_or(at, |l| at.min(l))
    }
}

/// The seek line's widgets and state.
struct Seek {
    line: gtk::DrawingArea,
    elapsed: gtk::Label,
    total: gtk::Label,
    length: Duration,
    /// Where a drag would land, while one is under way.
    preview: Cell<Option<f64>>,
    s: f64,
}

pub struct MediaPopout {
    popout: Rc<Popout>,
    /// Bumped on every open, so a stale ticker from an earlier open stops.
    generation: Cell<u32>,
    /// The player picked with a chip; otherwise the one the bar shows.
    chosen: RefCell<Option<String>>,
    shape: RefCell<String>,
    clock: Cell<Clock>,
    /// The track the clock is for.
    track: RefCell<String>,
    /// The last artist and album lines (0 and 1) each player filled.
    lines: RefCell<HashMap<(String, usize), String>>,
    /// When the current track started showing.
    track_since: Cell<Instant>,
    /// When the player vanished; the card waits a moment to see whether it's back.
    gone: Cell<Option<Instant>>,
    seek: RefCell<Option<Rc<Seek>>>,
    volume: RefCell<Option<(u32, Rc<Fader>)>>,
    /// The player's audio stream, once found.
    stream: RefCell<Option<Node>>,
    art: RefCell<(String, Option<ImageSurface>)>,
    art_area: RefCell<Option<gtk::DrawingArea>>,
    tint: Rc<Cell<Option<Rgba>>>,
    card: RefCell<Option<gtk::Overlay>>,
}

impl MediaPopout {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let popout = Rc::new(Self {
            popout: Popout::new(app, NAMESPACE),
            generation: Cell::new(0),
            chosen: RefCell::default(),
            shape: RefCell::default(),
            clock: Cell::new(Clock { at: Duration::ZERO, since: Instant::now(), running: false }),
            track: RefCell::default(),
            gone: Cell::new(None),
            lines: RefCell::default(),
            track_since: Cell::new(Instant::now()),
            seek: RefCell::default(),
            volume: RefCell::default(),
            stream: RefCell::default(),
            art: RefCell::default(),
            art_area: RefCell::default(),
            tint: Rc::default(),
            card: RefCell::default(),
        });
        let weak = Rc::downgrade(&popout);
        media::on_change(move || {
            if let Some(p) = weak.upgrade().filter(|p| p.popout.is_open()) {
                p.update();
            }
        });
        popout
    }

    pub fn set_monitor(&self, monitor: Option<gdk::Monitor>) {
        self.popout.set_monitor(monitor);
    }

    /// Opens centred under `x`.
    pub fn toggle(self: &Rc<Self>, x: f64) {
        if self.popout.is_open() {
            return self.popout.hide();
        }
        *self.chosen.borrow_mut() = None;
        *self.stream.borrow_mut() = None;
        self.shape.borrow_mut().clear();
        let popout = self.clone();
        glib::spawn_future_local(async move {
            popout.sync().await;
            popout.update();
            popout.popout.show(Anchor::Under(x));
        });

        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let ticks = Cell::new(0);
        let popout = Rc::downgrade(self);
        glib::timeout_add_local(TICK, move || match popout.upgrade() {
            Some(p) if p.popout.is_open() && p.generation.get() == generation => {
                ticks.set(ticks.get() + 1);
                if ticks.get() % SYNC_TICKS == 0 {
                    glib::spawn_future_local(async move { p.sync().await });
                } else {
                    p.show_position();
                }
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
    }

    fn player(&self) -> Option<Player> {
        let players = media::players();
        let chosen = self.chosen.borrow().clone();
        chosen.and_then(|name| players.iter().find(|p| p.name == name).cloned()).or_else(media::current)
    }

    /// Asks the player where it is and reads its stream's volume.
    async fn sync(self: &Rc<Self>) {
        let Some(player) = self.player() else { return };
        if let Some(at) = media::position(&player).await {
            self.clock.set(Clock { at, since: Instant::now(), running: player.playing });
        }
        let graph = audio::read().await;
        let stream = find_stream(&graph.streams, &player);
        let had = self.stream.borrow().as_ref().map(|n| n.id);
        *self.stream.borrow_mut() = stream.clone();
        if had != stream.as_ref().map(|n| n.id) {
            self.update();
        } else if let (Some(node), Some((_, fader))) = (stream, &*self.volume.borrow()) {
            fader.show(node.volume, node.muted);
        }
        self.show_position();
    }

    /// Rebuilds the card if what it shows changed.
    fn update(self: &Rc<Self>) {
        let player = self.player();
        let shape = shape(player.as_ref(), &media::players(), self.stream.borrow().as_ref().map(|n| n.id));
        if *self.shape.borrow() == shape {
            return;
        }
        // Players can drop their track for a moment while changing it; keep
        // the card unless they're still gone a little later.
        let showing_one = !self.shape.borrow().is_empty() && !self.shape.borrow().starts_with("none");
        if player.is_none() && showing_one {
            let since = self.gone.get().unwrap_or_else(|| {
                let popout = Rc::downgrade(self);
                glib::timeout_add_local_once(GONE_AFTER, move || {
                    if let Some(p) = popout.upgrade() {
                        p.update();
                    }
                });
                Instant::now()
            });
            self.gone.set(Some(since));
            if since.elapsed() < GONE_AFTER {
                return;
            }
        }
        self.gone.set(None);
        *self.shape.borrow_mut() = shape;
        if let Some(p) = &player {
            let track = format!("{} {} {}", p.name, p.track_id, p.title);
            let clock = self.clock.get();
            if *self.track.borrow() != track {
                // A new track starts from the top until the player says otherwise.
                *self.track.borrow_mut() = track;
                self.track_since.set(Instant::now());
                self.clock.set(Clock { at: Duration::ZERO, since: Instant::now(), running: p.playing });
                let popout = self.clone();
                glib::spawn_future_local(async move { popout.sync().await });
            } else if clock.running != p.playing {
                // A play or pause moves the clock's footing.
                self.clock.set(Clock { at: clock.now(p.length), since: Instant::now(), running: p.playing });
            }
        }
        self.rebuild(player.as_ref());
    }

    fn rebuild(self: &Rc<Self>, player: Option<&Player>) {
        *self.seek.borrow_mut() = None;
        *self.volume.borrow_mut() = None;
        *self.art_area.borrow_mut() = None;
        let s = self.popout.scale();
        let body = self.body(s, player);
        let card = popout::tinted_card(&body, self.tint.clone());
        self.popout.set_content(&card);
        *self.card.borrow_mut() = Some(card);
        self.load_art(player.map_or("", |p| p.art_url.as_str()));
        self.show_position();
    }

    fn show_position(&self) {
        let Some(seek) = self.seek.borrow().clone() else { return };
        let at = match seek.preview.get() {
            Some(fraction) => seek.length.mul_f64(fraction),
            None => self.clock.get().now(Some(seek.length)),
        };
        seek.elapsed.set_text(&clock_text(at));
        seek.total.set_text(&clock_text(seek.length));
        seek.line.queue_draw();
    }

    // ---- the card --------------------------------------------------------

    fn body(self: &Rc<Self>, s: f64, player: Option<&Player>) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(px(12.0))
            .width_request(px(WIDTH) - 2 * px(16.0))
            .margin_top(px(14.0))
            .margin_bottom(px(14.0))
            .margin_start(px(16.0))
            .margin_end(px(16.0))
            .build();
        let Some(player) = player else {
            let none = TextStyle::new(11.0 * s, pango::Weight::Medium, FAINT).label("Nothing playing");
            none.set_xalign(0.0);
            column.append(&none);
            return column;
        };

        let players = media::players();
        if players.len() > 1 {
            let chips = gtk::Box::new(gtk::Orientation::Horizontal, px(5.0));
            for other in &players {
                let (popout, name) = (Rc::downgrade(self), other.name.clone());
                chips.append(&player_chip(s, &display_name(other), other.name == player.name, move || {
                    if let Some(p) = popout.upgrade() {
                        *p.chosen.borrow_mut() = Some(name.clone());
                        p.shape.borrow_mut().clear();
                        glib::spawn_future_local(async move { p.sync().await });
                    }
                }));
            }
            column.append(&chips);
        }

        let head = gtk::Box::new(gtk::Orientation::Horizontal, px(14.0));
        head.append(&self.art_view(s));
        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(px(3.0)).valign(gtk::Align::Center).hexpand(true).build();
        text.append(&popout::micro_label(s, &display_name(player)));
        let title = TextStyle::new(13.5 * s, pango::Weight::Semibold, CREAM).label(&player.title);
        popout::fill(&title);
        title.set_wrap(true);
        title.set_wrap_mode(pango::WrapMode::WordChar);
        title.set_lines(2);
        text.append(&title);
        // Players send a new track's details in pieces, and may blank a line
        // before sending it again. Just after a change, an empty line keeps
        // what it said; after that, a line this player has filled before
        // keeps its room, so the card holds still.
        let settling = self.track_since.get().elapsed() < GONE_AFTER;
        let mut lines = self.lines.borrow_mut();
        let mut held = false;
        for (i, (line, color)) in [(&player.artists, SUBTLE), (&player.album, FAINT)].into_iter().enumerate() {
            let key = (player.name.clone(), i);
            let shown = if !line.is_empty() {
                lines.insert(key, line.clone());
                line.clone()
            } else {
                match lines.get(&key) {
                    Some(last) if settling => {
                        held = true;
                        last.clone()
                    }
                    // A space keeps an empty line its height.
                    Some(_) => " ".to_owned(),
                    None => continue,
                }
            };
            let label = TextStyle::new(11.0 * s, pango::Weight::Medium, color).label(&shown);
            popout::fill(&label);
            text.append(&label);
        }
        drop(lines);
        if held {
            // Look again once the wait is over, in case the line stays empty.
            let popout = Rc::downgrade(self);
            glib::timeout_add_local_once(GONE_AFTER, move || {
                if let Some(p) = popout.upgrade().filter(|p| p.popout.is_open()) {
                    p.shape.borrow_mut().clear();
                    p.update();
                }
            });
        }
        head.append(&text);
        column.append(&head);

        // A live stream has no length, so nothing to seek in.
        if let Some(length) = player.length {
            column.append(&self.seek_view(s, player, length));
        }
        column.append(&self.transport(s, player));

        if let Some(node) = self.stream.borrow().as_ref() {
            let fader = audio::volume_fader(s, node, ("volume_up", "volume_off"), 15.0 * s, true);
            column.append(&fader.widget);
            *self.volume.borrow_mut() = Some((node.id, fader));
        }
        column
    }

    fn art_view(self: &Rc<Self>, s: f64) -> gtk::DrawingArea {
        let size = (ART * s).round() as i32;
        let area = gtk::DrawingArea::builder().content_width(size).content_height(size).valign(gtk::Align::Start).build();
        let popout = Rc::downgrade(self);
        area.set_draw_func(move |_, cr, w, h| {
            let Some(p) = popout.upgrade() else { return };
            draw_art(cr, f64::from(w), f64::from(h), p.art.borrow().1.as_ref(), s);
        });
        *self.art_area.borrow_mut() = Some(area.clone());
        area
    }

    fn load_art(self: &Rc<Self>, url: &str) {
        if self.art.borrow().0 == url {
            return;
        }
        // The old cover and its wash stay up until the new one is ready.
        self.art.borrow_mut().0 = url.to_owned();
        if url.is_empty() {
            self.show_art(None);
            return;
        }
        let (popout, url) = (Rc::downgrade(self), url.to_owned());
        glib::spawn_future_local(async move {
            let surface = match gio::File::for_uri(&url).load_bytes_future().await {
                Ok((bytes, _)) => gdk::Texture::from_bytes(&bytes).ok().and_then(|t| theme::texture_surface(&t)),
                Err(_) => None,
            };
            let Some(p) = popout.upgrade() else { return };
            // The art may have moved on while this one loaded.
            if p.art.borrow().0 == url {
                p.show_art(surface);
            }
        });
    }

    /// Puts up a cover (or the placeholder) and the wash that goes with it.
    fn show_art(&self, surface: Option<ImageSurface>) {
        self.tint.set(surface.as_ref().and_then(average_colour));
        self.art.borrow_mut().1 = surface;
        if let Some(area) = &*self.art_area.borrow() {
            area.queue_draw();
        }
        if let Some(wash) = self.card.borrow().as_ref().and_then(|c| c.child()) {
            wash.queue_draw();
        }
    }

    fn seek_view(self: &Rc<Self>, s: f64, player: &Player, length: Duration) -> gtk::Box {
        let px = |v: f64| (v * s).round() as i32;
        let view = gtk::Box::new(gtk::Orientation::Vertical, px(4.0));
        let line = gtk::DrawingArea::builder().height_request(px(14.0)).hexpand(true).build();
        let times = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let style = TextStyle::new(9.5 * s, pango::Weight::Medium, DIM).mono();
        let elapsed = style.label("");
        elapsed.set_hexpand(true);
        elapsed.set_xalign(0.0);
        let total = style.label("");
        times.append(&elapsed);
        times.append(&total);
        view.append(&line);
        view.append(&times);

        let seek = Rc::new(Seek { line: line.clone(), elapsed, total, length, preview: Cell::new(None), s });
        let (popout, weak) = (Rc::downgrade(self), Rc::downgrade(&seek));
        line.set_draw_func(move |_, cr, w, h| {
            let (Some(p), Some(seek)) = (popout.upgrade(), weak.upgrade()) else { return };
            let fraction = seek.preview.get().unwrap_or_else(|| {
                let length = seek.length.as_secs_f64();
                if length > 0.0 { p.clock.get().now(Some(seek.length)).as_secs_f64() / length } else { 0.0 }
            });
            draw_seek(cr, f64::from(w), f64::from(h), fraction, seek.preview.get().is_some(), seek.s);
        });

        if player.can_seek {
            line.set_cursor_from_name(Some("pointer"));
            let drag = gtk::GestureDrag::new();
            let start = Rc::new(Cell::new(0.0));
            let fraction_at = |line: &gtk::DrawingArea, x: f64| (x / f64::from(line.width()).max(1.0)).clamp(0.0, 1.0);
            drag.connect_drag_begin({
                let (seek, start, popout) = (Rc::downgrade(&seek), start.clone(), Rc::downgrade(self));
                move |_, x, _| {
                    start.set(x);
                    if let (Some(seek), Some(p)) = (seek.upgrade(), popout.upgrade()) {
                        seek.preview.set(Some(fraction_at(&seek.line, x)));
                        p.show_position();
                    }
                }
            });
            drag.connect_drag_update({
                let (seek, start, popout) = (Rc::downgrade(&seek), start.clone(), Rc::downgrade(self));
                move |_, dx, _| {
                    if let (Some(seek), Some(p)) = (seek.upgrade(), popout.upgrade()) {
                        seek.preview.set(Some(fraction_at(&seek.line, start.get() + dx)));
                        p.show_position();
                    }
                }
            });
            drag.connect_drag_end({
                let (seek, popout, player) = (Rc::downgrade(&seek), Rc::downgrade(self), player.clone());
                move |_, dx, _| {
                    let (Some(seek), Some(p)) = (seek.upgrade(), popout.upgrade()) else { return };
                    let at = seek.length.mul_f64(fraction_at(&seek.line, start.get() + dx));
                    seek.preview.set(None);
                    media::set_position(&player, at);
                    let running = p.clock.get().running;
                    p.clock.set(Clock { at, since: Instant::now(), running });
                    p.show_position();
                }
            });
            line.add_controller(drag);
        }
        *self.seek.borrow_mut() = Some(seek);
        view
    }

    fn transport(self: &Rc<Self>, s: f64, player: &Player) -> gtk::CenterBox {
        let px = |v: f64| (v * s).round() as i32;
        let lit = theme::palette().accent_lit;
        let middle = gtk::Box::builder().spacing(px(18.0)).build();
        let act = |f: fn(&Player)| {
            let player = player.clone();
            move || f(&player)
        };
        middle.append(&icon_button(s, "skip_previous", 24.0, true, player.can_previous, ICON_DIM, act(media::previous)));
        middle.append(&play_button(s, player.playing, act(media::play_pause)));
        middle.append(&icon_button(s, "skip_next", 24.0, true, player.can_next, ICON_DIM, act(media::next)));

        let bar = gtk::CenterBox::new();
        bar.set_center_widget(Some(&middle));
        if let Some(on) = player.shuffle {
            let player = player.clone();
            bar.set_start_widget(Some(&icon_button(s, "shuffle", 18.0, false, true, if on { lit } else { ICON_DIM }, move || media::set_shuffle(&player, !on))));
        }
        if let Some(repeat) = player.repeat {
            let glyph = if repeat == Repeat::Track { "repeat_one" } else { "repeat" };
            let player = player.clone();
            let color = if repeat == Repeat::None { ICON_DIM } else { lit };
            bar.set_end_widget(Some(&icon_button(s, glyph, 18.0, false, true, color, move || media::set_repeat(&player, repeat.next()))));
        }
        bar
    }
}

/// Everything the card shows but the position and volume.
fn shape(player: Option<&Player>, players: &[Player], stream: Option<u32>) -> String {
    let names: Vec<&str> = players.iter().map(|p| p.name.as_str()).collect();
    let Some(p) = player else { return format!("none {names:?}") };
    format!(
        "{} {} {} {} {} {} {} {:?} {} {} {} {:?} {:?} {names:?} {stream:?}",
        p.name, p.title, p.artists, p.album, p.art_url, p.identity, p.playing, p.length, p.can_previous, p.can_next, p.can_seek, p.shuffle, p.repeat
    )
}

/// The player's stream: the one its process plays, else one named like it.
fn find_stream(streams: &[Node], player: &Player) -> Option<Node> {
    let by_pid = player.pid.and_then(|pid| streams.iter().find(|n| n.pid == Some(pid)));
    let name = display_name(player).to_lowercase();
    by_pid.or_else(|| streams.iter().find(|n| !name.is_empty() && (n.app.to_lowercase().contains(&name) || n.label.to_lowercase().contains(&name)))).cloned()
}

/// The player's name for itself, else its bus name's last part.
fn display_name(player: &Player) -> String {
    if !player.identity.is_empty() {
        return player.identity.clone();
    }
    let short = player.name.trim_start_matches("org.mpris.MediaPlayer2.");
    let short = short.split('.').next().unwrap_or(short);
    let mut chars = short.chars();
    chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

fn clock_text(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 { format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60) } else { format!("{}:{:02}", secs / 60, secs % 60) }
}

/// The art's average colour, from a coarse sample of its pixels.
fn average_colour(surface: &ImageSurface) -> Option<Rgba> {
    let (w, h) = (surface.width(), surface.height());
    if w <= 0 || h <= 0 {
        return None;
    }
    // Draw it small and read that back rather than walk every pixel.
    let mut small = ImageSurface::create(cairo::Format::ARgb32, 8, 8).ok()?;
    {
        let cr = cairo::Context::new(&small).ok()?;
        cr.scale(8.0 / f64::from(w), 8.0 / f64::from(h));
        cr.set_source_surface(surface, 0.0, 0.0).ok()?;
        cr.source().set_filter(cairo::Filter::Good);
        cr.paint().ok()?;
    }
    small.flush();
    let stride = small.stride() as usize;
    let data = small.data().ok()?;
    let (mut r, mut g, mut b, mut n) = (0.0, 0.0, 0.0, 0.0);
    for y in 0..8 {
        for x in 0..8 {
            // ARGB32 is native-endian: B, G, R, A in memory on little-endian.
            let i = y * stride + x * 4;
            let a = f64::from(data[i + 3]);
            if a > 0.0 {
                b += f64::from(data[i]) / a;
                g += f64::from(data[i + 1]) / a;
                r += f64::from(data[i + 2]) / a;
                n += 1.0;
            }
        }
    }
    (n > 0.0).then(|| Rgba { r: r / n, g: g / n, b: b / n, a: 1.0 })
}

// ---- drawing -----------------------------------------------------------------

/// The cover cropped to a rounded square, or a note on a tile while there is none.
fn draw_art(cr: &cairo::Context, w: f64, h: f64, art: Option<&ImageSurface>, s: f64) {
    let radius = 10.0 * s;
    theme::rounded_rect(cr, 0.0, 0.0, w, h, radius);
    match art {
        Some(surface) => {
            let _ = cr.save();
            cr.clip();
            let (tw, th) = (f64::from(surface.width()), f64::from(surface.height()));
            let scale = w.max(h) / tw.min(th);
            cr.translate((w - tw * scale) / 2.0, (h - th * scale) / 2.0);
            cr.scale(scale, scale);
            let _ = cr.set_source_surface(surface, 0.0, 0.0);
            cr.source().set_filter(cairo::Filter::Good);
            let _ = cr.paint();
            let _ = cr.restore();
        }
        None => {
            theme::set_source(cr, theme::palette().raised);
            let _ = cr.fill();
            let layout = pangocairo::functions::create_layout(cr);
            layout.set_text("music_note");
            layout.set_attributes(Some(&TextStyle { family: config::ICON_FONT, ..TextStyle::new(30.0 * s, pango::Weight::Medium, ICON_DIM) }.attrs()));
            let (_, logical) = layout.pixel_extents();
            cr.move_to((w - f64::from(logical.width())) / 2.0, (h - f64::from(logical.height())) / 2.0);
            pangocairo::functions::show_layout(cr, &layout);
        }
    }
    theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, radius);
    cr.set_line_width(1.0);
    theme::set_source(cr, theme::HAIR);
    let _ = cr.stroke();
}

/// A thin track, filled to `fraction`, with a knob that grows while dragged.
fn draw_seek(cr: &cairo::Context, w: f64, h: f64, fraction: f64, dragging: bool, s: f64) {
    let palette = theme::palette();
    let thread = 3.0 * s;
    let y = (h - thread) / 2.0;
    theme::rounded_rect(cr, 0.0, y, w, thread, thread / 2.0);
    theme::set_source(cr, BRIGHT.alpha(0.08));
    let _ = cr.fill();
    let filled = w * fraction.clamp(0.0, 1.0);
    if filled > 0.0 {
        let gradient = cairo::LinearGradient::new(0.0, 0.0, filled, 0.0);
        gradient.add_color_stop_rgba(0.0, palette.accent.r, palette.accent.g, palette.accent.b, 0.7);
        gradient.add_color_stop_rgba(1.0, palette.accent_lit.r, palette.accent_lit.g, palette.accent_lit.b, 1.0);
        theme::rounded_rect(cr, 0.0, y, filled.max(thread), thread, thread / 2.0);
        let _ = cr.set_source(&gradient);
        let _ = cr.fill();
    }
    let r = if dragging { 5.5 * s } else { 4.0 * s };
    cr.arc(filled.clamp(r, (w - r).max(r)), h / 2.0, r, 0.0, std::f64::consts::TAU);
    theme::set_source(cr, CREAM);
    let _ = cr.fill();
}

// ---- pieces ------------------------------------------------------------------

/// A glyph that brightens on hover and does `pressed` on a click; greyed and
/// inert when not `enabled`.
fn icon_button(s: f64, glyph: &'static str, px: f64, filled: bool, enabled: bool, color: Rgba, pressed: impl Fn() + 'static) -> gtk::Label {
    let make = move |c: Rgba| if filled { theme::filled_icon_glyph(glyph, px * s, c) } else { theme::icon_glyph(glyph, px * s, c) };
    let button = make(if enabled { color } else { FAINT });
    button.set_valign(gtk::Align::Center);
    if !enabled {
        return button;
    }
    on_hover(&button, {
        let button = button.downgrade();
        move |inside| {
            if let Some(b) = button.upgrade() {
                b.set_attributes(make(if inside { CREAM } else { color }).attributes().as_ref());
            }
        }
    });
    on_click(&button, pressed);
    button
}

/// Play or pause, in an accent circle that lightens on hover.
fn play_button(s: f64, playing: bool, pressed: impl Fn() + 'static) -> gtk::DrawingArea {
    let size = (42.0 * s).round() as i32;
    let area = gtk::DrawingArea::builder().content_width(size).content_height(size).valign(gtk::Align::Center).build();
    let hovered = Rc::new(Cell::new(false));
    area.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            let palette = theme::palette();
            let (w, h) = (f64::from(w), f64::from(h));
            cr.arc(w / 2.0, h / 2.0, w.min(h) / 2.0, 0.0, std::f64::consts::TAU);
            theme::set_source(cr, if hovered.get() { palette.accent_lit } else { palette.accent });
            let _ = cr.fill();
            let label = theme::filled_icon_glyph(if playing { "pause" } else { "play_arrow" }, 24.0 * s, BRIGHT);
            let layout = pangocairo::functions::create_layout(cr);
            layout.set_text(&label.text());
            if let Some(attrs) = label.attributes() {
                layout.set_attributes(Some(&attrs));
            }
            let (_, logical) = layout.pixel_extents();
            cr.move_to((w - f64::from(logical.width())) / 2.0, (h - f64::from(logical.height())) / 2.0);
            pangocairo::functions::show_layout(cr, &layout);
        }
    });
    on_hover(&area, {
        let area = area.downgrade();
        move |inside| {
            hovered.set(inside);
            if let Some(a) = area.upgrade() {
                a.queue_draw();
            }
        }
    });
    on_click(&area, pressed);
    area
}

/// A player's name on a pill; the current one is outlined in the accent.
fn player_chip(s: f64, name: &str, current: bool, pressed: impl Fn() + 'static) -> gtk::Overlay {
    let label = TextStyle::new(9.5 * s, pango::Weight::Semibold, if current { CREAM } else { DIM }).label(name);
    let (x, y) = ((9.0 * s).round() as i32, (3.0 * s).round() as i32);
    label.set_margin_start(x);
    label.set_margin_end(x);
    label.set_margin_top(y);
    label.set_margin_bottom(y);
    let hovered = Rc::new(Cell::new(false));
    let back = gtk::DrawingArea::new();
    back.set_draw_func({
        let hovered = hovered.clone();
        move |_, cr, w, h| {
            let palette = theme::palette();
            let (w, h) = (f64::from(w), f64::from(h));
            theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, (h - 1.0) / 2.0);
            if current || hovered.get() {
                theme::set_source(cr, palette.raised);
                let _ = cr.fill_preserve();
            }
            cr.set_line_width(1.0);
            theme::set_source(cr, if current { palette.accent_lit } else { palette.outline });
            let _ = cr.stroke();
        }
    });
    let chip = gtk::Overlay::new();
    chip.set_child(Some(&back));
    chip.add_overlay(&label);
    chip.set_measure_overlay(&label, true);
    if !current {
        on_hover(&chip, move |inside| {
            hovered.set(inside);
            back.queue_draw();
        });
        on_click(&chip, pressed);
    }
    chip
}

//! Annotations: what each tool leaves on a screenshot, how it's drawn, and
//! how a click finds one. Points are global logical coordinates, the
//! selection's space; a cairo context translated to that space draws them on
//! screen and at full resolution alike.

use std::f64::consts::PI;

use gtk::{cairo, pango};
use gtk4 as gtk;

use crate::theme::{self, Rgba};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Rect,
    Ellipse,
    Line,
    Arrow,
    Pen,
    Marker,
    Text,
    Blur,
    Pixelate,
    Magnify,
    Counter,
}

impl Kind {
    /// Drawn from the picture beneath rather than on top of it.
    pub fn is_effect(self) -> bool {
        matches!(self, Self::Blur | Self::Pixelate | Self::Magnify)
    }

    /// Has a hand-drawn variant.
    pub fn roughable(self) -> bool {
        matches!(self, Self::Rect | Self::Ellipse | Self::Line | Self::Arrow)
    }
}

/// The highlighter's own colour, whatever the swatch.
pub const MARKER: Rgba = Rgba::rgb8(0xf5, 0xd0, 0x20);

#[derive(Clone, Debug)]
pub struct Ann {
    pub kind: Kind,
    pub points: Vec<(f64, f64)>,
    pub color: Rgba,
    pub width: f64,
    pub rough: bool,
    pub text: String,
    /// Text size in px.
    pub size: f64,
    /// A numbered step's number.
    pub n: u32,
}

impl Ann {
    pub fn new(kind: Kind, points: Vec<(f64, f64)>, color: Rgba, width: f64) -> Self {
        Self { kind, points, color, width, rough: false, text: String::new(), size: 16.0, n: 0 }
    }

    pub fn shifted(&self, dx: f64, dy: f64) -> Self {
        let mut a = self.clone();
        a.points.iter_mut().for_each(|p| *p = (p.0 + dx, p.1 + dy));
        a
    }

    /// The two-point box, normalised: x, y, w, h.
    pub fn rect(&self) -> (f64, f64, f64, f64) {
        let (p0, p1) = (self.points[0], self.points[self.points.len() - 1]);
        (p0.0.min(p1.0), p0.1.min(p1.1), (p1.0 - p0.0).abs(), (p1.1 - p0.1).abs())
    }

    pub fn counter_radius(&self) -> f64 {
        self.width * 2.5 + 9.0
    }

    /// The box a selection outline goes around.
    pub fn bbox(&self) -> (f64, f64, f64, f64) {
        let xs = self.points.iter().map(|p| p.0);
        let ys = self.points.iter().map(|p| p.1);
        let (x0, x1) = (xs.clone().fold(f64::MAX, f64::min), xs.fold(f64::MIN, f64::max));
        let (y0, y1) = (ys.clone().fold(f64::MAX, f64::min), ys.fold(f64::MIN, f64::max));
        match self.kind {
            Kind::Text => (x0, y0, (self.text.chars().count().max(1) as f64 * self.size * 0.6).max(self.size), self.size * 1.4),
            Kind::Counter => {
                let r = self.counter_radius();
                (x0 - r, y0 - r, 2.0 * r, 2.0 * r)
            }
            _ => (x0, y0, x1 - x0, y1 - y0),
        }
    }

    pub fn hit(&self, x: f64, y: f64) -> bool {
        let tol = self.width.max(8.0);
        let in_box = |(bx, by, bw, bh): (f64, f64, f64, f64), pad: f64| x >= bx - pad && x <= bx + bw + pad && y >= by - pad && y <= by + bh + pad;
        match self.kind {
            Kind::Rect | Kind::Marker | Kind::Blur | Kind::Pixelate | Kind::Magnify => in_box(self.bbox(), tol),
            Kind::Text => in_box(self.bbox(), 0.0),
            Kind::Counter => in_box(self.bbox(), tol),
            Kind::Line | Kind::Arrow => dist_to_segment((x, y), self.points[0], self.points[1]) <= tol,
            Kind::Pen => self.points.windows(2).any(|s| dist_to_segment((x, y), s[0], s[1]) <= tol),
            Kind::Ellipse => {
                let (bx, by, bw, bh) = self.bbox();
                let (rx, ry) = (bw / 2.0 + tol, bh / 2.0 + tol);
                let (nx, ny) = ((x - (bx + bw / 2.0)) / rx, (y - (by + bh / 2.0)) / ry);
                nx * nx + ny * ny <= 1.0
            }
        }
    }
}

fn dist_to_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    if len2 == 0.0 {
        return (p.0 - a.0).hypot(p.1 - a.1);
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0);
    (p.0 - (a.0 + t * dx)).hypot(p.1 - (a.1 + t * dy))
}

/// The annotations with undo and redo, as whole snapshots.
#[derive(Default)]
pub struct Model {
    pub items: Vec<Ann>,
    undo: Vec<Vec<Ann>>,
    redo: Vec<Vec<Ann>>,
}

impl Model {
    fn commit(&mut self) {
        self.undo.push(self.items.clone());
        self.redo.clear();
    }

    pub fn add(&mut self, a: Ann) {
        self.commit();
        self.items.push(a);
    }

    pub fn shift(&mut self, i: usize, dx: f64, dy: f64) {
        if i < self.items.len() {
            self.commit();
            self.items[i] = self.items[i].shifted(dx, dy);
        }
    }

    pub fn remove(&mut self, i: usize) {
        if i < self.items.len() {
            self.commit();
            self.items.remove(i);
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(&mut self.items, prev));
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(&mut self.items, next));
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The topmost annotation under a point.
    pub fn hit(&self, x: f64, y: f64) -> Option<usize> {
        self.items.iter().rposition(|a| a.hit(x, y))
    }

    pub fn counters(&self) -> u32 {
        self.items.iter().filter(|a| a.kind == Kind::Counter).count() as u32
    }
}

// ---- drawing ------------------------------------------------------------------

/// Draws a vector annotation (not an effect). `caret` puts a text cursor after
/// a text annotation being typed.
pub fn draw(cr: &cairo::Context, a: &Ann, caret: bool) {
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    match a.kind {
        Kind::Text => return draw_text(cr, a, caret),
        Kind::Counter => return draw_counter(cr, a),
        _ if a.kind.is_effect() || a.points.len() < 2 => return,
        _ => {}
    }
    if a.rough && a.kind.roughable() {
        theme::set_source(cr, a.color);
        cr.set_line_width(a.width);
        polyline(cr, &rough_main(a));
        let _ = cr.stroke();
        if a.kind == Kind::Arrow {
            polyline(cr, &rough_head(a));
            let _ = cr.stroke();
        }
        return;
    }
    match a.kind {
        Kind::Rect => {
            // Qt draws a border inside the box.
            let (x, y, w, h) = a.rect();
            let inset = a.width / 2.0;
            cr.rectangle(x + inset, y + inset, (w - a.width).max(0.0), (h - a.width).max(0.0));
            theme::set_source(cr, a.color);
            cr.set_line_width(a.width);
            cr.set_line_join(cairo::LineJoin::Miter);
            let _ = cr.stroke();
        }
        Kind::Marker => {
            let (x, y, w, h) = a.rect();
            cr.rectangle(x, y, w, h);
            theme::set_source(cr, a.color.alpha(0.4));
            let _ = cr.fill();
        }
        Kind::Ellipse => {
            let (x, y, w, h) = a.rect();
            if w > 0.0 && h > 0.0 {
                let _ = cr.save();
                cr.translate(x + w / 2.0, y + h / 2.0);
                cr.scale(w / 2.0, h / 2.0);
                cr.arc(0.0, 0.0, 1.0, 0.0, 2.0 * PI);
                let _ = cr.restore();
                theme::set_source(cr, a.color);
                cr.set_line_width(a.width);
                let _ = cr.stroke();
            }
        }
        Kind::Line | Kind::Pen => {
            polyline(cr, &a.points);
            theme::set_source(cr, a.color);
            cr.set_line_width(a.width);
            let _ = cr.stroke();
        }
        Kind::Arrow => {
            polyline(cr, &a.points[..2]);
            theme::set_source(cr, a.color);
            cr.set_line_width(a.width);
            let _ = cr.stroke();
            let (tip, l, r) = arrow_head(a);
            cr.move_to(tip.0, tip.1);
            cr.line_to(l.0, l.1);
            cr.line_to(r.0, r.1);
            cr.close_path();
            let _ = cr.fill_preserve();
            let _ = cr.stroke();
        }
        _ => {}
    }
}

fn polyline(cr: &cairo::Context, pts: &[(f64, f64)]) {
    if let Some(first) = pts.first() {
        cr.move_to(first.0, first.1);
        pts[1..].iter().for_each(|p| cr.line_to(p.0, p.1));
    }
}

fn arrow_head(a: &Ann) -> ((f64, f64), (f64, f64), (f64, f64)) {
    let (p0, p1) = (a.points[0], a.points[1]);
    let angle = (p1.1 - p0.1).atan2(p1.0 - p0.0);
    let len = (a.width * 5.0).max(22.0);
    let spread = 0.45;
    let side = |s: f64| (p1.0 - len * (angle + s).cos(), p1.1 - len * (angle + s).sin());
    (p1, side(-spread), side(spread))
}

pub fn text_font(size: f64) -> pango::FontDescription {
    let mut font = theme::TextStyle::new(size, pango::Weight::Medium, theme::CREAM).font();
    font.set_absolute_size(size * f64::from(pango::SCALE));
    font
}

fn draw_text(cr: &cairo::Context, a: &Ann, caret: bool) {
    let layout = pangocairo::functions::create_layout(cr);
    layout.set_font_description(Some(&text_font(a.size)));
    layout.set_text(&a.text);
    let (x, y) = a.points[0];
    theme::set_source(cr, a.color);
    cr.move_to(x, y);
    pangocairo::functions::show_layout(cr, &layout);
    if caret {
        let (_, logical) = layout.pixel_extents();
        let cx = x + f64::from(logical.width()) + 1.0;
        cr.rectangle(cx, y + a.size * 0.1, (a.size / 14.0).max(1.5), a.size * 1.15);
        let _ = cr.fill();
    }
}

fn draw_counter(cr: &cairo::Context, a: &Ann) {
    let r = a.counter_radius();
    let (x, y) = a.points[0];
    cr.arc(x, y, r - 1.0, 0.0, 2.0 * PI);
    theme::set_source(cr, a.color);
    let _ = cr.fill_preserve();
    cr.set_line_width(2.0);
    theme::set_source(cr, Rgba::rgb8(0xff, 0xff, 0xff));
    let _ = cr.stroke();
    let layout = pangocairo::functions::create_layout(cr);
    let mut font = text_font(r * 1.05);
    font.set_weight(pango::Weight::Bold);
    layout.set_font_description(Some(&font));
    layout.set_text(&a.n.to_string());
    let (ink, _) = layout.pixel_extents();
    cr.move_to(x - f64::from(ink.x()) - f64::from(ink.width()) / 2.0, y - f64::from(ink.y()) - f64::from(ink.height()) / 2.0);
    pangocairo::functions::show_layout(cr, &layout);
}

// ---- hand-drawn style -----------------------------------------------------------
// A deterministic per-item jitter, seeded from the item's points so it stays
// put across repaints, wobbles the stroke; a Catmull-Rom spline through the
// jittered points makes it flow like a pen.

fn seed(a: &Ann) -> u64 {
    let (p0, p1) = (a.points[0], a.points[a.points.len() - 1]);
    ((p0.0 * 13.1 + p0.1 * 7.3 + p1.0 * 3.7 + p1.1 * 5.9).round().abs() as u64 % 100_000) + 1
}

struct Jitter(u64);

impl Jitter {
    fn next(&mut self) -> f64 {
        self.0 = (self.0 * 9301 + 49297) % 233_280;
        self.0 as f64 / 233_280.0
    }
}

fn rough_segment(from: (f64, f64), to: (f64, f64), amp: f64, rnd: &mut Jitter, out: &mut Vec<(f64, f64)>) {
    out.push((from.0 + (rnd.next() - 0.5) * amp * 0.6, from.1 + (rnd.next() - 0.5) * amp * 0.6));
    for i in 1..3 {
        let t = f64::from(i) / 3.0;
        out.push((from.0 + (to.0 - from.0) * t + (rnd.next() - 0.5) * amp, from.1 + (to.1 - from.1) * t + (rnd.next() - 0.5) * amp));
    }
    out.push((to.0 + (rnd.next() - 0.5) * amp * 0.6, to.1 + (rnd.next() - 0.5) * amp * 0.6));
}

fn smooth(pts: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    if pts.len() < 3 {
        return pts;
    }
    let mut out = Vec::new();
    let segments = 8;
    for i in 0..pts.len() - 1 {
        let p0 = pts[i.saturating_sub(1)];
        let (p1, p2) = (pts[i], pts[i + 1]);
        let p3 = pts[(i + 2).min(pts.len() - 1)];
        for j in 0..segments {
            let t = f64::from(j) / f64::from(segments);
            let (t2, t3) = (t * t, t * t * t);
            let f = |a: f64, b: f64, c: f64, d: f64| 0.5 * (2.0 * b + (-a + c) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2 + (-a + 3.0 * b - 3.0 * c + d) * t3);
            out.push((f(p0.0, p1.0, p2.0, p3.0), f(p0.1, p1.1, p2.1, p3.1)));
        }
    }
    out.push(pts[pts.len() - 1]);
    out
}

fn rough_main(a: &Ann) -> Vec<(f64, f64)> {
    let mut rnd = Jitter(seed(a));
    let amp = (a.width * 0.9).clamp(2.5, 7.0);
    let (p0, p1) = (a.points[0], a.points[1]);
    let mut out = Vec::new();
    match a.kind {
        Kind::Line | Kind::Arrow => rough_segment(p0, p1, amp, &mut rnd, &mut out),
        Kind::Rect => {
            let (xa, xb, ya, yb) = (p0.0.min(p1.0), p0.0.max(p1.0), p0.1.min(p1.1), p0.1.max(p1.1));
            for (from, to) in [((xa, ya), (xb, ya)), ((xb, ya), (xb, yb)), ((xb, yb), (xa, yb)), ((xa, yb), (xa, ya))] {
                rough_segment(from, to, amp, &mut rnd, &mut out);
            }
        }
        Kind::Ellipse => {
            let (cx, cy) = ((p0.0 + p1.0) / 2.0, (p0.1 + p1.1) / 2.0);
            let (rx, ry) = ((p1.0 - p0.0).abs() / 2.0, (p1.1 - p0.1).abs() / 2.0);
            let steps = 22;
            for i in 0..=steps {
                let angle = f64::from(i) / f64::from(steps) * 2.0 * PI;
                let jr = 1.0 + (rnd.next() - 0.5) * 0.06;
                out.push((cx + angle.cos() * rx * jr, cy + angle.sin() * ry * jr));
            }
        }
        _ => {}
    }
    smooth(out)
}

fn rough_head(a: &Ann) -> Vec<(f64, f64)> {
    let mut rnd = Jitter(seed(a) + 7);
    let (tip, l, r) = arrow_head(a);
    let mut out = Vec::new();
    rough_segment(l, tip, 2.5, &mut rnd, &mut out);
    rough_segment(tip, r, 2.5, &mut rnd, &mut out);
    smooth(out)
}

// ---- effects -------------------------------------------------------------------

/// Paints an effect annotation from `base`, the picture under the selection:
/// an image whose pixel (0, 0) sits at global (`ox`, `oy`), `scale` pixels per
/// logical px. `cr` is in global logical coordinates.
pub fn draw_effect(cr: &cairo::Context, a: &Ann, base: &cairo::ImageSurface, ox: f64, oy: f64, scale: f64) {
    if a.points.len() < 2 {
        return;
    }
    let (x, y, w, h) = a.rect();
    if w < 1.0 || h < 1.0 {
        return;
    }
    let _ = cr.save();
    match a.kind {
        Kind::Blur => {
            // Qt's FastBlur at radius 64: shrink by 8, blur the small copy,
            // and stretch it back with smooth filtering.
            let k = 8.0;
            let (sw, sh) = (((w / k).ceil() as i32).max(1), ((h / k).ceil() as i32).max(1));
            if let Some(small) = shrink(base, (x - ox) * scale, (y - oy) * scale, w * scale, h * scale, sw, sh) {
                box_blur(&small, 3, 2);
                cr.rectangle(x, y, w, h);
                cr.clip();
                cr.translate(x, y);
                cr.scale(w / f64::from(sw), h / f64::from(sh));
                let _ = cr.set_source_surface(&small, 0.0, 0.0);
                pad_smooth(cr);
                let _ = cr.paint();
            }
        }
        Kind::Pixelate => {
            let block = a.width * 2.0 + 8.0;
            let (sw, sh) = (((w / block).round() as i32).max(1), ((h / block).round() as i32).max(1));
            if let Some(small) = shrink(base, (x - ox) * scale, (y - oy) * scale, w * scale, h * scale, sw, sh) {
                cr.rectangle(x, y, w, h);
                cr.clip();
                cr.translate(x, y);
                cr.scale(w / f64::from(sw), h / f64::from(sh));
                let _ = cr.set_source_surface(&small, 0.0, 0.0);
                cr.source().set_filter(cairo::Filter::Nearest);
                let _ = cr.paint();
            }
        }
        Kind::Magnify => {
            let zoom = 2.0;
            let d = w.min(h);
            let (cx, cy) = (x + w / 2.0, y + h / 2.0);
            cr.arc(cx, cy, d / 2.0, 0.0, 2.0 * PI);
            let _ = cr.save();
            cr.clip();
            // The source square, d/zoom wide, scaled up around the centre.
            cr.translate(cx, cy);
            cr.scale(zoom / scale, zoom / scale);
            let _ = cr.set_source_surface(base, -(cx - ox) * scale, -(cy - oy) * scale);
            cr.source().set_filter(cairo::Filter::Good);
            let _ = cr.paint();
            let _ = cr.restore();
            cr.arc(cx, cy, d / 2.0, 0.0, 2.0 * PI);
            cr.set_line_width((d * 0.03).max(2.0));
            theme::set_source(cr, Rgba::rgb8(0xff, 0xff, 0xff));
            let _ = cr.stroke();
        }
        _ => {}
    }
    let _ = cr.restore();
}

fn pad_smooth(cr: &cairo::Context) {
    let source = cr.source();
    source.set_filter(cairo::Filter::Good);
    source.set_extend(cairo::Extend::Pad);
}

/// The base's (x, y, w, h) pixel box shrunk to sw x sh.
fn shrink(base: &cairo::ImageSurface, x: f64, y: f64, w: f64, h: f64, sw: i32, sh: i32) -> Option<cairo::ImageSurface> {
    let small = cairo::ImageSurface::create(cairo::Format::ARgb32, sw, sh).ok()?;
    {
        let cr = cairo::Context::new(&small).ok()?;
        cr.scale(f64::from(sw) / w, f64::from(sh) / h);
        cr.set_source_surface(base, -x, -y).ok()?;
        pad_smooth(&cr);
        cr.paint().ok()?;
    }
    small.flush();
    Some(small)
}

/// A box blur of `radius` px, `passes` times (three approach a gaussian).
fn box_blur(surface: &cairo::ImageSurface, radius: i32, passes: u32) {
    let (w, h, stride) = (surface.width() as usize, surface.height() as usize, surface.stride() as usize);
    let mut surface = surface.clone();
    let Ok(mut data) = surface.data() else { return };
    let mut line = vec![[0u32; 4]; w.max(h)];
    let r = radius as isize;
    for _ in 0..passes {
        for horizontal in [true, false] {
            let (count, len) = if horizontal { (h, w) } else { (w, h) };
            for i in 0..count {
                let at = |j: usize| if horizontal { i * stride + j * 4 } else { j * stride + i * 4 };
                for (j, px) in line.iter_mut().enumerate().take(len) {
                    let o = at(j);
                    *px = [u32::from(data[o]), u32::from(data[o + 1]), u32::from(data[o + 2]), u32::from(data[o + 3])];
                }
                for j in 0..len as isize {
                    let mut sum = [0u32; 4];
                    for k in -r..=r {
                        let p = line[(j + k).clamp(0, len as isize - 1) as usize];
                        (0..4).for_each(|c| sum[c] += p[c]);
                    }
                    let o = at(j as usize);
                    let n = (2 * r + 1) as u32;
                    (0..4).for_each(|c| data[o + c] = (sum[c] / n) as u8);
                }
            }
        }
    }
}

//! The toolbar's line icons, as SVG path data on a 24-unit grid (ryoshot's
//! own set), and enough of an SVG path reader to stroke them with cairo.

use std::f64::consts::PI;

use gtk::cairo;
use gtk4 as gtk;

fn path(name: &str) -> &'static str {
    match name {
        "select" => "M4 3l7 17 2-7 7-2z",
        "rect" => "M5 6h14a1 1 0 0 1 1 1v10a1 1 0 0 1 -1 1H5a1 1 0 0 1 -1 -1V7a1 1 0 0 1 1 -1z",
        "ellipse" => "M20 12a8 6 0 0 1 -16 0a8 6 0 0 1 16 0z",
        "line" => "M5 19L19 5",
        "arrow" => "M5 19L19 5M19 5h-7M19 5v7",
        "pen" => "M4 18c3-1 4-6 8-9s6-4 8-5c-1 2-2 5-5 8s-8 5-9 8z",
        "text" => "M5 6h14M12 6v13",
        "marker" => "M4 15l10-10 4 4-10 10H4z",
        "blur" => "M11 9a2 2 0 0 1 -4 0a2 2 0 0 1 4 0z M17 13a2 2 0 0 1 -4 0a2 2 0 0 1 4 0z M10.5 16a1.5 1.5 0 0 1 -3 0a1.5 1.5 0 0 1 3 0z",
        "undo" => "M9 7L4 12l5 5M4 12h11a5 5 0 0 1 0 10",
        "redo" => "M15 7l5 5-5 5M20 12H9a5 5 0 0 0 0 10",
        "copy" => "M11 9h9a2 2 0 0 1 2 2v9a2 2 0 0 1 -2 2h-9a2 2 0 0 1 -2 -2v-9a2 2 0 0 1 2 -2z M5 15V5a2 2 0 0 1 2 -2h10",
        "save" => "M5 3h12l4 4v14H5zM8 3v6h8M8 21v-7h8v7",
        "upload" => "M12 16V4M7 9l5-5 5 5M5 20h14",
        "gear" => "M12 8.5a3.5 3.5 0 0 1 0 7a3.5 3.5 0 0 1 0 -7z M12 2.5l1.4 2.2 2.6-.5.4 2.6 2.3 1.3-1.1 2.4 1.1 2.4-2.3 1.3-.4 2.6-2.6-.5L12 21.5l-1.4-2.2-2.6.5-.4-2.6-2.3-1.3 1.1-2.4-1.1-2.4 2.3-1.3.4-2.6 2.6.5z",
        "counter" => "M12 3.6a8.4 8.4 0 1 1 -0.1 0z M11 9.2l1.7 -1.1V16",
        "pixelate" => "M4 4h6v6H4z M14 4h6v6h-6z M4 14h6v6H4z M14 14h6v6h-6z",
        "sketch" => "M4 16c1.6 -3.4 3.2 3.4 4.8 0s3.2 3.4 4.8 0 3.2 3.4 4.4 1",
        "magnify" => "M10 4a6 6 0 1 0 0.01 0z M14.4 14.4L21 21",
        _ => "",
    }
}

/// Strokes the icon `name` centred in a `size` px square at (x, y).
pub fn draw(cr: &cairo::Context, name: &str, x: f64, y: f64, size: f64) {
    let _ = cr.save();
    cr.translate(x, y);
    cr.scale(size / 24.0, size / 24.0);
    trace(cr, path(name));
    cr.set_line_width(1.7);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    let _ = cr.stroke();
    let _ = cr.restore();
}

/// The numbers and command letters of path data, in order.
fn tokens(d: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let bytes = d.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_alphabetic() {
            out.push(Token::Cmd(c));
            i += 1;
        } else if c == '-' || c == '.' || c.is_ascii_digit() {
            let start = i;
            i += 1;
            let mut dot = c == '.';
            while i < bytes.len() {
                let n = bytes[i] as char;
                if n.is_ascii_digit() || (n == '.' && !dot) {
                    dot |= n == '.';
                    i += 1;
                } else {
                    break;
                }
            }
            out.push(Token::Num(d[start..i].parse().unwrap_or(0.0)));
        } else {
            i += 1;
        }
    }
    out
}

enum Token {
    Cmd(char),
    Num(f64),
}

/// Adds path data to cairo's current path: M L H V C S A Z, either case.
fn trace(cr: &cairo::Context, d: &str) {
    let mut nums: Vec<f64> = Vec::new();
    let mut cmd = 'M';
    let (mut cur, mut start, mut last_ctrl) = ((0.0, 0.0), (0.0, 0.0), None::<(f64, f64)>);
    for token in tokens(d) {
        match token {
            Token::Cmd(c) => {
                cmd = c;
                nums.clear();
                if arity(cmd) == 0 {
                    cr.close_path();
                    cur = start;
                    last_ctrl = None;
                }
                continue;
            }
            Token::Num(n) => nums.push(n),
        }
        // A full set of numbers runs the command; more sets repeat it.
        if nums.len() == arity(cmd) {
            let rel = cmd.is_ascii_lowercase();
            let (bx, by) = if rel { cur } else { (0.0, 0.0) };
            let n = &nums;
            match cmd.to_ascii_uppercase() {
                'M' => {
                    cur = (bx + n[0], by + n[1]);
                    start = cur;
                    cr.move_to(cur.0, cur.1);
                    // Further pairs after a moveto are linetos.
                    cmd = if rel { 'l' } else { 'L' };
                    last_ctrl = None;
                }
                'L' => {
                    cur = (bx + n[0], by + n[1]);
                    cr.line_to(cur.0, cur.1);
                    last_ctrl = None;
                }
                'H' => {
                    cur.0 = if rel { cur.0 + n[0] } else { n[0] };
                    cr.line_to(cur.0, cur.1);
                    last_ctrl = None;
                }
                'V' => {
                    cur.1 = if rel { cur.1 + n[0] } else { n[0] };
                    cr.line_to(cur.0, cur.1);
                    last_ctrl = None;
                }
                'C' => {
                    let (c1, c2, end) = ((bx + n[0], by + n[1]), (bx + n[2], by + n[3]), (bx + n[4], by + n[5]));
                    cr.curve_to(c1.0, c1.1, c2.0, c2.1, end.0, end.1);
                    last_ctrl = Some(c2);
                    cur = end;
                }
                'S' => {
                    let c1 = last_ctrl.map_or(cur, |c| (2.0 * cur.0 - c.0, 2.0 * cur.1 - c.1));
                    let (c2, end) = ((bx + n[0], by + n[1]), (bx + n[2], by + n[3]));
                    cr.curve_to(c1.0, c1.1, c2.0, c2.1, end.0, end.1);
                    last_ctrl = Some(c2);
                    cur = end;
                }
                'A' => {
                    let end = (bx + n[5], by + n[6]);
                    arc(cr, cur, n[0], n[1], n[2], n[3] != 0.0, n[4] != 0.0, end);
                    cur = end;
                    last_ctrl = None;
                }
                _ => {}
            }
            nums.clear();
        }
    }
}

fn arity(cmd: char) -> usize {
    match cmd.to_ascii_uppercase() {
        'M' | 'L' => 2,
        'H' | 'V' => 1,
        'C' => 6,
        'S' => 4,
        'A' => 7,
        _ => 0,
    }
}

/// An SVG elliptical arc from `p0` to `p1`, by the endpoint-to-centre
/// conversion in the SVG spec (appendix F.6).
#[allow(clippy::too_many_arguments)]
fn arc(cr: &cairo::Context, p0: (f64, f64), rx: f64, ry: f64, rotation: f64, large: bool, sweep: bool, p1: (f64, f64)) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx == 0.0 || ry == 0.0 || p0 == p1 {
        cr.line_to(p1.0, p1.1);
        return;
    }
    let phi = rotation.to_radians();
    let (cos, sin) = (phi.cos(), phi.sin());
    let (dx, dy) = ((p0.0 - p1.0) / 2.0, (p0.1 - p1.1) / 2.0);
    let (x1, y1) = (cos * dx + sin * dy, -sin * dx + cos * dy);
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = (rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1).max(0.0);
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut k = (num / den).sqrt();
    if large == sweep {
        k = -k;
    }
    let (cx1, cy1) = (k * rx * y1 / ry, -k * ry * x1 / rx);
    let (cx, cy) = (cos * cx1 - sin * cy1 + (p0.0 + p1.0) / 2.0, sin * cx1 + cos * cy1 + (p0.1 + p1.1) / 2.0);
    let angle = |ux: f64, uy: f64, vx: f64, vy: f64| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let theta1 = angle(1.0, 0.0, (x1 - cx1) / rx, (y1 - cy1) / ry);
    let mut delta = angle((x1 - cx1) / rx, (y1 - cy1) / ry, (-x1 - cx1) / rx, (-y1 - cy1) / ry);
    if !sweep && delta > 0.0 {
        delta -= 2.0 * PI;
    } else if sweep && delta < 0.0 {
        delta += 2.0 * PI;
    }
    let _ = cr.save();
    cr.translate(cx, cy);
    cr.rotate(phi);
    cr.scale(rx, ry);
    if delta > 0.0 {
        cr.arc(0.0, 0.0, 1.0, theta1, theta1 + delta);
    } else {
        cr.arc_negative(0.0, 0.0, 1.0, theta1, theta1 + delta);
    }
    let _ = cr.restore();
}

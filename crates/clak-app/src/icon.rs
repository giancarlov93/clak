//! The icon in the tray (Windows) or in the menu bar (Mac): that's where the app lives. It tells
//! at a glance whether an agent is using something (green) or a question is waiting for the
//! user (amber).
//!
//! A head with an antenna — the agent — whose face is a prompt, `>_`: the shell. On
//! Windows in color, on a 32 grid, like the executable's icon; on the Mac black on an 18
//! grid with the prompt "punched out", a *template* image the system colors like the others
//! in the bar (until there is a dot, which is in color).

use tiny_skia::{Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The hub isn't there.
    Offline,
    /// The hub is there, nobody is using anything.
    Idle,
    /// A question is waiting for the user.
    Pending,
    /// An agent has permission to use a program.
    Linked,
}

const ACCENT: (u8, u8, u8) = (0x2F, 0x5B, 0xEA);
const AMBER: (u8, u8, u8) = (0xF2, 0xA9, 0x00);
const GREEN: (u8, u8, u8) = (0x35, 0xC7, 0x6B);

/// The icon's RGBA pixels, its size, and whether it is a template image.
pub struct Drawn {
    pub rgba: Vec<u8>,
    pub size: u32,
    pub template: bool,
}

fn paint(rgb: (u8, u8, u8), alpha: u8) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(rgb.0, rgb.1, rgb.2, alpha));
    paint.anti_alias = true;
    paint
}

fn round_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    // The corners, with the Bézier curves that approximate a quarter circle.
    let k = 0.5523 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

fn circle(cx: f32, cy: f32, r: f32) -> Option<tiny_skia::Path> {
    PathBuilder::from_circle(cx, cy, r)
}

fn stroke(width: f32) -> Stroke {
    Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Stroke::default() }
}

/// The color icon, like the executable's: for the Windows tray.
pub fn windows(kind: Kind, pixels: u32) -> Drawn {
    let mut map = Pixmap::new(pixels, pixels).expect("size");
    let scale = Transform::from_scale(pixels as f32 / 32.0, pixels as f32 / 32.0);
    let body = if kind == Kind::Offline { paint((0x7A, 0x7A, 0x7A), 255) } else { paint(ACCENT, 255) };
    let mut pb = PathBuilder::new();
    pb.move_to(16.0, 2.5);
    pb.line_to(16.0, 7.0);
    if let Some(antenna) = pb.finish() {
        map.stroke_path(&antenna, &body, &stroke(2.6), scale, None);
    }
    if let Some(ball) = circle(16.0, 2.6, 2.2) {
        map.fill_path(&ball, &body, FillRule::Winding, scale, None);
    }
    if let Some(head) = round_rect(2.0, 6.0, 28.0, 25.0, 7.0) {
        map.fill_path(&head, &body, FillRule::Winding, scale, None);
    }
    let white = paint((255, 255, 255), 255);
    let mut pb = PathBuilder::new();
    pb.move_to(8.5, 13.0);
    pb.line_to(13.5, 18.0);
    pb.line_to(8.5, 23.0);
    pb.move_to(16.5, 23.5);
    pb.line_to(23.5, 23.5);
    if let Some(prompt) = pb.finish() {
        map.stroke_path(&prompt, &white, &stroke(3.2), scale, None);
    }
    if let Some(dot) = dot(kind) {
        if let Some(rim) = circle(25.0, 25.0, 7.0) {
            map.fill_path(&rim, &white, FillRule::Winding, scale, None);
        }
        if let Some(core) = circle(25.0, 25.0, 5.0) {
            map.fill_path(&core, &paint(dot, 255), FillRule::Winding, scale, None);
        }
    }
    Drawn { rgba: unpremultiply(map), size: pixels, template: false }
}

/// The icon for the Mac menu bar: black (template) when there is no dot.
pub fn macos(kind: Kind, pixels: u32) -> Drawn {
    let mut map = macos_head(kind, pixels);
    let dot = dot(kind);
    if let Some(color) = dot {
        map.draw_pixmap(0, 0, macos_dot(color, pixels).as_ref(), &tiny_skia::PixmapPaint::default(), Transform::identity(), None);
    }
    Drawn { rgba: unpremultiply(map), size: pixels, template: dot.is_none() }
}

/// With a dot the icon can't be a template, or the dot would lose its color: the bar
/// draws it in two layers (menubar.rs), the head in the color of the bar's text and the
/// dot as it is. PNG: head, dot.
#[cfg(target_os = "macos")]
pub fn macos_layers(kind: Kind, pixels: u32) -> Option<(Vec<u8>, Vec<u8>)> {
    let color = dot(kind)?;
    Some((macos_head(kind, pixels).encode_png().ok()?, macos_dot(color, pixels).encode_png().ok()?))
}

/// The head, black, with the prompt and, if there is a dot, its place punched out.
fn macos_head(kind: Kind, pixels: u32) -> Pixmap {
    let mut map = Pixmap::new(pixels, pixels).expect("size");
    // The Mac's 18 grid, with the y axis pointing down (in AppKit it points up).
    let s = pixels as f32 / 18.0;
    let scale = Transform::from_scale(s, s);
    let flip = |y: f32| 18.0 - y;
    let ink = paint((0, 0, 0), if kind == Kind::Offline { 90 } else { 255 });
    let mut pb = PathBuilder::new();
    pb.move_to(9.0, flip(14.0));
    pb.line_to(9.0, flip(16.5));
    if let Some(antenna) = pb.finish() {
        map.stroke_path(&antenna, &ink, &stroke(1.5), scale, None);
    }
    if let Some(ball) = circle(9.0, flip(16.55), 1.25) {
        map.fill_path(&ball, &ink, FillRule::Winding, scale, None);
    }
    if let Some(head) = round_rect(1.1, flip(14.6), 15.8, 14.0, 3.9) {
        map.fill_path(&head, &ink, FillRule::Winding, scale, None);
    }
    // The prompt is a hole in the head.
    let mut clear = paint((0, 0, 0), 255);
    clear.blend_mode = tiny_skia::BlendMode::Clear;
    let mut pb = PathBuilder::new();
    pb.move_to(4.8, flip(10.7));
    pb.line_to(7.6, flip(7.9));
    pb.line_to(4.8, flip(5.1));
    pb.move_to(9.3, flip(4.8));
    pb.line_to(13.2, flip(4.8));
    if let Some(prompt) = pb.finish() {
        map.stroke_path(&prompt, &clear, &stroke(1.8), scale, None);
    }
    if dot(kind).is_some() {
        if let Some(hole) = circle(14.0, 18.0 - 4.0, 4.5) {
            map.fill_path(&hole, &clear, FillRule::Winding, scale, None);
        }
    }
    map
}

/// The dot alone, in its color, where the head leaves room for it.
fn macos_dot(color: (u8, u8, u8), pixels: u32) -> Pixmap {
    let mut map = Pixmap::new(pixels, pixels).expect("size");
    let s = pixels as f32 / 18.0;
    if let Some(core) = circle(14.0, 18.0 - 4.0, 3.0) {
        map.fill_path(&core, &paint(color, 255), FillRule::Winding, Transform::from_scale(s, s), None);
    }
    map
}

fn dot(kind: Kind) -> Option<(u8, u8, u8)> {
    match kind {
        Kind::Pending => Some(AMBER),
        Kind::Linked => Some(GREEN),
        _ => None,
    }
}

/// tiny-skia works in premultiplied alpha; the system icons want it straight.
fn unpremultiply(map: Pixmap) -> Vec<u8> {
    let mut data = map.take();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    data
}

/// The right icon for this system.
pub fn current(kind: Kind) -> Drawn {
    if cfg!(target_os = "macos") { macos(kind, 36) } else { windows(kind, 32) }
}

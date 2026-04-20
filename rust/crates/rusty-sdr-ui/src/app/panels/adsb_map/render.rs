//! Rendering helpers: Cohen-Sutherland clip and aircraft triangle icon.

use egui::{Pos2, Rect, Vec2};

// ── Cohen-Sutherland line clipping ────────────────────────────────────────────

/// Clip line segment (p0, p1) to the rectangle `clip`.
/// Returns the clipped endpoints, or `None` if the segment is entirely outside.
pub(super) fn clip_segment(mut p0: Pos2, mut p1: Pos2, clip: Rect) -> Option<(Pos2, Pos2)> {
    const LEFT: u8 = 1; const RIGHT: u8 = 2; const BOTTOM: u8 = 4; const TOP: u8 = 8;
    let code = |p: Pos2| -> u8 {
        let mut c = 0u8;
        if p.x < clip.left()   { c |= LEFT; }
        if p.x > clip.right()  { c |= RIGHT; }
        if p.y < clip.top()    { c |= TOP; }
        if p.y > clip.bottom() { c |= BOTTOM; }
        c
    };
    let mut c0 = code(p0);
    let mut c1 = code(p1);
    loop {
        if c0 | c1 == 0 { return Some((p0, p1)); }  // both inside
        if c0 & c1 != 0 { return None; }             // trivially outside
        let c = if c0 != 0 { c0 } else { c1 };
        let pt = if c & TOP != 0 {
            Pos2::new(p0.x + (p1.x - p0.x) * (clip.top()    - p0.y) / (p1.y - p0.y), clip.top())
        } else if c & BOTTOM != 0 {
            Pos2::new(p0.x + (p1.x - p0.x) * (clip.bottom() - p0.y) / (p1.y - p0.y), clip.bottom())
        } else if c & RIGHT != 0 {
            Pos2::new(clip.right(),  p0.y + (p1.y - p0.y) * (clip.right()  - p0.x) / (p1.x - p0.x))
        } else {
            Pos2::new(clip.left(),   p0.y + (p1.y - p0.y) * (clip.left()   - p0.x) / (p1.x - p0.x))
        };
        if c == c0 { p0 = pt; c0 = code(p0); } else { p1 = pt; c1 = code(p1); }
    }
}

// ── Aircraft icon ─────────────────────────────────────────────────────────────

/// Generate three vertices of an aircraft-shaped triangle centered at `pos`,
/// pointing in direction `heading_deg` (0 = North, clockwise).
pub(crate) fn aircraft_triangle(pos: Pos2, heading_deg: f32, r: f32) -> [Pos2; 3] {
    let a = heading_deg.to_radians();
    let (sin_a, cos_a) = (a.sin(), a.cos());

    // Local-space points: nose up, tail split
    let local = [
        (0.0f32, -r * 1.4),             // nose
        (-r * 0.7, r * 0.8),            // left wing-tip
        (r * 0.7, r * 0.8),             // right wing-tip
    ];

    local.map(|(x, y)| {
        let rx = x * cos_a - y * sin_a;
        let ry = x * sin_a + y * cos_a;
        pos + Vec2::new(rx, ry)
    })
}

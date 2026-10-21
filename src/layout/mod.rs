pub mod dwindle;
pub mod master;
pub mod tree;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    /// Shrinks each side by the given amounts (never below 1px).
    pub fn inset(&self, top: i32, right: i32, bottom: i32, left: i32) -> Rect {
        Rect {
            x: self.x + left,
            y: self.y + top,
            w: (self.w - left - right).max(1),
            h: (self.h - top - bottom).max(1),
        }
    }
    pub fn scaled_about_center(&self, f: f32) -> Rect {
        let w = ((self.w as f32) * f).round() as i32;
        let h = ((self.h as f32) * f).round() as i32;
        Rect { x: self.x + (self.w - w) / 2, y: self.y + (self.h - h) / 2, w: w.max(1), h: h.max(1) }
    }
}

/// Applies Hyprland gap semantics: `gaps_out` between tiles and the screen
/// edge, `gaps_in` between neighbouring tiles.
///
/// The layout runs on the work area grown by half an inner gap, then every
/// tile shrinks by half an inner gap — so edges see exactly `gaps_out` and
/// shared edges exactly `gaps_in`.
pub fn with_gaps(work: Rect, gaps_in: i32, gaps_out: i32, layout: impl FnOnce(Rect) -> Vec<Rect>) -> Vec<Rect> {
    layout(gap_outer(work, gaps_in, gaps_out)).into_iter().map(|r| gap_inner(r, gaps_in)).collect()
}

/// The area a layout should fill (see [`with_gaps`]).
pub fn gap_outer(work: Rect, gaps_in: i32, gaps_out: i32) -> Rect {
    let half_lo = gaps_in / 2;
    let half_hi = gaps_in - half_lo;
    work.inset(gaps_out - half_hi, gaps_out - half_lo, gaps_out - half_lo, gaps_out - half_hi)
}

/// Shrinks one laid-out tile by half an inner gap on each side.
pub fn gap_inner(r: Rect, gaps_in: i32) -> Rect {
    let half_lo = gaps_in / 2;
    let half_hi = gaps_in - half_lo;
    r.inset(half_hi, half_lo, half_lo, half_hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_are_exact() {
        let work = Rect::new(0, 0, 1000, 800);
        let tiles = with_gaps(work, 5, 10, |a| dwindle::layout(a, 2, 1.0, false, false));
        assert_eq!(tiles[0].x, 10);
        assert_eq!(tiles[0].y, 10);
        assert_eq!(tiles[1].right(), 990);
        assert_eq!(tiles[1].bottom(), 790);
        assert_eq!(tiles[1].x - tiles[0].right(), 5);
    }
}

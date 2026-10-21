//! Dwindle: every new window halves the space left by the previous one,
//! splitting along the longer axis. With `spiral`, the remainder rotates
//! around the screen (right, down, left, up) — a fibonacci spiral.

use super::Rect;

/// `ratio` follows Hyprland's `default_split_ratio` (0.1 – 1.9, 1.0 = even).
/// `flip` inverts the long-axis rule (bound to `togglesplit`).
pub fn layout(area: Rect, n: usize, ratio: f32, spiral: bool, flip: bool) -> Vec<Rect> {
    let mut out = Vec::with_capacity(n);
    if n == 0 {
        return out;
    }
    let frac = (ratio / 2.0).clamp(0.05, 0.95);
    let mut rest = area;
    for i in 0..n {
        if i == n - 1 {
            out.push(rest);
            break;
        }
        // Square regions split top/bottom, as in Hyprland.
        let side_by_side = (rest.w > rest.h) != flip;
        // In spiral mode every third and fourth split puts the new remainder
        // on the leading side, so the free space winds inwards.
        let reverse = spiral && (i % 4 == 2 || i % 4 == 3);
        let (first, second) = if side_by_side {
            let w1 = ((rest.w as f32) * frac).round() as i32;
            let a = Rect::new(rest.x, rest.y, w1, rest.h);
            let b = Rect::new(rest.x + w1, rest.y, rest.w - w1, rest.h);
            if reverse { (b, a) } else { (a, b) }
        } else {
            let h1 = ((rest.h as f32) * frac).round() as i32;
            let a = Rect::new(rest.x, rest.y, rest.w, h1);
            let b = Rect::new(rest.x, rest.y + h1, rest.w, rest.h - h1);
            if reverse { (b, a) } else { (a, b) }
        };
        out.push(first);
        rest = second;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area_sum(v: &[Rect]) -> i64 {
        v.iter().map(|r| r.w as i64 * r.h as i64).sum()
    }

    #[test]
    fn covers_area_without_overlap() {
        let a = Rect::new(0, 0, 1920, 1080);
        for n in 1..12 {
            for spiral in [false, true] {
                let v = layout(a, n, 1.0, spiral, false);
                assert_eq!(v.len(), n);
                assert_eq!(area_sum(&v), 1920 * 1080);
                for (i, r) in v.iter().enumerate() {
                    for s in &v[i + 1..] {
                        let overlap = r.x < s.right() && s.x < r.right() && r.y < s.bottom() && s.y < r.bottom();
                        assert!(!overlap, "{r:?} overlaps {s:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn splits_long_axis() {
        let v = layout(Rect::new(0, 0, 1000, 500), 3, 1.0, false, false);
        assert_eq!(v[0], Rect::new(0, 0, 500, 500));
        assert_eq!(v[1], Rect::new(500, 0, 500, 250));
        assert_eq!(v[2], Rect::new(500, 250, 500, 250));
    }

    #[test]
    fn spiral_rotates() {
        let v = layout(Rect::new(0, 0, 1600, 1000), 4, 1.0, true, false);
        // third split lands on the right-hand side, leaving the remainder on the left
        assert!(v[2].x > v[3].x);
    }
}

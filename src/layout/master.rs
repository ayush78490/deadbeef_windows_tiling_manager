//! Master-stack: `nmaster` windows share the master area (`mfact` of the
//! screen); the rest stack evenly in the remaining space.

use super::Rect;
use crate::config::types::Orientation;

pub fn layout(area: Rect, n: usize, mfact: f32, nmaster: usize, orientation: Orientation) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let nmaster = nmaster.clamp(1, n);
    let nstack = n - nmaster;
    let horizontal = matches!(orientation, Orientation::Left | Orientation::Right);

    let (master_area, stack_area) = if nstack == 0 {
        (area, None)
    } else if horizontal {
        let mw = ((area.w as f32) * mfact).round() as i32;
        let (m, s) = (Rect::new(area.x, area.y, mw, area.h), Rect::new(area.x + mw, area.y, area.w - mw, area.h));
        if orientation == Orientation::Left { (m, Some(s)) } else { (Rect { x: area.right() - mw, ..m }, Some(Rect { x: area.x, ..s })) }
    } else {
        let mh = ((area.h as f32) * mfact).round() as i32;
        let (m, s) = (Rect::new(area.x, area.y, area.w, mh), Rect::new(area.x, area.y + mh, area.w, area.h - mh));
        if orientation == Orientation::Top { (m, Some(s)) } else { (Rect { y: area.bottom() - mh, ..m }, Some(Rect { y: area.y, ..s })) }
    };

    // Masters split across the long side of the master area; stack windows
    // split along the axis perpendicular to the master/stack division.
    let mut out = split(master_area, nmaster, !horizontal);
    if let Some(s) = stack_area {
        out.extend(split(s, nstack, !horizontal));
    }
    out
}

/// Splits `r` into `n` equal parts, side by side when `side_by_side`.
fn split(r: Rect, n: usize, side_by_side: bool) -> Vec<Rect> {
    let n = n as i32;
    (0..n)
        .map(|i| {
            if side_by_side {
                let x0 = r.x + r.w * i / n;
                let x1 = r.x + r.w * (i + 1) / n;
                Rect::new(x0, r.y, x1 - x0, r.h)
            } else {
                let y0 = r.y + r.h * i / n;
                let y1 = r.y + r.h * (i + 1) / n;
                Rect::new(r.x, y0, r.w, y1 - y0)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_master() {
        let v = layout(Rect::new(0, 0, 1000, 900), 4, 0.6, 1, Orientation::Left);
        assert_eq!(v[0], Rect::new(0, 0, 600, 900));
        assert_eq!(v[1], Rect::new(600, 0, 400, 300));
        assert_eq!(v[3], Rect::new(600, 600, 400, 300));
    }

    #[test]
    fn right_and_top() {
        let v = layout(Rect::new(0, 0, 1000, 900), 2, 0.6, 1, Orientation::Right);
        assert_eq!(v[0], Rect::new(400, 0, 600, 900));
        assert_eq!(v[1], Rect::new(0, 0, 400, 900));
        let v = layout(Rect::new(0, 0, 1000, 1000), 3, 0.5, 1, Orientation::Top);
        assert_eq!(v[0], Rect::new(0, 0, 1000, 500));
        assert_eq!(v[1], Rect::new(0, 500, 500, 500));
    }

    #[test]
    fn single_window_fills() {
        let a = Rect::new(5, 5, 100, 100);
        assert_eq!(layout(a, 1, 0.55, 1, Orientation::Left), vec![a]);
    }
}

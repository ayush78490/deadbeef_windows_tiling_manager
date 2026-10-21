//! Hyprland-style dwindle: a binary split tree per (workspace, monitor).
//!
//! A new window splits the *focused* window in half — side by side when the
//! focused tile is wider than tall, stacked otherwise — and the split keeps
//! its direction and ratio afterwards. Closing a window gives its space to
//! its sibling. `togglesplit` flips the split containing the focused window;
//! `splitratio` resizes it.

use super::Rect;

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Leaf(isize),
    Split { side_by_side: bool, ratio: f32, a: Box<Node>, b: Box<Node> },
}

#[derive(Debug, Clone, Default)]
pub struct Tree {
    root: Option<Node>,
}

/// Fraction of the split given to the first child; `ratio` follows
/// Hyprland's `default_split_ratio` (0.1 – 1.9, 1.0 = even).
fn frac(ratio: f32) -> f32 {
    (ratio / 2.0).clamp(0.05, 0.95)
}

fn split_rect(r: Rect, side_by_side: bool, ratio: f32) -> (Rect, Rect) {
    split_rect_min(r, side_by_side, ratio, 0, 0)
}

/// Splits `r`, but never gives a child less than its minimum along the split
/// axis (`amin`, `bmin`). If both minimums can't fit, space is shared in
/// proportion to them.
fn split_rect_min(r: Rect, side_by_side: bool, ratio: f32, amin: i32, bmin: i32) -> (Rect, Rect) {
    let len = if side_by_side { r.w } else { r.h };
    let want = (len as f32 * frac(ratio)).round() as i32;
    let a = if amin + bmin <= len {
        want.clamp(amin, len - bmin)
    } else if amin + bmin > 0 {
        (len as i64 * amin as i64 / (amin + bmin) as i64) as i32
    } else {
        want
    };
    if side_by_side {
        (Rect::new(r.x, r.y, a, r.h), Rect::new(r.x + a, r.y, r.w - a, r.h))
    } else {
        (Rect::new(r.x, r.y, r.w, a), Rect::new(r.x, r.y + a, r.w, r.h - a))
    }
}

/// Which edge of a tile the user dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// Minimum (w, h) of a window's tile; (0, 0) when unknown.
pub type Mins<'a> = &'a dyn Fn(isize) -> (i32, i32);

impl Node {
    fn leaves(&self, out: &mut Vec<isize>) {
        match self {
            Node::Leaf(h) => out.push(*h),
            Node::Split { a, b, .. } => {
                a.leaves(out);
                b.leaves(out);
            }
        }
    }

    /// Smallest (w, h) this subtree can be laid out in.
    fn min_size(&self, mins: Mins) -> (i32, i32) {
        match self {
            Node::Leaf(h) => mins(*h),
            Node::Split { side_by_side, a, b, .. } => {
                let (aw, ah) = a.min_size(mins);
                let (bw, bh) = b.min_size(mins);
                if *side_by_side {
                    (aw + bw, ah.max(bh))
                } else {
                    (aw.max(bw), ah + bh)
                }
            }
        }
    }

    fn children_rects(&self, r: Rect, mins: Mins) -> Option<(Rect, Rect)> {
        match self {
            Node::Leaf(_) => None,
            Node::Split { side_by_side, ratio, a, b } => {
                let (am, bm) = (a.min_size(mins), b.min_size(mins));
                let (amin, bmin) = if *side_by_side { (am.0, bm.0) } else { (am.1, bm.1) };
                Some(split_rect_min(r, *side_by_side, *ratio, amin, bmin))
            }
        }
    }

    fn layout(&self, r: Rect, mins: Mins, out: &mut Vec<(isize, Rect)>) {
        match self {
            Node::Leaf(h) => out.push((*h, r)),
            Node::Split { a, b, .. } => {
                let (ra, rb) = self.children_rects(r, mins).unwrap();
                a.layout(ra, mins, out);
                b.layout(rb, mins, out);
            }
        }
    }

    /// Moves the split boundary next to `h`'s `edge` to `pos` (deepest
    /// matching split wins). Returns true if a split was changed.
    fn set_edge(&mut self, h: isize, r: Rect, edge: Edge, pos: i32, mins: Mins) -> bool {
        let Some((ra, rb)) = self.children_rects(r, mins) else { return false };
        let Node::Split { side_by_side, ratio, a, b } = self else { return false };
        let in_a = a.contains(h);
        if !in_a && !b.contains(h) {
            return false;
        }
        // Prefer a deeper split adjacent to the same edge.
        let deeper = if in_a { a.set_edge(h, ra, edge, pos, mins) } else { b.set_edge(h, rb, edge, pos, mins) };
        if deeper {
            return true;
        }
        let matches = match edge {
            Edge::Right => *side_by_side && in_a,
            Edge::Left => *side_by_side && !in_a,
            Edge::Bottom => !*side_by_side && in_a,
            Edge::Top => !*side_by_side && !in_a,
        };
        if !matches {
            return false;
        }
        let (start, len) = if *side_by_side { (r.x, r.w) } else { (r.y, r.h) };
        if len <= 0 {
            return false;
        }
        *ratio = (2.0 * (pos - start) as f32 / len as f32).clamp(0.1, 1.9);
        true
    }

    /// Replaces leaf `target` with a split of (target, new).
    fn split_leaf(&mut self, target: isize, new: isize, rect: Rect, ratio: f32) -> bool {
        match self {
            Node::Leaf(h) if *h == target => {
                *self = Node::Split {
                    side_by_side: rect.w > rect.h,
                    ratio,
                    a: Box::new(Node::Leaf(target)),
                    b: Box::new(Node::Leaf(new)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { side_by_side, ratio: r, a, b } => {
                let (ra, rb) = split_rect(rect, *side_by_side, *r);
                a.split_leaf(target, new, ra, ratio) || b.split_leaf(target, new, rb, ratio)
            }
        }
    }

    /// Removes leaf `h`; its sibling takes the parent's place.
    fn remove(self, h: isize) -> Option<Node> {
        match self {
            Node::Leaf(x) if x == h => None,
            Node::Leaf(_) => Some(self),
            Node::Split { side_by_side, ratio, a, b } => match (a.remove(h), b.remove(h)) {
                (Some(a), Some(b)) => Some(Node::Split { side_by_side, ratio, a: Box::new(a), b: Box::new(b) }),
                (Some(n), None) | (None, Some(n)) => Some(n),
                (None, None) => None,
            },
        }
    }

    fn swap(&mut self, x: isize, y: isize) {
        match self {
            Node::Leaf(h) if *h == x => *h = y,
            Node::Leaf(h) if *h == y => *h = x,
            Node::Leaf(_) => {}
            Node::Split { a, b, .. } => {
                a.swap(x, y);
                b.swap(x, y);
            }
        }
    }

    /// The split directly containing leaf `h`, as (side_by_side, ratio, h is first child).
    fn parent_of(&mut self, h: isize) -> Option<(&mut bool, &mut f32, bool)> {
        match self {
            Node::Leaf(_) => None,
            Node::Split { side_by_side, ratio, a, b } => {
                let in_a = matches!(**a, Node::Leaf(x) if x == h);
                let in_b = matches!(**b, Node::Leaf(x) if x == h);
                if in_a || in_b {
                    return Some((side_by_side, ratio, in_a));
                }
                // Reborrow trick: search children without holding the parent fields.
                if a.contains(h) {
                    a.parent_of(h)
                } else {
                    b.parent_of(h)
                }
            }
        }
    }

    fn contains(&self, h: isize) -> bool {
        match self {
            Node::Leaf(x) => *x == h,
            Node::Split { a, b, .. } => a.contains(h) || b.contains(h),
        }
    }
}

impl Tree {
    pub fn contains(&self, h: isize) -> bool {
        self.root.as_ref().is_some_and(|r| r.contains(h))
    }

    pub fn leaves(&self) -> Vec<isize> {
        let mut v = Vec::new();
        if let Some(r) = &self.root {
            r.leaves(&mut v);
        }
        v
    }

    /// Inserts `new` by splitting `target` (or the last window if `target`
    /// is not in this tree). `area` is the layout area, used to pick the
    /// split direction from the target's current shape.
    pub fn insert(&mut self, new: isize, target: Option<isize>, area: Rect, ratio: f32) {
        if self.contains(new) {
            return;
        }
        let Some(root) = &mut self.root else {
            self.root = Some(Node::Leaf(new));
            return;
        };
        let target = target.filter(|t| root.contains(*t)).or_else(|| {
            let mut v = Vec::new();
            root.leaves(&mut v);
            v.last().copied()
        });
        if let Some(t) = target {
            root.split_leaf(t, new, area, ratio);
        }
    }

    pub fn remove(&mut self, h: isize) {
        if let Some(r) = self.root.take() {
            self.root = r.remove(h);
        }
    }

    /// Keeps exactly `windows` in the tree: drops the rest, inserts the
    /// missing ones by splitting `focus`.
    pub fn sync(&mut self, windows: &[isize], focus: Option<isize>, area: Rect, ratio: f32) {
        for h in self.leaves() {
            if !windows.contains(&h) {
                self.remove(h);
            }
        }
        for &h in windows {
            if !self.contains(h) {
                self.insert(h, focus, area, ratio);
            }
        }
    }

    pub fn swap(&mut self, x: isize, y: isize) {
        if let Some(r) = &mut self.root {
            if r.contains(x) && r.contains(y) {
                r.swap(x, y);
            }
        }
    }

    pub fn toggle_split(&mut self, h: isize) -> bool {
        match self.root.as_mut().and_then(|r| r.parent_of(h)) {
            Some((sbs, _, _)) => {
                *sbs = !*sbs;
                true
            }
            None => false,
        }
    }

    /// Grows (delta > 0) or shrinks the focused window's side of its split.
    pub fn resize(&mut self, h: isize, delta: f32, exact: bool) -> Option<f32> {
        let (_, ratio, first) = self.root.as_mut()?.parent_of(h)?;
        let d = if first { delta } else { -delta };
        *ratio = if exact { delta } else { *ratio + d }.clamp(0.1, 1.9);
        Some(*ratio)
    }

    #[cfg(test)]
    pub fn layout(&self, area: Rect) -> Vec<(isize, Rect)> {
        self.layout_min(area, &|_| (0, 0))
    }

    /// Layout that gives every window at least its minimum size where possible.
    pub fn layout_min(&self, area: Rect, mins: Mins) -> Vec<(isize, Rect)> {
        let mut out = Vec::new();
        if let Some(r) = &self.root {
            r.layout(area, mins, &mut out);
        }
        out
    }

    /// Total pixels by which windows fall short of their minimum size.
    pub fn deficit(&self, area: Rect, mins: Mins) -> i64 {
        self.layout_min(area, mins)
            .iter()
            .map(|(h, r)| {
                let (mw, mh) = mins(*h);
                ((mw - r.w).max(0) + (mh - r.h).max(0)) as i64
            })
            .sum()
    }

    fn split_count(n: &Node) -> usize {
        match n {
            Node::Leaf(_) => 0,
            Node::Split { a, b, .. } => 1 + Self::split_count(a) + Self::split_count(b),
        }
    }

    /// Flips the `i`-th split in pre-order; returns the remaining index.
    fn flip_nth(n: &mut Node, i: usize) -> Option<usize> {
        match n {
            Node::Leaf(_) => Some(i),
            Node::Split { side_by_side, a, b, .. } => {
                if i == 0 {
                    *side_by_side = !*side_by_side;
                    return None;
                }
                let rest = Self::flip_nth(a, i - 1)?;
                Self::flip_nth(b, rest)
            }
        }
    }

    /// If windows can't all get their minimum size, try flipping splits
    /// (side-by-side ↔ stacked) and keep any flip that reduces the shortfall.
    /// Returns the remaining shortfall (0 = everything fits).
    pub fn fit(&mut self, area: Rect, mins: Mins) -> i64 {
        let mut best = self.deficit(area, mins);
        let Some(root) = &self.root else { return best };
        let n = Self::split_count(root);
        for _ in 0..3 {
            if best == 0 {
                break;
            }
            let mut improved = false;
            for i in 0..n {
                Self::flip_nth(self.root.as_mut().unwrap(), i);
                let d = self.deficit(area, mins);
                if d < best {
                    best = d;
                    improved = true;
                } else {
                    Self::flip_nth(self.root.as_mut().unwrap(), i);
                }
            }
            if !improved {
                break;
            }
        }
        best
    }

    /// Drag-resize: move the split boundary on `h`'s `edge` to `pos`.
    pub fn set_edge(&mut self, h: isize, area: Rect, edge: Edge, pos: i32, mins: Mins) -> bool {
        match &mut self.root {
            Some(r) => r.set_edge(h, area, edge, pos, mins),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0, 0, 1600, 900);

    #[test]
    fn new_window_halves_the_focused_one() {
        let mut t = Tree::default();
        t.insert(1, None, AREA, 1.0);
        t.insert(2, Some(1), AREA, 1.0);
        // focus the left window (1) and open a third: it splits window 1 only
        t.insert(3, Some(1), AREA, 1.0);
        let l = t.layout(AREA);
        assert_eq!(l, vec![(1, Rect::new(0, 0, 800, 450)), (3, Rect::new(0, 450, 800, 450)), (2, Rect::new(800, 0, 800, 900))]);
    }

    #[test]
    fn closing_gives_space_to_sibling() {
        let mut t = Tree::default();
        t.sync(&[1, 2, 3], Some(1), AREA, 1.0);
        t.remove(1);
        let l = t.layout(AREA);
        assert_eq!(l.len(), 2);
        let total: i64 = l.iter().map(|(_, r)| r.w as i64 * r.h as i64).sum();
        assert_eq!(total, 1600 * 900);
    }

    #[test]
    fn minimum_sizes_are_respected() {
        let mut t = Tree::default();
        t.sync(&[1, 2, 3, 4], Some(1), AREA, 1.0);
        // Window 4 needs 1000x700 — more than an even split would give it.
        let mins = |h: isize| if h == 4 { (1000, 700) } else { (200, 150) };
        let l = t.layout_min(AREA, &mins);
        let r4 = l.iter().find(|(h, _)| *h == 4).unwrap().1;
        assert!(r4.w >= 1000 && r4.h >= 700, "{r4:?}");
        let total: i64 = l.iter().map(|(_, r)| r.w as i64 * r.h as i64).sum();
        assert_eq!(total, 1600 * 900);
    }

    #[test]
    fn flips_splits_to_fit_minimums() {
        // Three windows: a fixed-split arrangement stacks 2 over 3, but 2 and 3
        // are both too tall to stack — side by side they fit.
        let mut t = Tree::default();
        t.sync(&[1, 2], None, AREA, 1.0);
        t.insert(3, Some(2), Rect::new(800, 0, 800, 900), 1.0);
        let mins = |h: isize| match h {
            2 => (500, 600),
            3 => (450, 600),
            _ => (200, 200),
        };
        assert!(t.deficit(AREA, &mins) > 0);
        assert_eq!(t.fit(AREA, &mins), 0);
    }

    #[test]
    fn dragging_an_edge_moves_the_split() {
        let mut t = Tree::default();
        t.sync(&[1, 2], None, AREA, 1.0);
        assert!(t.set_edge(1, AREA, Edge::Right, 1200, &|_| (0, 0)));
        assert_eq!(t.layout(AREA)[0].1, Rect::new(0, 0, 1200, 900));
        assert!(t.set_edge(2, AREA, Edge::Left, 400, &|_| (0, 0)));
        assert_eq!(t.layout(AREA)[1].1, Rect::new(400, 0, 1200, 900));
        // No vertical split exists, so dragging a bottom edge changes nothing.
        assert!(!t.set_edge(1, AREA, Edge::Bottom, 500, &|_| (0, 0)));
    }

    #[test]
    fn toggle_resize_swap() {
        let mut t = Tree::default();
        t.sync(&[1, 2], None, AREA, 1.0);
        assert!(t.toggle_split(2));
        assert_eq!(t.layout(AREA)[1].1, Rect::new(0, 450, 1600, 450));
        assert_eq!(t.resize(1, 0.2, false), Some(1.2));
        assert_eq!(t.layout(AREA)[0].1.h, 540);
        t.swap(1, 2);
        assert_eq!(t.leaves(), vec![2, 1]);
    }
}

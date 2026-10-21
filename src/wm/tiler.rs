//! Turns layouts into `SetWindowPos` calls.

use std::time::{Duration, Instant};

use crate::config::types::LayoutKind;
use crate::layout::{self, dwindle, master, Rect};
use crate::state::State;
use crate::win;

impl State {
    /// Shown on the current virtual desktop, not minimised.
    ///
    /// The shell cloaks every window that belongs to another virtual
    /// desktop, so the (in-process, sub-microsecond) DWM cloak check answers
    /// "is it on this desktop" without a cross-process
    /// IsWindowOnCurrentVirtualDesktop round-trip per window per retile.
    pub fn is_tile_visible(&self, h: isize) -> bool {
        win::is_window(h) && win::is_visible(h) && !win::is_iconic(h) && !win::is_cloaked(h)
    }

    /// Recomputes and applies the layout on every monitor.
    pub fn retile(&mut self) {
        // Monitors are cached; refreshed on reload / WM_DISPLAYCHANGE / work-area change.
        if self.monitors.is_empty() {
            self.monitors = win::monitors();
        }
        let t0 = Instant::now();
        if self.monitors.is_empty() {
            return;
        }
        let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); self.monitors.len()];
        let mut fullscreen: Vec<usize> = Vec::new();
        for (i, c) in self.clients.iter().enumerate() {
            if !self.is_tile_visible(c.hwnd) {
                continue;
            }
            if c.fullscreen.is_some() {
                fullscreen.push(i);
                continue;
            }
            // A window being dragged stays in the layout (so it keeps its place in
            // the split tree); it just isn't moved until the drag ends.
            if c.floating {
                continue;
            }
            if win::is_zoomed(c.hwnd) {
                // Tiled windows are never left maximised (see State::manage).
                win::restore(c.hwnd);
            }
            let mi = self.monitor_index_of(c.hwnd);
            if self.config.monitor_rule(mi).is_some_and(|r| r.disabled) {
                continue;
            }
            buckets[mi].push(i);
        }

        let kind = self.layout_for_current();
        let g = &self.config.general;
        let (gaps_in, gaps_out) = (g.gaps_in, g.gaps_out);
        let mut placements: Vec<(usize, Rect)> = Vec::new();
        for (mi, idxs) in buckets.iter().enumerate() {
            if idxs.is_empty() {
                continue;
            }
            let work = self.work_area(&self.monitors[mi]);
            let n = idxs.len();
            if kind == LayoutKind::Dwindle && !self.config.dwindle.spiral {
                // Split tree per (workspace, monitor): new windows halve the
                // window that was focused when they opened.
                let ws = idxs.iter().find_map(|&i| self.clients[i].workspace).unwrap_or(0);
                let hwnds: Vec<isize> = idxs.iter().map(|&i| self.clients[i].hwnd).collect();
                let outer = layout::gap_outer(work, gaps_in, gaps_out);
                let ratio = self.config.dwindle.default_split_ratio;
                let focus = self.last_tiled_focus;
                let mins = self.min_sizes();
                let mins_fn = |w: isize| mins.get(&w).copied().unwrap_or((0, 0));
                let mut hwnds = hwnds;
                let mut overflow = Vec::new();
                let tree = self.trees.entry((ws, mi)).or_default();
                loop {
                    tree.sync(&hwnds, focus, outer, ratio);
                    // Rearrange splits so minimum sizes fit; if no arrangement
                    // can, the newest window floats until there is room.
                    if tree.fit(outer, &mins_fn) == 0 || hwnds.len() <= 1 {
                        break;
                    }
                    let victim = hwnds.pop().unwrap();
                    tree.remove(victim);
                    overflow.push(victim);
                }
                let layout = tree.layout_min(outer, &mins_fn);
                for v in overflow {
                    if let Some(i) = self.index_of(v) {
                        let c = &mut self.clients[i];
                        c.floating = true;
                        c.auto_floated = true;
                        c.target = None;
                        crate::log_info!("{} doesn't fit next to the other windows; floating it until there is room", c.exe);
                        let area = work;
                        let r = win::visual_rect(v).unwrap_or(area);
                        let (w, h) = (r.w.min(area.w), r.h.min(area.h));
                        let centered = Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - h) / 2, w, h);
                        win::move_window_raw(v, win::outer_for_visual(v, centered));
                        win::bring_to_top(v);
                    }
                }
                for (h, r) in layout {
                    if let Some(&i) = idxs.iter().find(|&&i| self.clients[i].hwnd == h) {
                        placements.push((i, layout::gap_inner(r, gaps_in)));
                    }
                }
                continue;
            }
            let rects = layout::with_gaps(work, gaps_in, gaps_out, |area| match kind {
                LayoutKind::Dwindle => {
                    dwindle::layout(area, n, self.split_ratio, self.config.dwindle.spiral, self.split_flip)
                }
                LayoutKind::Master => master::layout(area, n, self.mfact, self.nmaster, self.config.master.orientation),
            });
            placements.extend(idxs.iter().copied().zip(rects));
        }
        for i in fullscreen {
            let h = self.clients[i].hwnd;
            let m = self.monitor_of(h);
            // 0 = true fullscreen (covers the bar); 1 = maximise with gaps.
            let gaps = self.config.general.gaps_out;
            let r = if self.clients[i].fullscreen == Some(0) { m.rect } else { self.work_area(&m).inset(gaps, gaps, gaps, gaps) };
            placements.push((i, r));
            win::bring_to_top(h);
        }
        if let Some(d) = self.dragging {
            placements.retain(|(i, _)| self.clients[*i].hwnd != d);
        }
        let count = placements.len();
        for (i, r) in placements {
            self.place(i, r);
        }
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if ms > 5.0 {
            crate::log_warn!("retile of {count} windows took {ms:.2}ms");
        }
    }

    /// Moves client `i` so its visible frame is exactly `visual`.
    fn place(&mut self, i: usize, visual: Rect) {
        let h = self.clients[i].hwnd;
        let outer = win::outer_for_visual(h, visual);
        let current = win::window_rect(h);
        if self.clients[i].target == Some(visual)
            && (current == Some(outer) || self.animator.target(h) == Some(outer))
        {
            return;
        }
        let fresh = std::mem::replace(&mut self.clients[i].fresh, false);
        self.clients[i].target = Some(visual);

        let anim_name = if fresh { "windowsIn" } else { "windowsMove" };
        // Live drag-resize must track the mouse exactly — no easing.
        let anim = if self.resizing { None } else { self.config.animations.get(anim_name).cloned() };
        match (anim, current) {
            (Some(a), Some(from)) if from != outer => {
                let from = if fresh && a.style.starts_with("popin") {
                    let pct = a
                        .style
                        .split_whitespace()
                        .nth(1)
                        .and_then(|p| p.trim_end_matches('%').parse::<f32>().ok())
                        .unwrap_or(80.0);
                    outer.scaled_about_center(pct / 100.0)
                } else {
                    from
                };
                let curve = self.curves.get(&a.curve).or_else(|| self.curves.get("default")).cloned().unwrap();
                let duration = Duration::from_secs_f32(a.speed * 0.1);
                self.animator.animate(h, from, outer, duration, curve);
            }
            _ => win::move_window_raw(h, outer),
        }
    }
}

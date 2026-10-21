//! Window-move animations on a dedicated thread.
//!
//! The ticker paces itself with `DwmFlush` (blocks until the next DWM
//! composition, i.e. the display refresh) and falls back to sleeping one
//! refresh interval. Each frame calls `SetWindowPos` with
//! `SWP_ASYNCWINDOWPOS`, so a hung target app cannot stall the ticker.

pub mod bezier;

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub use bezier::CubicBezier;

use crate::layout::Rect;

struct Job {
    hwnd: isize,
    from: Rect,
    to: Rect,
    start: Instant,
    duration: Duration,
    curve: Arc<CubicBezier>,
}

impl Job {
    fn rect_at(&self, now: Instant) -> (Rect, bool) {
        let elapsed = now.saturating_duration_since(self.start);
        if elapsed >= self.duration {
            return (self.to, true);
        }
        let p = self.curve.ease(elapsed.as_secs_f32() / self.duration.as_secs_f32());
        let lerp = |a: i32, b: i32| a + ((b - a) as f32 * p).round() as i32;
        let r = Rect {
            x: lerp(self.from.x, self.to.x),
            y: lerp(self.from.y, self.to.y),
            w: lerp(self.from.w, self.to.w).max(1),
            h: lerp(self.from.h, self.to.h).max(1),
        };
        (r, false)
    }
}

#[derive(Default)]
struct Shared {
    jobs: Vec<Job>,
    shutdown: bool,
}

pub struct Animator {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Animator {
    pub fn spawn(refresh_hz: u32) -> Self {
        let shared = Arc::new((Mutex::new(Shared::default()), Condvar::new()));
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("deadbeef-anim".into())
            .spawn(move || ticker(s, refresh_hz.max(30)))
            .expect("spawn animation thread");
        Self { shared, thread: Some(thread) }
    }

    /// Animates a window (outer window rect, frame included) from `from` to
    /// `to`. If the window is already animating, the new animation starts
    /// from its current interpolated position.
    pub fn animate(&self, hwnd: isize, from: Rect, to: Rect, duration: Duration, curve: Arc<CubicBezier>) {
        let (lock, cv) = &*self.shared;
        let mut s = lock.lock().unwrap();
        let now = Instant::now();
        let mut from = from;
        if let Some(i) = s.jobs.iter().position(|j| j.hwnd == hwnd) {
            from = s.jobs[i].rect_at(now).0;
            s.jobs.swap_remove(i);
        }
        s.jobs.push(Job { hwnd, from, to, start: now, duration, curve });
        cv.notify_one();
    }

    /// Stops animating `hwnd` (e.g. it was destroyed or is now dragged).
    pub fn cancel(&self, hwnd: isize) {
        let mut s = self.shared.0.lock().unwrap();
        s.jobs.retain(|j| j.hwnd != hwnd);
    }

    /// Final target of a running animation, if any.
    pub fn target(&self, hwnd: isize) -> Option<Rect> {
        let s = self.shared.0.lock().unwrap();
        s.jobs.iter().find(|j| j.hwnd == hwnd).map(|j| j.to)
    }
}

impl Drop for Animator {
    fn drop(&mut self) {
        {
            let mut s = self.shared.0.lock().unwrap();
            s.shutdown = true;
            self.shared.1.notify_one();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn ticker(shared: Arc<(Mutex<Shared>, Condvar)>, refresh_hz: u32) {
    let (lock, cv) = &*shared;
    let frame = Duration::from_secs_f64(1.0 / refresh_hz as f64);
    let mut frame_moves: Vec<(isize, Rect)> = Vec::new();
    loop {
        {
            let mut s = lock.lock().unwrap();
            while s.jobs.is_empty() && !s.shutdown {
                s = cv.wait(s).unwrap();
            }
            if s.shutdown {
                return;
            }
            let now = Instant::now();
            frame_moves.clear();
            s.jobs.retain(|j| {
                let (r, done) = j.rect_at(now);
                frame_moves.push((j.hwnd, r));
                !done
            });
        }
        // Lock released: SetWindowPos never runs while holding it.
        for &(hwnd, r) in &frame_moves {
            crate::win::move_window_raw(hwnd, r);
        }
        let t = Instant::now();
        if !crate::win::dwm_flush() || t.elapsed() < Duration::from_micros(500) {
            // DWM composition unavailable or returned immediately: pace manually.
            std::thread::sleep(frame.saturating_sub(t.elapsed()));
        }
    }
}

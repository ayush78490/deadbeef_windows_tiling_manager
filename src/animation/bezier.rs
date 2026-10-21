//! CSS-style cubic bezier easing with P0 = (0,0) and P3 = (1,1).
//!
//! `x(t)` is inverted with Newton-Raphson (bisection fallback) once per LUT
//! entry at construction; evaluation is a 256-entry table lookup with linear
//! interpolation, so per-frame cost is constant.

pub const LUT_SIZE: usize = 256;

#[derive(Debug, Clone)]
pub struct CubicBezier {
    pub p1: (f32, f32),
    pub p2: (f32, f32),
    lut: Box<[f32; LUT_SIZE]>,
}

impl CubicBezier {
    pub fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        let mut b = Self { p1: (x1.clamp(0.0, 1.0), y1), p2: (x2.clamp(0.0, 1.0), y2), lut: Box::new([0.0; LUT_SIZE]) };
        for i in 0..LUT_SIZE {
            let x = i as f32 / (LUT_SIZE - 1) as f32;
            b.lut[i] = b.sample_y(b.solve_t(x));
        }
        b
    }

    /// Hyprland's built-in `default` curve.
    pub fn default_curve() -> Self {
        Self::new(0.0, 0.75, 0.15, 1.0)
    }

    pub fn from_points(p: [f32; 4]) -> Self {
        Self::new(p[0], p[1], p[2], p[3])
    }

    // Polynomial coefficients for one axis: B(t) = ((a t + b) t + c) t.
    fn coeffs(p1: f32, p2: f32) -> (f32, f32, f32) {
        let c = 3.0 * p1;
        let b = 3.0 * (p2 - p1) - c;
        let a = 1.0 - c - b;
        (a, b, c)
    }

    fn sample_x(&self, t: f32) -> f32 {
        let (a, b, c) = Self::coeffs(self.p1.0, self.p2.0);
        ((a * t + b) * t + c) * t
    }

    fn sample_y(&self, t: f32) -> f32 {
        let (a, b, c) = Self::coeffs(self.p1.1, self.p2.1);
        ((a * t + b) * t + c) * t
    }

    fn sample_dx(&self, t: f32) -> f32 {
        let (a, b, c) = Self::coeffs(self.p1.0, self.p2.0);
        (3.0 * a * t + 2.0 * b) * t + c
    }

    /// Finds t such that x(t) = x.
    fn solve_t(&self, x: f32) -> f32 {
        let mut t = x;
        for _ in 0..8 {
            let err = self.sample_x(t) - x;
            if err.abs() < 1e-6 {
                return t;
            }
            let d = self.sample_dx(t);
            if d.abs() < 1e-6 {
                break;
            }
            t -= err / d;
        }
        // Newton stalled (flat derivative) — fall back to bisection.
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        t = x;
        for _ in 0..32 {
            let v = self.sample_x(t);
            if (v - x).abs() < 1e-6 {
                break;
            }
            if v < x {
                lo = t;
            } else {
                hi = t;
            }
            t = (lo + hi) * 0.5;
        }
        t
    }

    /// Eased progress for linear progress `x` in [0, 1]. May overshoot 1.
    pub fn ease(&self, x: f32) -> f32 {
        if x <= 0.0 {
            return 0.0;
        }
        if x >= 1.0 {
            return 1.0;
        }
        let pos = x * (LUT_SIZE - 1) as f32;
        let i = pos as usize;
        let frac = pos - i as f32;
        let a = self.lut[i];
        let b = self.lut[(i + 1).min(LUT_SIZE - 1)];
        a + (b - a) * frac
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_is_identity() {
        let b = CubicBezier::new(0.25, 0.25, 0.75, 0.75);
        for i in 0..=20 {
            let x = i as f32 / 20.0;
            assert!((b.ease(x) - x).abs() < 1e-3, "x={x} got {}", b.ease(x));
        }
    }

    #[test]
    fn endpoints_and_monotone_ease_out() {
        let b = CubicBezier::new(0.0, 0.0, 0.58, 1.0);
        assert_eq!(b.ease(0.0), 0.0);
        assert_eq!(b.ease(1.0), 1.0);
        let mut prev = 0.0;
        for i in 1..100 {
            let y = b.ease(i as f32 / 100.0);
            assert!(y >= prev);
            prev = y;
        }
        // ease-out: ahead of linear at the midpoint
        assert!(b.ease(0.5) > 0.5);
    }

    #[test]
    fn overshoot() {
        let b = CubicBezier::new(0.05, 0.9, 0.1, 1.1);
        assert!((0..100).map(|i| b.ease(i as f32 / 100.0)).fold(0.0f32, f32::max) > 1.0);
    }

    #[test]
    fn matches_direct_solution() {
        let b = CubicBezier::new(0.42, 0.0, 0.58, 1.0);
        for i in 0..=50 {
            let x = i as f32 / 50.0;
            let direct = b.sample_y(b.solve_t(x));
            assert!((b.ease(x) - direct).abs() < 2e-3);
        }
    }
}

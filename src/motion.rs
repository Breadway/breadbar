//! BAR-31: exact CSS timing curves for the spotlight pill, surtitle and
//! stage, driven on the GTK frame clock.
//!
//! GTK CSS can transition `opacity` and colours, but not a layer-shell
//! surface's margin or a widget's position, so the mockup's slides
//! (`transform .42s var(--spring)` on the pill, `.35s` on the surtitle,
//! `.45s` on the stage column) are interpolated here instead. Unlike
//! `bread_theme::anim::spring_to`, which approximates one curve and clamps
//! its overshoot, this solves the actual `cubic-bezier()` the mockup names,
//! overshoot included, so a slide lands exactly the way the browser draws it.

use gtk4::glib::ControlFlow;
use gtk4::prelude::*;

/// A CSS `cubic-bezier(x1, y1, x2, y2)` timing function.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Curve(pub f64, pub f64, pub f64, pub f64);

/// The mockup's `--spring: cubic-bezier(.22,1.2,.36,1)`.
pub const SPRING: Curve = Curve(0.22, 1.2, 0.36, 1.0);
/// CSS `ease`, the default when a transition names no curve.
pub const EASE: Curve = Curve(0.25, 0.1, 0.25, 1.0);

impl Curve {
    /// The eased progress at time fraction `t` (0..=1). Solves the curve's
    /// x(s) = t for the bezier parameter s by bisection (x is monotonic for
    /// any valid CSS curve, since x1 and x2 are within 0..=1), then returns
    /// y(s). Can exceed 1.0 for an overshooting curve such as [`SPRING`].
    pub fn at(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        if t <= 0.0 {
            return 0.0;
        }
        if t >= 1.0 {
            return 1.0;
        }
        let Curve(x1, y1, x2, y2) = self;
        let bez = |a: f64, b: f64, s: f64| {
            let r = 1.0 - s;
            3.0 * r * r * s * a + 3.0 * r * s * s * b + s * s * s
        };
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for _ in 0..40 {
            let mid = (lo + hi) / 2.0;
            if bez(x1, x2, mid) < t {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        bez(y1, y2, (lo + hi) / 2.0)
    }
}

/// Linear interpolation from `from` to `to` at eased progress `p`.
pub fn lerp(from: f64, to: f64, p: f64) -> f64 {
    from + (to - from) * p
}

/// Whether the user wants motion at all: GTK's own `gtk-enable-animations`
/// (the desktop equivalent of the mockup's
/// `@media (prefers-reduced-motion:reduce)`). GTK already drops CSS
/// transitions when this is off; frame-clock animations have to check it
/// themselves.
pub fn animations_enabled() -> bool {
    gtk4::Settings::default().is_none_or(|s| s.is_gtk_enable_animations())
}

/// Runs `on_frame(eased)` on `widget`'s frame clock for `duration_ms`,
/// after an initial `delay_ms`, with `curve` easing, finishing with exactly
/// `on_frame(1.0)`. With animations disabled it jumps straight to 1.0.
/// Returns the tick id so a caller can cancel an in-flight run.
pub fn animate(
    widget: &impl IsA<gtk4::Widget>,
    duration_ms: f64,
    delay_ms: f64,
    curve: Curve,
    on_frame: impl Fn(f64) + 'static,
) -> Option<gtk4::TickCallbackId> {
    if !animations_enabled() || duration_ms <= 0.0 {
        on_frame(1.0);
        return None;
    }
    let started = std::time::Instant::now();
    Some(widget.add_tick_callback(move |_, _| {
        let elapsed = started.elapsed().as_secs_f64() * 1000.0 - delay_ms;
        if elapsed >= duration_ms {
            on_frame(1.0);
            return ControlFlow::Break;
        }
        on_frame(curve.at((elapsed / duration_ms).max(0.0)));
        ControlFlow::Continue
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_exact() {
        for c in [SPRING, EASE] {
            assert_eq!(c.at(0.0), 0.0);
            assert_eq!(c.at(1.0), 1.0);
        }
    }

    #[test]
    fn spring_overshoots_like_the_css_curve() {
        // y1 = 1.2 pulls the curve above 1 before it settles.
        let peak = (1..100)
            .map(|i| SPRING.at(i as f64 / 100.0))
            .fold(0.0_f64, f64::max);
        assert!(peak > 1.0 && peak < 1.1, "peak {peak}");
    }

    #[test]
    fn ease_is_monotonic_and_within_range() {
        let mut prev = 0.0;
        for i in 0..=100 {
            let v = EASE.at(i as f64 / 100.0);
            assert!(v >= prev - 1e-9 && (0.0..=1.0).contains(&v));
            prev = v;
        }
    }

    #[test]
    fn linear_curve_is_identity() {
        let linear = Curve(0.0, 0.0, 1.0, 1.0);
        for i in 0..=10 {
            let t = i as f64 / 10.0;
            assert!((linear.at(t) - t).abs() < 1e-6);
        }
    }

    #[test]
    fn ease_matches_known_css_sample() {
        // Chromium's `ease` at t = 0.5 is about 0.8024.
        assert!((EASE.at(0.5) - 0.8024).abs() < 0.002, "{}", EASE.at(0.5));
    }

    #[test]
    fn lerp_basics() {
        assert_eq!(lerp(10.0, 20.0, 0.5), 15.0);
        assert_eq!(lerp(10.0, 20.0, 1.2), 22.0);
    }
}

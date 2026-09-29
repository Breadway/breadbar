//! BAR-27 phase 1: the spotlight pill's auto-hide state machine.
//!
//! Mirrors `bos-ui-demos/proposed/spotlight-stage.html`'s own `wickState()`:
//! the pill is shown whenever auto-hide is off, the pointer is over the pill
//! or its hot-zone, the battery is low, or a timed "peek" (triggered by a
//! notification, a volume/brightness change, or a workspace switch) hasn't
//! expired yet. Kept gtk-free and pure (an `Instant`-driven [`State`] plus a
//! [`State::visible`] query) so the decision itself is unit-testable without
//! a display; `main.rs` owns the GTK side (the tick-animated layer-shell
//! margin, the hover controllers, the timers that call back into this).

use std::time::Instant;

/// The auto-hide decision inputs, sampled at some instant. Everything
/// breadbar's own timers/controllers feed into [`State`] eventually reduces
/// to this shape, which is why `visible` takes it as a value rather than
/// `&self` touching wall-clock time directly: a test can hand it whatever
/// `now` it likes.
#[derive(Debug, Clone, Copy, Default)]
pub struct State {
    /// Auto-hide is off: always visible, full stop (the theme's own
    /// `bar.autohide.enabled = false`, or the OFF end of a future runtime
    /// toggle).
    pub enabled: bool,
    /// The pointer is over the pill itself or the hot-zone surface.
    pub hovering: bool,
    /// The battery is at/under the low-battery threshold
    /// (`bar::stats::is_low_battery`).
    pub low_battery: bool,
    /// A timed peek (notification/level-change/workspace-switch) is holding
    /// the pill down until this instant. `None` when no peek is pending.
    pub hold_until: Option<Instant>,
    /// The stage (BAR-30 phase 2) is open. Mirrors the mockup's own
    /// `isOpen()` term in `wickState()`: the pill stays down for as long as
    /// the stage is up, not just for a timed peek, since the stage's search
    /// field sits directly under it.
    pub open: bool,
}

impl State {
    /// Whether the pill should be shown at `now`. Matches the mockup's
    /// `wickState()`: `!autoHide || hovering || isOpen() || lowBat ||
    /// Date.now()<holdUntil`.
    pub fn visible(&self, now: Instant) -> bool {
        !self.enabled
            || self.hovering
            || self.open
            || self.low_battery
            || self.hold_until.is_some_and(|t| now < t)
    }
}

/// BAR-31: transparent room the pill's window keeps around the visible pill
/// so its drop shadow (`box-shadow: 0 6px 24px`) renders instead of being
/// cut off at the surface edge: 24px of blur each side, reaching 18px above
/// (24 - 6 offset) and 30px below (24 + 6). base.css draws the pill on the
/// window's inner box with exactly these margins.
pub const SHADOW_TOP: i32 = 18;
pub const SHADOW_SIDE: i32 = 24;
pub const SHADOW_BOTTOM: i32 = 30;

/// How far the pill travels up when it tucks (`.wick.tucked
/// {transform:translate(-50%,-54px)}`).
pub const TUCK_TRAVEL: i32 = 54;

/// The layer-shell top margin for the shown and tucked pill, given the
/// theme's own `margin.top` (where the visible pill's top edge sits).
pub fn margins(theme_top: i32) -> (i32, i32) {
    let shown = theme_top - SHADOW_TOP;
    (shown, shown - TUCK_TRAVEL)
}

/// The part of a `width` x `height` pill surface that takes input: the pill
/// itself, not the transparent shadow margin around it.
pub fn input_rect(width: i32, height: i32) -> (i32, i32, i32, i32) {
    (
        SHADOW_SIDE,
        SHADOW_TOP,
        (width - 2 * SHADOW_SIDE).max(0),
        (height - SHADOW_TOP - SHADOW_BOTTOM).max(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn base() -> State {
        State {
            enabled: true,
            hovering: false,
            low_battery: false,
            hold_until: None,
            open: false,
        }
    }

    #[test]
    fn disabled_is_always_visible_regardless_of_everything_else() {
        let s = State {
            enabled: false,
            ..base()
        };
        assert!(s.visible(Instant::now()));
    }

    #[test]
    fn enabled_with_nothing_else_is_tucked() {
        let s = base();
        assert!(!s.visible(Instant::now()));
    }

    #[test]
    fn hovering_is_visible() {
        let s = State {
            hovering: true,
            ..base()
        };
        assert!(s.visible(Instant::now()));
    }

    #[test]
    fn low_battery_is_visible() {
        let s = State {
            low_battery: true,
            ..base()
        };
        assert!(s.visible(Instant::now()));
    }

    #[test]
    fn unexpired_peek_is_visible() {
        let now = Instant::now();
        let s = State {
            hold_until: Some(now + Duration::from_millis(500)),
            ..base()
        };
        assert!(s.visible(now));
    }

    #[test]
    fn expired_peek_is_tucked() {
        let now = Instant::now();
        let s = State {
            hold_until: Some(now - Duration::from_millis(1)),
            ..base()
        };
        assert!(!s.visible(now));
    }

    #[test]
    fn peek_expiring_exactly_now_is_tucked() {
        // `now < t`, not `<=`: the instant a hold expires, it's tucked.
        let now = Instant::now();
        let s = State {
            hold_until: Some(now),
            ..base()
        };
        assert!(!s.visible(now));
    }

    #[test]
    fn open_stage_is_visible() {
        let s = State {
            open: true,
            ..base()
        };
        assert!(s.visible(Instant::now()));
    }

    #[test]
    fn open_stage_overrides_an_expired_peek() {
        let now = Instant::now();
        let s = State {
            open: true,
            hold_until: Some(now - Duration::from_secs(10)),
            ..base()
        };
        assert!(s.visible(now));
    }

    #[test]
    fn low_battery_keeps_it_down_even_after_a_peek_expires() {
        let now = Instant::now();
        let s = State {
            low_battery: true,
            hold_until: Some(now - Duration::from_secs(10)),
            ..base()
        };
        assert!(s.visible(now));
    }

    #[test]
    fn shown_pill_edge_sits_at_the_theme_margin() {
        let (shown, tucked) = margins(10);
        assert_eq!(shown + SHADOW_TOP, 10);
        assert_eq!(shown - tucked, TUCK_TRAVEL);
    }

    #[test]
    fn input_rect_is_the_pill_only() {
        // A 300px pill, 30px tall, inside its shadow margin.
        let w = 300 + 2 * SHADOW_SIDE;
        let h = 30 + SHADOW_TOP + SHADOW_BOTTOM;
        assert_eq!(input_rect(w, h), (SHADOW_SIDE, SHADOW_TOP, 300, 30));
        assert_eq!(input_rect(0, 0), (SHADOW_SIDE, SHADOW_TOP, 0, 0));
    }
}

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use futures_lite::StreamExt;
use gtk4::glib::ControlFlow;
use gtk4::prelude::*;
use hyprland::{
    data::{Monitors, Workspaces},
    event_listener::{Event, EventStream},
    prelude::*,
    shared::WorkspaceId,
};
use relm4::ComponentSender;

use crate::AppInput;

/// Stock Hyprland accepts `hyprctl dispatch workspace N`. Lua-config
/// Hyprland (BOS) rewrites that as `hl.dispatch(workspace N)`, which is
/// a syntax error — the working form is `hl.dsp.focus({workspace=N})`.
async fn switch_workspace(id: hyprland::shared::WorkspaceId) {
    let arg = id.to_string();
    let stock = tokio::process::Command::new("hyprctl")
        .args(["dispatch", "workspace", &arg])
        .output()
        .await;
    if let Ok(o) = &stock {
        let err = String::from_utf8_lossy(&o.stderr);
        let out = String::from_utf8_lossy(&o.stdout);
        if o.status.success() && !err.contains("hl.dispatch") && !out.contains("hl.dispatch") {
            return;
        }
    }
    let expr = format!("hl.dispatch(hl.dsp.focus({{workspace={arg}}}))");
    let lua = tokio::process::Command::new("hyprctl")
        .args(["eval", &expr])
        .output()
        .await;
    match lua {
        Ok(o) if o.status.success() => {}
        Ok(o) => eprintln!(
            "breadbar: workspace {arg}: {}",
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(e) => eprintln!("breadbar: workspace {arg}: {e}"),
    }
}

/// Stretch to the old→new span, then snap onto the destination — CSS
/// transitions cannot widen a pill across two buttons, so the trail's
/// Fixed allocation is interpolated on the frame clock instead.
const STRETCH_MS: f64 = 220.0;
const SNAP_MS: f64 = 380.0;

/// Full workspace + per-monitor active snapshot. Each bar filters this to
/// its own output so a second display does not inherit the laptop's set.
async fn sync_state(sender: &ComponentSender<crate::App>) {
    let workspaces = Workspaces::get_async()
        .await
        .map(|w| w.to_vec())
        .unwrap_or_default();
    let mut actives = std::collections::HashMap::new();
    if let Ok(mons) = Monitors::get_async().await {
        for m in mons {
            if !m.disabled {
                actives.insert(m.name, m.active_workspace.id);
            }
        }
    }
    sender.input(AppInput::WorkspaceSync {
        workspaces,
        actives,
    });
}

pub fn spawn_watcher(sender: ComponentSender<crate::App>) {
    relm4::spawn(async move {
        sync_state(&sender).await;

        // Hyprland's IPC event socket can drop out from under us — a
        // Hyprland restart/reload, or just a transient hiccup — at which
        // point `stream.next()` yields `None` (or an `Err`, also excluded
        // by this `while let Some(Ok(..))` pattern). That used to just fall
        // through and end this whole task permanently, freezing every
        // workspace button for the rest of the bar's life. Reconnect with a
        // capped exponential backoff instead of giving up.
        let mut backoff = std::time::Duration::from_millis(500);
        const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);

        loop {
            let mut stream = EventStream::new();
            while let Some(Ok(event)) = stream.next().await {
                backoff = std::time::Duration::from_millis(500);
                match event {
                    Event::WorkspaceChanged(_)
                    | Event::WorkspaceAdded(_)
                    | Event::WorkspaceDeleted(_) => {
                        sync_state(&sender).await;
                    }
                    Event::MonitorAdded(data) => {
                        sender.input(AppInput::MonitorAdded(data.name));
                        sync_state(&sender).await;
                    }
                    Event::MonitorRemoved(name) => {
                        sender.input(AppInput::MonitorRemoved(name));
                        sync_state(&sender).await;
                    }
                    Event::ActiveWindowChanged(_) => {
                        sender.input(AppInput::DismissPanels);
                    }
                    _ => {}
                }
            }

            eprintln!(
                "breadbar: Hyprland event stream ended (restart/reload/IPC hiccup); \
                 reconnecting in {:?}",
                backoff
            );
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
            sync_state(&sender).await;
        }
    });
}

pub fn make_button(
    id: WorkspaceId,
    name: &str,
    active: WorkspaceId,
    occupied: bool,
) -> gtk4::Button {
    let btn = gtk4::Button::with_label(name);
    btn.add_css_class("workspace-btn");
    if occupied {
        btn.add_css_class("occupied");
    }
    if id == active {
        btn.add_css_class("active");
    }
    btn.set_valign(gtk4::Align::Center);
    btn.set_halign(gtk4::Align::Center);
    btn.set_vexpand(false);
    btn.set_hexpand(false);
    // `crate::theme::approved_chip_height`, not `tokens().chip_height()`:
    // the latter is the stale pre-demo `breadbar::CHIP_HEIGHT` token (32
    // for this Trail/Pill style's theme) and, as a hard `set_size_request`
    // minimum, would out-rank the CSS `min-height` the demo actually wants
    // (26px Trail / 22px Pill) — see that function's doc comment.
    let style = crate::theme::shell_theme().modules().workspaces.style;
    btn.set_size_request(-1, crate::theme::approved_chip_height(style) as i32);
    if let Some(child) = btn.child() {
        child.set_halign(gtk4::Align::Center);
        child.set_valign(gtk4::Align::Center);
    }
    btn.connect_clicked(move |_| {
        relm4::spawn(async move {
            switch_workspace(id).await;
        });
    });
    btn
}

/// `style = "dots"` (theme 04/spotlight): a label-less pill whose WIDTH
/// encodes `windows` (0/1/2/3-or-more open). Distinct from [`make_button`]
/// (Trail/Pill) rather than a variant of it because dots carry no text at
/// all (`04-spotlight.html`'s `.dots button` has no label); reusing
/// `Button::with_label("")` would still measure/lay out an empty label box
/// that a genuinely childless button doesn't. Width is a hard
/// `set_size_request` snap, not animated — GTK CSS min-width transitions
/// don't participate in a directly-set size request the way an opacity/
/// background-color transition does, and the plan only calls out the
/// capsule's own expand/collapse as worth the `anim::spring_to` treatment.
///
/// `_dot_widths` (the manifest's `modules.workspaces.dot_widths`) is
/// accepted but deliberately unused — see `APPROVED_DOT_WIDTHS` below,
/// which overrides it with the approved Option B numbers the manifest's
/// own value predates. Kept in the signature rather than dropped so the
/// call site still documents where a real per-theme width would flow from
/// once `theme.toml` catches up.
pub fn make_dot_button(
    id: WorkspaceId,
    active: WorkspaceId,
    windows: i32,
    _dot_widths: bread_theme::shell::DotWidths,
) -> gtk4::Button {
    let btn = gtk4::Button::new();
    btn.add_css_class("workspace-dot");
    if windows > 0 {
        btn.add_css_class("occupied");
    }
    if id == active {
        btn.add_css_class("active");
    }
    btn.set_valign(gtk4::Align::Center);
    btn.set_halign(gtk4::Align::Center);
    btn.set_vexpand(false);
    btn.set_hexpand(false);
    // Height is a deliberate departure from `04-spotlight.html`'s own 6px
    // (see the demo's `.dots button { height: 6px }`): reported as "too
    // small and hard to click", and also genuinely hard to *see* on a real
    // display, not just hard to hit. Option B (approved): 10px tall — up
    // from an earlier 9px pass that undershot the approved number by 1px.
    // Keeps the dots reading as slim pills rather than growing into little
    // chips (which would fight the capsule's minimal, text-first look),
    // while being clearly perceptible against the 36px-tall bar.
    const DOT_HEIGHT: i32 = 10;
    // Option B widths (approved): 8/13/17/22px for 0/1/2/3-or-more open
    // windows — `[8, 13, 17, 22]`, not the `dot_widths` parameter's own
    // manifest value. `theme.toml`'s `modules.workspaces.dot_widths =
    // [6, 10, 14, 18]` predates this pass and was never updated to match;
    // hardcoded here (ignoring the passed-in `dot_widths`) rather than
    // edited upstream, since bread-ecosystem is a sibling agent's repo
    // this pass. Flagged in the task report — `dot_widths` should become
    // `[8, 13, 17, 22]` in `assets/shell/spotlight/theme.toml`.
    const APPROVED_DOT_WIDTHS: bread_theme::shell::DotWidths = [8, 13, 17, 22];
    btn.set_size_request(
        APPROVED_DOT_WIDTHS[dot_width_index(windows)],
        DOT_HEIGHT,
    );
    btn.connect_clicked(move |_| {
        relm4::spawn(async move {
            switch_workspace(id).await;
        });
    });
    btn
}

/// Maps an open-window count to a [`bread_theme::shell::DotWidths`] index:
/// 0/1/2 pass through, 3-or-more all collapse onto index 3 (the demo's own
/// `.dots button[data-n="3"]` never has a "4" variant). Pulled out of
/// [`make_dot_button`] as its own pure function purely so it's testable
/// without a GTK display — the isolated screenshot harness
/// (`bread-capture`) has no Hyprland IPC, so it can never exercise a
/// nonzero window count, and this is what stands in for that visual proof
/// (see the task notes on that gap).
fn dot_width_index(windows: i32) -> usize {
    (windows.max(0) as usize).min(3)
}

#[cfg(test)]
mod dot_width_tests {
    use super::dot_width_index;

    #[test]
    fn zero_and_one_and_two_pass_through() {
        assert_eq!(dot_width_index(0), 0);
        assert_eq!(dot_width_index(1), 1);
        assert_eq!(dot_width_index(2), 2);
    }

    #[test]
    fn three_or_more_all_collapse_onto_index_three() {
        assert_eq!(dot_width_index(3), 3);
        assert_eq!(dot_width_index(4), 3);
        assert_eq!(dot_width_index(50), 3);
    }

    #[test]
    fn negative_windows_clamps_to_zero_rather_than_panicking() {
        // Hyprland's `windows` count is unsigned (u16) in practice, but
        // `make_dot_button` takes a plain i32 — a negative value must
        // never underflow the `dot_widths` index and panic.
        assert_eq!(dot_width_index(-1), 0);
        assert_eq!(dot_width_index(i32::MIN), 0);
    }
}

#[derive(Clone, Copy)]
struct Geom {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

struct TrailInner {
    tick: Option<gtk4::TickCallbackId>,
    geom: Geom,
    /// Last width/height measured from a button that was actually allocated.
    /// A row rebuild (switching to an empty workspace makes Hyprland create
    /// and destroy it) can leave every button unallocated for a frame, and
    /// without a remembered size the trail had nothing safe to animate from
    /// and fell back to an instant `place()` — which is what made a switch
    /// snap instead of move.
    natural: Option<(f64, f64)>,
}

/// Overlay + Fixed pill sitting *behind* the workspace buttons. The
/// Overlay's measured size comes from the button row; the pill is the
/// main child so it paints underneath and never steals clicks.
pub struct WorkspaceTrail {
    pub overlay: gtk4::Overlay,
    pub buttons: gtk4::Box,
    host: gtk4::Fixed,
    pill: gtk4::Box,
    inner: Rc<RefCell<TrailInner>>,
}

impl WorkspaceTrail {
    pub fn new() -> Self {
        let overlay = gtk4::Overlay::new();
        overlay.add_css_class("workspace-overlay");
        overlay.set_valign(gtk4::Align::Center);
        overlay.set_vexpand(false);

        let host = gtk4::Fixed::new();
        host.set_can_target(false);

        let pill = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        pill.add_css_class("workspace-trail");
        pill.set_can_target(false);
        pill.set_visible(false);
        host.put(&pill, 0.0, 0.0);

        let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 1);
        buttons.set_halign(gtk4::Align::Fill);
        buttons.set_valign(gtk4::Align::Center);
        buttons.set_vexpand(false);

        overlay.set_child(Some(&host));
        overlay.add_overlay(&buttons);
        overlay.set_measure_overlay(&buttons, true);

        let inner = Rc::new(RefCell::new(TrailInner {
            tick: None,
            natural: None,
            geom: Geom {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
        }));

        Self {
            overlay,
            buttons,
            host,
            pill,
            inner,
        }
    }

    pub fn cancel(&self) {
        if let Some(id) = self.inner.borrow_mut().tick.take() {
            id.remove();
        }
    }

    pub fn clear(&self) {
        self.cancel();
        self.pill.set_visible(false);
        self.inner.borrow_mut().geom.w = 0.0;
    }

    pub fn place(&self, btn: &gtk4::Button) {
        self.cancel();
        if let Some(g) = button_geom(btn, &self.host) {
            apply_geom(&self.host, &self.pill, &self.inner, &inset_pill(g));
            return;
        }
        let pill = self.pill.clone();
        let host = self.host.clone();
        let inner = self.inner.clone();
        let btn = btn.clone();
        let id = self.overlay.add_tick_callback(move |_, _| {
            let Some(g) = button_geom(&btn, &host) else {
                return ControlFlow::Continue;
            };
            apply_geom(&host, &pill, &inner, &inset_pill(g));
            inner.borrow_mut().tick = None;
            ControlFlow::Break
        });
        self.inner.borrow_mut().tick = Some(id);
    }

    pub fn stretch(&self, from: Option<&gtk4::Button>, to: &gtk4::Button) {
        self.cancel();
        let Some(from_g) = self.from_geom(from, to) else {
            self.place(to);
            return;
        };
        let dest = to.clone();
        let pill = self.pill.clone();
        let host = self.host.clone();
        let inner = self.inner.clone();
        let started = Instant::now();
        let id = self.overlay.add_tick_callback(move |_, _| {
            let to_g = resolved_dest(&dest, &host, &from_g);
            let mid = {
                let span_x = from_g.x.min(to_g.x);
                let span_w = (from_g.x + from_g.w).max(to_g.x + to_g.w) - span_x;
                Geom {
                    x: span_x,
                    y: to_g.y,
                    w: span_w,
                    h: to_g.h,
                }
            };
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            let (g, done) = if elapsed < STRETCH_MS {
                let t = ease(elapsed / STRETCH_MS);
                (lerp_geom(&from_g, &mid, t), false)
            } else if elapsed < STRETCH_MS + SNAP_MS {
                let t = ease_overshoot((elapsed - STRETCH_MS) / SNAP_MS);
                (lerp_geom(&mid, &to_g, t), false)
            } else {
                (to_g, true)
            };
            // Squash-and-stretch (ANIMATION WORK #1): compress the pill's
            // height a few px while it's mid-flight and let it spring back
            // — slightly taller than normal, then settling — as it lands,
            // so the trail reads as having weight instead of sliding like a
            // rigid box. A pure post-process on top of the already-correct
            // x/w/y trajectory above, applied only to the frame actually
            // painted (`g`) — never fed back into `TrailInner::natural`
            // (`from_geom`'s own width/height memory for the *next*
            // animation), which stays driven solely by real button geometry
            // as before. Skipped on the final `done` frame so the resting
            // geometry is exactly `to_g`, unperturbed.
            let g = if done { g } else { squash_geom(g, squash_factor(elapsed)) };
            apply_geom(&host, &pill, &inner, &g);
            if done {
                inner.borrow_mut().tick = None;
                ControlFlow::Break
            } else {
                ControlFlow::Continue
            }
        });
        self.inner.borrow_mut().tick = Some(id);
    }

    // `from` is a noun here (the source button we animate away from), not a
    // conversion — `&self` is correct.
    #[allow(clippy::wrong_self_convention)]
    fn from_geom(&self, from: Option<&gtk4::Button>, to: &gtk4::Button) -> Option<Geom> {
        let st = self.inner.borrow();
        let live = if self.pill.is_visible() && st.geom.w > 0.5 {
            Some(st.geom)
        } else {
            None
        };
        let cached = st.natural;
        drop(st);

        // Width must always come from a button that is CURRENTLY IN THE ROW.
        // Switching to an empty workspace makes Hyprland create and destroy it,
        // which rebuilds the button row mid-animation and can leave `from`
        // detached — `button_geom` then returns None. Falling back to the live
        // geometry there handed the wide mid-stretch span straight back in,
        // which is exactly the accumulation this function exists to prevent
        // (reproduced by spamming between workspace 1 and an empty 6). The
        // destination button is always live, so it is the correct fallback.
        let natural = from
            .and_then(|b| button_geom(b, &self.host).map(inset_pill))
            .or_else(|| button_geom(to, &self.host).map(inset_pill));

        // Continuity without accumulation.
        //
        // Interrupting an in-flight stretch should carry the pill's CURRENT
        // POSITION into the next animation, so a rapid sequence of switches
        // reads as one continuous movement. It must not carry the current
        // WIDTH: mid-stretch the pill deliberately spans both the old and new
        // buttons, and `ease_overshoot` (c = 1.4) pushes it wider still past
        // the target. Feeding that span back in as the next `from` made each
        // interrupted switch start wider than the last, so spamming workspace
        // switches grew the pill until it hit MAX_CHIP_W — which only capped
        // the runaway, it never stopped the compounding.
        //
        // Taking position from the live geometry and width from the source
        // button's natural size keeps the motion continuous while making width
        // a pure function of which button we started from.
        if let Some(n) = natural {
            self.inner.borrow_mut().natural = Some((n.w, n.h));
        }
        // Only x comes from the live geometry. That is what makes an
        // interrupted switch continue from where the pill currently is instead
        // of jumping back. y/w/h always come from a real button: taking y from
        // a mid-animation or post-rebuild geometry is what made the pill sit
        // low, and taking w from it is what let the width compound.
        let size = natural.map(|n| (n.w, n.h)).or(cached);
        match (live, natural, size) {
            (Some(live), _, Some((w, h))) => Some(Geom {
                x: live.x,
                y: natural.map(|n| n.y).unwrap_or(live.y),
                w,
                h,
            }),
            (None, Some(natural), _) => Some(natural),
            _ => None,
        }
    }
}

/// Keep the trail slimmer than the hit target so the fill doesn't look
/// like a second, fatter button.
const PILL_INSET_X: f64 = 5.0;
const PILL_INSET_Y: f64 = 3.0;
/// One workspace chip is a digit + padding. Wider than this is the overlay
/// or the whole button row leaking through `compute_bounds`.
const MAX_CHIP_W: f64 = 72.0;

fn inset_pill(g: Geom) -> Geom {
    let w = (g.w - PILL_INSET_X * 2.0).max(10.0);
    let h = (g.h - PILL_INSET_Y * 2.0).max(18.0);
    Geom {
        x: g.x + (g.w - w) * 0.5,
        y: g.y + (g.h - h) * 0.5,
        w,
        h,
    }
}

fn resolved_dest(btn: &gtk4::Button, host: &gtk4::Fixed, from: &Geom) -> Geom {
    match button_geom(btn, host) {
        Some(g) if !still_placeholder(btn, &g) => inset_pill(g),
        Some(g) => {
            let centered = inset_pill(g);
            Geom {
                x: centered.x + (centered.w - from.w) * 0.5,
                y: centered.y + (centered.h - from.h) * 0.5,
                w: from.w,
                h: from.h,
            }
        }
        None => *from,
    }
}

/// Position in the Fixed host's space — that's what `host.move_` uses.
/// Measuring against the Overlay instead left the pill a few px left of
/// the digit whenever the host and overlay origins disagreed.
fn button_geom(btn: &gtk4::Button, host: &gtk4::Fixed) -> Option<Geom> {
    let r = btn.compute_bounds(host)?;
    let w = f64::from(r.width());
    let h = f64::from(r.height());
    if w < 8.0 || h < 8.0 || w > MAX_CHIP_W {
        return None;
    }
    Some(Geom {
        x: f64::from(r.x()),
        y: f64::from(r.y()),
        w,
        h,
    })
}

fn apply_geom(host: &gtk4::Fixed, pill: &gtk4::Box, inner: &Rc<RefCell<TrailInner>>, g: &Geom) {
    inner.borrow_mut().geom = Geom {
        x: g.x,
        y: g.y,
        w: g.w,
        h: g.h,
    };
    let w = g.w.max(1.0).round() as i32;
    let h = g.h.max(1.0).round() as i32;
    // Clearing first lets GTK shrink; size-request is a minimum.
    pill.set_size_request(-1, -1);
    pill.set_size_request(w, h);
    host.move_(pill, g.x, g.y);
    pill.set_visible(true);
}

fn still_placeholder(btn: &gtk4::Button, g: &Geom) -> bool {
    let (min_w, nat_w, _, _) = btn.measure(gtk4::Orientation::Horizontal, -1);
    g.w <= f64::from(min_w) + 1.0 || g.w + 0.5 < f64::from(nat_w)
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn lerp_geom(a: &Geom, b: &Geom, t: f64) -> Geom {
    Geom {
        x: lerp(a.x, b.x, t),
        y: lerp(a.y, b.y, t),
        w: lerp(a.w, b.w, t),
        h: lerp(a.h, b.h, t),
    }
}

fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Approximates the demo's cubic-bezier(.22, 1.4, .36, 1) snap.
fn ease_overshoot(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let c = 1.4;
    let t1 = t - 1.0;
    1.0 + t1 * t1 * ((c + 1.0) * t1 + c)
}

/// Lowest fraction of the resting height the pill compresses to, at the
/// peak of the stretch phase (fastest travel).
const SQUASH_MIN: f64 = 0.80;

/// The squash-and-stretch height multiplier for a given point in
/// `stretch`'s own STRETCH_MS-then-SNAP_MS timeline: eases DOWN to
/// `SQUASH_MIN` across the stretch phase (using the same `ease` curve the
/// width stretch already uses), then eases back UP to 1.0 across the snap
/// phase using `ease_overshoot` — which legitimately overshoots past 1.0
/// partway through, so the pill also plumps up slightly taller than its
/// resting height right before settling, the same "spring" read the width
/// snap already has. Past both phases (a caller-side `elapsed` this large
/// only happens if something calls this after `stretch`'s own `done` cutoff)
/// this is exactly 1.0, i.e. a no-op.
fn squash_factor(elapsed: f64) -> f64 {
    if elapsed < STRETCH_MS {
        lerp(1.0, SQUASH_MIN, ease(elapsed / STRETCH_MS))
    } else if elapsed < STRETCH_MS + SNAP_MS {
        lerp(SQUASH_MIN, 1.0, ease_overshoot((elapsed - STRETCH_MS) / SNAP_MS))
    } else {
        1.0
    }
}

/// Applies a height multiplier to `g`, keeping it vertically centred on
/// `g`'s own centre (so the squash reads as compression, not a pill that
/// sinks or rises) and leaving x/w untouched.
fn squash_geom(g: Geom, factor: f64) -> Geom {
    let h = g.h * factor;
    Geom {
        x: g.x,
        y: g.y + (g.h - h) * 0.5,
        w: g.w,
        h,
    }
}

#[cfg(test)]
mod squash_tests {
    use super::*;

    #[test]
    fn factor_starts_and_ends_at_one() {
        assert!((squash_factor(0.0) - 1.0).abs() < 1e-9);
        assert!((squash_factor(STRETCH_MS + SNAP_MS) - 1.0).abs() < 1e-9);
        assert_eq!(squash_factor(STRETCH_MS + SNAP_MS + 500.0), 1.0);
    }

    #[test]
    fn factor_dips_below_one_mid_stretch() {
        let mid_stretch = STRETCH_MS * 0.5;
        assert!(squash_factor(mid_stretch) < 1.0);
        assert!(squash_factor(mid_stretch) >= SQUASH_MIN);
    }

    #[test]
    fn squash_geom_keeps_the_vertical_centre_fixed() {
        let g = Geom {
            x: 10.0,
            y: 20.0,
            w: 30.0,
            h: 26.0,
        };
        let centre = g.y + g.h * 0.5;
        let squashed = squash_geom(g, 0.8);
        assert!((squashed.h - 20.8).abs() < 1e-9);
        assert!((squashed.y + squashed.h * 0.5 - centre).abs() < 1e-9);
        assert_eq!(squashed.x, g.x);
        assert_eq!(squashed.w, g.w);
    }

    #[test]
    fn squash_geom_factor_one_is_identity() {
        let g = Geom {
            x: 1.0,
            y: 2.0,
            w: 3.0,
            h: 4.0,
        };
        let out = squash_geom(g, 1.0);
        assert_eq!(out.y, g.y);
        assert_eq!(out.h, g.h);
    }
}

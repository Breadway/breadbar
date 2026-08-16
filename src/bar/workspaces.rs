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
    btn.set_vexpand(false);
    btn.set_size_request(-1, crate::CHIP_HEIGHT);
    btn.connect_clicked(move |_| {
        relm4::spawn(async move {
            switch_workspace(id).await;
        });
    });
    btn
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

        let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 2);
        buttons.set_halign(gtk4::Align::Fill);
        buttons.set_valign(gtk4::Align::Center);
        buttons.set_vexpand(false);

        overlay.set_child(Some(&host));
        overlay.add_overlay(&buttons);
        overlay.set_measure_overlay(&buttons, true);

        let inner = Rc::new(RefCell::new(TrailInner {
            tick: None,
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
        let pill = self.pill.clone();
        let host = self.host.clone();
        let inner = self.inner.clone();
        self.when_stable(btn, move |g| {
            apply_geom(&host, &pill, &inner, &g);
        });
    }

    pub fn stretch(&self, from: Option<&gtk4::Button>, to: &gtk4::Button) {
        let from_g = self.from_geom(from);
        let overlay = self.overlay.clone();
        let pill = self.pill.clone();
        let host = self.host.clone();
        let inner = self.inner.clone();
        let dest = to.clone();
        self.when_stable(to, move |to_g| {
            stretch_geom_on(&overlay, &host, &pill, &inner, from_g, to_g, Some(dest));
        });
    }

    /// New empty-workspace buttons first allocate at CSS `min-width` (32px)
    /// and only then grow to the padded label. Two stable frames of that
    /// placeholder is not enough — empty→empty used to shrink the pill to it.
    fn when_stable(&self, btn: &gtk4::Button, then: impl FnOnce(Geom) + 'static) {
        self.cancel();
        let btn = btn.clone();
        let inner = self.inner.clone();
        let last = std::cell::Cell::new(None::<Geom>);
        let same = std::cell::Cell::new(0u8);
        let frames = std::cell::Cell::new(0u8);
        let then = std::cell::Cell::new(Some(then));
        let id = self.overlay.add_tick_callback(move |ov, _| {
            frames.set(frames.get().saturating_add(1));
            let n = frames.get();
            let Some(g) = button_geom(&btn, ov) else {
                last.set(None);
                same.set(0);
                return if n > 24 {
                    inner.borrow_mut().tick = None;
                    ControlFlow::Break
                } else {
                    ControlFlow::Continue
                };
            };
            // Still sitting on the 32px min-width slot, or smaller than the
            // button's natural request — keep waiting for the real layout.
            if still_placeholder(&btn, &g) && n < 20 {
                last.set(None);
                same.set(0);
                return ControlFlow::Continue;
            }
            let stable = last.get().is_some_and(|p| geom_close(&p, &g));
            last.set(Some(g));
            same.set(if stable { same.get().saturating_add(1) } else { 0 });
            if same.get() >= 2 || n > 22 {
                inner.borrow_mut().tick = None;
                if let Some(f) = then.take() {
                    f(g);
                }
                return ControlFlow::Break;
            }
            ControlFlow::Continue
        });
        self.inner.borrow_mut().tick = Some(id);
    }

    fn from_geom(&self, from: Option<&gtk4::Button>) -> Option<Geom> {
        let st = self.inner.borrow();
        if self.pill.is_visible() && st.geom.w > 0.5 {
            return Some(st.geom);
        }
        drop(st);
        from.and_then(|b| button_geom(b, &self.overlay))
    }

}

fn stretch_geom_on(
    overlay: &gtk4::Overlay,
    host: &gtk4::Fixed,
    pill: &gtk4::Box,
    inner: &Rc<RefCell<TrailInner>>,
    from_g: Option<Geom>,
    to_g: Geom,
    dest: Option<gtk4::Button>,
) {
    let Some(from_g) = from_g else {
        apply_geom(host, pill, inner, &to_g);
        return;
    };
    if (from_g.x - to_g.x).abs() < 0.5 && (from_g.w - to_g.w).abs() < 0.5 {
        apply_geom(host, pill, inner, &to_g);
        return;
    }

    let span_x = from_g.x.min(to_g.x);
    let span_w = (from_g.x + from_g.w).max(to_g.x + to_g.w) - span_x;
    let mid = Geom {
        x: span_x,
        y: to_g.y,
        w: span_w,
        h: to_g.h,
    };

    if let Some(id) = inner.borrow_mut().tick.take() {
        id.remove();
    }
    let started = Instant::now();
    let pill = pill.clone();
    let host = host.clone();
    let inner_tick = inner.clone();
    let dest = dest.clone();
    let ov = overlay.clone();
    let id = overlay.add_tick_callback(move |_, _| {
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        let (g, done) = if elapsed < STRETCH_MS {
            let t = ease(elapsed / STRETCH_MS);
            (lerp_geom(&from_g, &mid, t), false)
        } else if elapsed < STRETCH_MS + SNAP_MS {
            let t = ease_overshoot((elapsed - STRETCH_MS) / SNAP_MS);
            (lerp_geom(&mid, &to_g, t), false)
        } else {
            let end = dest
                .as_ref()
                .and_then(|b| button_geom(b, &ov))
                .unwrap_or(to_g);
            (end, true)
        };
        apply_geom(&host, &pill, &inner_tick, &g);
        if done {
            inner_tick.borrow_mut().tick = None;
            ControlFlow::Break
        } else {
            ControlFlow::Continue
        }
    });
    inner.borrow_mut().tick = Some(id);
}

fn button_geom(btn: &gtk4::Button, overlay: &gtk4::Overlay) -> Option<Geom> {
    let r = btn.compute_bounds(overlay)?;
    let w = f64::from(r.width());
    let h = f64::from(r.height());
    if w < 1.0 || h < 1.0 {
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

fn geom_close(a: &Geom, b: &Geom) -> bool {
    (a.x - b.x).abs() < 0.5
        && (a.y - b.y).abs() < 0.5
        && (a.w - b.w).abs() < 0.5
        && (a.h - b.h).abs() < 0.5
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

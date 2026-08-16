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
        self.cancel();
        if let Some(g) = button_geom(btn, &self.overlay) {
            self.apply(&g);
            return;
        }
        // First map: the button exists but has no allocation yet.
        let pill = self.pill.clone();
        let host = self.host.clone();
        let btn = btn.clone();
        let inner = self.inner.clone();
        let id = self.overlay.add_tick_callback(move |ov, _| {
            let Some(g) = button_geom(&btn, ov) else {
                return ControlFlow::Continue;
            };
            apply_geom(&host, &pill, &inner, &g);
            inner.borrow_mut().tick = None;
            ControlFlow::Break
        });
        self.inner.borrow_mut().tick = Some(id);
    }

    pub fn stretch(&self, from: Option<&gtk4::Button>, to: &gtk4::Button) {
        let from_g = self.from_geom(from);
        if let Some(to_g) = button_geom(to, &self.overlay) {
            self.stretch_geom(from_g, to_g);
            return;
        }
        // Destination just appeared (empty workspace becoming active) and
        // has no allocation yet. Wait one layout pass, then stretch from
        // the last pill — don't snap via place().
        self.cancel();
        let overlay = self.overlay.clone();
        let pill = self.pill.clone();
        let host = self.host.clone();
        let inner = self.inner.clone();
        let btn = to.clone();
        let id = self.overlay.add_tick_callback(move |ov, _| {
            let Some(to_g) = button_geom(&btn, ov) else {
                return ControlFlow::Continue;
            };
            inner.borrow_mut().tick = None;
            stretch_geom_on(&overlay, &host, &pill, &inner, from_g, to_g);
            ControlFlow::Break
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

    fn stretch_geom(&self, from_g: Option<Geom>, to_g: Geom) {
        stretch_geom_on(
            &self.overlay,
            &self.host,
            &self.pill,
            &self.inner,
            from_g,
            to_g,
        );
    }

    fn apply(&self, g: &Geom) {
        apply_geom(&self.host, &self.pill, &self.inner, g);
    }
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
    pill.set_size_request(g.w.max(1.0).round() as i32, g.h.max(1.0).round() as i32);
    host.move_(pill, g.x, g.y);
    pill.set_visible(true);
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

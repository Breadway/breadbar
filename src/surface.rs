//! Applies a `[surfaces.<namespace>]` entry (plan §4/§6, Phase 2) to a
//! satellite layer-shell window: anchor, margin (from `offset`), width and
//! layer. Deliberately narrow — it only understands the four anchor shapes
//! breadbar's four built-in surfaces actually use today ("breadbar-notif",
//! "breadbar-osd", "breadbar-panel", "breadbar-dismiss": `top_right`,
//! `bottom_right`, `bottom_centre`, `fill`), not a general anchor DSL (the
//! plan's own anti-goal, §2). `bottom_right` was added for daylight (plan
//! §11 phase 7, axis 2) — the first bottom-anchored bar, whose satellites
//! need to hug the bottom-right corner the way every top-anchored theme's
//! already hug the top-right one. `exclusive` zone and `keyboard` mode
//! aren't part of the `[surfaces.*]` schema (`bread_theme::shell::Surface`
//! has no such fields) and stay hardcoded at each call site, same as before
//! this refactor.

use bread_theme::shell::SurfaceWidth;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, Layer, LayerShell};

/// `namespace` should be a key in the active theme's `[surfaces.*]` table —
/// every call site in this crate passes one of breadbar's own namespace
/// literals, so a miss here means the active theme fell out of sync with
/// the Rust source, not a bad runtime value. Logs and leaves the window at
/// gtk4-layer-shell's own defaults rather than panicking, matching every
/// other "malformed/incomplete theme" fallback in this system.
///
/// The width applied here is not authoritative for a namespace shared by
/// more than one window with genuinely different widths (`breadbar-notif`'s
/// live toast is 320px, its history sibling is 360px, and only the toast's
/// width is modeled in `[surfaces.*]` — see the Phase 0 constant inventory);
/// callers that need a different width than the theme's own set it
/// explicitly afterward. Because a `Px` width is pinned with BOTH
/// `set_default_width` and `set_size_request` (see below — the latter is
/// what actually holds against a wide child), such a caller must override
/// both, not just `set_default_width`, or the pin from here wins.
pub fn apply(window: &gtk4::Window, namespace: &str) {
    let theme = crate::theme::shell_theme();
    let Some(surf) = theme.surfaces().get(namespace) else {
        eprintln!(
            "breadbar: no [surfaces.{namespace}] entry in the active theme; \
             window left at layer-shell defaults"
        );
        return;
    };

    window.set_layer(if surf.layer == "top" {
        Layer::Top
    } else {
        Layer::Overlay
    });

    match surf.anchor.as_str() {
        "top_right" => {
            window.set_anchor(Edge::Top, true);
            window.set_anchor(Edge::Right, true);
            // offset = [right, top] for this anchor shape.
            let right = surf.offset.first().copied().unwrap_or(0.0) as i32;
            let top = surf.offset.get(1).copied().unwrap_or(0.0) as i32;
            window.set_margin(Edge::Right, right);
            window.set_margin(Edge::Top, top);
        }
        // Axis 2 (daylight): every theme through spotlight anchors its bar
        // to the TOP, so `top_right` always put breadbar-notif/
        // breadbar-panel naturally close to the bar. A bottom-anchored bar
        // has nothing in the original three shapes that keeps its
        // satellites near it — `top_right` would land them at the opposite
        // corner of the screen from the dock they belong to. Mirrors
        // `top_right` exactly, just on the bottom edge.
        "bottom_right" => {
            window.set_anchor(Edge::Bottom, true);
            window.set_anchor(Edge::Right, true);
            // offset = [right, bottom], same convention as top_right's
            // [right, top].
            let right = surf.offset.first().copied().unwrap_or(0.0) as i32;
            let bottom = surf.offset.get(1).copied().unwrap_or(0.0) as i32;
            window.set_margin(Edge::Right, right);
            window.set_margin(Edge::Bottom, bottom);
        }
        "bottom_centre" => {
            window.set_anchor(Edge::Bottom, true);
            let bottom = surf.offset.first().copied().unwrap_or(0.0) as i32;
            window.set_margin(Edge::Bottom, bottom);
        }
        "fill" => {
            for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                window.set_anchor(edge, true);
            }
            // offset = [top, bottom] — a fullscreen click-away scrim that
            // leaves a gap clear of the bar on whichever edge the bar
            // actually anchors to. Every theme through spotlight anchors
            // top, so only `offset[0]` (top) was ever meaningful before
            // daylight; a single-value `offset` (every existing theme's
            // manifest) still means exactly what it always did, since
            // `offset.get(1)` falls back to 0 — a bottom-anchored theme is
            // the first to give this a real, nonzero bottom value instead.
            let top = surf.offset.first().copied().unwrap_or(0.0) as i32;
            let bottom = surf.offset.get(1).copied().unwrap_or(0.0) as i32;
            window.set_margin(Edge::Top, top);
            window.set_margin(Edge::Bottom, bottom);
        }
        other => eprintln!(
            "breadbar: surfaces.{namespace}.anchor = \"{other}\" is not one of \
             top_right|bottom_right|bottom_centre|fill — breadbar's satellite \
             windows don't understand any other shape yet, leaving this window \
             unanchored"
        ),
    }

    if let SurfaceWidth::Px(px) = surf.width {
        window.set_default_width(px);
        // set_default_width alone is only a preference — a wide child (an
        // unwrapped app-name label, or a long summary/body before GTK has
        // any allocation narrower than its natural width to wrap against)
        // overrides it, so the window renders wider than the theme's
        // requested px and stops matching the theme. Same trap, same fix,
        // as main.rs's capsule `Width::Px` handling — see its comment.
        window.set_size_request(px, -1);
    }
}

/// Sets `window`'s input region to the union of `widgets`' current
/// allocations, each measured relative to `window` itself (the surface's
/// own coordinate space, same as `bar::workspaces::button_geom`'s own
/// `compute_bounds` call relative to its Fixed host) — everywhere else on
/// the surface stays click-through. An empty (or all-invisible/all-
/// unallocated) `widgets` slice is not a special case:
/// `Region::create_rectangles(&[])` is already the fully click-through
/// empty region — passing `&[]` here is exactly the old blanket
/// `Region::create()` this function replaces (see git history around
/// `notifications/popup.rs`'s "stop toast popups from stealing focus or
/// blocking clicks" fix, and NOTIFICATION INTERACTION #B in the current
/// task notes: a toast must never block clicks or steal focus from
/// whatever's underneath it EXCEPT on its own buttons — an all-empty
/// region made those unreachable too).
///
/// Only meaningful after `window.surface()` exists (i.e. from
/// `connect_map` onward — the surface doesn't exist before the window is
/// mapped) and after `widgets` have a real allocation — a widget with no
/// allocation yet (`compute_bounds` returning `None`) is simply skipped
/// rather than contributing a garbage rectangle, so a call made one frame
/// too early just yields a smaller-than-intended region for that one frame
/// rather than a wrong one.
///
/// Callers are responsible for RECOMPUTING this every time the hittable
/// set could have moved: a widget added or removed, or a layout pass (an
/// entrance animation, a push-down reflow) still in flight. A stale region
/// either swallows clicks meant for the window below or leaves a real
/// button dead.
pub fn set_hit_region(window: &gtk4::Window, widgets: &[gtk4::Widget]) {
    let Some(surface) = window.surface() else {
        return;
    };
    let rects: Vec<gtk4::cairo::RectangleInt> = widgets
        .iter()
        .filter(|w| w.is_visible())
        .filter_map(|w| {
            let b = w.compute_bounds(window)?;
            Some(gtk4::cairo::RectangleInt::new(
                b.x().floor() as i32,
                b.y().floor() as i32,
                b.width().ceil() as i32,
                b.height().ceil() as i32,
            ))
        })
        .collect();
    surface.set_input_region(Some(&gtk4::cairo::Region::create_rectangles(&rects)));
}

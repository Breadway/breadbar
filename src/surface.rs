//! Applies a `[surfaces.<namespace>]` entry (plan §4/§6, Phase 2) to a
//! satellite layer-shell window: anchor, margin (from `offset`), width and
//! layer. Deliberately narrow — it only understands the three anchor shapes
//! breadbar's four built-in surfaces actually use today ("breadbar-notif",
//! "breadbar-osd", "breadbar-panel", "breadbar-dismiss": `top_right`,
//! `bottom_centre`, `fill`), not a general anchor DSL (the plan's own
//! anti-goal, §2). `exclusive` zone and `keyboard` mode aren't part of the
//! `[surfaces.*]` schema (`bread_theme::shell::Surface` has no such fields)
//! and stay hardcoded at each call site, same as before this refactor.

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
/// Does not set `set_default_width` for a namespace shared by more than one
/// window with genuinely different widths (`breadbar-notif`'s live toast is
/// 320px, its history sibling is 360px, and only the toast's width is
/// modeled in `[surfaces.*]` — see the Phase 0 constant inventory); callers
/// that need a different width than the theme's own set it explicitly
/// afterward.
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
        "bottom_centre" => {
            window.set_anchor(Edge::Bottom, true);
            let bottom = surf.offset.first().copied().unwrap_or(0.0) as i32;
            window.set_margin(Edge::Bottom, bottom);
        }
        "fill" => {
            for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                window.set_anchor(edge, true);
            }
            // Only a top margin is meaningful here — a fullscreen click-away
            // scrim that starts below the bar rather than covering it.
            let top = surf.offset.first().copied().unwrap_or(0.0) as i32;
            window.set_margin(Edge::Top, top);
        }
        other => eprintln!(
            "breadbar: surfaces.{namespace}.anchor = \"{other}\" is not one of \
             top_right|bottom_centre|fill — breadbar's satellite windows don't \
             understand any other shape yet, leaving this window unanchored"
        ),
    }

    if let SurfaceWidth::Px(px) = surf.width {
        window.set_default_width(px);
    }
}

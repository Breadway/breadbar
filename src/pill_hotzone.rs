//! BAR-27 phase 1: the spotlight pill's hot-zone + "lip" surface.
//!
//! A small always-on-top layer-shell window, separate from the bar's own
//! surface, that only exists while the active theme's `[bar.autohide]` is
//! enabled. It does two jobs at once: it's the top-middle hover target that
//! brings a tucked pill back down
//! (`bos-ui-demos/proposed/spotlight-stage.html`'s `.hotzone`), and it draws
//! the faint mark showing where the pill lives while tucked (`.lip`). One
//! surface, not two: the hot-zone's own footprint already contains the
//! lip's, and a window's hover events don't discriminate by which of its
//! own child widgets the pointer is over, so a second surface would add
//! nothing.
//!
//! Deliberately its own window rather than an oversized invisible region on
//! the bar's own surface: the whole point of tucking is that the bar
//! surface itself moves off-screen (see `main.rs::animate_pill`, which
//! drives the layer-shell top margin), so nothing on that surface can catch
//! a hover once it's gone. This window never moves; only the `lip` widget
//! fades (in while tucked, out while the pill itself is down, the mockup's
//! `.lip.gone{opacity:0}` with its `.25s` transition).

use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, Layer, LayerShell};

/// The lip's own fixed footprint (`.lip{width:44px;height:3px}` in the
/// mockup); independent of the theme-configurable hot-zone size, which is
/// what the pointer has to enter, not what's drawn.
const LIP_WIDTH: i32 = 44;
const LIP_HEIGHT: i32 = 3;

/// Height, in px, of the thin strip left clickable/hoverable at the very
/// top edge while the pill is tucked (phase-1 review must-fix). The hot
/// zone's theme-configured footprint (`[bar.autohide].hover_zone_width` x
/// `hover_zone_height`, e.g. 420x14) is how far the mockup's own CSS draws
/// the `.hotzone` div, but that whole box taking real pointer input on a
/// `Layer::Top` surface means every click landing in that strip, even ones
/// meant for a window underneath, never reaches it: the surface always
/// wins. Kept within the spec's asked-for 2-4px range.
const TUCKED_INPUT_HEIGHT: i32 = 3;

/// Sets the hot-zone window's actual input region: a `TUCKED_INPUT_HEIGHT`
/// px strip spanning the full configured `width`, in BOTH pill states.
///
/// It used to be emptied while the pill was shown, but the shown pill sits
/// `margin.top` px below the screen edge, so an empty region left a dead gap
/// between the edge and the pill: a pointer parked at the very top revealed
/// the pill, the region then vanished under it, the next motion sent a
/// hover-leave, the pill re-tucked, the strip came back under the pointer
/// and revealed it again (a steady flicker while moving along the top
/// edge). Keeping the strip matches the mockup (its `.hotzone` stays live
/// while the pill is down) and still never overlaps the pill itself, so it
/// swallows nothing more than the tucked state already does.
///
/// A no-op before the window is mapped (`window.surface()` is `None`);
/// `spawn` also applies it from a `connect_map` handler so the very first
/// paint already has the narrow region rather than the full-footprint
/// default gtk4-layer-shell otherwise leaves in place.
pub fn set_input_state(window: &gtk4::Window, width: i32) {
    let Some(surface) = window.surface() else {
        return;
    };
    let rect = gtk4::cairo::RectangleInt::new(0, 0, width, TUCKED_INPUT_HEIGHT);
    let region = gtk4::cairo::Region::create_rectangles(&[rect]);
    surface.set_input_region(Some(&region));
}

/// Toggles a debug overlay the exact size of the real tucked input strip
/// (see [`TUCKED_INPUT_HEIGHT`]), for the `pill-tucked-input` screenshot
/// view only: an input region paints nothing on its own, so there is
/// otherwise nothing to see in a capture that the phase-1 review's must-fix
/// actually changed. Never called outside screenshot mode.
pub fn set_debug_visible(debug: &gtk4::Widget, visible: bool) {
    debug.set_visible(visible);
}

/// Builds the hot-zone/lip window and wires `on_enter`/`on_leave` to an
/// `EventControllerMotion` spanning its whole surface (the mockup's own
/// `.hotzone`/`.wick` share one `hoverOn`/`hoverOff` pair; this is the
/// hot-zone's half of it, `main.rs` wires the pill window's own motion
/// controller to the same pair). Returns the window, the lip widget (so the
/// caller can toggle its visibility as the pill tucks/shows), and the debug
/// overlay [`set_debug_visible`] controls.
pub fn spawn(
    width: i32,
    height: i32,
    on_enter: impl Fn() + 'static,
    on_leave: impl Fn() + 'static,
) -> (gtk4::Window, gtk4::Widget, gtk4::Widget) {
    let window = gtk4::Window::new();
    window.add_css_class("breadbar-pill-hotzone");
    window.init_layer_shell();
    window.set_namespace(Some("breadbar-pill-hotzone"));
    window.set_layer(Layer::Top);
    window.set_anchor(Edge::Top, true);
    window.set_margin(Edge::Top, 0);
    // No exclusive zone: this is a hover target, not bar chrome; windows use
    // the full screen exactly like the pill itself.
    window.set_exclusive_zone(-1);
    window.set_default_width(width);
    window.set_size_request(width, height);

    let overlay = gtk4::Overlay::new();
    overlay.set_size_request(width, height);

    // The hit area: transparent, fills the whole hot-zone footprint, is
    // what the motion controller below is actually attached to.
    let hit = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    hit.set_size_request(width, height);
    overlay.set_child(Some(&hit));

    let lip = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    lip.add_css_class("pill-lip");
    // Starts faded out: the pill itself starts down (see `spawn`'s caller).
    // `update_pill` toggles `gone` and the lip fades on its CSS transition.
    lip.add_css_class("gone");
    lip.set_size_request(LIP_WIDTH, LIP_HEIGHT);
    lip.set_halign(gtk4::Align::Center);
    lip.set_valign(gtk4::Align::Start);
    overlay.add_overlay(&lip);
    overlay.set_measure_overlay(&lip, false);

    // Screenshot-only debug strip (see `set_debug_visible`): same footprint
    // as the real tucked input region, hidden by default.
    let debug = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    debug.add_css_class("pill-hotzone-debug");
    debug.set_size_request(width, TUCKED_INPUT_HEIGHT);
    debug.set_halign(gtk4::Align::Fill);
    debug.set_valign(gtk4::Align::Start);
    debug.set_visible(false);
    overlay.add_overlay(&debug);
    overlay.set_measure_overlay(&debug, false);

    window.set_child(Some(&overlay));

    let motion = gtk4::EventControllerMotion::new();
    motion.connect_enter(move |_, _, _| on_enter());
    motion.connect_leave(move |_| on_leave());
    window.add_controller(motion);

    // Starts tucked (the pill's own initial state whenever autohide is
    // enabled; see `main.rs`'s `pill_shown: true` immediately followed by
    // an `update_pill()` call that flips it): narrows the input region the
    // moment the surface actually exists, instead of leaving the
    // full-footprint gtk4-layer-shell default live until the first real
    // show/tuck transition happens to fire.
    window.connect_map(move |w| set_input_state(w, width));

    (window, lip.upcast(), debug.upcast())
}

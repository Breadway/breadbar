//! Standalone layer-shell panels for wifi / control / media.
//!
//! GTK `Popover` is an xdg_popup child of the island, so it paints over the
//! bar and Hyprland can only fade it. These are their own surfaces, parked
//! *below* the exclusive zone, and Hyprland slides `breadbar-panel` in from
//! the right.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4::gdk::Key;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, LayerShell};

use crate::{bind_layer_monitor, theme};

/// Outside this rectangle's local x/y span, the dismiss window's own real
/// size (Wayland clips an input region to the surface's actual bounds, same
/// as `surface::set_hit_region`'s empty-region trick) — big enough to cover
/// any realistic monitor layout, including a negative-origin secondary
/// output (`hyprctl layers -j` reported `x: -1080` for this machine's own
/// DVI-I-1). Centered on the origin so it's safe regardless of which way a
/// hole's coordinates end up signed.
const HOLE_CANVAS_SPAN: i32 = 20_000;

/// A boxed, ref-counted, optionally-unset click-away callback — see
/// `PanelSet::on_dismiss`'s own doc comment.
type DismissCallback = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// The capsule-column hole punched in the dismiss scrim's input region —
/// local x, y, width, height — see `PanelSet::dismiss_hole`'s own doc
/// comment. Shared (not just passed by value) so `make_dismiss`'s
/// `connect_map` hook and every `show_capsule_dismiss`/`reset_dismiss_margin`
/// call agree on the current value.
type DismissHole = Rc<Cell<Option<(i32, i32, i32, i32)>>>;

#[derive(Clone)]
pub struct PanelSet {
    pub connectivity: gtk4::Window,
    pub control: gtk4::Window,
    pub media: gtk4::Window,
    dismiss: gtk4::Window,
    // Theme 04/spotlight's capsule (plan §7 phase 6c): an extra click-away
    // callback invoked alongside the popover-dismiss path below, so the
    // SAME `breadbar-dismiss` surface/click-catcher also collapses the
    // capsule's drawer — see `show_capsule_dismiss`/`hide_dismiss` and
    // `set_on_dismiss`. `None` under every other theme (never set).
    on_dismiss: DismissCallback,
    // The rectangle (local to `dismiss`'s own surface coordinates) that
    // should stay click-through even while the scrim otherwise covers the
    // screen — `show_capsule_dismiss`'s own doc comment explains why this
    // exists and how it's computed. `None` = no hole, the plain
    // margin-based popover behaviour applies instead. Read inside
    // `dismiss`'s own `connect_map` (the input region can only be set once
    // the surface is real — see `surface::set_hit_region`'s doc comment for
    // the same constraint) and, for the case where `dismiss` is already
    // mapped from a prior show, applied immediately too.
    dismiss_hole: DismissHole,
}

impl PanelSet {
    pub fn new(
        monitor: &str,
        connectivity_child: &impl IsA<gtk4::Widget>,
        control_child: &impl IsA<gtk4::Widget>,
        media_child: &impl IsA<gtk4::Widget>,
    ) -> Self {
        let connectivity = make_panel("wifi-popover", connectivity_child, monitor);
        let control = make_panel("control-panel", control_child, monitor);
        let media = make_panel("media-popover", media_child, monitor);
        let dismiss_hole: DismissHole = Rc::new(Cell::new(None));
        let dismiss = make_dismiss(monitor, &dismiss_hole);

        let set = Self {
            connectivity,
            control,
            media,
            dismiss,
            on_dismiss: Rc::new(RefCell::new(None)),
            dismiss_hole,
        };
        set.wire_dismiss();
        set.wire_escape();
        set
    }

    pub fn toggle(&self, which: &gtk4::Window) {
        if which.is_visible() {
            self.hide_all();
        } else {
            self.show(which);
        }
    }

    pub fn show(&self, which: &gtk4::Window) {
        self.hide_panels();
        // A prior capsule search (see `show_capsule_dismiss`) may have left
        // the shared dismiss surface's top margin pushed down past its
        // popover-shaped default — restore it before this popover uses it.
        self.reset_dismiss_margin();
        // Dismiss first so the panel maps above it (same Overlay layer).
        self.dismiss.set_visible(true);
        self.dismiss.present();
        which.set_visible(true);
        which.present();
    }

    pub fn hide_all(&self) {
        self.hide_panels();
        self.dismiss.set_visible(false);
    }

    fn hide_panels(&self) {
        self.connectivity.set_visible(false);
        self.control.set_visible(false);
        self.media.set_visible(false);
    }

    /// Theme 04/spotlight's capsule (plan §7 phase 6c): registers `cb` to
    /// run whenever the shared dismiss surface is clicked, alongside the
    /// popovers' own `hide_all`. `cb` is expected to no-op when the capsule
    /// isn't actually open (matching `close_fn`'s own guard in main.rs), so
    /// this firing on an ordinary popover click-away is harmless.
    pub fn set_on_dismiss(&self, cb: impl Fn() + 'static) {
        *self.on_dismiss.borrow_mut() = Some(Rc::new(cb));
    }

    /// Shows the dismiss scrim with its clickable region starting at
    /// `top_margin` px from the screen top, rather than the theme's own
    /// popover-shaped default. See the call site in main.rs's capsule
    /// `open_fn` for why this needs to be at least the capsule's own row
    /// height plus the drawer's maximum possible height: the dismiss
    /// surface's layer (`overlay`) always renders above the bar's own
    /// (`top`), so if its clickable region ever reached up into where the
    /// drawer is actually drawn, it would swallow clicks meant for a
    /// result row instead of forwarding them.
    ///
    /// Before this fix, that safety was bought with a `set_margin` that
    /// pushed the scrim's *entire width* down by `top_margin` — leaving a
    /// full-screen-wide dead band above it (up to ~470px on a 1200px-tall
    /// display) where a click neither dismissed nor hit anything else, the
    /// "it only sometimes is dismissed when you click somewhere else"
    /// report. `hole`, when known (local-to-this-surface x-start/width, in
    /// `main.rs`'s `capsule_dismiss_hole`), keeps exactly the same
    /// vertical safety margin but scopes the dead band to the capsule's
    /// own column instead of the full width, so everywhere else in that
    /// band is dismiss-clickable too. `None` (geometry unavailable, e.g.
    /// `hyprctl` failed) falls back to the old full-width behaviour rather
    /// than risk a hole in the wrong place.
    pub fn show_capsule_dismiss(&self, top_margin: i32, hole: Option<(i32, i32)>) {
        match hole {
            Some((x, w)) if w > 0 => {
                self.dismiss.set_margin(Edge::Top, 0);
                self.dismiss_hole.set(Some((x, 0, w, top_margin)));
            }
            _ => {
                self.dismiss.set_margin(Edge::Top, top_margin);
                self.dismiss_hole.set(None);
            }
        }
        apply_dismiss_hole(&self.dismiss, &self.dismiss_hole);
        self.dismiss.set_visible(true);
        self.dismiss.present();
    }

    /// Hides the dismiss scrim and restores its margin to the theme's own
    /// popover default, so a later popover `show()` isn't left with a
    /// leftover capsule-sized gap.
    pub fn hide_dismiss(&self) {
        self.reset_dismiss_margin();
        self.dismiss.set_visible(false);
    }

    fn reset_dismiss_margin(&self) {
        let theme = theme::shell_theme();
        if let Some(surf) = theme.surfaces().get("breadbar-dismiss") {
            let top = surf.offset.first().copied().unwrap_or(0.0) as i32;
            self.dismiss.set_margin(Edge::Top, top);
        }
        // A stale capsule-shaped hole must not leak into a popover's own
        // full-width dead zone.
        self.dismiss_hole.set(None);
        apply_dismiss_hole(&self.dismiss, &self.dismiss_hole);
    }

    fn wire_dismiss(&self) {
        let set = self.clone();
        let click = gtk4::GestureClick::new();
        click.set_button(0);
        click.connect_pressed(move |_, _, _, _| {
            set.hide_all();
            if let Some(cb) = set.on_dismiss.borrow().as_ref() {
                cb();
            }
        });
        if let Some(child) = self.dismiss.child() {
            child.add_controller(click);
        } else {
            self.dismiss.add_controller(click);
        }
    }

    fn wire_escape(&self) {
        for win in [&self.connectivity, &self.control, &self.media] {
            let set = self.clone();
            let keys = gtk4::EventControllerKey::new();
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == Key::Escape {
                    set.hide_all();
                    gtk4::glib::Propagation::Stop
                } else {
                    gtk4::glib::Propagation::Proceed
                }
            });
            win.add_controller(keys);
        }
    }
}

fn make_panel(class: &str, child: &impl IsA<gtk4::Widget>, monitor: &str) -> gtk4::Window {
    let window = gtk4::Window::new();
    window.add_css_class("breadbar-panel");
    window.add_css_class(class);
    window.set_decorated(false);
    window.set_resizable(false);
    window.init_layer_shell();
    window.set_namespace(Some("breadbar-panel"));
    crate::surface::apply(&window, "breadbar-panel");
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(KeyboardMode::OnDemand);
    window.set_child(Some(child));
    bind_layer_monitor(&window, monitor);
    theme::bind_output(&window, monitor);
    window.set_visible(false);
    window
}

fn make_dismiss(monitor: &str, hole: &DismissHole) -> gtk4::Window {
    let window = gtk4::Window::new();
    window.add_css_class("breadbar-dismiss");
    window.init_layer_shell();
    window.set_namespace(Some("breadbar-dismiss"));
    // Overlay with the panels, but mapped first so they sit above it.
    // Top margin keeps the island's chips clickable.
    //
    // NOTE — deliberate, not a bug: this surface's top margin is 8px less
    // than `breadbar-panel`'s (see `make_panel` above / `[surfaces.*]` in
    // the active theme). The panels start 8px lower than the dismiss
    // scrim's clickable region. This predates Phase 2 and is preserved
    // exactly for pixel-identical rendering — do not "fix" this gap.
    crate::surface::apply(&window, "breadbar-dismiss");
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(KeyboardMode::None);
    // An empty window never maps a hit region. A filling child + a hair of
    // alpha is what actually receives the click-away.
    let hit = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    hit.add_css_class("breadbar-dismiss-hit");
    hit.set_hexpand(true);
    hit.set_vexpand(true);
    window.set_child(Some(&hit));
    bind_layer_monitor(&window, monitor);
    theme::bind_output(&window, monitor);
    window.set_visible(false);
    // The underlying `GdkSurface` (and therefore `window.surface()`, which
    // `apply_dismiss_hole` needs) doesn't exist until the window is mapped
    // — same constraint `surface::set_hit_region` documents. This surface
    // gets hidden/shown repeatedly (every popover open/close, every
    // capsule search), and GTK4 unmaps-then-remaps a toplevel each time
    // its visibility toggles off then on, so re-applying here on every
    // `map` (not just the first) is what keeps a freshly (re)shown surface
    // honouring whatever hole was set before this particular `present()`.
    {
        let hole = Rc::clone(hole);
        window.connect_map(move |win| apply_dismiss_hole(win, &hole));
    }
    window
}

/// Sets `dismiss`'s click-away input region to "everywhere" minus `hole`
/// (if any) — see `PanelSet::show_capsule_dismiss`'s doc comment for why.
/// Only takes effect once `dismiss.surface()` is real, i.e. the window is
/// currently mapped; harmlessly no-ops otherwise (the `connect_map` hook in
/// `make_dismiss` re-runs this the moment that stops being true).
fn apply_dismiss_hole(dismiss: &gtk4::Window, hole: &DismissHole) {
    let Some(surface) = dismiss.surface() else {
        return;
    };
    match hole.get() {
        Some((x, y, w, h)) => {
            let canvas = gtk4::cairo::RectangleInt::new(
                -HOLE_CANVAS_SPAN,
                -HOLE_CANVAS_SPAN,
                HOLE_CANVAS_SPAN * 2,
                HOLE_CANVAS_SPAN * 2,
            );
            let region = gtk4::cairo::Region::create_rectangle(&canvas);
            let punch = gtk4::cairo::RectangleInt::new(x, y, w, h);
            if region.subtract_rectangle(&punch).is_ok() {
                surface.set_input_region(Some(&region));
            } else {
                // Punching the hole failed for some reason (an invalid
                // cairo status on a plain rectangle op, effectively
                // unreachable in practice) — falling back to `None` (the
                // protocol's documented "no input region set: whole
                // surface hits") is still safer than leaving whatever
                // region predates this call in place, which could be
                // stale from a completely different mode (e.g. an old
                // popover-shaped margin-only region with no hole at all).
                eprintln!(
                    "breadbar: could not punch capsule hole in dismiss scrim's input region"
                );
                surface.set_input_region(None);
            }
        }
        None => surface.set_input_region(None),
    }
}

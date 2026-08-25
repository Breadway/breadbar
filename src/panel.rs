//! Standalone layer-shell panels for wifi / control / media.
//!
//! GTK `Popover` is an xdg_popup child of the island, so it paints over the
//! bar and Hyprland can only fade it. These are their own surfaces, parked
//! *below* the exclusive zone, and Hyprland slides `breadbar-panel` in from
//! the right.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::gdk::Key;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, LayerShell};

use crate::{bind_layer_monitor, theme};

/// A boxed, ref-counted, optionally-unset click-away callback — see
/// `PanelSet::on_dismiss`'s own doc comment.
type DismissCallback = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

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
        let dismiss = make_dismiss(monitor);

        let set = Self {
            connectivity,
            control,
            media,
            dismiss,
            on_dismiss: Rc::new(RefCell::new(None)),
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
    pub fn show_capsule_dismiss(&self, top_margin: i32) {
        self.dismiss.set_margin(Edge::Top, top_margin);
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

fn make_dismiss(monitor: &str) -> gtk4::Window {
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
    window
}

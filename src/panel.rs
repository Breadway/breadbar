//! Standalone layer-shell panels for wifi / control / media.
//!
//! GTK `Popover` is an xdg_popup child of the island, so it paints over the
//! bar and Hyprland can only fade it. These are their own surfaces, parked
//! *below* the exclusive zone, and Hyprland slides `breadbar-panel` in from
//! the right.

use gtk4::gdk::Key;
use gtk4::prelude::*;
use gtk4_layer_shell::{KeyboardMode, LayerShell};

use crate::{bind_layer_monitor, theme};

#[derive(Clone)]
pub struct PanelSet {
    pub connectivity: gtk4::Window,
    pub control: gtk4::Window,
    pub media: gtk4::Window,
    dismiss: gtk4::Window,
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

    fn wire_dismiss(&self) {
        let set = self.clone();
        let click = gtk4::GestureClick::new();
        click.set_button(0);
        click.connect_pressed(move |_, _, _, _| {
            set.hide_all();
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

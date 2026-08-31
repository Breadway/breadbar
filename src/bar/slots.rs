//! Module registry for the theme manifest's `[bar.slots]` (plan Phase 3a),
//! extended in Phase 3b to also route `widget:<key>` entries.
//!
//! Each bar module (`workspaces`, `media`, `clock`, `volume`, `wifi`,
//! `battery`, `control`, …) is still built exactly where it always was in
//! `main.rs` — this registry only decouples the ORDER in which those
//! already-constructed widgets get appended into the left/centre/right
//! containers from the fixed source-code order they were built in. main.rs
//! registers each widget by its manifest module name once construction is
//! done, then walks `ShellTheme::slots()` to append them in the theme's
//! order instead of a hardcoded sequence.
//!
//! A slot entry may also be `widget:<key>`, where `<key>` is either a
//! `WidgetPlacement` alias (`right_of_workspaces`, `left_of_clock`,
//! `right_of_clock`, `left_of_stats`, `tray`) or a Lua module name (see
//! `bread_shared::widget::WidgetSpec::module`) — these route through
//! `for_each_in_slot`'s `on_widget` callback rather than this registry,
//! since their containers are Lua-widget slots created on demand by the
//! caller, not modules registered here. `WidgetPlacement` itself is a wire
//! type from `bread-shared` and is never referenced in this file.

use gtk4::prelude::*;
use std::collections::HashMap;

/// Maps a `[bar.slots]` module name to its already-built widget.
#[derive(Default)]
pub struct ModuleRegistry(HashMap<&'static str, gtk4::Widget>);

impl ModuleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `widget` under `name` (a `[bar.slots]` module name, e.g.
    /// `"workspaces"` or `"clock"`).
    pub fn register(&mut self, name: &'static str, widget: &impl IsA<gtk4::Widget>) {
        self.0.insert(name, widget.clone().upcast());
    }

    /// Walks every entry named in `names` (a manifest slot list, in theme
    /// order). A `widget:<key>` entry calls `on_widget(key)`, letting the
    /// caller create-or-fetch that Lua widget container and append it at
    /// this exact position. Anything else is looked up as a registered
    /// module name and passed to `on_module`; a name with no registered
    /// widget is logged and skipped — an unrecognized or unmapped module in
    /// a theme manifest must never crash the bar.
    pub fn for_each_in_slot(
        &self,
        names: &[String],
        mut on_module: impl FnMut(&str, &gtk4::Widget),
        mut on_widget: impl FnMut(&str),
    ) {
        for name in names {
            if let Some(key) = name.strip_prefix("widget:") {
                on_widget(key);
                continue;
            }
            match self.0.get(name.as_str()) {
                Some(widget) => on_module(name, widget),
                None => eprintln!("breadbar: [bar.slots] names unknown module '{name}' — skipping"),
            }
        }
    }
}

/// Returns the widget container keyed `key` in `containers`, creating it
/// (a plain horizontal box, styled like every other Lua widget slot) on
/// first use. Called from `for_each_in_slot`'s `on_widget` callback so a
/// `widget:<key>` slot entry gets a container the first time a theme
/// places one there, regardless of whether `key` is a `WidgetPlacement`
/// alias or a Lua module name — `reconcile_widgets` (main.rs) is what
/// gives that distinction meaning when it routes specs into these
/// containers.
pub fn widget_slot_container(
    containers: &mut HashMap<String, gtk4::Box>,
    key: &str,
) -> gtk4::Box {
    containers
        .entry(key.to_string())
        .or_insert_with(|| {
            let b = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
            b.add_css_class("bread-widget-slot");
            b
        })
        .clone()
}

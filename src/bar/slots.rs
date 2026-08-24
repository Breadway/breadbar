//! Module registry for the theme manifest's `[bar.slots]` (plan Phase 3a).
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
//! The Lua-declared `widget_*` containers (`WidgetPlacement`) are NOT part
//! of this registry — their fixed interleave (right-of-workspaces,
//! left/right-of-clock, left-of-stats) stays exactly as it is today.
//! Generalizing their placement is Phase 3b, not this task.

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

    /// Appends every module named in `names` (a manifest slot list, in
    /// theme order) into `container` via `on_widget`, which lets the
    /// caller interleave fixed Lua widget containers around specific
    /// modules (e.g. the clock). A name with no registered widget is
    /// logged and skipped — an unrecognized or unmapped module in a theme
    /// manifest must never crash the bar.
    pub fn for_each_in_slot(
        &self,
        names: &[String],
        mut on_widget: impl FnMut(&str, &gtk4::Widget),
    ) {
        for name in names {
            match self.0.get(name.as_str()) {
                Some(widget) => on_widget(name, widget),
                None => eprintln!("breadbar: [bar.slots] names unknown module '{name}' — skipping"),
            }
        }
    }
}

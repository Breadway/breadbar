use bread_theme::shell::ShellTheme;
use bread_theme::{gtk as bgtk, ink_on, load_palette, Palette};
use gtk4::glib::WeakRef;
use gtk4::prelude::{Cast, IsA, ObjectExt};
use gtk4::CssProvider;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

thread_local! {
    static USER_PROVIDER: RefCell<Option<CssProvider>> = const { RefCell::new(None) };

    // Every persistent (`bind_output`) window, kept as a weak ref so a
    // dropped satellite monitor's windows fall out on the next sweep. On a
    // shell-theme change (`reload`) each live one is re-bound so its
    // per-output USER-10/USER-9 providers rebuild against the new theme —
    // see `reload`'s doc comment for why `apply()` alone can't reach them.
    static BOUND_WINDOWS: RefCell<Vec<(WeakRef<gtk4::Widget>, String)>> =
        const { RefCell::new(Vec::new()) };

    // Outputs already warned about a missing `palettes/<output>.json`, so
    // the fallback-to-global-palette notice fires once per output, not once
    // per window bound to it (bar + 4 panels + dialogs).
    static PALETTE_FALLBACK_WARNED: RefCell<HashSet<String>> =
        RefCell::new(HashSet::new());

    // Loaded lazily on first access and cached — the underlying
    // `bread_theme::shell::load()` call happens at most once per process,
    // not once per read site (plan §5/§6, Phase 2). Stored as an `Rc` so
    // window/surface-geometry call sites elsewhere in the crate can hold a
    // cheap clone rather than re-reading the cell each time.
    static SHELL_THEME: RefCell<Rc<ShellTheme>> =
        RefCell::new(Rc::new(bread_theme::shell::load()));
}

/// The active shell theme — window geometry, `[surfaces.*]`, and CSS tokens
/// (plan §5). Every consumer (this module's own `load_css`, plus main.rs,
/// osd.rs, panel.rs, notifications/, and `surface::apply`) reads through
/// this single shared instance instead of calling `bread_theme::shell::load()`
/// itself.
pub fn shell_theme() -> Rc<ShellTheme> {
    SHELL_THEME.with(|cell| cell.borrow().clone())
}

/// Replaces the shared shell theme in place. Only used by the `theme.toml`
/// hot-reload watch (see [`watch_hot_reload`]), which calls [`reload`] straight
/// after. CSS token values (colours, radii, springs, alphas, the `light` axis,
/// launcher geometry) re-resolve live via [`reload`]. A window-geometry or
/// widget-structure change is caught by [`needs_restart`] *before* this point
/// and triggers a re-exec instead, since those are read once at construction.
pub fn set_shell_theme(theme: ShellTheme) {
    SHELL_THEME.with(|cell| *cell.borrow_mut() = Rc::new(theme));
}

/// One bar-chip height per row (vol/wifi/battery/menu/media, plus the
/// Trail/Pill workspace pills) — the same value `bread_theme::shell`'s CSS
/// derivation bakes into `min-height`, re-exposed here for
/// `bar::workspaces`' `set_size_request` GTK minimum, which a CSS
/// `min-height` alone cannot out-rank.
pub fn approved_chip_height(style: bread_theme::shell::WorkspaceStyle) -> i64 {
    bread_theme::shell::chip_height(style)
}

/// Returns the ink colour for icon tinting in the stats bar — the same
/// luminance-picked colour the bar's text uses, so icons stay legible on the
/// bar whatever lightness pywal gives the background.
///
/// FIXED — axis 1, daylight: this used to be unconditionally
/// `ink_on(&load_palette().background)`. `load_palette().background` is
/// `bread_theme::palette::FIXED_BACKGROUND` (`"#0c0c0c"`), pinned dark
/// regardless of pywal OR the active shell theme (see that constant's own
/// doc comment) — so this always resolved to the SAME near-white value,
/// baked directly into a rasterised SVG texture at icon-build time
/// (`svg_texture_sized`, the only call site), completely outside CSS and
/// therefore untouched by `load_css`'s own `panel`/`ink` swap. Every icon
/// built through [`crate::svg_image`] (volume, wifi, battery, hamburger,
/// media transport, the OSD glyph) was near-white on
/// every existing (dark) theme, which read as correct by construction —
/// until daylight's near-white paper pills made the SAME near-white glyph
/// nearly invisible against its own background. Confirmed empirically
/// (isolated `bread-capture` OSD-volume screenshot, pre-fix) before this
/// fix. Mirrors `load_css`'s own `ink` local exactly: the dark theme's
/// unchanged `ink_on(background)` (near-white), or — for a light theme —
/// `background` itself (the fixed dark hex IS the correct dark ink, the
/// same identity `load_css`'s `ink = "@bg"` case relies on).
///
/// NOT per-output, and correct today only by construction: the ink is
/// derived purely from `Palette.background`, and `bread_theme::output`
/// pins EVERY output's `background` to `FIXED_BACKGROUND` (per-output JSON
/// stores accents only — `color1..6` — and `from_wal_json` always forces
/// the fixed dark bg/surface/overlay/fg back in). So `load_palette()` and
/// `load_palette_for(any_output)` yield byte-identical ink, and baking the
/// global value into every monitor's icon textures happens to be right.
/// A `fg_color_for(output)` accessor used to sit here for the day that
/// stops being true; it was dead code (icons are baked by
/// `crate::svg_texture_sized`, reached only via the free function
/// `crate::svg_image`, which has no output name in scope — threading one
/// through every call site across main.rs, osd.rs, panel.rs and the bar
/// modules is the real fix and is out of scope here).
// TODO(per-monitor-ink): if per-output palettes ever diverge beyond accents
// (see the `light` axis being process-global, TODO in `load_css`), thread the
// bound output name into `svg_texture_sized` and resolve ink via
// `load_palette_for(output).background` instead of the global palette.
pub fn fg_color() -> String {
    let p = load_palette();
    if shell_theme().tokens().light() {
        p.background.clone()
    } else {
        ink_on(&p.background).to_string()
    }
}

/// Bind this window (and its popover children) to `output`'s palette.
///
/// App CSS still uses `@accent` / `@on-bg` tokens; `bind_window_with_app_css`
/// resolves them against that output. Display-level [`apply`] stays as the
/// SIGHUP / single-output fallback.
///
/// The window is also recorded in `BOUND_WINDOWS` (weak) so [`reload`] can
/// re-bind it when the shell theme changes — the per-output providers this
/// installs at USER-10/USER-9 shadow the display-global one [`apply`]
/// reloads, so nothing else would refresh them.
pub fn bind_output(widget: &impl IsA<gtk4::Widget>, output: &str) {
    warn_if_palette_missing(output);
    bgtk::bind_window_with_app_css(widget, output, load_css_for);

    let w: gtk4::Widget = widget.clone().upcast();
    BOUND_WINDOWS.with(|cell| {
        let mut v = cell.borrow_mut();
        // Drop dead entries and any prior registration for this same widget
        // (re-bind on monitor move / repeated `bind_output`) before pushing.
        v.retain(|(weak, _)| weak.upgrade().is_some_and(|other| other != w));
        v.push((w.downgrade(), output.to_string()));
    });
}

/// Pin a transient satellite window (OSD, notification toast, history) to
/// the currently focused Hyprland output and bind its palette to that same
/// output. Call immediately before `set_visible(true)` on a long-lived,
/// *reused* window.
///
/// [`bind_auto`] alone is unreliable for these: the window outlives many
/// show/hide cycles, its `GdkSurface` is recreated on each map, and
/// `bind_auto` only re-hooks `enter-monitor` on realize — so a window first
/// shown on the primary keeps the primary's accent when it later reappears
/// on a secondary. An explicit pin (`set_monitor` + [`bind_output`]) makes
/// the accent correct from the first frame. No-op outside a Hyprland
/// session (e.g. screenshot mode), where the display-global [`apply`]
/// fallback already covers the single output.
pub fn pin_focused_output<W>(window: &W)
where
    W: IsA<gtk4::Widget> + gtk4_layer_shell::LayerShell,
{
    if let Some(name) = crate::primary_hypr_monitor() {
        crate::bind_layer_monitor(window, &name);
        bind_output(window, &name);
    }
}

/// One-time notice when an output has no `palettes/<output>.json` and
/// `bind_window`/`load_palette_for` will silently fall back to the global
/// pywal palette — its bar then shows the primary monitor's accent, which
/// is easy to misread as "per-monitor theming is broken". Fires once per
/// output name for the process.
fn warn_if_palette_missing(output: &str) {
    let path = bread_theme::output_palette_path(output);
    if path.exists() {
        return;
    }
    PALETTE_FALLBACK_WARNED.with(|cell| {
        if cell.borrow_mut().insert(output.to_string()) {
            tracing::warn!(
                output = %output,
                path = %path.display(),
                "no per-output palette; using the global palette for this monitor \
                 (run `bread-theme generate-output {output} <wallpaper>` to give it its own accent)"
            );
        }
    });
}

/// Re-bind every live persistent window against the current shell theme /
/// per-output palette. Called by [`reload`].
fn rebind_bound_windows() {
    BOUND_WINDOWS.with(|cell| {
        let mut v = cell.borrow_mut();
        v.retain(|(weak, output)| match weak.upgrade() {
            Some(w) => {
                bgtk::bind_window_with_app_css(&w, output, load_css_for);
                true
            }
            None => false,
        });
    });
}

/// Bind a satellite window (notification, history, OSD, wifi dialog) to
/// whichever output it is actually rendered on.
pub fn bind_auto(window: &impl IsA<gtk4::Native>) {
    bgtk::bind_window_auto_with_app_css(window, load_css_for);
}

/// The active shell theme's full stylesheet — `bread_theme::shell::ShellTheme::css`.
/// `@name` palette references are left intact for `bind_window_with_app_css`
/// to inline against the bound output's palette; `css()` ignores its palette
/// argument for exactly that reason, so this passes the default.
fn load_css_for(_palette: &Palette) -> String {
    shell_theme().css(&Palette::default())
}

/// Apply (or reload) the theme CSS. Safe to call from `glib::MainContext::invoke`.
pub fn apply() {
    // Shared ecosystem base (fonts, palette, generic widgets) — applied first
    // (and self-reloading) so breadbar's own rules below layer on top.
    bgtk::apply_shared();

    // The active shell theme's stylesheet, hot-reloaded on `bread-theme
    // reload` / a `theme.toml` edit so the bar recolours without a restart.
    bgtk::apply_app_css(|| load_css_for(&Palette::default()));

    // User override, at USER priority (beats every `bind_output` provider).
    // This is a single DISPLAY-GLOBAL provider: `@name` colour references in
    // it (`@accent`, `@on-bg`, …) resolve against the display-global
    // `@define-color` block, i.e. the primary/global palette — NOT the
    // per-monitor palette of whichever bar the rule paints. On a
    // multi-monitor setup a `style.css` that hard-codes hex is applied
    // identically everywhere (predictable); one that leans on `@accent`
    // tracks the primary monitor only. Documented in README.
    // TODO(per-monitor-user-css): to make `@accent` in user CSS follow each
    // bar's own output, fold this file into `load_css_for` so it goes
    // through `bind_window`'s per-output `resolve_color_names` — at the cost
    // of dropping from USER (800) to USER-9 priority.
    let home = std::env::var("HOME").unwrap_or_default();
    let user_path = std::path::PathBuf::from(format!("{home}/.config/breadbar/style.css"));
    USER_PROVIDER.with(|cell| bgtk::apply_user_css(&user_path, cell));
}

/// Full theme refresh: [`apply`] the display-global providers, then re-bind
/// every persistent window so its widget-level per-output providers (which
/// GTK4 ranks *above* the display-global one regardless of selector) also
/// rebuild, and finally re-bake the icon textures whose tint is resolved in
/// Rust outside CSS (`fg_color`). This is what a pywal palette change gets
/// for free via `apply_app_css` + `reload_binds_for_sanitized`; a
/// `theme.toml` edit or a SIGHUP has to drive it explicitly.
///
/// Safe to call from `glib::MainContext::invoke` (it is `fn()`).
pub fn reload() {
    apply();
    rebind_bound_windows();
    crate::rebake_icons();
}

thread_local! {
    // `bread_theme::shell::ThemeWatch`, not a bare `gio::FileMonitor`: the
    // watch now re-arms itself onto a new theme's directory when the active
    // theme id changes underneath it (see that type's doc comment), so the
    // handle we keep alive is opaque, not a single fixed monitor.
    static SHELL_THEME_MONITOR: RefCell<Option<bread_theme::shell::ThemeWatch>> =
        const { RefCell::new(None) };
}

/// True if going from `cur` to `next` changes something the live path
/// (`App::rebuild_from_theme`) cannot yet apply, so breadbar re-execs:
/// - **workspace/clock style** (`modules()` — a widget-type swap: flip digits
///   vs plain label, trail vs pill vs label-less dots)
/// - **launcher mode** (Overlay vs Embedded — the capsule drawer wiring)
/// - **`[panel].sections`** — the control-panel body is assembled once in `init`
/// - **`[osd].enabled`** — the volume/brightness watcher threads are started once
///
/// Everything else applies live: CSS via [`reload`], `[bar.window]` geometry
/// via `apply_window_spec`, `[bar.slots]` order via `assemble_bar_slots`,
/// `[[bar.widget]]` via the poller refresh, `[panel].min_width` /
/// `[osd].dismiss_ms` via CSS / the next OSD.
pub(crate) fn needs_restart(cur: &ShellTheme, next: &ShellTheme) -> bool {
    cur.modules() != next.modules()
        || cur.launcher().mode != next.launcher().mode
        || cur.panel().sections != next.panel().sections
        || cur.osd().enabled != next.osd().enabled
}

/// Wires `bread_theme::shell::watch()` (plan §10) — fires whenever the active
/// theme's directory changes on disk, or `shell.toml`'s `active` moves to a
/// different id.
///
/// - **Live change** — CSS, `[bar.window]` geometry, `[bar.slots]` order: the
///   watch swaps the process-global theme and calls `on_live_change`, wired to
///   `AppInput::ShellThemeChanged` (→ geometry re-applied, slots re-filled,
///   `reload()` for CSS/icons). No restart, no blink.
/// - **Workspace/clock style or launcher mode** ([`needs_restart`]) — a
///   widget-type swap the live path can't do yet, so breadbar re-execs
///   itself: one blink.
///
/// Call once at startup (primary instance only — every satellite calling this
/// would just re-arm the same watch redundantly).
pub fn watch_hot_reload(on_live_change: impl Fn(ShellTheme) + 'static) {
    let monitor = bread_theme::shell::watch(move |new_theme| {
        if needs_restart(&shell_theme(), &new_theme) {
            tracing::info!(
                theme = %new_theme.id(),
                "shell theme workspace/clock style or launcher mode changed — restarting breadbar"
            );
            // Returns only if the restart could not be launched — then fall
            // through to an in-place reload (CSS + geometry + slots update;
            // the style/mode swap still needs a manual restart).
            self_restart();
            tracing::warn!("self-restart failed — applying CSS + geometry in place");
            set_shell_theme(new_theme);
            reload();
        } else {
            tracing::info!(
                theme = %new_theme.id(),
                "shell theme changed (CSS / geometry / slots) — applying in place"
            );
            set_shell_theme(new_theme.clone());
            on_live_change(new_theme);
        }
    });
    SHELL_THEME_MONITOR.with(|cell| *cell.borrow_mut() = Some(monitor));
}

/// Start a fresh `breadbar` (same argv) and exit this one, so a shell-theme
/// switch picks up window geometry and widget structure, not just CSS.
///
/// Deliberately spawn-then-`exit`, NOT `exec`: `exec` keeps this PID and its
/// child processes, so any in-flight `breadcrumbs`/`nmcli` the bar had
/// running is inherited by a fresh tokio runtime that never `wait()`s for
/// it — a permanent `<defunct>` once it exits. Letting this process exit
/// instead reparents those children to init, which reaps them.
///
/// Returns only when the replacement could not be launched, leaving the
/// caller to fall back to an in-place token reload.
fn self_restart() {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "current_exe() failed");
            return;
        }
    };
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    match std::process::Command::new(&exe).args(&args).spawn() {
        Ok(_) => std::process::exit(0),
        Err(e) => tracing::error!(error = %e, exe = %exe.display(), "spawn() failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::{needs_restart, shell_theme, Palette};
    use bread_theme::shell::load_named;

    #[test]
    fn needs_restart_true_between_structurally_different_builtins() {
        let lm = load_named("liquid-motion").unwrap();
        let gw = load_named("glass-workbench").unwrap();
        // Different bar height + workspace/clock style + slot lists.
        assert!(needs_restart(&lm, &gw));
        assert!(needs_restart(&gw, &lm));
    }

    #[test]
    fn needs_restart_false_for_the_same_theme() {
        let a = load_named("daylight").unwrap();
        let b = load_named("daylight").unwrap();
        assert!(!needs_restart(&a, &b));
    }

    /// Every `#rrggbb`-style literal in a string, with a trailing
    /// alphanumeric/`_` boundary (so `#000000` counts, `#fff` counts, but a
    /// CSS id selector fragment like `#foo-bar` in prose would still be
    /// caught — there is none). Hand-rolled to avoid a `regex` dep.
    fn hex_literals(s: &str) -> Vec<String> {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] != b'#' {
                i += 1;
                continue;
            }
            let start = i + 1;
            let mut j = start;
            while j < b.len() && b[j].is_ascii_hexdigit() {
                j += 1;
            }
            let len = j - start;
            let boundary = j >= b.len() || !(b[j].is_ascii_alphanumeric() || b[j] == b'_');
            if (3..=8).contains(&len) && boundary {
                out.push(s[i..j].to_string());
            }
            i = j.max(i + 1);
        }
        out
    }

    /// Contract (finding #7 / `load_css_for`): the active theme's rendered
    /// stylesheet never carries a raw colour literal — every colour is a
    /// bread-theme token name (`@accent`, `@on-bg`, …) so `bind_window` can
    /// inline it against the bound output's palette. A hardcoded hex in the
    /// `bread-theme` base template would silently break per-monitor theming
    /// on every bound surface. Allow-list: pure black, used only as a
    /// ~2%-alpha wash on the invisible dismiss-scrim hit surface.
    #[test]
    fn load_css_emits_no_raw_hex_colours() {
        const ALLOWED: &[&str] = &["#000000"];
        let css = shell_theme().css(&Palette::default());
        let offending: Vec<String> = hex_literals(&css)
            .into_iter()
            .filter(|h| !ALLOWED.contains(&h.to_ascii_lowercase().as_str()))
            .collect();
        assert!(
            offending.is_empty(),
            "load_css() emitted raw hex colour literal(s) {offending:?} — use a bread-theme \
             @token instead, or extend ALLOWED if this is a genuinely palette-independent colour"
        );
    }

    #[test]
    fn hex_literal_scanner_sanity() {
        assert_eq!(hex_literals("alpha(#000000, 0.02)"), ["#000000"]);
        assert_eq!(hex_literals("border: 1px solid #abcdef;"), ["#abcdef"]);
        assert!(hex_literals("@accent alpha(@on-bg, 0.1)").is_empty());
        assert!(hex_literals("nth-child(2)").is_empty());
    }
}

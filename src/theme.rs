use bread_theme::shell::ShellTheme;
use bread_theme::{gtk as bgtk, ink_on, load_palette, load_palette_for, Palette};
use gtk4::prelude::IsA;
use gtk4::CssProvider;
use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    static USER_PROVIDER: RefCell<Option<CssProvider>> = const { RefCell::new(None) };
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

/// Replaces the shared shell theme in place. Only used by the optional
/// `theme.toml` hot-reload watch (see `watch_hot_reload` below) — per plan
/// §10, a window-spec change (anchors, margins, exclusive zone, keyboard)
/// still needs a restart to take effect, since those are read once at
/// window-construction time; only CSS token values re-resolve live, the
/// next time `load_css` runs.
pub fn set_shell_theme(theme: ShellTheme) {
    SHELL_THEME.with(|cell| *cell.borrow_mut() = Rc::new(theme));
}

/// The one bar-chip height every chip in the row shares (vol/wifi/battery/
/// menu/media, and the workspace pills for Trail/Pill styles) — see the
/// `chip_h` local in `load_css` for why this is a hardcoded per-`
/// WorkspaceStyle` override rather than `Tokens::chip_height()`. Used both
/// by that CSS and by `bar::workspaces::make_button`'s `set_size_request`,
/// which otherwise still forces the stale token value as a hard GTK
/// minimum that a CSS `min-height` alone cannot out-rank.
pub fn approved_chip_height(style: bread_theme::shell::WorkspaceStyle) -> i64 {
    use bread_theme::shell::WorkspaceStyle::*;
    match style {
        Trail => 26,
        Pill | Dots => 22,
    }
}

fn load_css() -> String {
    // breadbar-specific rules only — fonts, base colours, and generic widgets
    // come from the shared ecosystem stylesheet (applied first in `apply()`).
    // Colour is set on each surface (bar, active workspace pill, notification
    // card) and child labels inherit it, so text stays legible whatever lightness
    // pywal hands a given slot. `on_*` are luminance-picked ink (black/white) for
    // that background — the pywal hues themselves are untouched.
    //
    // Glass workbench: 16px island on the bar, 12px cards/popovers, pill OSD.
    // Hyprland `layerrule = blur, breadbar` frosts the translucent fills —
    // the CSS just leaves alpha. Colours are bread-theme tokens so pywal
    // accents (`@accent`) flow through on SIGHUP / `bread-theme reload`.
    //
    // These ~250 lines are breadbar-specific chrome (notifications, wifi
    // popover, control panel, media widget) that `ShellTheme::css()` does
    // not template — only the window/workspace/clock chrome the manifest's
    // own concepts model does (plan §6 scope note). This function stays
    // hand-written CSS; it now just reads its radius/pad/easing numbers from
    // the theme's tokens instead of hardcoding them.
    let theme = shell_theme();
    let tokens = theme.tokens();
    let radius = format!("{}px", tokens.radius_card());
    let radius_bar = format!("{}px", tokens.radius_bar());
    let radius_sm = format!("{}px", tokens.radius_sm());
    let radius_pill = format!("{}px", tokens.radius_pill());
    let pad = format!("{}px", tokens.pad());
    // Two curves, not one: `spring` is the overshoot/bounce curve (clock
    // flips, pop-ins, the workspace caret draw); `spring_settle` is the
    // flatter curve used for hovers and background/opacity transitions.
    // Do not collapse these — they read differently and cover different
    // sites below (see the Phase 0 constant inventory).
    let spring = tokens.spring();
    let spring_settle = tokens.spring_settle();
    let bg_alpha = tokens.bg_alpha();
    // Palette token NAME (never hex — see every builtin theme.toml's own
    // comment on this), used below by the dots/launcher-entry/drawer rules.
    // liquid-motion/glass-workbench never render those (see the match arms
    // and unconditional-but-unused block below), so this being "accent" vs
    // "green" vs "pink" per theme has no visible effect on them.
    let accent_from = tokens.accent_from();
    // `accent_to` (Trail's own gradient end stop) and `accent2` (a second,
    // independent accent — daylight's amber equaliser, distinct from its
    // teal workspace-trail) — both palette token names, never hex, same
    // reasoning as `accent_from` above.
    let accent_to = tokens.accent_to();
    let accent2 = tokens.accent2();

    // Axis 1 (daylight, plan §11 phase 7): `tokens.light()` — see that
    // method's doc comment (bread-theme) for the full reasoning. Every
    // surface/ink pair in this stylesheet through spotlight hardcoded
    // `@bg` (a FIXED, never-pywal-derived dark hex — see
    // `bread_theme::palette`'s `FIXED_BACKGROUND`) as the translucent
    // surface fill and `@on-bg` (that fixed dark colour's computed-legible,
    // therefore always near-white, ink) as the text/wash colour. That is
    // exactly backwards for an ink-on-paper theme: `panel`/`ink` swap which
    // of those two FIXED, anti-correlated tokens plays which role. This is
    // NOT a general "pick any light surface colour" mechanism — it works
    // only because `@bg` is pinned dark and `@on-bg` is its computed
    // opposite, by construction, regardless of pywal. See the task report
    // for the full inventory of every site this swap had to reach.
    let light = tokens.light();
    let (panel, ink): (&str, &str) = if light { ("@on-bg", "@bg") } else { ("@bg", "@on-bg") };
    // The notification/history/OSD/wifi-add-dialog cards (0.70) and the
    // control/wifi/media popover window (0.72) hardcode their own alpha
    // literals independent of `tokens.bg_alpha()` — reasonable for a dark
    // "glass" surface (0.70-0.72 alpha over a dark fill still reads as
    // deliberately translucent glass), but daylight's demo draws these as
    // fully OPAQUE paper (`.note`/`.osd { background: var(--paper) }`, no
    // alpha at all) — at the old 0.70 literal, daylight's near-white
    // `{panel}` fill instead reads as pale, background-tinted glass, which
    // is exactly the "physical object" read the design brief calls for
    // NOT doing. Swapping these two literals wholesale to `{bg_alpha}`
    // would also move liquid-motion/glass-workbench/spotlight's own
    // rendering (0.70/0.72 -> 0.72/0.82 for spotlight) — a real, visible
    // regression the task rules out — so this only substitutes `bg_alpha`
    // (0.94 for daylight) in for `light` themes and keeps the exact prior
    // literal for every other theme.
    let card_alpha = if light { bg_alpha } else { 0.70 };
    let panel_surface_alpha = if light { bg_alpha } else { 0.72 };
    // The OSD/widget-node progress troughs' UNFILLED track is
    // `alpha(@accent, 0.25)` — a teal-at-25%-alpha tint that reads as a
    // faint, visible track against a dark pill (every theme through
    // spotlight), but is nearly indistinguishable from daylight's own
    // near-white pill (confirmed empirically: the OSD volume slider's
    // empty track was barely visible against its own paper background).
    // `{ink}`-based for light themes gives a neutral faint-dark track
    // instead, independent of whatever hue the accent happens to be;
    // every other theme's literal `alpha(@accent, 0.25)` is unchanged.
    let trough_bg = if light {
        format!("alpha({ink}, 0.14)")
    } else {
        "alpha(@accent, 0.25)".to_string()
    };
    // `[launcher].search_radius` (plan §7 phase 6c) — `LauncherMode::
    // Embedded` only (spotlight); `.launcher().radius` itself already
    // equals `radius_bar` for that theme (see its own theme.toml comment),
    // so a theme that omits `search_radius` gets `radius_search ==
    // radius_bar` here too, i.e. no visible shrink, matching bread-theme's
    // own "default to the idle value" fallback.
    let radius_search = format!("{}px", theme.launcher().search_radius);

    // `tokens.bar_border()` (plan §11 Phase 5): "full" (default, liquid-
    // motion's floating island) draws a border on all four edges; "bottom"
    // (glass-workbench's flush edge-to-edge bar) draws only the hairline
    // the demo's `.bar { border-bottom: 1px solid #ffffff12 }` calls for —
    // a full border on a bar flush against the screen's top/left/right
    // edges would otherwise show as a stray line along those edges an
    // island never has to worry about. Reused below for the centerbox's
    // horizontal padding too: the flush bar's demo padding (`0 12px`,
    // symmetric) differs from the island's own asymmetric `0 8px 0 6px`.
    let flush = tokens.bar_border() == "bottom";
    // Axis 3 (daylight, plan §11 phase 7): `bar_border == "segmented"` —
    // `window.breadbar` itself draws NO fill/border/radius/shadow at all
    // (fully transparent); the bar's three slot-group containers
    // (`workspace_row`/`center_area`/`stats_box`, each carrying a
    // `.bar-segment` class added unconditionally in main.rs) draw their own
    // pill surfaces instead — see `segment_css` below. This is what lets
    // one GTK window read as three detached floating pills rather than one
    // continuous strip; see the task report for exactly what this can and
    // can't express (the window is still ONE input-hit-testable surface —
    // clicking in a transparent gap between pills does not click through to
    // whatever's behind the bar, only true multi-window segmentation would
    // do that).
    let segmented = tokens.bar_border() == "segmented";
    let window_chrome = if segmented {
        "background-color: transparent; border: none; box-shadow: none;".to_string()
    } else if flush {
        format!(
            "background-color: alpha({panel}, {bg_alpha}); border: none; \
             border-bottom: 1px solid alpha({ink}, 0.07);"
        )
    } else {
        format!("background-color: alpha({panel}, {bg_alpha}); border: 1px solid alpha({ink}, 0.08);")
    };
    let bar_radius = if segmented {
        "0px".to_string()
    } else {
        radius_bar.clone()
    };
    let centerbox_padding = if flush || segmented { "0 14px" } else { "0 8px 0 6px" };
    // The three detached pills themselves — a no-op empty rule under every
    // non-segmented theme, so `.bar-segment` (added unconditionally to all
    // three slot-group boxes in main.rs) renders nothing extra for them.
    // Demo: `.seg { background: rgba(255,255,255,.94); border: 1px solid
    // rgba(26,29,34,.10); border-radius: 14px;
    // box-shadow: 0 2px 10px rgba(26,29,34,.13), 0 0 0 .5px rgba(255,255,255,.7) inset }`.
    // Safe to pair a real box-shadow with an unblurred surface (axis 4:
    // daylight's own `[compositor.breadbar].blur = false`) — a box-shadow
    // above a BLURRED, `ignore_alpha`d surface is the shadow-halo bug fixed
    // in breadbox by removing the shadow outright; with blur off there is
    // no blur pass to catch this rectangle inside.
    let segment_css = if segmented {
        format!(
            ".bar-segment {{ background-color: alpha({panel}, {bg_alpha}); \
                 border: 1px solid alpha({ink}, 0.10); border-radius: {radius_bar}; \
                 box-shadow: 0 2px 10px alpha({ink}, 0.13); }}\
             "
        )
    } else {
        String::new()
    };

    // Radius for `.stat-pair` (vol/wifi/battery/hamburger chips): radius_sm
    // for liquid-motion (9px) and glass-workbench (6px, exact match to that
    // demo's `.chip` radius) reads as "the same small-control rounding this
    // theme uses everywhere else" — but spotlight's overall language is
    // dramatically rounder (radius_bar 22px, workspace dots at radius_pill
    // 999px) than either sibling theme, so its one `.stat-pair` occupant
    // (battery — the only slot entry besides a Lua widget under
    // `[bar.slots].right`) looked like a stray sharp-cornered rectangle
    // dropped inside a capsule and next to fully-round dots (reported:
    // "the spotlight battery chip... radii that don't match their
    // neighbours"). Keying off the same `WorkspaceStyle` enum
    // `workspace_css` below already switches on, rather than the theme id,
    // so this stays in step if a future theme ever reuses the "dots" style.
    let chip_radius = match theme.modules().workspaces.style {
        bread_theme::shell::WorkspaceStyle::Dots => radius_pill.clone(),
        _ => radius_sm.clone(),
    };

    // ONE chip highlight height per bar, vertically centred — every chip
    // (vol/wifi/battery/menu/media, icon-only and labelled alike) shares
    // it so their fills align, instead of each sizing to its own content
    // box (reported: battery sits high, wifi/menu are taller than their
    // neighbours). Liquid Motion 26px / Glass Workbench 22px / Spotlight
    // 22px, per the approved demo spec.
    //
    // HARDCODED, not `tokens.chip_height()`: that token is `breadbar::
    // CHIP_HEIGHT` (32) carried over from before this design pass and was
    // never updated for the three builtin `theme.toml`s (32/20/36) — it
    // predates and disagrees with the demo numbers above. bread-ecosystem
    // is owned by a sibling agent this pass, so this stays a local
    // override (same `WorkspaceStyle` this file already keys `chip_radius`
    // off) rather than an edit to that repo's schema/manifests. Flagged in
    // the task report: `chip_height` should become 26/22/22 upstream.
    let chip_h = approved_chip_height(theme.modules().workspaces.style);
    let chip_height_px = format!("{chip_h}px");

    // `modules.workspaces.style` (plan §11 Phase 5): "trail" (default,
    // liquid-motion) is exactly today's CSS, unchanged byte-for-byte —
    // dimmed/translucent buttons with the gradient trail overlay supplying
    // the active fill. "pill"/"dots" (glass-workbench, Phase 6+) render the
    // active state as a solid accent fill on the button itself instead,
    // since neither style ever calls `WorkspaceTrail::place`/`stretch`
    // (see `App::rebuild_buttons`) — the trail's own `.workspace-trail`
    // pill CSS is therefore irrelevant for them (it's never made visible).
    let workspace_css = match theme.modules().workspaces.style {
        // Radius and height were hardcoded (12px, 28px) instead of reading
        // from this theme's own tokens/demo — 12px is neither radius_sm
        // (9px) nor any other token this theme defines, and the demo's
        // `.ws-btn`/`.trail` both draw a 26px-tall, 9px-radius pill (not
        // 28px/12px). radius_sm happens to be an exact match for the
        // demo's 9px here, unlike glass-workbench's Pill style below where
        // it's also used but for a different, already-correct reason.
        // Reported: "the pills on liquid motion just look off".
        bread_theme::shell::WorkspaceStyle::Trail => format!(
            // `@{{accent_from}}`/`@{{accent_to}}`, not the literal
            // `@accent`/`@teal` this hardcoded through spotlight: harmless
            // while every Trail-style theme's own accent_from/accent_to
            // happened to BE "accent"/"teal" (liquid-motion — the only
            // other Trail theme so far), but daylight sets
            // accent_from = accent_to = "teal" for a FLAT fill, and the old
            // hardcode would have painted a stray blue-to-teal gradient
            // over it regardless of that setting. See Tokens::accent_to's
            // doc comment (bread-theme) — this is the fix that finally
            // consumes it for real.
            ".workspace-trail {{ background-image: linear-gradient(90deg, @{accent_from}, @{accent_to});\
                 background-color: @{accent_from}; border-radius: {radius_sm}; }}\
             .workspace-btn {{ background: transparent; opacity: 0.36; color: {ink};\
                 border-radius: {radius_sm}; border: none; outline: none; box-shadow: none;\
                 min-width: 28px; min-height: {chip_height_px}; margin: 0; padding: 0 7px;\
                 font-size: 22px; font-weight: bold;\
                 transition: opacity 0.22s {spring_settle},\
                     background-color 0.22s {spring_settle}; }}\
             .workspace-btn:hover {{ opacity: 0.85; background: alpha({ink}, 0.08); }}\
             .workspace-btn.occupied {{ opacity: 0.78; }}\
             .workspace-btn.active {{ background: transparent; color: @on-accent; opacity: 1; }}\
             .workspace-btn.active:hover {{ background: transparent; }}\
             .workspace-btn.ws-in {{ animation: row-in 0.32s {spring_settle} both; }}",
        ),
        bread_theme::shell::WorkspaceStyle::Pill => {
            let accent = theme.tokens().accent_from();
            format!(
                ".workspace-btn {{ background: transparent; opacity: 1; color: alpha({ink}, 0.4);\
                     border-radius: {radius_sm}; border: none; outline: none; box-shadow: none;\
                     min-width: 22px; min-height: {chip_height_px}; margin: 0; padding: 0 6px;\
                     font-size: 12px; font-weight: 600;\
                     transition: background-color 0.22s {spring_settle},\
                         color 0.22s {spring_settle}, opacity 0.22s {spring_settle}; }}\
                 .workspace-btn:hover {{ background: alpha({ink}, 0.08); }}\
                 .workspace-btn.occupied {{ color: alpha({ink}, 0.8); }}\
                 .workspace-btn:not(.occupied):not(.active) {{ opacity: 0.35; }}\
                 .workspace-btn.active {{ background: @{accent}; color: @on-accent; opacity: 1; }}\
                 .workspace-btn.active:hover {{ background: @{accent}; }}\
                 .workspace-btn.ws-in {{ animation: row-in 0.32s {spring_settle} both; }}",
            )
        }
        // "dots" (theme 04/spotlight): a label-less pill whose WIDTH comes
        // from `modules.workspaces.dot_widths` and is set directly via
        // `Widget::set_size_request` in `bar::workspaces::make_dot_button`
        // — GTK CSS has no per-instance variable width, so unlike the demo's
        // `.dots button[data-n="N"]` rules this class only supplies colour/
        // opacity/radius, never a width. `04-spotlight.html`'s own base
        // rule (`background: #5a4a54`) is a *dim, desaturated* grey, not the
        // bar's ink colour — approximated here as a low-alpha `@on-bg` fill
        // so it still tracks pywal instead of hardcoding a hex that would
        // clash with a light palette.
        bread_theme::shell::WorkspaceStyle::Dots => {
            let accent = theme.tokens().accent_from();
            format!(
                // 9px, not the demo's 6px — kept in sync with
                // `bar::workspaces::make_dot_button`'s `DOT_HEIGHT` const,
                // which is the value that actually governs the rendered
                // size (a direct `set_size_request`, not CSS min-height
                // participating in layout the normal way) — see that
                // constant's own doc comment for why. This min-height
                // exists mainly so the property isn't silently absent from
                // the stylesheet a reader would expect to define it.
                ".workspace-dot {{ background-color: alpha({ink}, 0.35); color: transparent;\
                     border-radius: {radius_pill}; border: none; outline: none; box-shadow: none;\
                     min-height: 9px; margin: 0; padding: 0;\
                     transition: background-color 0.25s {spring_settle},\
                         opacity 0.25s {spring_settle}; }}\
                 .workspace-dot:hover {{ background-color: alpha({ink}, 0.55); }}\
                 .workspace-dot:not(.occupied):not(.active) {{ opacity: 0.35; }}\
                 .workspace-dot.active {{ background-color: @{accent}; opacity: 1; }}\
                 .workspace-dot.active:hover {{ background-color: @{accent}; }}",
            )
        }
    };

    format!(
        "@keyframes notif-in {{ from {{ opacity: 0; margin-right: -16px; }} }}\
         @keyframes osd-in {{ from {{ opacity: 0; margin-bottom: -8px; }} }}\
         @keyframes media-eq {{ to {{ min-height: 14px; }} }}\
         @keyframes pop-in {{ from {{ opacity: 0; margin-top: -10px; }} to {{ opacity: 1; margin-top: 0; }} }}\
         @keyframes pop-out {{ from {{ opacity: 1; margin-top: 0; }} to {{ opacity: 0; margin-top: -6px; }} }}\
         @keyframes row-in {{ from {{ opacity: 0; margin-top: 8px; }} to {{ opacity: 1; margin-top: 0; }} }}\
         @keyframes digit-flip {{ from {{ opacity: 0; margin-top: 7px; }} to {{ opacity: 1; margin-top: 0; }} }}\
         @keyframes caret-draw {{ from {{ margin-right: 200px; opacity: 0.2; }} to {{ margin-right: 4px; opacity: 1; }} }}\
         /* ANIMATION WORK #3, bar entrance on first map: opacity ONLY —\
            no margin/geometry term — so this can never perturb any\
            descendant's own box-model size (see main.rs's own long\
            comment on this, next to where `bar-entrance` gets added, for\
            why that matters to the workspace trail specifically).\
            Liquid Motion additionally springs the surface's own\
            layer-shell top margin via `anim::spring_to` in Rust, which\
            this keyframe knows nothing about; glass-workbench never adds\
            this class at all. */\
         @keyframes bar-in {{ from {{ opacity: 0; }} }}\
         .bar-entrance {{ animation: bar-in 0.4s {spring_settle} both; }}\
         window.breadbar {{ color: {ink}; border-radius: {bar_radius}; {window_chrome}\
             transition: border-radius 0.3s {spring_settle}; }}\
         /* `[launcher].search_radius` (plan §7 phase 6c, spotlight only —\
            `launcher_entry` never gets focus under any other theme, so\
            `.searching` never lands on `window.breadbar` there). */\
         window.breadbar.searching {{ border-radius: {radius_search}; }}\
         /* `> box > centerbox`, not `> centerbox`: the root is a vbox (bar\
            row + drawer, plan §2) as of the `drawer` slot wiring — every\
            theme's centerbox is now one level deeper than before, this\
            selector just follows it there. Since `padding` doesn't depend\
            on nesting depth, liquid-motion/glass-workbench render byte-\
            identical CSS either way. */\
         window.breadbar > box > centerbox {{ padding: {centerbox_padding}; }}\
         /* `color` here for the same reason `window.breadbar-panel button`\
            (below) needs it: a real GtkButton's own text (the bar's\
            hamburger, `.control-panel-btn`) doesn't inherit `color` from\
            this window — the shared, ecosystem-wide `button {{ color:\
            @on-surface }}` rule (lib.rs) matches it directly first. See\
            that rule's own comment for the full explanation. */\
         window.breadbar button {{ min-height: 0; min-width: 0; color: {ink}; }}\
         {segment_css}\
         {workspace_css}\
         .clock-box {{ padding: 0 4px; }}\
         .clock-label {{ font-size: 24px; font-weight: bold; letter-spacing: 0.04em;\
             min-height: 0; padding: 0; margin-top: 3px; }}\
         .clock-digit {{ font-size: 24px; font-weight: bold; letter-spacing: 0.04em;\
             min-width: 15px; min-height: 0; padding: 0; margin: 0; }}\
         .clock-colon {{ min-width: 10px; opacity: 0.7; }}\
         .clock-digit.flip {{ animation: digit-flip 0.45s {spring} both; }}\
         .clock-plain {{ padding: 0 4px; }}\
         .clock-plain-time {{ font-size: 15px; font-weight: 600; letter-spacing: 0.04em; }}\
         .date-label {{ font-size: 12px; opacity: 0.48; letter-spacing: 0.04em; }}\
         .stat-label {{ font-size: 14px; letter-spacing: 0.02em; opacity: 0.92; }}\
         .stat-label.tick {{ animation: digit-flip 0.35s {spring} both; }}\
         /* Odometer digit chips (volume/battery, ANIMATION WORK #2): one\
            `.stat-label` per character instead of one label for the whole\
            number, each with a fixed min-width so a `9` -> `10` or\
            `8` -> `9` transition doesn't jitter the chip's overall width\
            as narrower/wider glyphs swap in. `.flip` reuses the exact\
            `digit-flip` keyframe + timing the clock's `.clock-digit.flip`\
            already plays. */\
         .stat-digit {{ min-width: 9px; padding: 0; margin: 0; }}\
         .stat-digit.flip {{ animation: digit-flip 0.35s {spring} both; }}\
         .stats-box {{ margin-right: 0; }}\
         /* Radius was a hardcoded 10px here regardless of theme — right by\
            coincidence for liquid-motion's demo (`.chip {{ border-radius:\
            10px }}`, this theme's radius_sm is 9px, a 1px rounding-off),\
            wrong for glass-workbench (demo's `.chip` is 6px, exactly this\
            theme's radius_sm — the hardcoded 10px never matched it), and\
            wrong-in-spirit for spotlight even though no `.chip` class\
            exists in that demo to compare against: a small, sharp-ish\
            radius reads as a stray rectangle inside a 22px-radius capsule\
            sitting right next to 999px-radius workspace dots (reported:\
            spotlight's battery chip not matching its neighbours). See\
            `chip_radius` above — radius_sm for the other two themes,\
            radius_pill for spotlight, so every theme's stat chips round\
            the way that theme's *other* rounded chrome already does,\
            instead of all three sharing one borrowed hardcoded number. */\
         /* min-height (not the old `min-height: 0`): decision #1 — ONE\
            chip highlight height per bar, vertically centred, shared by\
            every chip so their fills align instead of each sizing to its\
            own content box (reported: battery sat high, wifi/menu were\
            taller than their neighbours). See `chip_height_px` above. */\
         .stat-pair {{ margin: 0; border-radius: {chip_radius}; padding: 5px 9px;\
             min-height: {chip_height_px};\
             transition: background-color 0.22s {spring_settle},\
                 opacity 0.18s ease; }}\
         .stat-pair:hover {{ background: alpha({ink}, 0.12); }}\
         .stat-pair:active {{ background: alpha({ink}, 0.18); }}\
         /* No border-radius override here (was a hardcoded 999px, making\
            wifi/hamburger — the only two `.icon-only` chips — fully\
            circular while their row neighbours vol/battery stayed a\
            rounded rect at `.stat-pair`'s own radius: a visible rounding\
            mismatch inside one row, reported against the liquid-motion\
            hamburger specifically). Every demo's `.chip` class (liquid-\
            motion, glass-workbench) draws vol/wifi/bat/menu identically,\
            none of them circular — dropping the override here just lets\
            `.stat-pair`'s own `chip_radius` cascade through unchanged, so\
            the icon-only chips match their siblings instead of standing\
            out (spotlight has no icon-only chip today, but would get the\
            same pill radius as its one `.stat-pair` sibling if it ever did). */\
         .stat-pair.icon-only {{ padding: 4px;\
             min-width: {chip_height_px}; min-height: {chip_height_px}; }}\
         .stat-icon {{ margin-right: 6px; }}\
         .stat-pair.icon-only .stat-icon {{ margin: 0; }}\
         .bt-icon {{ margin-right: 8px; }}
         separator.bar-sep {{ min-height: 12px; min-width: 1px; margin: 0 10px 0 2px;\
             background: alpha({ink}, 0.10); }}\
         window.breadbar-notification {{ background-color: transparent; color: {ink}; }}\
         window.breadbar-history {{ background-color: alpha({panel}, {card_alpha}); color: {ink};\
             border-radius: {radius}; border: 1px solid alpha({ink}, 0.10);\
             animation: pop-in 0.45s {spring} both; }}\
         .notification-card {{ background: alpha({panel}, {card_alpha}); color: {ink}; border-radius: {radius};\
             padding: {pad}; margin-bottom: 8px; border: 1px solid alpha({ink}, 0.10);\
             border-left: 3px solid transparent;\
             animation: notif-in 0.45s {spring_settle} both; }}\
         .notification-card.urgency-critical {{ border-left-color: @red; }}\
         .notification-card.urgency-normal {{ border-left-color: @accent; }}\
         .notification-summary {{ font-weight: bold; }}\
         .notification-app {{ opacity: 0.55; font-size: 11px; letter-spacing: 0.04em; }}\
         .notification-actions {{ margin-top: 6px; }}\
         .notification-action {{ padding: 2px 8px; font-size: 11px; border-radius: {radius_sm}; }}\
         .notification-reply {{ margin-top: 6px; }}\
         .notification-reply-entry {{ min-width: 0; }}\
         /* NOTIFICATION INTERACTION #A: a direct dismiss control, floated\
            in the card's top-right corner via an Overlay (see popup.rs's\
            `make_card`) rather than a full extra header row, so it doesn't\
            add vertical bulk the approved demo's own card never has. */\
         .notification-dismiss {{ min-width: 18px; min-height: 18px; padding: 0;\
             margin: 2px; border-radius: {radius_pill}; background: transparent;\
             color: {ink}; opacity: 0.45; font-size: 12px; font-weight: bold;\
             border: none; outline: none; box-shadow: none;\
             transition: background-color 0.18s {spring_settle}, opacity 0.18s ease; }}\
         .notification-dismiss:hover {{ opacity: 1; background: alpha({ink}, 0.16); }}\
         .notification-dismiss:active {{ background: alpha({ink}, 0.24); }}\
         .history-title {{ font-weight: bold; font-size: 13px; }}\
         .history-close {{ padding: 2px 8px; }}\
         .history-empty {{ opacity: 0.5; padding: 8px 0; }}\
         .history-time {{ opacity: 0.5; font-size: 11px; }}\
         .history-body {{ opacity: 0.75; }}\
         .history-card {{ margin-bottom: 6px; }}\
         window.breadbar-osd {{ background-color: alpha({panel}, {card_alpha}); color: {ink};\
             border-radius: {radius_pill}; border: 1px solid alpha({ink}, 0.10);\
             animation: osd-in 0.4s {spring_settle} both; }}\
         .osd-icon {{ opacity: 0.85; margin-right: 8px; }}\
         .osd-icon-muted {{ opacity: 0.35; }}\
         progressbar.osd-bar {{ min-height: 6px; }}\
         progressbar.osd-bar trough {{ background-image: none; background-color: {trough_bg};\
             border-radius: 3px; min-height: 6px; }}\
         progressbar.osd-bar trough progress {{ background-image: none; background-color: @accent;\
             border-radius: 3px; min-height: 6px; }}\
         window.breadbar-panel {{ background-color: alpha({panel}, {panel_surface_alpha}); color: {ink};\
             border-radius: 14px; border: 1px solid alpha({ink}, 0.12); }}\
         /* A real GtkButton's own label text does NOT inherit `color` from\
            an ancestor window: `bread_theme::stylesheet()`'s shared,\
            ecosystem-wide `button {{ color: @on-surface }}` rule (lib.rs,\
            applied to every bread app before this file's CSS layers on\
            top) matches the button element directly, and a direct match\
            always beats inheritance regardless of specificity. `@on-surface`\
            is `ink_on(@surface)`, and `@surface` is `bread_theme::palette`'s\
            FIXED_SURFACE constant — pinned dark, same as `@bg` — so every\
            button's label (power row, hamburger, wifi/bluetooth popover\
            rows, the add-network dialog's Cancel/Connect) rendered\
            near-white regardless of the active shell theme. Invisible-but-\
            correct on every dark theme through spotlight; confirmed\
            empirically as near-invisible ghost text under daylight (isolated\
            `bread-capture` control-panel screenshot, pre-fix). One rule,\
            scoped by ancestor class so its specificity beats the shared\
            unscoped `button` rule, instead of patching each `.power-btn`/\
            `.control-panel-btn`/`.wifi-popover-row`/etc. class individually. */\
         window.breadbar-panel button, window.wifi-popover button,\
         window.wifi-add-dialog button, window.breadbar-notification button,\
         window.breadbar-history button {{ color: {ink}; }}\
         window.breadbar-dismiss, .breadbar-dismiss-hit {{\
             background-color: alpha(#000000, 0.02); }}\
         .popover-caret {{ min-height: 2px; margin: 2px 4px 10px; border-radius: 2px;\
             background-color: @accent;\
             background-image: linear-gradient(90deg, @accent, @teal);\
             animation: caret-draw 0.45s {spring} both; }}\
         .wifi-popover-inner {{ min-width: 228px; padding: {pad}; }}\
         window.wifi-popover button {{ min-height: 0; min-width: 0; }}\
         .popover-tab-row {{ background: alpha({ink}, 0.06); border-radius: 10px;\
             padding: 3px; margin-bottom: 10px; }}\
         .popover-tab {{ background: transparent; color: {ink}; border: none; box-shadow: none;\
             outline: none; border-radius: 999px; padding: 0 14px; min-height: 32px;\
             font-size: 17px; font-weight: bold; opacity: 0.55;\
             transition: background-color 0.22s {spring_settle},\
                 opacity 0.22s ease, color 0.22s ease; }}\
         .popover-tab:hover {{ opacity: 0.8; }}\
         .popover-tab:checked {{ background: alpha(@accent, 0.22); color: @accent; opacity: 1; }}\
         .popover-tab label {{ padding: 0; margin: 0; }}\
         .wifi-popover-ssid {{ font-weight: bold; font-size: 18px; }}\
         .wifi-popover-ip {{ opacity: 0.6; font-size: 16px; }}\
         .wifi-popover-status {{ font-size: 16px; margin-top: 2px; }}\
         .wifi-popover-section {{ font-size: 13px; font-weight: bold; opacity: 0.45;\
             letter-spacing: 0.12em; }}\
         .wifi-popover-row {{ background: transparent; border: none; box-shadow: none;\
             outline: none; border-radius: 10px; padding: 0 12px; min-height: 42px;\
             transition: background-color 0.18s {spring_settle}; }}\
         .wifi-popover-row label {{ font-size: 18px; }}\
         .wifi-popover-row:hover {{ background: alpha({ink}, 0.08); }}\
         .wifi-popover-row-active {{ background: alpha(@accent, 0.14); color: @accent; }}\
         .wifi-popover-row-active:hover {{ background: alpha(@accent, 0.20); }}\
         .row-in {{ animation: row-in 0.32s {spring} both; }}\
         .stagger-0 {{ animation-delay: 0ms; }} .stagger-1 {{ animation-delay: 28ms; }}\
         .stagger-2 {{ animation-delay: 56ms; }} .stagger-3 {{ animation-delay: 84ms; }}\
         .stagger-4 {{ animation-delay: 112ms; }} .stagger-5 {{ animation-delay: 140ms; }}\
         .stagger-6 {{ animation-delay: 168ms; }} .stagger-7 {{ animation-delay: 196ms; }}\
         .stagger-8 {{ animation-delay: 224ms; }} .stagger-9 {{ animation-delay: 252ms; }}\
         .stagger-10 {{ animation-delay: 280ms; }} .stagger-11 {{ animation-delay: 308ms; }}\
         .wifi-popover-row-unsaved {{ opacity: 0.4; }}\
         .wifi-popover-loading {{ opacity: 0.5; padding: 8px; }}\
         switch.bt-switch, switch.bt-switch:hover, switch.bt-switch:checked,\
         switch.bt-switch:checked:hover {{ min-width: 42px; min-height: 24px; padding: 2px;\
             border: none; outline: none; box-shadow: none; background-image: none;\
             border-radius: 99px; }}\
         switch.bt-switch {{ background-color: alpha({ink}, 0.14);\
             transition: background-color 0.25s {spring_settle}; }}\
         switch.bt-switch:checked {{ background-color: @accent; }}\
         switch.bt-switch slider {{ min-width: 20px; min-height: 20px; margin: 0;\
             border-radius: 99px; border: none; outline: none; box-shadow: none;\
             background-image: none; background-color: {ink}; }}\
         window.wifi-add-dialog {{ background-color: alpha({panel}, {card_alpha}); color: {ink}; min-width: 240px;\
             border-radius: {radius}; border: 1px solid alpha({ink}, 0.10);\
             animation: pop-in 0.45s {spring} both; }}\
         window.wifi-add-dialog headerbar {{ background-color: alpha({panel}, {card_alpha}); color: {ink};\
             border-top-left-radius: {radius}; border-top-right-radius: {radius};\
             border-bottom: 1px solid alpha({ink}, 0.10); box-shadow: none; }}\
         .confirm-button {{ background-color: @accent; color: @on-accent; }}\
         .confirm-button:hover {{ background-color: alpha(@accent, 0.85); }}\
         /* min-height: decision #1 — the media chip is a bar chip like\
            any other, so it shares the same row height instead of sizing\
            to its own eq-bar/label content. */\
         .media-widget {{ border-radius: 10px; padding: 4px 8px; min-height: {chip_height_px};\
             transition: background-color 0.22s {spring_settle}; }}\
         .media-widget:hover {{ background: alpha({ink}, 0.08); }}\
         .media-widget.media-in {{ animation: row-in 0.4s {spring} both; }}\
         .media-eq {{ min-height: 14px; margin-right: 4px; }}\
         .media-eq-bar {{ min-width: 3px; min-height: 5px; background-color: @{accent2};\
             border-radius: 2px; }}\
         .media-widget.playing .media-eq-bar {{\
             animation: media-eq 0.85s ease-in-out infinite alternate; }}\
         .media-widget.playing .media-eq-bar:nth-child(2) {{ animation-delay: 0.1s; min-height: 11px; }}\
         .media-widget.playing .media-eq-bar:nth-child(3) {{ animation-delay: 0.22s; min-height: 7px; }}\
         .media-widget.playing .media-eq-bar:nth-child(4) {{ animation-delay: 0.06s; min-height: 13px; }}\
         .media-track-lbl {{ font-size: 17px; }}\
         .media-controls {{ padding: 4px; }}\
         .media-btn {{ min-width: 32px; padding: 4px 8px; border-radius: {radius_sm};\
             transition: background-color 0.18s ease; }}\
         .media-btn:hover {{ background: alpha({ink}, 0.10); }}\
         /* No padding/border-radius/min-width/min-height here (was\
            `padding: 5px 8px; border-radius: 10px; min-width: 0;\
            min-height: 0`): the hamburger is the only button carrying both\
            `.stat-pair.icon-only` AND `.control-panel-btn`, and because\
            this rule sits later in the cascade its hardcoded 10px radius\
            and 0 min-size were silently winning over `.stat-pair`'s own\
            `chip_radius`/`chip_height_px` — the exact hamburger-corner-\
            mismatch bug decision #2 describes, and a second copy of\
            decision #1's height bug, both reintroduced by\
            this one class alone. Dropping the four properties lets\
            `.stat-pair`/`.stat-pair.icon-only` cascade through unchanged,\
            same fix shape as the icon-only border-radius removal above. */\
         .control-panel-btn {{ margin: 0;\
             opacity: 0.92; font-size: 18px; line-height: 1;\
             background: transparent; border: none; outline: none; box-shadow: none;\
             transition: background-color 0.22s {spring_settle},\
                 opacity 0.18s ease; }}\
         .control-panel-btn:hover {{ opacity: 1; background: alpha({ink}, 0.10); }}\
         .control-panel-btn:active {{ background: alpha({ink}, 0.16); }}\
         .control-panel {{ }}\
         .control-panel-inner {{ min-width: 248px; padding: {pad}; }}\
         .sys-grid {{ margin: 2px 0 6px; }}\
         .sys-stat {{ padding: 4px 2px; background: transparent; }}\
         .sys-stat:hover {{ background: transparent; }}\
         .control-panel-header {{ font-size: 12px; font-weight: bold; letter-spacing: 0.12em;\
             opacity: 0.45; margin-bottom: 8px; }}\
         .control-panel-row {{ margin: 8px 0; }}\
         .control-panel-row-label {{ font-size: 16px; opacity: 0.78; }}\
         .control-panel-slider {{ margin: 0; padding: 0; min-height: 18px; }}\
         scale.control-panel-slider trough {{ min-height: 6px; border-radius: 99px;\
             background-image: none; background-color: alpha({ink}, 0.12);\
             border: none; outline: none; box-shadow: none; }}\
         scale.control-panel-slider highlight {{ min-height: 6px; border-radius: 99px;\
             background-image: none; background-color: @accent; }}\
         scale.control-panel-slider slider {{ min-width: 0; min-height: 0; margin: 0;\
             padding: 0; opacity: 0; background: transparent; border: none;\
             outline: none; box-shadow: none; }}\
         .control-panel-section {{ margin: 8px 0 0; }}\
         .sink-row label {{ font-size: 15px; }}\
         .power-row {{ margin-top: 8px; }}\
         .power-btn {{ min-width: 0; min-height: 0; padding: 8px 10px; border-radius: 8px;\
             background: alpha({ink}, 0.08); font-size: 13px; border: none;\
             outline: none; box-shadow: none;\
             transition: background-color 0.2s {spring_settle}; }}\
         .power-btn:hover {{ background: alpha({ink}, 0.14); }}\
         .power-btn:active {{ background: alpha(@accent, 0.22); }}\
         .notification-action {{ transition: background-color 0.18s ease; }}\
         .tray-btn {{ transition: opacity 0.2s ease, background-color 0.2s ease; }}\
         separator {{ margin: 4px 0; background: alpha({ink}, 0.10); }}\
         /* Lua-declared widgets (see Documentation.md's Widgets §style): the\
            slot rule below is what the four inline `.bread-widget-slot`\
            containers in main.rs rely on for the same 12px stat-pair rhythm\
            everything else in the bar uses (they carried the class with no\
            rule defining it until now). Everything after that is the fixed,\
            closed `style` vocabulary a `WidgetNode` can opt into — one class\
            per enum variant, so a module can only ever pick from this set,\
            never inject arbitrary CSS. The progress-bar rules give an\
            unstyled Progress node an intentional accent-colored fill instead\
            of Adwaita's default blue-on-gray, and let `style.color` retint\
            that fill the same way it retints label/icon text. */\
         .bread-widget-slot {{ margin-right: 12px; }}\
         progressbar.bread-widget-node trough {{ background-image: none; background-color: {trough_bg}; border-radius: 3px; min-height: 6px; }}\
         progressbar.bread-widget-node trough progress {{ background-image: none; background-color: @accent; border-radius: 3px; min-height: 6px; }}\
         progressbar.bread-widget-node.bread-color-fg trough progress {{ background-color: @fg; }}\
         progressbar.bread-widget-node.bread-color-dim trough progress {{ background-color: alpha(@fg, 0.6); }}\
         progressbar.bread-widget-node.bread-color-accent trough progress {{ background-color: @accent; }}\
         progressbar.bread-widget-node.bread-color-red trough progress {{ background-color: @red; }}\
         progressbar.bread-widget-node.bread-color-green trough progress {{ background-color: @green; }}\
         progressbar.bread-widget-node.bread-color-yellow trough progress {{ background-color: @yellow; }}\
         progressbar.bread-widget-node.bread-color-blue trough progress {{ background-color: @blue; }}\
         progressbar.bread-widget-node.bread-color-pink trough progress {{ background-color: @pink; }}\
         progressbar.bread-widget-node.bread-color-teal trough progress {{ background-color: @teal; }}\
         .bread-color-fg {{ color: @fg; }}\
         .bread-color-dim {{ color: @fg; opacity: 0.6; }}\
         .bread-color-accent {{ color: @accent; }}\
         .bread-color-red {{ color: @red; }}\
         .bread-color-green {{ color: @green; }}\
         .bread-color-yellow {{ color: @yellow; }}\
         .bread-color-blue {{ color: @blue; }}\
         .bread-color-pink {{ color: @pink; }}\
         .bread-color-teal {{ color: @teal; }}\
         .bread-weight-normal {{ font-weight: normal; }}\
         .bread-weight-bold {{ font-weight: bold; }}\
         .bread-size-xs {{ font-size: 10px; }}\
         .bread-size-sm {{ font-size: 12px; }}\
         .bread-size-md {{ font-size: 14px; }}\
         .bread-size-lg {{ font-size: 16px; }}\
         .bread-size-xl {{ font-size: 20px; }}\
         .bread-bg-none {{ background-color: transparent; }}\
         .bread-bg-surface {{ background-color: @surface; color: @on-surface; }}\
         .bread-bg-card {{ background-color: @surface; color: @on-surface; border-radius: 8px; padding: 12px; }}\
         .bread-radius-none {{ border-radius: 0; }}\
         .bread-radius-sm {{ border-radius: 4px; }}\
         .bread-radius-md {{ border-radius: 8px; }}\
         .bread-radius-full {{ border-radius: 999px; }}\
         .bread-padding-none {{ padding: 0; }}\
         .bread-padding-xs {{ padding: 4px; }}\
         .bread-padding-sm {{ padding: 8px; }}\
         .bread-padding-md {{ padding: 12px; }}\
         /* Theme 04/spotlight's embedded launcher (plan §7). Unconditional,\
            like `.clock-plain-time` above: `launcher_entry`/`launcher_results`\
            are built regardless of the active theme (see main.rs's \"Assemble\"\
            section), just never placed in a slot outside spotlight, so these\
            rules render nothing on liquid-motion/glass-workbench. */\
         .launcher-entry {{ background: transparent; color: {ink}; border: none;\
             outline: none; box-shadow: none; caret-color: @{accent_from};\
             font-size: 13px; font-weight: 500; letter-spacing: 0.06em;\
             padding: 0; margin: 0; min-height: 0; }}\
         .launcher-entry.searching {{ font-size: 15px; letter-spacing: 0; }}\
         .bread-drawer {{ min-height: 0; }}\
         .bread-drawer.open {{ border-top: 1px solid alpha({ink}, 0.08);\
             margin-top: 6px; padding-top: 2px; }}\
         .bread-drawer listbox {{ background: transparent; padding: 2px 0; }}\
         .bread-drawer row {{ padding: 8px 14px; border-radius: {radius_sm};\
             color: {ink}; background-color: transparent; }}\
         .bread-drawer row:hover {{ background-color: alpha({ink}, 0.08); }}\
         .bread-drawer row:selected {{ background-color: alpha(@{accent_from}, 0.18);\
             color: {ink}; }}\
         .bread-drawer .app-name {{ font-size: 14px; font-weight: 500; }}\
         .bread-drawer .app-muted {{ opacity: 0.45; font-size: 11px; }}\
         /* `[launcher].sections` (plan §7 phase 6c) — the idle drawer's\
            \"Recent\"/\"Apps\" group labels (`bread_launcher::gtk::\
            build_header_row`). Unconditional, same reasoning as every other\
            launcher rule above: only spotlight ever builds a row with this\
            class at all. */\
         .bread-drawer-section-header {{ padding: 6px 14px 2px; }}\
         .section-header-label {{ font-size: 11px; font-weight: 600;\
             letter-spacing: 0.08em; text-transform: uppercase;\
             opacity: 0.45; }}",
        // Implicit capture (2021 edition) for every `{name}` above: each
        // matches an in-scope `let` binding of the same name (`radius`,
        // `spring`, `ink`, `panel`, `bar_radius`, `window_chrome`,
        // `segment_css`, `accent_from`, `accent2`, ...) rather than a hand-
        // maintained `name = name,` list — axis 1's `panel`/`ink` swap and
        // axis 3's `segment_css`/`window_chrome`/`bar_radius` locals both
        // needed a growing, easy-to-desync explicit list to stay in step
        // with the string body above; switching the whole call to implicit
        // capture removes that failure mode instead of extending it further.
        // (`radius_bar`, `flush`, `light`, `segmented`, `window_border`
        // itself, and `accent_to` are each read by name ABOVE this literal,
        // not inside it, so they're deliberately absent here — an unused
        // implicit-capture name is a hard compile error, same discipline
        // this crate already applies to unread `theme.toml` keys.)
    )
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
/// built through [`crate::svg_image`]/[`crate::svg_texture`] (volume, wifi,
/// battery, hamburger, media transport, the OSD glyph) was near-white on
/// every existing (dark) theme, which read as correct by construction —
/// until daylight's near-white paper pills made the SAME near-white glyph
/// nearly invisible against its own background. Confirmed empirically
/// (isolated `bread-capture` OSD-volume screenshot, pre-fix) before this
/// fix. Mirrors `load_css`'s own `ink` local exactly: the dark theme's
/// unchanged `ink_on(background)` (near-white), or — for a light theme —
/// `background` itself (the fixed dark hex IS the correct dark ink, the
/// same identity `load_css`'s `ink = "@bg"` case relies on).
pub fn fg_color() -> String {
    let p = load_palette();
    if shell_theme().tokens().light() {
        p.background.clone()
    } else {
        ink_on(&p.background).to_string()
    }
}

/// Ink colour for the given Hyprland output's wallpaper palette. See
/// [`fg_color`]'s doc comment for the same light-theme fix; kept in step
/// even though this accessor has no call site today.
#[allow(dead_code)]
pub fn fg_color_for(output: &str) -> String {
    let p = load_palette_for(output);
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
pub fn bind_output(widget: &impl IsA<gtk4::Widget>, output: &str) {
    bgtk::bind_window_with_app_css(widget, output, load_css_for);
}

/// Bind a satellite window (notification, history, OSD, wifi dialog) to
/// whichever output it is actually rendered on.
pub fn bind_auto(window: &impl IsA<gtk4::Native>) {
    bgtk::bind_window_auto_with_app_css(window, load_css_for);
}

fn load_css_for(_palette: &Palette) -> String {
    load_css()
}

/// Apply (or reload) the theme CSS. Safe to call from `glib::MainContext::invoke`.
pub fn apply() {
    // Shared ecosystem base (fonts, palette, generic widgets) — applied first
    // (and self-reloading) so breadbar's own rules below layer on top.
    bgtk::apply_shared();

    // breadbar's own rules, hot-reloaded on `bread-theme reload`: the closure
    // re-reads the pywal palette each time so the bar recolours without restart.
    bgtk::apply_app_css(load_css);

    let home = std::env::var("HOME").unwrap_or_default();
    let user_path = std::path::PathBuf::from(format!("{home}/.config/breadbar/style.css"));
    USER_PROVIDER.with(|cell| bgtk::apply_user_css(&user_path, cell));
}

thread_local! {
    // `bread_theme::shell::ThemeWatch`, not a bare `gio::FileMonitor`: the
    // watch now re-arms itself onto a new theme's directory when the active
    // theme id changes underneath it (see that type's doc comment), so the
    // handle we keep alive is opaque, not a single fixed monitor.
    static SHELL_THEME_MONITOR: RefCell<Option<bread_theme::shell::ThemeWatch>> =
        const { RefCell::new(None) };
}

/// Wires `bread_theme::shell::watch()` (plan §10) so editing the active
/// theme's `theme.toml`/`extra.css` on disk re-resolves CSS tokens without a
/// restart, the same way a pywal palette change already does via
/// `apply_app_css`. Window-spec values (anchors, margins, exclusive zone,
/// keyboard mode) are read once at window-construction time and are *not*
/// re-applied here — per plan §10 those need a restart, since live-swapping
/// a mapped layer-shell surface's anchors/exclusive-zone is a lot of
/// teardown risk for a rare operation.
///
/// Call once at startup (primary instance only — every satellite window
/// calling this would just re-arm the same watch redundantly).
pub fn watch_hot_reload() {
    let monitor = bread_theme::shell::watch(|new_theme| {
        set_shell_theme(new_theme);
        apply();
    });
    SHELL_THEME_MONITOR.with(|cell| *cell.borrow_mut() = Some(monitor));
}

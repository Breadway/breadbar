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
    let window_border = if flush {
        "border: none; border-bottom: 1px solid alpha(@on-bg, 0.07);".to_string()
    } else {
        "border: 1px solid alpha(@on-bg, 0.08);".to_string()
    };
    let centerbox_padding = if flush { "0 12px" } else { "0 8px 0 6px" };

    // `modules.workspaces.style` (plan §11 Phase 5): "trail" (default,
    // liquid-motion) is exactly today's CSS, unchanged byte-for-byte —
    // dimmed/translucent buttons with the gradient trail overlay supplying
    // the active fill. "pill"/"dots" (glass-workbench, Phase 6+) render the
    // active state as a solid accent fill on the button itself instead,
    // since neither style ever calls `WorkspaceTrail::place`/`stretch`
    // (see `App::rebuild_buttons`) — the trail's own `.workspace-trail`
    // pill CSS is therefore irrelevant for them (it's never made visible).
    let workspace_css = match theme.modules().workspaces.style {
        bread_theme::shell::WorkspaceStyle::Trail => format!(
            ".workspace-trail {{ background-image: linear-gradient(90deg, @accent, @teal);\
                 background-color: @accent; border-radius: 12px; }}\
             .workspace-btn {{ background: transparent; opacity: 0.36; color: @on-bg;\
                 border-radius: 12px; border: none; outline: none; box-shadow: none;\
                 min-width: 28px; min-height: 28px; margin: 0; padding: 0 7px;\
                 font-size: 22px; font-weight: bold;\
                 transition: opacity 0.22s {spring_settle},\
                     background-color 0.22s {spring_settle}; }}\
             .workspace-btn:hover {{ opacity: 0.85; background: alpha(@on-bg, 0.08); }}\
             .workspace-btn.occupied {{ opacity: 0.78; }}\
             .workspace-btn.active {{ background: transparent; color: @on-accent; opacity: 1; }}\
             .workspace-btn.active:hover {{ background: transparent; }}\
             .workspace-btn.ws-in {{ animation: row-in 0.32s {spring_settle} both; }}",
        ),
        bread_theme::shell::WorkspaceStyle::Pill => {
            let accent = theme.tokens().accent_from();
            format!(
                ".workspace-btn {{ background: transparent; opacity: 1; color: alpha(@on-bg, 0.4);\
                     border-radius: {radius_sm}; border: none; outline: none; box-shadow: none;\
                     min-width: 22px; min-height: 20px; margin: 0; padding: 0 6px;\
                     font-size: 12px; font-weight: 600;\
                     transition: background-color 0.22s {spring_settle},\
                         color 0.22s {spring_settle}, opacity 0.22s {spring_settle}; }}\
                 .workspace-btn:hover {{ background: alpha(@on-bg, 0.08); }}\
                 .workspace-btn.occupied {{ color: alpha(@on-bg, 0.8); }}\
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
                ".workspace-dot {{ background-color: alpha(@on-bg, 0.35); color: transparent;\
                     border-radius: {radius_pill}; border: none; outline: none; box-shadow: none;\
                     min-height: 6px; margin: 0; padding: 0;\
                     transition: background-color 0.25s {spring_settle},\
                         opacity 0.25s {spring_settle}; }}\
                 .workspace-dot:hover {{ background-color: alpha(@on-bg, 0.55); }}\
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
         window.breadbar {{ background-color: alpha(@bg, {bg_alpha}); color: @on-bg;\
             border-radius: {radius_bar}; {window_border} }}\
         /* `> box > centerbox`, not `> centerbox`: the root is a vbox (bar\
            row + drawer, plan §2) as of the `drawer` slot wiring — every\
            theme's centerbox is now one level deeper than before, this\
            selector just follows it there. Since `padding` doesn't depend\
            on nesting depth, liquid-motion/glass-workbench render byte-\
            identical CSS either way. */\
         window.breadbar > box > centerbox {{ padding: {centerbox_padding}; }}\
         window.breadbar button {{ min-height: 0; min-width: 0; }}\
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
         .stats-box {{ margin-right: 0; }}\
         .stat-pair {{ margin: 0; border-radius: 10px; padding: 5px 9px; min-height: 0;\
             transition: background-color 0.22s {spring_settle},\
                 opacity 0.18s ease; }}\
         .stat-pair:hover {{ background: alpha(@on-bg, 0.12); }}\
         .stat-pair:active {{ background: alpha(@on-bg, 0.18); }}\
         .stat-pair.icon-only {{ padding: 4px; border-radius: 999px;\
             min-width: 32px; min-height: 32px; }}\
         .stat-icon {{ margin-right: 6px; }}\
         .stat-pair.icon-only .stat-icon {{ margin: 0; }}\
         .bt-icon {{ margin-right: 8px; }}
         separator.bar-sep {{ min-height: 12px; min-width: 1px; margin: 0 10px 0 2px;\
             background: alpha(@on-bg, 0.10); }}\
         window.breadbar-notification {{ background-color: transparent; color: @on-bg; }}\
         window.breadbar-history {{ background-color: alpha(@bg, 0.70); color: @on-bg;\
             border-radius: {radius}; border: 1px solid alpha(@on-bg, 0.10);\
             animation: pop-in 0.45s {spring} both; }}\
         .notification-card {{ background: alpha(@bg, 0.70); color: @on-bg; border-radius: {radius};\
             padding: {pad}; margin-bottom: 8px; border: 1px solid alpha(@on-bg, 0.10);\
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
         .history-title {{ font-weight: bold; font-size: 13px; }}\
         .history-close {{ padding: 2px 8px; }}\
         .history-empty {{ opacity: 0.5; padding: 8px 0; }}\
         .history-time {{ opacity: 0.5; font-size: 11px; }}\
         .history-body {{ opacity: 0.75; }}\
         .history-card {{ margin-bottom: 6px; }}\
         window.breadbar-osd {{ background-color: alpha(@bg, 0.70); color: @on-bg;\
             border-radius: {radius_pill}; border: 1px solid alpha(@on-bg, 0.10);\
             animation: osd-in 0.4s {spring_settle} both; }}\
         .osd-icon {{ opacity: 0.85; margin-right: 8px; }}\
         .osd-icon-muted {{ opacity: 0.35; }}\
         progressbar.osd-bar {{ min-height: 6px; }}\
         progressbar.osd-bar trough {{ background-image: none; background-color: alpha(@accent, 0.25);\
             border-radius: 3px; min-height: 6px; }}\
         progressbar.osd-bar trough progress {{ background-image: none; background-color: @accent;\
             border-radius: 3px; min-height: 6px; }}\
         .wifi-pair {{ padding: 6px; }}\
         window.breadbar-panel {{ background-color: alpha(@bg, 0.72); color: @on-bg;\
             border-radius: 14px; border: 1px solid alpha(@on-bg, 0.12); }}\
         window.breadbar-dismiss, .breadbar-dismiss-hit {{\
             background-color: alpha(#000000, 0.02); }}\
         .popover-caret {{ min-height: 2px; margin: 2px 4px 10px; border-radius: 2px;\
             background-color: @accent;\
             background-image: linear-gradient(90deg, @accent, @teal);\
             animation: caret-draw 0.45s {spring} both; }}\
         .wifi-popover-inner {{ min-width: 228px; padding: {pad}; }}\
         window.wifi-popover button {{ min-height: 0; min-width: 0; }}\
         .popover-tab-row {{ background: alpha(@on-bg, 0.06); border-radius: 10px;\
             padding: 3px; margin-bottom: 10px; }}\
         .popover-tab {{ background: transparent; color: @on-bg; border: none; box-shadow: none;\
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
         .wifi-popover-row:hover {{ background: alpha(@on-bg, 0.08); }}\
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
         switch.bt-switch {{ background-color: alpha(@on-bg, 0.14);\
             transition: background-color 0.25s {spring_settle}; }}\
         switch.bt-switch:checked {{ background-color: @accent; }}\
         switch.bt-switch slider {{ min-width: 20px; min-height: 20px; margin: 0;\
             border-radius: 99px; border: none; outline: none; box-shadow: none;\
             background-image: none; background-color: @on-bg; }}\
         window.wifi-add-dialog {{ background-color: alpha(@bg, 0.70); color: @on-bg; min-width: 240px;\
             border-radius: {radius}; border: 1px solid alpha(@on-bg, 0.10);\
             animation: pop-in 0.45s {spring} both; }}\
         window.wifi-add-dialog headerbar {{ background-color: alpha(@bg, 0.70); color: @on-bg;\
             border-top-left-radius: {radius}; border-top-right-radius: {radius};\
             border-bottom: 1px solid alpha(@on-bg, 0.10); box-shadow: none; }}\
         .confirm-button {{ background-color: @accent; color: @on-accent; }}\
         .confirm-button:hover {{ background-color: alpha(@accent, 0.85); }}\
         .media-widget {{ border-radius: 10px; padding: 4px 8px; min-height: 0;\
             transition: background-color 0.22s {spring_settle}; }}\
         .media-widget:hover {{ background: alpha(@on-bg, 0.08); }}\
         .media-widget.media-in {{ animation: row-in 0.4s {spring} both; }}\
         .media-eq {{ min-height: 14px; margin-right: 4px; }}\
         .media-eq-bar {{ min-width: 3px; min-height: 5px; background-color: @accent;\
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
         .media-btn:hover {{ background: alpha(@on-bg, 0.10); }}\
         .control-panel-btn {{ padding: 5px 8px; margin: 0; border-radius: 10px;\
             opacity: 0.92; font-size: 18px; line-height: 1; min-width: 0; min-height: 0;\
             background: transparent; border: none; outline: none; box-shadow: none;\
             transition: background-color 0.22s {spring_settle},\
                 opacity 0.18s ease; }}\
         .control-panel-btn:hover {{ opacity: 1; background: alpha(@on-bg, 0.10); }}\
         .control-panel-btn:active {{ background: alpha(@on-bg, 0.16); }}\
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
             background-image: none; background-color: alpha(@on-bg, 0.12);\
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
             background: alpha(@on-bg, 0.08); font-size: 13px; border: none;\
             outline: none; box-shadow: none;\
             transition: background-color 0.2s {spring_settle}; }}\
         .power-btn:hover {{ background: alpha(@on-bg, 0.14); }}\
         .power-btn:active {{ background: alpha(@accent, 0.22); }}\
         .notification-action {{ transition: background-color 0.18s ease; }}\
         .tray-btn {{ transition: opacity 0.2s ease, background-color 0.2s ease; }}\
         separator {{ margin: 4px 0; background: alpha(@on-bg, 0.10); }}\
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
         progressbar.bread-widget-node trough {{ background-image: none; background-color: alpha(@accent, 0.25); border-radius: 3px; min-height: 6px; }}\
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
         .launcher-entry {{ background: transparent; color: @on-bg; border: none;\
             outline: none; box-shadow: none; caret-color: @{accent_from};\
             font-size: 13px; font-weight: 500; letter-spacing: 0.06em;\
             padding: 0; margin: 0; min-height: 0; }}\
         .launcher-entry.searching {{ font-size: 15px; letter-spacing: 0; }}\
         .bread-drawer {{ min-height: 0; }}\
         .bread-drawer.open {{ border-top: 1px solid alpha(@on-bg, 0.08);\
             margin-top: 6px; padding-top: 2px; }}\
         .bread-drawer listbox {{ background: transparent; padding: 2px 0; }}\
         .bread-drawer row {{ padding: 8px 14px; border-radius: {radius_sm};\
             color: @on-bg; background-color: transparent; }}\
         .bread-drawer row:hover {{ background-color: alpha(@on-bg, 0.08); }}\
         .bread-drawer row:selected {{ background-color: alpha(@{accent_from}, 0.18);\
             color: @on-bg; }}\
         .bread-drawer .app-name {{ font-size: 14px; font-weight: 500; }}\
         .bread-drawer .app-muted {{ opacity: 0.45; font-size: 11px; }}",
        radius = radius,
        radius_bar = radius_bar,
        radius_sm = radius_sm,
        radius_pill = radius_pill,
        pad = pad,
        spring = spring,
        spring_settle = spring_settle,
        window_border = window_border,
        centerbox_padding = centerbox_padding,
        workspace_css = workspace_css,
        bg_alpha = bg_alpha,
        accent_from = accent_from,
    )
}

/// Returns the ink colour for icon tinting in the stats bar — the same
/// luminance-picked colour the bar's text uses, so icons stay legible on the bar
/// whatever lightness pywal gives the background.
pub fn fg_color() -> String {
    ink_on(&load_palette().background).to_string()
}

/// Ink colour for the given Hyprland output's wallpaper palette.
#[allow(dead_code)]
pub fn fg_color_for(output: &str) -> String {
    ink_on(&load_palette_for(output).background).to_string()
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
    static SHELL_THEME_MONITOR: RefCell<Option<gtk4::gio::FileMonitor>> =
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

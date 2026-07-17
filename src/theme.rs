use bread_theme::{gtk as bgtk, hex_to_rgba, ink_on, load_palette};
use gtk4::CssProvider;
use std::cell::RefCell;

thread_local! {
    static USER_PROVIDER: RefCell<Option<CssProvider>> = const { RefCell::new(None) };
}

fn load_css() -> String {
    let p = load_palette();
    // breadbar-specific rules only — fonts, base colours, and generic widgets
    // come from the shared ecosystem stylesheet (applied first in `apply()`).
    // Colour is set on each surface (bar, active workspace pill, notification
    // card) and child labels inherit it, so text stays legible whatever lightness
    // pywal hands a given slot. `on_*` are luminance-picked ink (black/white) for
    // that background — the pywal hues themselves are untouched.
    //
    // Shared tokens: one radius and one padding rhythm reused across every
    // popover/card/OSD surface so they read as one design system rather than
    // four different ones. `radius_pill` is only for the tiny transient OSD.
    let radius = "10px";
    let radius_sm = "6px";
    let radius_pill = "20px";
    let pad = "10px";

    format!(
        "window.breadbar {{ background-color: {bg_rgba}; color: {on_bg}; border-radius: 0; }}\
         .workspace-btn {{ background: transparent; opacity: 0.45; color: {on_bg};\
             border-radius: {radius_sm}; border: none; outline: none; box-shadow: none;\
             min-width: 20px; margin: 5px 2px; padding: 2px 9px; }}\
         .workspace-btn:hover {{ opacity: 0.8; }}\
         .workspace-btn.active {{ background: alpha({accent}, 0.18); color: {accent}; opacity: 1; }}\
         .stats-box {{ margin-right: 8px; }}\
         .stat-pair {{ margin-right: 14px; }}\
         .stat-icon {{ margin-right: 2px; }}\
         .bt-icon {{ margin-right: 14px; }}\
         separator.bar-sep {{ min-height: 14px; margin: 0 8px 0 0; background: alpha({on_bg}, 0.14); }}\
         window.breadbar-notification {{ background-color: alpha({bg_plain}, 0.95); color: {on_bg}; }}\
         .notification-card {{ background: {surface}; color: {on_surface}; border-radius: {radius};\
             padding: {pad}; margin-bottom: 8px; border-left: 3px solid transparent; }}\
         .notification-card.urgency-critical {{ border-left-color: {critical}; }}\
         .notification-card.urgency-normal {{ border-left-color: {accent}; }}\
         .notification-summary {{ font-weight: bold; }}\
         .notification-app {{ opacity: 0.6; }}\
         window.breadbar-osd {{ background-color: alpha({bg_plain}, 0.95); color: {on_bg}; border-radius: {radius_pill}; }}\
         .osd-icon {{ opacity: 0.85; margin-right: 8px; }}\
         .osd-icon-muted {{ opacity: 0.35; }}\
         progressbar.osd-bar {{ min-height: 6px; }}\
         progressbar.osd-bar trough {{ background-image: none; background-color: {trough}; border-radius: 3px; min-height: 6px; }}\
         progressbar.osd-bar trough progress {{ background-image: none; background-color: {accent}; border-radius: 3px; min-height: 6px; }}\
         .clickable {{ cursor: pointer; }}\
         .wifi-pair {{ border-radius: {radius_sm}; padding: 0 2px; }}\
         .wifi-pair:hover {{ background: alpha({on_bg}, 0.12); }}\
         .wifi-popover-inner {{ min-width: 200px; padding: {pad}; }}\
         .popover-tab-row {{ margin-bottom: {pad}; }}\
         .popover-tab {{ background: transparent; color: {on_bg}; border: none; box-shadow: none;\
             outline: none; border-radius: {radius_sm}; padding: 4px 10px; font-size: 11px;\
             font-weight: bold; opacity: 0.55; }}\
         .popover-tab:hover {{ opacity: 0.8; }}\
         .popover-tab:checked {{ background: alpha({accent}, 0.18); color: {accent}; opacity: 1; }}\
         .wifi-popover-ssid {{ font-weight: bold; font-size: 13px; }}\
         .wifi-popover-ip {{ opacity: 0.6; font-size: 11px; }}\
         .wifi-popover-status {{ font-size: 11px; margin-top: 2px; }}\
         .wifi-popover-section {{ font-size: 10px; font-weight: bold; opacity: 0.5; letter-spacing: 0.08em; }}\
         .wifi-popover-row {{ background: transparent; border: none; box-shadow: none;\
             border-radius: {radius_sm}; padding: 4px 6px; }}\
         .wifi-popover-row:hover {{ background: alpha({on_bg}, 0.08); }}\
         .wifi-popover-row-active {{ color: {accent}; }}\
         .wifi-popover-row-unsaved {{ opacity: 0.4; }}\
         .wifi-popover-loading {{ opacity: 0.5; padding: 8px; }}\
         window.wifi-add-dialog {{ background-color: {bg_rgba}; color: {on_bg}; min-width: 240px; }}\
         .media-widget {{ border-radius: {radius_sm}; padding: 0 6px; cursor: pointer; }}\
         .media-widget:hover {{ background: alpha({on_bg}, 0.10); }}\
         .media-indicator {{ font-size: 11px; opacity: 0.7; margin-right: 2px; }}\
         .media-track-lbl {{ font-size: 12px; }}\
         .media-controls {{ padding: 2px; }}\
         .media-btn {{ min-width: 32px; padding: 4px 8px; }}\
         .control-panel-btn {{ padding: 0 6px; margin-left: 6px; border-radius: {radius_sm}; }}\
         .control-panel {{ }}\
         .control-panel-inner {{ min-width: 220px; padding: {pad}; }}\
         .control-panel-row {{ margin: 4px 0; }}\
         .control-panel-row-icon {{ opacity: 1; margin-right: 4px; }}\
         .control-panel-slider {{ margin: 0; }}\
         .control-panel-stats {{ margin: {pad} 0; }}\
         .control-panel-stat {{ font-size: 12px; opacity: 0.85; margin: 1px 0; }}\
         .control-panel-section {{ margin: {pad} 0; }}\
         .control-panel-section-header {{ font-size: 10px; font-weight: bold; opacity: 0.5;\
             letter-spacing: 0.08em; margin-bottom: 4px; }}\
         .control-panel-sink-dropdown {{ }}\
         .power-row {{ margin-top: 2px; }}\
         .power-btn {{ min-width: 40px; padding: 8px; border-radius: {radius_sm}; }}\
         separator {{ margin: 4px 0; }}",
        bg_plain   = p.background,
        bg_rgba    = hex_to_rgba(&p.background, 0.92),
        surface    = p.color0,
        accent     = p.color4,
        critical   = p.color1,
        on_bg      = ink_on(&p.background),
        on_surface = ink_on(&p.color0),
        trough     = hex_to_rgba(&p.color4, 0.25),
    )
}

/// Returns the ink colour for icon tinting in the stats bar — the same
/// luminance-picked colour the bar's text uses, so icons stay legible on the bar
/// whatever lightness pywal gives the background.
pub fn fg_color() -> String {
    ink_on(&load_palette().background).to_string()
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

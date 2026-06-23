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
    format!(
        "window.breadbar {{ background-color: {bg_rgba}; color: {on_bg}; border-radius: 0; }}\
         .workspace-btn {{ background: transparent; opacity: 0.45;\
             border-radius: 0; border: none; outline: none; box-shadow: none;\
             min-width: 24px; padding: 4px 8px; }}\
         .workspace-btn:hover {{ opacity: 0.8; }}\
         .workspace-btn.active {{ background: {accent}; color: {on_accent}; opacity: 1; }}\
         .stats-box {{ margin-right: 8px; }}\
         .stat-pair {{ margin-right: 14px; }}\
         .stat-icon {{ margin-right: 2px; }}\
         .bt-icon {{ margin-right: 14px; }}\
         window.breadbar-notification {{ background-color: alpha({bg_plain}, 0.95); color: {on_bg}; }}\
         .notification-card {{ background: {surface}; color: {on_surface}; border-radius: 8px;\
             padding: 12px; margin-bottom: 8px; }}\
         .notification-summary {{ font-weight: bold; }}\
         .notification-app {{ opacity: 0.6; }}\
         window.breadbar-osd {{ background-color: alpha({bg_plain}, 0.95); color: {on_bg}; border-radius: 8px; }}\
         .osd-kind {{ opacity: 0.75; font-size: 12px; }}\
         .osd-pct {{ font-weight: bold; font-size: 12px; }}\
         progressbar.osd-bar {{ min-height: 8px; }}\
         progressbar.osd-bar trough {{ background-image: none; background-color: {trough}; border-radius: 4px; min-height: 8px; }}\
         progressbar.osd-bar trough progress {{ background-image: none; background-color: {accent}; border-radius: 4px; min-height: 8px; }}\
         .clickable {{ cursor: pointer; }}\
         .wifi-pair {{ border-radius: 4px; padding: 0 2px; }}\
         .wifi-pair:hover {{ background: alpha({on_bg}, 0.12); }}\
         .wifi-popover-inner {{ min-width: 180px; padding: 2px; }}\
         .wifi-popover-ssid {{ font-weight: bold; font-size: 13px; }}\
         .wifi-popover-ip {{ opacity: 0.6; font-size: 11px; }}\
         .wifi-popover-status {{ font-size: 11px; margin-top: 2px; }}\
         .wifi-popover-section {{ font-size: 10px; font-weight: bold; opacity: 0.5; letter-spacing: 0.08em; }}\
         .wifi-popover-row {{ background: transparent; border: none; box-shadow: none;\
             border-radius: 4px; padding: 2px 6px; }}\
         .wifi-popover-row:hover {{ background: alpha({on_bg}, 0.08); }}\
         .wifi-popover-row-active {{ color: {accent}; }}\
         .wifi-popover-loading {{ opacity: 0.5; padding: 8px; }}\
         .media-widget {{ border-radius: 4px; padding: 0 6px; cursor: pointer; }}\
         .media-widget:hover {{ background: alpha({on_bg}, 0.10); }}\
         .media-indicator {{ font-size: 11px; opacity: 0.7; margin-right: 2px; }}\
         .media-track-lbl {{ font-size: 12px; }}\
         .media-controls {{ padding: 2px; }}\
         .media-btn {{ font-size: 16px; min-width: 36px; padding: 2px 8px; }}\
         .control-panel-btn {{ font-size: 14px; padding: 0 6px; margin-left: 6px; border-radius: 4px; }}\
         .control-panel {{ }}\
         .control-panel-inner {{ min-width: 240px; padding: 8px; }}\
         .control-panel-row {{ margin: 4px 0; }}\
         .control-panel-row-icon {{ opacity: 0.75; }}\
         .control-panel-slider {{ margin: 0; }}\
         .control-panel-stats {{ margin: 8px 0; }}\
         .control-panel-stat {{ font-size: 12px; opacity: 0.85; margin: 1px 0; }}\
         .control-panel-section {{ margin: 6px 0; }}\
         .control-panel-section-header {{ font-size: 10px; font-weight: bold; opacity: 0.5;\
             letter-spacing: 0.08em; margin-bottom: 4px; }}\
         .control-panel-sink-dropdown {{ }}\
         separator {{ margin: 4px 0; }}",
        bg_plain   = p.background,
        bg_rgba    = hex_to_rgba(&p.background, 0.92),
        surface    = p.color0,
        accent     = p.color4,
        on_bg      = ink_on(&p.background),
        on_surface = ink_on(&p.color0),
        on_accent  = ink_on(&p.color4),
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

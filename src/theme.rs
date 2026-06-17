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
         .stat-pair {{ margin-right: 12px; }}\
         .stat-icon {{ margin-right: 5px; }}\
         .bt-icon {{ margin-right: 12px; }}\
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
         progressbar.osd-bar trough progress {{ background-image: none; background-color: {accent}; border-radius: 4px; min-height: 8px; }}",
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

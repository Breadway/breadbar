use bread_theme::{gtk as bgtk, hex_to_rgba, load_palette};
use gtk4::CssProvider;
use std::cell::RefCell;

thread_local! {
    static PROVIDER:      RefCell<Option<CssProvider>> = const { RefCell::new(None) };
    static USER_PROVIDER: RefCell<Option<CssProvider>> = const { RefCell::new(None) };
}

fn load_css() -> String {
    let p = load_palette();
    // breadbar-specific rules only — fonts, base colours, and generic widgets
    // come from the shared ecosystem stylesheet (applied first in `apply()`).
    format!(
        "window.breadbar {{ background-color: {bg_rgba}; border-radius: 0; }}\
         label {{ color: {fg}; }}\
         .workspace-btn {{ background: transparent; color: {fg}; opacity: 0.45;\
             border-radius: 0; border: none; outline: none; box-shadow: none;\
             min-width: 24px; padding: 4px 8px; }}\
         .workspace-btn:hover {{ opacity: 0.8; }}\
         .workspace-btn.active {{ background: {accent}; opacity: 1; }}\
         .stats-box {{ margin-right: 8px; }}\
         .stat-pair {{ margin-right: 12px; }}\
         .stat-icon {{ margin-right: 5px; }}\
         .bt-icon {{ margin-right: 12px; }}\
         window.breadbar-notification {{ background-color: alpha({bg_plain}, 0.95); }}\
         .notification-card {{ background: {surface}; border-radius: 8px;\
             padding: 12px; margin-bottom: 8px; }}\
         .notification-summary {{ font-weight: bold; color: {fg}; }}\
         .notification-app {{ color: {fg}; opacity: 0.6; }}\
         .notification-body {{ color: {fg}; }}\
         window.breadbar-osd {{ background-color: alpha({bg_plain}, 0.95); border-radius: 8px; }}\
         .osd-kind {{ color: {fg}; opacity: 0.75; font-size: 12px; }}\
         .osd-pct {{ color: {fg}; font-weight: bold; font-size: 12px; }}\
         progressbar.osd-bar {{ min-height: 8px; }}\
         progressbar.osd-bar trough {{ background-image: none; background-color: {trough}; border-radius: 4px; min-height: 8px; }}\
         progressbar.osd-bar trough progress {{ background-image: none; background-color: {accent}; border-radius: 4px; min-height: 8px; }}",
        bg_plain = p.background,
        bg_rgba  = hex_to_rgba(&p.background, 0.92),
        surface  = p.color0,
        fg       = p.foreground,
        accent   = p.color4,
        trough   = hex_to_rgba(&p.color4, 0.25),
    )
}

/// Returns the current foreground colour (used for icon tinting in the stats bar).
pub fn fg_color() -> String {
    load_palette().foreground
}

/// Apply (or reload) the theme CSS. Safe to call from `glib::MainContext::invoke`.
pub fn apply() {
    // Shared ecosystem base (fonts, palette, generic widgets) — applied first
    // so breadbar's own rules below layer on top.
    bgtk::apply_shared();

    let css = load_css();
    PROVIDER.with(|cell| bgtk::apply_css(&css, cell));

    let home = std::env::var("HOME").unwrap_or_default();
    let user_path = std::path::PathBuf::from(format!("{home}/.config/breadbar/style.css"));
    USER_PROVIDER.with(|cell| bgtk::apply_user_css(&user_path, cell));
}

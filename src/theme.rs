use bread_theme::{gtk as bgtk, ink_on, load_palette, load_palette_for, Palette};
use gtk4::prelude::IsA;
use gtk4::CssProvider;
use std::cell::RefCell;

thread_local! {
    static USER_PROVIDER: RefCell<Option<CssProvider>> = const { RefCell::new(None) };
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
    let radius = "12px";
    let radius_bar = "16px";
    let radius_sm = "9px";
    let radius_pill = "999px";
    let pad = "12px";

    format!(
        "@keyframes notif-in {{ from {{ opacity: 0; margin-right: -16px; }} }}\
         @keyframes osd-in {{ from {{ opacity: 0; margin-bottom: -8px; }} }}\
         @keyframes media-eq {{ to {{ min-height: 14px; }} }}\
         @keyframes pop-in {{ from {{ opacity: 0; margin-top: -10px; }} to {{ opacity: 1; margin-top: 0; }} }}\
         @keyframes pop-out {{ from {{ opacity: 1; margin-top: 0; }} to {{ opacity: 0; margin-top: -6px; }} }}\
         @keyframes row-in {{ from {{ opacity: 0; margin-top: 8px; }} to {{ opacity: 1; margin-top: 0; }} }}\
         @keyframes digit-flip {{ from {{ opacity: 0; margin-top: 7px; }} to {{ opacity: 1; margin-top: 0; }} }}\
         @keyframes caret-draw {{ from {{ margin-right: 200px; opacity: 0.2; }} to {{ margin-right: 4px; opacity: 1; }} }}\
         window.breadbar {{ background-color: alpha(@bg, 0.72); color: @on-bg;\
             border-radius: {radius_bar}; border: 1px solid alpha(@on-bg, 0.08); }}\
         window.breadbar > centerbox {{ padding: 0 8px 0 6px; }}\
         window.breadbar button {{ min-height: 0; min-width: 0; }}\
         .workspace-trail {{ background-image: linear-gradient(90deg, @accent, @teal);\
             background-color: @accent; border-radius: 999px; }}\
         .workspace-btn {{ background: transparent; opacity: 0.36; color: @on-bg;\
             border-radius: 999px; border: none; outline: none; box-shadow: none;\
             min-width: 32px; min-height: 32px; margin: 0 2px; padding: 0 12px;\
             font-size: 22px; font-weight: bold;\
             transition: opacity 0.22s cubic-bezier(0.22, 1.2, 0.36, 1),\
                 background-color 0.22s cubic-bezier(0.22, 1.2, 0.36, 1); }}\
         .workspace-btn:hover {{ opacity: 0.85; background: alpha(@on-bg, 0.08); }}\
         .workspace-btn.occupied {{ opacity: 0.78; }}\
         .workspace-btn.active {{ background: transparent; color: @on-accent; opacity: 1; }}\
         .workspace-btn.active:hover {{ background: transparent; }}\
         .workspace-btn.ws-in {{ animation: row-in 0.32s cubic-bezier(0.22, 1.2, 0.36, 1) both; }}\
         .clock-box {{ padding: 0 4px; }}\
         .clock-label {{ font-size: 24px; font-weight: bold; letter-spacing: 0.04em;\
             min-height: 0; padding: 0; margin-top: 3px; }}\
         .clock-digit {{ font-size: 24px; font-weight: bold; letter-spacing: 0.04em;\
             min-width: 15px; min-height: 0; padding: 0; margin: 0; }}\
         .clock-colon {{ min-width: 10px; opacity: 0.7; }}\
         .clock-digit.flip {{ animation: digit-flip 0.45s cubic-bezier(0.22, 1.35, 0.36, 1) both; }}\
         .date-label {{ font-size: 14px; opacity: 0.52; letter-spacing: 0.04em; }}\
         .stat-label {{ font-size: 14px; letter-spacing: 0.02em; opacity: 0.92; }}\
         .stat-label.tick {{ animation: digit-flip 0.35s cubic-bezier(0.22, 1.35, 0.36, 1) both; }}\
         .stats-box {{ margin-right: 0; }}\
         .stat-pair {{ margin: 0; border-radius: 10px; padding: 5px 9px; min-height: 0;\
             transition: background-color 0.22s cubic-bezier(0.22, 1.2, 0.36, 1),\
                 opacity 0.18s ease; }}\
         .stat-pair:hover {{ background: alpha(@on-bg, 0.12); }}\
         .stat-pair:active {{ background: alpha(@on-bg, 0.18); }}\
         .stat-pair.icon-only {{ padding: 6px; border-radius: 999px; }}\
         .stat-icon {{ margin-right: 6px; }}\
         .stat-pair.icon-only .stat-icon {{ margin-right: 0; }}\
         .bt-icon {{ margin-right: 8px; }}
         separator.bar-sep {{ min-height: 12px; min-width: 1px; margin: 0 10px 0 2px;\
             background: alpha(@on-bg, 0.10); }}\
         window.breadbar-notification {{ background-color: transparent; color: @on-bg; }}\
         window.breadbar-history {{ background-color: alpha(@bg, 0.70); color: @on-bg;\
             border-radius: {radius}; border: 1px solid alpha(@on-bg, 0.10);\
             animation: pop-in 0.45s cubic-bezier(0.22, 1.35, 0.36, 1) both; }}\
         .notification-card {{ background: alpha(@bg, 0.70); color: @on-bg; border-radius: {radius};\
             padding: {pad}; margin-bottom: 8px; border: 1px solid alpha(@on-bg, 0.10);\
             border-left: 3px solid transparent;\
             animation: notif-in 0.45s cubic-bezier(0.22, 1.2, 0.36, 1) both; }}\
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
             animation: osd-in 0.4s cubic-bezier(0.22, 1.2, 0.36, 1) both; }}\
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
             animation: caret-draw 0.45s cubic-bezier(0.22, 1.35, 0.36, 1) both; }}\
         .wifi-popover-inner {{ min-width: 228px; padding: {pad}; }}\
         window.wifi-popover button {{ min-height: 0; min-width: 0; }}\
         .popover-tab-row {{ background: alpha(@on-bg, 0.06); border-radius: 10px;\
             padding: 3px; margin-bottom: 10px; }}\
         .popover-tab {{ background: transparent; color: @on-bg; border: none; box-shadow: none;\
             outline: none; border-radius: 999px; padding: 0 14px; min-height: 32px;\
             font-size: 17px; font-weight: bold; opacity: 0.55;\
             transition: background-color 0.22s cubic-bezier(0.22, 1.2, 0.36, 1),\
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
             transition: background-color 0.18s cubic-bezier(0.22, 1.2, 0.36, 1); }}\
         .wifi-popover-row label {{ font-size: 18px; }}\
         .wifi-popover-row:hover {{ background: alpha(@on-bg, 0.08); }}\
         .wifi-popover-row-active {{ background: alpha(@accent, 0.14); color: @accent; }}\
         .wifi-popover-row-active:hover {{ background: alpha(@accent, 0.20); }}\
         .row-in {{ animation: row-in 0.32s cubic-bezier(0.22, 1.35, 0.36, 1) both; }}\
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
             transition: background-color 0.25s cubic-bezier(0.22, 1.2, 0.36, 1); }}\
         switch.bt-switch:checked {{ background-color: @accent; }}\
         switch.bt-switch slider {{ min-width: 20px; min-height: 20px; margin: 0;\
             border-radius: 99px; border: none; outline: none; box-shadow: none;\
             background-image: none; background-color: @on-bg; }}\
         window.wifi-add-dialog {{ background-color: alpha(@bg, 0.70); color: @on-bg; min-width: 240px;\
             border-radius: {radius}; border: 1px solid alpha(@on-bg, 0.10);\
             animation: pop-in 0.45s cubic-bezier(0.22, 1.35, 0.36, 1) both; }}\
         window.wifi-add-dialog headerbar {{ background-color: alpha(@bg, 0.70); color: @on-bg;\
             border-top-left-radius: {radius}; border-top-right-radius: {radius};\
             border-bottom: 1px solid alpha(@on-bg, 0.10); box-shadow: none; }}\
         .confirm-button {{ background-color: @accent; color: @on-accent; }}\
         .confirm-button:hover {{ background-color: alpha(@accent, 0.85); }}\
         .media-widget {{ border-radius: 10px; padding: 4px 8px; min-height: 0;\
             transition: background-color 0.22s cubic-bezier(0.22, 1.2, 0.36, 1); }}\
         .media-widget:hover {{ background: alpha(@on-bg, 0.08); }}\
         .media-widget.media-in {{ animation: row-in 0.4s cubic-bezier(0.22, 1.35, 0.36, 1) both; }}\
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
             transition: background-color 0.22s cubic-bezier(0.22, 1.2, 0.36, 1),\
                 opacity 0.18s ease; }}\
         .control-panel-btn:hover {{ opacity: 1; background: alpha(@on-bg, 0.10); }}\
         .control-panel-btn:active {{ background: alpha(@on-bg, 0.16); }}\
         .control-panel {{ }}\
         .control-panel-inner {{ min-width: 220px; padding: {pad}; }}\
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
         .power-row {{ margin-top: 8px; }}\
         .power-btn {{ min-width: 0; min-height: 0; padding: 8px 10px; border-radius: 8px;\
             background: alpha(@on-bg, 0.08); font-size: 13px; border: none;\
             outline: none; box-shadow: none;\
             transition: background-color 0.2s cubic-bezier(0.22, 1.2, 0.36, 1); }}\
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
         .bread-padding-md {{ padding: 12px; }}",
        radius = radius,
        radius_bar = radius_bar,
        radius_sm = radius_sm,
        radius_pill = radius_pill,
        pad = pad,
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

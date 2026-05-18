use gtk4::CssProvider;
use serde::Deserialize;

#[derive(Deserialize)]
struct WalColors {
    special: Special,
    colors: Colors,
}

#[derive(Deserialize)]
struct Special {
    background: String,
}

#[derive(Deserialize)]
struct Colors {
    color0: String,
    color1: String,
    color15: String,
}

fn hex_to_rgba(hex: &str, alpha: f32) -> String {
    let h = hex.trim_start_matches('#');
    let r = u8::from_str_radix(&h[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&h[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&h[4..6], 16).unwrap_or(0);
    format!("rgba({r},{g},{b},{alpha})")
}

fn load_css() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let text = std::fs::read_to_string(format!("{home}/.cache/wal/colors.json"))
        .unwrap_or_default();

    let (bg, surface, fg, accent) = if let Ok(wal) = serde_json::from_str::<WalColors>(&text) {
        (wal.special.background, wal.colors.color0, wal.colors.color15, wal.colors.color1)
    } else {
        (
            "#1e1e2e".to_string(),
            "#181825".to_string(),
            "#cdd6f4".to_string(),
            "#89b4fa".to_string(),
        )
    };

    format!(
        "* {{ font-family: 'JetBrainsMono Nerd Font Mono', monospace; font-size: 12px; }}\
         window.breadbar {{ background-color: {bg_rgba}; border-radius: 0; }}\
         .workspace-btn {{ background: transparent; color: {fg}; opacity: 0.45;\
             border-radius: 0 0 8px 8px; border: none; outline: none; box-shadow: none;\
             min-width: 24px; padding: 2px 8px; }}\
         .workspace-btn:hover {{ opacity: 0.8; }}\
         .workspace-btn.active {{ background: {accent}; opacity: 1; }}\
         label {{ color: {fg}; }}\
         window.breadbar-notification {{ background-color: alpha({bg_plain}, 0.95); }}\
         .notification-card {{ background: {surface}; border-radius: 6px;\
             padding: 10px; margin-bottom: 4px; }}\
         .notification-summary {{ font-weight: bold; color: {fg}; }}\
         .notification-app {{ color: {fg}; opacity: 0.6; }}\
         .notification-body {{ color: {fg}; }}",
        bg_plain = bg,
        bg_rgba = hex_to_rgba(&bg, 0.92),
        surface = surface,
        fg = fg,
        accent = accent,
    )
}

pub fn apply() {
    let provider = CssProvider::new();
    provider.load_from_string(&load_css());
    gtk4::style_context_add_provider_for_display(
        &gtk4::gdk::Display::default().expect("no display"),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

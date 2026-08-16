//! Turns a `WidgetNode` tree into a live GTK4 widget tree.
//!
//! There is no diffing at the node level — see `client.rs`'s module doc for
//! why the whole thing is simply rebuilt whenever a widget's spec changes.
//! This keeps the renderer a pure, stateless `WidgetNode -> gtk4::Widget`
//! function.

use super::client;
use bread_shared::widget::{
    Align as StyleAlign, Background, FontWeight, Orientation as NodeOrientation, Padding, Radius,
    SemanticColor, TextSize, WidgetNode, WidgetStyle,
};
use gtk4::prelude::*;

/// Default max width for a Label node, in characters, absent an explicit
/// `size`/other override — the `style` vocabulary (see Documentation.md's
/// Widgets §style) has no dedicated width field yet, so this stays fixed for
/// every label rather than becoming a half-exposed knob.
const DEFAULT_LABEL_MAX_WIDTH_CHARS: i32 = 32;

/// Curated bundled icons a widget can reference by name, so module authors
/// don't need to ship an SVG just to show a battery or bluetooth glyph.
/// Anything else goes through `icon.path` instead (see `bundled_or_path_icon`).
fn bundled_icon(name: &str) -> Option<&'static str> {
    use crate::bar::stats::{
        AC_POWER, BAT_HIGH, BAT_LOW, BAT_MID, BT_OFF, BT_ON, ICON_BRIGHTNESS, ICON_LOCK,
        ICON_RESTART, ICON_SHUTDOWN, ICON_SLEEP, ICON_VOLUME, WIFI_MEDIUM, WIFI_OFF, WIFI_STRONG,
        WIFI_WEAK,
    };
    Some(match name {
        "ac-power" => AC_POWER,
        "battery-high" => BAT_HIGH,
        "battery-mid" => BAT_MID,
        "battery-low" => BAT_LOW,
        "bluetooth-on" => BT_ON,
        "bluetooth-off" => BT_OFF,
        "wifi-strong" => WIFI_STRONG,
        "wifi-medium" => WIFI_MEDIUM,
        "wifi-weak" => WIFI_WEAK,
        "wifi-off" => WIFI_OFF,
        "lock" => ICON_LOCK,
        "sleep" => ICON_SLEEP,
        "restart" => ICON_RESTART,
        "shutdown" => ICON_SHUTDOWN,
        "volume" => ICON_VOLUME,
        "brightness" => ICON_BRIGHTNESS,
        _ => return None,
    })
}

fn icon_texture(
    widget_id: &str,
    name: Option<&str>,
    path: Option<&str>,
    px: u32,
) -> Option<gtk4::gdk::Texture> {
    if let Some(n) = name {
        return match bundled_icon(n) {
            Some(svg) => Some(crate::svg_texture_sized(svg, px)),
            None => {
                eprintln!("breadbar: widget {widget_id}: unknown bundled icon name '{n}'");
                None
            }
        };
    }
    let Some(path) = path else {
        eprintln!("breadbar: widget {widget_id}: icon node has neither 'name' nor 'path'");
        return None;
    };
    let expanded = bread_shared::expand_path(path);
    match std::fs::read_to_string(&expanded) {
        Ok(svg) => Some(crate::svg_texture_sized(&svg, px)),
        Err(e) => {
            eprintln!("breadbar: widget {widget_id}: failed to read icon path '{path}': {e}");
            None
        }
    }
}

/// Map a node's typed `style` onto predefined CSS classes (see `theme.rs` for
/// the class definitions) — this is the only path from Lua's `style` field to
/// the widget, kept as narrow `Some(field) -> one class` mappings so there is
/// no way for it to become raw style injection.
fn apply_style(widget: &gtk4::Widget, style: &WidgetStyle) {
    if let Some(color) = style.color {
        widget.add_css_class(match color {
            SemanticColor::Fg => "bread-color-fg",
            SemanticColor::Dim => "bread-color-dim",
            SemanticColor::Accent => "bread-color-accent",
            SemanticColor::Red => "bread-color-red",
            SemanticColor::Green => "bread-color-green",
            SemanticColor::Yellow => "bread-color-yellow",
            SemanticColor::Blue => "bread-color-blue",
            SemanticColor::Pink => "bread-color-pink",
            SemanticColor::Teal => "bread-color-teal",
        });
    }
    if let Some(weight) = style.weight {
        widget.add_css_class(match weight {
            FontWeight::Normal => "bread-weight-normal",
            FontWeight::Bold => "bread-weight-bold",
        });
    }
    if let Some(size) = style.size {
        widget.add_css_class(match size {
            TextSize::Xs => "bread-size-xs",
            TextSize::Sm => "bread-size-sm",
            TextSize::Md => "bread-size-md",
            TextSize::Lg => "bread-size-lg",
            TextSize::Xl => "bread-size-xl",
        });
    }
    if let Some(background) = style.background {
        widget.add_css_class(match background {
            Background::None => "bread-bg-none",
            Background::Surface => "bread-bg-surface",
            Background::Card => "bread-bg-card",
        });
    }
    if let Some(radius) = style.radius {
        widget.add_css_class(match radius {
            Radius::None => "bread-radius-none",
            Radius::Sm => "bread-radius-sm",
            Radius::Md => "bread-radius-md",
            Radius::Full => "bread-radius-full",
        });
    }
    if let Some(padding) = style.padding {
        widget.add_css_class(match padding {
            Padding::None => "bread-padding-none",
            Padding::Xs => "bread-padding-xs",
            Padding::Sm => "bread-padding-sm",
            Padding::Md => "bread-padding-md",
        });
    }
    // GTK CSS has no text-align/justify-content equivalent — alignment is a
    // widget property, not a stylesheet rule, so it's set directly instead
    // of routing through an inert CSS class like the fields above.
    if let Some(align) = style.align {
        widget.set_halign(match align {
            StyleAlign::Start => gtk4::Align::Start,
            StyleAlign::Center => gtk4::Align::Center,
            StyleAlign::End => gtk4::Align::End,
        });
    }
}

/// Build (or rebuild) the GTK widget tree for `node`, belonging to widget
/// `widget_id` (fully-qualified `<module>.<id>`, used to tag any click).
pub fn build_node(node: &WidgetNode, widget_id: &str) -> gtk4::Widget {
    let widget: gtk4::Widget = match node {
        WidgetNode::Box {
            orientation,
            spacing,
            children,
            ..
        } => {
            let gtk_orientation = match orientation {
                NodeOrientation::Horizontal => gtk4::Orientation::Horizontal,
                NodeOrientation::Vertical => gtk4::Orientation::Vertical,
            };
            let container = gtk4::Box::new(gtk_orientation, spacing.unwrap_or(4));
            for child in children {
                container.append(&build_node(child, widget_id));
            }
            container.upcast()
        }
        WidgetNode::Label { text, .. } => {
            let label = gtk4::Label::new(Some(text));
            // Unbounded, this is a bar-width-blowout waiting to happen from
            // any buggy or malicious module — see Documentation.md issue #6.
            label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            label.set_max_width_chars(DEFAULT_LABEL_MAX_WIDTH_CHARS);
            label.upcast()
        }
        WidgetNode::Icon { name, path, size, .. } => {
            let px = size.unwrap_or(16).max(1) as u32;
            let texture = icon_texture(widget_id, name.as_deref(), path.as_deref(), px);
            let image = gtk4::Image::from_paintable(texture.as_ref());
            crate::prepare_icon(&image, px as i32);
            image.upcast()
        }
        WidgetNode::Progress { value, .. } => {
            let bar = gtk4::ProgressBar::new();
            bar.set_fraction(value.clamp(0.0, 1.0));
            // GtkProgressBar's natural expand behavior is to fill all
            // available width, which — unlike Label/Box/Image, which hug
            // their content by default — propagates up through every
            // ancestor Box that doesn't set hexpand explicitly, all the way
            // to the bar's end_widget. Pin it to a small fixed footprint so
            // it reads as an inline meter instead of swallowing the bar.
            bar.set_hexpand(false);
            bar.set_valign(gtk4::Align::Center);
            bar.set_size_request(40, 6);
            bar.upcast()
        }
    };

    widget.add_css_class("bread-widget-node");
    if let Some(class) = node.class() {
        widget.add_css_class(class);
    }
    if let Some(style) = node.style() {
        apply_style(&widget, style);
    }

    if let Some(action) = node.on_click() {
        widget.add_css_class("clickable");
        let widget_id = widget_id.to_string();
        let action = action.clone();
        let gesture = gtk4::GestureClick::new();
        gesture.connect_released(move |_, _, _, _| {
            client::emit_click(&widget_id, &action);
        });
        widget.add_controller(gesture);
    }

    widget
}

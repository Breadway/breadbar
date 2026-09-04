//! Theme-declared bar widgets (`[[bar.widget]]`).
//!
//! A shell theme can declare a small live widget — a poll command plus a
//! node tree — without a Lua module. This module maps
//! `bread_theme::shell::ThemeWidget` onto the exact same
//! `bread_shared::widget::WidgetSpec` the daemon path produces, so
//! `App::reconcile_widgets` renders both through one code path
//! (`widgets::build_node`). The poll runs the `bind.cmd` on an interval and
//! feeds its trimmed stdout back as `{value}` in the tree.

use bread_shared::widget::{
    FontWeight, Orientation, SemanticColor, TextSize, WidgetNode, WidgetPlacement, WidgetSpec,
    WidgetStyle,
};
use bread_theme::shell::{ThemeNode, ThemeWidget};
use relm4::ComponentSender;

use crate::{App, AppInput};

/// Aborts its poll task when dropped — a theme reload replaces the whole set.
pub struct PollHandle(tokio::task::JoinHandle<()>);

impl Drop for PollHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn color(s: &str) -> Option<SemanticColor> {
    Some(match s {
        "fg" => SemanticColor::Fg,
        "dim" => SemanticColor::Dim,
        "accent" => SemanticColor::Accent,
        "red" => SemanticColor::Red,
        "green" => SemanticColor::Green,
        "yellow" => SemanticColor::Yellow,
        "blue" => SemanticColor::Blue,
        "pink" => SemanticColor::Pink,
        "teal" => SemanticColor::Teal,
        _ => return None,
    })
}

fn weight(s: &str) -> Option<FontWeight> {
    match s {
        "normal" => Some(FontWeight::Normal),
        "bold" => Some(FontWeight::Bold),
        _ => None,
    }
}

fn size(s: &str) -> Option<TextSize> {
    Some(match s {
        "xs" => TextSize::Xs,
        "sm" => TextSize::Sm,
        "md" => TextSize::Md,
        "lg" => TextSize::Lg,
        "xl" => TextSize::Xl,
        _ => return None,
    })
}

fn label_style(
    c: &Option<String>,
    w: &Option<String>,
    sz: &Option<String>,
) -> Option<WidgetStyle> {
    let color = c.as_deref().and_then(color);
    let weight = w.as_deref().and_then(weight);
    let size = sz.as_deref().and_then(size);
    if color.is_none() && weight.is_none() && size.is_none() {
        return None;
    }
    Some(WidgetStyle {
        color,
        weight,
        size,
        ..WidgetStyle::default()
    })
}

/// `bread_theme::shell::ThemeNode` → `bread_shared::widget::WidgetNode`,
/// substituting `{value}` into every label as it goes. The two enums are
/// deliberately the same shape, so this is a mechanical 1:1 map.
fn map_node(node: &ThemeNode, value: &str) -> WidgetNode {
    match node {
        ThemeNode::Box {
            orientation,
            spacing,
            class,
            children,
        } => WidgetNode::Box {
            orientation: if orientation == "vertical" {
                Orientation::Vertical
            } else {
                Orientation::Horizontal
            },
            spacing: spacing.map(|s| s as i32),
            class: class.clone(),
            style: None,
            on_click: None,
            children: children.iter().map(|c| map_node(c, value)).collect(),
        },
        ThemeNode::Label {
            text,
            class,
            color,
            weight,
            size,
        } => WidgetNode::Label {
            text: text.replace("{value}", value),
            class: class.clone(),
            style: label_style(color, weight, size),
            on_click: None,
        },
        ThemeNode::Icon {
            name,
            path,
            size,
            class,
        } => WidgetNode::Icon {
            name: name.clone(),
            path: path.clone(),
            size: size.map(|s| s as i32),
            class: class.clone(),
            style: None,
            on_click: None,
        },
        ThemeNode::Progress { value: v, class } => WidgetNode::Progress {
            value: *v,
            class: class.clone(),
            style: None,
            on_click: None,
        },
    }
}

/// Build the `WidgetSpec` for `tw` given its latest polled `value`. `module`
/// is set to the theme's `slot` so `reconcile_widgets`' existing "the slot
/// entry's `widget:<module>` container wins" branch routes it into place.
pub fn to_spec(tw: &ThemeWidget, value: &str) -> WidgetSpec {
    WidgetSpec {
        id: format!("theme.{}", tw.id),
        module: tw.slot.clone(),
        placement: WidgetPlacement::LeftOfStats,
        order: tw.order,
        visible: true,
        tooltip: None,
        root: map_node(&tw.node, value),
        updated_at: 0,
    }
}

/// One poll task per widget: run `sh -c <cmd>`, trim stdout, send it back as
/// an `AppInput::ThemeWidgetTick`, sleep `every_ms`, repeat. The first tick
/// fires immediately so the widget isn't blank until the first interval.
pub fn spawn_pollers(widgets: &[ThemeWidget], sender: &ComponentSender<App>) -> Vec<PollHandle> {
    widgets
        .iter()
        .map(|w| {
            let id = w.id.clone();
            let cmd = w.bind.cmd.clone();
            let every = std::time::Duration::from_millis(w.bind.every_ms);
            let sender = sender.clone();
            let handle = relm4::spawn(async move {
                loop {
                    let out = tokio::process::Command::new("sh")
                        .arg("-c")
                        .arg(&cmd)
                        .output()
                        .await;
                    let value = match out {
                        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
                        Err(e) => {
                            eprintln!("breadbar: bar.widget '{id}' command failed: {e}");
                            String::new()
                        }
                    };
                    sender.input(AppInput::ThemeWidgetTick {
                        id: id.clone(),
                        value,
                    });
                    tokio::time::sleep(every).await;
                }
            });
            PollHandle(handle)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bread_shared::widget::WidgetNode;
    use bread_theme::shell::{Bind, ThemeNode, ThemeWidget};

    fn widget(node: ThemeNode) -> ThemeWidget {
        ThemeWidget {
            id: "load".into(),
            slot: "right_of_clock".into(),
            order: 3,
            bind: Bind {
                cmd: "true".into(),
                every_ms: 5000,
            },
            node,
        }
    }

    #[test]
    fn to_spec_routes_by_slot_and_substitutes_value() {
        let tw = widget(ThemeNode::Box {
            orientation: "horizontal".into(),
            spacing: Some(4),
            class: Some("bread-chip".into()),
            children: vec![
                ThemeNode::Icon {
                    name: Some("cpu".into()),
                    path: None,
                    size: Some(14),
                    class: None,
                },
                ThemeNode::Label {
                    text: "load {value}".into(),
                    class: None,
                    color: Some("accent".into()),
                    weight: Some("bold".into()),
                    size: None,
                },
            ],
        });
        let spec = to_spec(&tw, "2.71");
        assert_eq!(spec.id, "theme.load");
        assert_eq!(spec.module, "right_of_clock");
        assert_eq!(spec.order, 3);
        match spec.root {
            WidgetNode::Box { children, .. } => match &children[1] {
                WidgetNode::Label { text, style, .. } => {
                    assert_eq!(text, "load 2.71");
                    assert!(style.is_some());
                }
                _ => panic!("expected label child"),
            },
            _ => panic!("expected box root"),
        }
    }

    #[test]
    fn unknown_style_value_maps_to_none_not_a_panic() {
        let tw = widget(ThemeNode::Label {
            text: "{value}".into(),
            class: None,
            color: Some("chartreuse".into()),
            weight: None,
            size: None,
        });
        let spec = to_spec(&tw, "x");
        match spec.root {
            WidgetNode::Label { style, .. } => assert!(style.is_none()),
            _ => panic!(),
        }
    }
}

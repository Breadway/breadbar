//! BAR-30 phase 4 / BAR-31: the surtitle, a one-line notification caption
//! shown under the pill when `[notifications].style = "surtitle"`
//! (`bos-ui-demos/proposed/spotlight-stage.html`'s `.surtitle`). Its own
//! small layer-shell window, not part of the bar surface itself or the
//! standard toast window: `notifications::popup::run` skips building a
//! toast card entirely under this style and calls into here instead via
//! `main.rs`'s `AppInput::NotificationArrived`.
//!
//! Motion follows the mockup: the caption fades over .25s while the surface
//! slides down its last 6px on the spring curve over .35s, and hiding plays
//! the same in reverse before the window unmaps. It sits on the Overlay
//! layer so, like the mockup's `z-index:39` caption, it shows above an open
//! stage.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, Layer, LayerShell};

use crate::motion;

/// Distance from the screen top to the caption (`.surtitle{top:48px}`).
const TOP_MARGIN: i32 = 48;
/// `.surtitle{transform:translate(-50%,-6px)}` while hidden.
const SLIDE: i32 = 6;
/// `.surtitle{max-width:min(560px,90vw)}`.
const MAX_WIDTH: i32 = 560;
/// Horizontal chrome around the two labels: 14px padding and a 1px border
/// each side, plus the 10px gap between app name and message.
const CHROME: i32 = 2 * 14 + 2 + 10;

/// The message's width budget in px for a caption whose app label measures
/// `app_width`, on an output `output_width` px wide.
pub fn message_budget(output_width: i32, app_width: i32) -> i32 {
    let cap = if output_width > 0 {
        MAX_WIDTH.min(output_width * 9 / 10)
    } else {
        MAX_WIDTH
    };
    (cap - CHROME - app_width).max(0)
}

#[derive(Clone)]
pub struct Surtitle {
    window: gtk4::Window,
    row: gtk4::Box,
    app_lbl: gtk4::Label,
    msg_lbl: gtk4::Label,
    /// Debounce token: a newer `show()` or `hide()` must not be undone by an
    /// older call's timer.
    token: Rc<Cell<u32>>,
    slide: Rc<RefCell<Option<gtk4::TickCallbackId>>>,
    /// A critical caption is showing: it stays until clicked or the stage
    /// opens, rather than timing out.
    critical: Rc<Cell<bool>>,
}

impl Surtitle {
    /// Shows the caption for `duration` (the theme's `notification_peek_ms`,
    /// the same 3.8s the mockup's `notify()` uses for both). A critical
    /// notification stays up until it is clicked or the stage opens, with
    /// its app name in the critical colour: a normal caption would let an
    /// urgent message vanish unread after 3.8s.
    pub fn show(&self, app: &str, msg: &str, critical: bool, duration: Duration) {
        self.app_lbl.set_label(&app.to_uppercase());
        self.msg_lbl.set_label(msg);
        if critical {
            self.row.add_css_class("critical");
        } else {
            self.row.remove_css_class("critical");
        }
        self.critical.set(critical);
        crate::theme::pin_focused_output(&self.window);
        self.fit_width();

        let token = self.bump();
        let already_up = self.window.is_visible() && self.row.has_css_class("show");
        self.window.set_visible(true);
        self.window.present();
        if !already_up {
            self.row.remove_css_class("show");
            self.window.set_margin(Edge::Top, TOP_MARGIN - SLIDE);
            // Next main-loop turn, so the row is mapped at opacity 0 before
            // the class that fades it in lands (a CSS transition needs a
            // start state).
            let s = self.clone();
            gtk4::glib::idle_add_local_once(move || {
                if s.token.get() == token {
                    s.row.add_css_class("show");
                    s.slide_to(TOP_MARGIN);
                }
            });
        }
        if !critical {
            let s = self.clone();
            gtk4::glib::timeout_add_local_once(duration, move || {
                if s.token.get() == token {
                    s.hide();
                }
            });
        }
    }

    /// Hides the caption (its timer ran out, it was clicked, or the stage
    /// opened: the mockup's `setOpen(true)` clears `.surtitle.show`).
    pub fn hide(&self) {
        let token = self.bump();
        self.critical.set(false);
        if !self.window.is_visible() {
            return;
        }
        self.row.remove_css_class("show");
        self.slide_to(TOP_MARGIN - SLIDE);
        let s = self.clone();
        let fade = if motion::animations_enabled() { 350 } else { 0 };
        gtk4::glib::timeout_add_local_once(Duration::from_millis(fade), move || {
            if s.token.get() == token {
                s.window.set_visible(false);
            }
        });
    }

    /// Destroys the caption window; see `App::shutdown`.
    pub fn destroy(&self) {
        self.window.destroy();
    }

    fn bump(&self) -> u32 {
        let token = self.token.get().wrapping_add(1);
        self.token.set(token);
        token
    }

    fn slide_to(&self, target: i32) {
        if let Some(id) = self.slide.borrow_mut().take() {
            id.remove();
        }
        let from = f64::from(LayerShell::margin(&self.window, Edge::Top));
        let win = self.window.clone();
        *self.slide.borrow_mut() =
            motion::animate(&self.window, 350.0, 0.0, motion::SPRING, move |p| {
                win.set_margin(
                    Edge::Top,
                    motion::lerp(from, f64::from(target), p).round() as i32,
                );
            });
    }

    /// Caps the message so the whole caption stays within
    /// `min(560px, 90vw)`. GTK CSS has no max-width; a label's
    /// `max-width-chars` is measured in the font's approximate character
    /// width, so the pixel budget converts through that same metric.
    fn fit_width(&self) {
        let output_width = LayerShell::monitor(&self.window)
            .map(|m| m.geometry().width())
            .unwrap_or(0);
        let (_, app_width, _, _) = self.app_lbl.measure(gtk4::Orientation::Horizontal, -1);
        let budget = message_budget(output_width, app_width);
        let metrics = self.msg_lbl.pango_context().metrics(None, None);
        let char_px = (metrics.approximate_char_width() / gtk4::pango::SCALE).max(1);
        self.msg_lbl.set_max_width_chars((budget / char_px).max(1));
    }

    /// Whether a critical caption is currently pinned up.
    pub fn showing_critical(&self) -> bool {
        self.critical.get()
    }

    /// The underlying window, for the "surtitle" screenshot view's capture
    /// (`screenshot::dispatch`'s `Handles::surtitle_window`).
    pub fn window_for_screenshot(&self) -> gtk4::Window {
        self.window.clone()
    }
}

pub fn spawn() -> Surtitle {
    let window = gtk4::Window::new();
    window.add_css_class("breadbar-surtitle");
    window.init_layer_shell();
    window.set_namespace(Some("breadbar-surtitle"));
    window.set_layer(Layer::Overlay);
    window.set_anchor(Edge::Top, true);
    window.set_margin(Edge::Top, TOP_MARGIN - SLIDE);
    window.set_exclusive_zone(-1);
    window.set_visible(false);

    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    row.add_css_class("surtitle-row");
    // `align-items:baseline`.
    let app_lbl = gtk4::Label::new(None);
    app_lbl.add_css_class("surtitle-app");
    app_lbl.set_valign(gtk4::Align::Baseline);
    row.append(&app_lbl);
    let msg_lbl = gtk4::Label::new(None);
    msg_lbl.add_css_class("surtitle-msg");
    msg_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    msg_lbl.set_single_line_mode(true);
    msg_lbl.set_valign(gtk4::Align::Baseline);
    msg_lbl.set_xalign(0.0);
    row.append(&msg_lbl);
    window.set_child(Some(&row));

    let surtitle = Surtitle {
        window,
        row,
        app_lbl,
        msg_lbl,
        token: Rc::new(Cell::new(0)),
        slide: Rc::new(RefCell::new(None)),
        critical: Rc::new(Cell::new(false)),
    };
    // A click dismisses the caption (the only way to clear a critical one
    // short of opening the stage).
    let click = gtk4::GestureClick::new();
    let s = surtitle.clone();
    click.connect_released(move |_, _, _, _| s.hide());
    surtitle.row.add_controller(click);
    surtitle
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_is_560_minus_chrome_and_app_on_a_wide_output() {
        assert_eq!(message_budget(1920, 50), 560 - CHROME - 50);
    }

    #[test]
    fn budget_follows_ninety_percent_of_a_narrow_output() {
        assert_eq!(message_budget(400, 50), 360 - CHROME - 50);
    }

    #[test]
    fn budget_never_goes_negative_and_handles_an_unknown_output() {
        assert_eq!(message_budget(10, 500), 0);
        assert_eq!(message_budget(0, 0), 560 - CHROME);
    }
}

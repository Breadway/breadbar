use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use tokio::sync::mpsc::Receiver;

use super::{Expire, NotifEvent};

type Cards = Rc<RefCell<HashMap<u32, gtk4::Box>>>;
// Bumped every time an id gets a (re)placed card — an auto-dismiss timer
// scheduled for an earlier Show captures the generation it was scheduled
// under, and checks it's still current before dismissing. Without this, a
// notification that replaces an existing id (replaces_id) doesn't cancel
// the original's timer, so the *replacement* card gets dismissed on the
// *original*'s deadline instead of its own.
type Generations = Rc<RefCell<HashMap<u32, u64>>>;

/// NotificationClosed reason codes per the freedesktop spec.
mod close_reason {
    pub const EXPIRED: u32 = 1;
    #[allow(dead_code)] // no in-app dismiss button exists yet (see make_card)
    pub const DISMISSED_BY_USER: u32 = 2;
    pub const CLOSE_NOTIFICATION_CALL: u32 = 3;
}

pub async fn run(mut rx: Receiver<NotifEvent>, conn: zbus::Connection) {
    let window = create_window();
    let cards_box = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    cards_box.set_margin_top(8);
    cards_box.set_margin_bottom(8);
    cards_box.set_margin_start(8);
    cards_box.set_margin_end(8);
    window.set_child(Some(&cards_box));

    let cards: Cards = Rc::new(RefCell::new(HashMap::new()));
    let generations: Generations = Rc::new(RefCell::new(HashMap::new()));

    while let Some(event) = rx.recv().await {
        match event {
            NotifEvent::Show {
                id,
                app_name,
                summary,
                body,
                expire,
            } => {
                // Replace existing card with same id (replaces_id case)
                if let Some(old) = cards.borrow_mut().remove(&id) {
                    cards_box.remove(&old);
                }
                let card = make_card(&app_name, &summary, &body);
                cards_box.prepend(&card);
                cards.borrow_mut().insert(id, card.clone());
                window.set_visible(true);

                let my_generation = {
                    let mut gens = generations.borrow_mut();
                    let g = gens.entry(id).or_insert(0);
                    *g += 1;
                    *g
                };

                // `Expire::Never` (expire_timeout=0, or a critical-urgency
                // notification with no explicit timeout) schedules no timer
                // at all — it persists until an explicit CloseNotification.
                if let Expire::After(duration) = expire {
                    let cards_clone = cards.clone();
                    let cards_box_clone = cards_box.clone();
                    let win_clone = window.clone();
                    let generations_clone = generations.clone();
                    let conn_clone = conn.clone();
                    relm4::spawn_local(async move {
                        gtk4::glib::timeout_future(duration).await;
                        let still_current =
                            generations_clone.borrow().get(&id) == Some(&my_generation);
                        if still_current
                            && dismiss(&cards_box_clone, &win_clone, &cards_clone, id)
                        {
                            emit_closed(&conn_clone, id, close_reason::EXPIRED).await;
                        }
                    });
                }
            }
            NotifEvent::Close(id) => {
                if dismiss(&cards_box, &window, &cards, id) {
                    emit_closed(&conn, id, close_reason::CLOSE_NOTIFICATION_CALL).await;
                }
            }
        }
    }
}

/// Removes `id`'s card if present. Returns whether a card was actually
/// removed, so callers only emit `NotificationClosed` for a real dismissal
/// (not a no-op on an id that's already gone or was never shown).
fn dismiss(cards_box: &gtk4::Box, window: &gtk4::Window, cards: &Cards, id: u32) -> bool {
    let removed = cards.borrow_mut().remove(&id);
    let Some(card) = removed else {
        return false;
    };
    cards_box.remove(&card);
    if cards.borrow().is_empty() {
        window.set_visible(false);
    }
    true
}

/// Emits the spec-mandated `NotificationClosed(id, reason)` signal. Sent
/// directly over the connection rather than through the zbus interface
/// macro's generated helper, since the dismiss decision happens here in the
/// popup task, not inside `NotifServer`'s own method bodies.
async fn emit_closed(conn: &zbus::Connection, id: u32, reason: u32) {
    let result = conn
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "NotificationClosed",
            &(id, reason),
        )
        .await;
    if let Err(e) = result {
        eprintln!("breadbar: failed to emit NotificationClosed for {id}: {e}");
    }
}

fn create_window() -> gtk4::Window {
    let window = gtk4::Window::new();
    window.add_css_class("breadbar-notification");
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Right, true);
    window.set_margin(Edge::Top, 20);
    window.set_margin(Edge::Right, 20);
    window.set_default_width(320);
    window
}

fn make_card(app_name: &str, summary: &str, body: &str) -> gtk4::Box {
    let card = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    card.add_css_class("notification-card");

    if !app_name.is_empty() {
        let lbl = gtk4::Label::new(Some(app_name));
        lbl.add_css_class("notification-app");
        lbl.set_xalign(0.0);
        card.append(&lbl);
    }

    let summary_lbl = gtk4::Label::new(Some(summary));
    summary_lbl.add_css_class("notification-summary");
    summary_lbl.set_xalign(0.0);
    summary_lbl.set_wrap(true);
    card.append(&summary_lbl);

    if !body.is_empty() {
        let body_lbl = gtk4::Label::new(Some(body));
        body_lbl.add_css_class("notification-body");
        body_lbl.set_xalign(0.0);
        body_lbl.set_wrap(true);
        card.append(&body_lbl);
    }

    card
}

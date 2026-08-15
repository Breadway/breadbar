use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use tokio::sync::mpsc::Receiver;

use super::{history, Action, Expire, NotifEvent, Urgency, INLINE_REPLY_KEY};

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
    pub const DISMISSED_BY_USER: u32 = 2;
    pub const CLOSE_NOTIFICATION_CALL: u32 = 3;
}

/// Builds the notification window synchronously — so a caller (screenshot
/// mode in particular) has a real window to hook `connect_map` on before
/// `run`'s event loop, which needs an async `zbus::Connection` handshake in
/// the real path, ever starts.
pub fn build_window() -> (gtk4::Window, gtk4::Box) {
    let window = create_window();
    let cards_box = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    cards_box.set_margin_top(8);
    cards_box.set_margin_bottom(8);
    cards_box.set_margin_start(8);
    cards_box.set_margin_end(8);
    window.set_child(Some(&cards_box));
    (window, cards_box)
}

/// `conn`: `None` in screenshot mode, which skips real D-Bus registration
/// entirely (see `super::spawn`) — there's no external client that needs to
/// reach a screenshot-only instance, and registering the well-known name
/// would just race the real breadbar for it. `NotificationClosed` is a
/// spec-mandated signal for real clients only, so it's simply not emitted
/// when there's no real connection to emit it on.
pub async fn run(
    window: gtk4::Window,
    cards_box: gtk4::Box,
    mut rx: Receiver<NotifEvent>,
    conn: Option<zbus::Connection>,
    history_ui: Option<history::Ui>,
) {
    let cards: Cards = Rc::new(RefCell::new(HashMap::new()));
    let generations: Generations = Rc::new(RefCell::new(HashMap::new()));

    while let Some(event) = rx.recv().await {
        match event {
            NotifEvent::Show {
                id,
                app_name,
                summary,
                body,
                urgency,
                expire,
                actions,
                inline_reply,
            } => {
                // Replace existing card with same id (replaces_id case)
                if let Some(old) = cards.borrow_mut().remove(&id) {
                    cards_box.remove(&old);
                }
                let card = make_card(CardSpec {
                    id,
                    app_name: &app_name,
                    summary: &summary,
                    body: &body,
                    urgency,
                    actions: &actions,
                    inline_reply: inline_reply.as_deref(),
                    conn: conn.clone(),
                    cards: cards.clone(),
                    cards_box: cards_box.clone(),
                    window: window.clone(),
                });
                cards_box.prepend(&card);
                cards.borrow_mut().insert(id, card.clone());
                window.set_visible(true);
                if let Some(ui) = &history_ui {
                    history::refresh_if_visible(ui);
                }

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
                        if still_current && dismiss(&cards_box_clone, &win_clone, &cards_clone, id)
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
            NotifEvent::ToggleHistory => {
                if let Some(ui) = &history_ui {
                    history::toggle(ui);
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
/// popup task, not inside `NotifServer`'s own method bodies. No-op when
/// `conn` is `None` (screenshot mode — see `run`'s doc comment).
async fn emit_closed(conn: &Option<zbus::Connection>, id: u32, reason: u32) {
    let Some(conn) = conn else { return };
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
    // OnDemand so an inline-reply GtkEntry can take keys without the popup
    // stealing every keystroke the rest of the time.
    window.set_keyboard_mode(KeyboardMode::OnDemand);
    window
}

struct CardSpec<'a> {
    id: u32,
    app_name: &'a str,
    summary: &'a str,
    body: &'a str,
    urgency: Urgency,
    actions: &'a [Action],
    inline_reply: Option<&'a str>,
    conn: Option<zbus::Connection>,
    cards: Cards,
    cards_box: gtk4::Box,
    window: gtk4::Window,
}

fn make_card(spec: CardSpec<'_>) -> gtk4::Box {
    let card = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    card.add_css_class("notification-card");
    if let Some(class) = spec.urgency.css_class() {
        card.add_css_class(class);
    }

    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 4);

    // Senders often set the title/summary to their own app name (e.g. a bare
    // "Spotify" notification) — showing app_name above an identical summary
    // is pure repetition, so skip the app label in that case.
    if !spec.app_name.is_empty() && !spec.app_name.eq_ignore_ascii_case(spec.summary) {
        let lbl = gtk4::Label::new(Some(spec.app_name));
        lbl.add_css_class("notification-app");
        lbl.set_xalign(0.0);
        content.append(&lbl);
    }

    let summary_lbl = gtk4::Label::new(Some(spec.summary));
    summary_lbl.add_css_class("notification-summary");
    summary_lbl.set_xalign(0.0);
    summary_lbl.set_wrap(true);
    content.append(&summary_lbl);

    if !spec.body.is_empty() {
        let body_lbl = gtk4::Label::new(None);
        body_lbl.add_css_class("notification-body");
        body_lbl.set_xalign(0.0);
        body_lbl.set_wrap(true);
        apply_body_text(&body_lbl, spec.body);
        content.append(&body_lbl);
    }

    if spec.actions.iter().any(|a| a.key == "default") {
        content.add_css_class("notification-default");
        let gesture = gtk4::GestureClick::new();
        let invoke = Invoke {
            conn: spec.conn.clone(),
            cards: spec.cards.clone(),
            cards_box: spec.cards_box.clone(),
            window: spec.window.clone(),
            id: spec.id,
        };
        gesture.connect_released(move |_, _, _, _| {
            invoke_action(invoke.clone(), "default");
        });
        content.add_controller(gesture);
    }

    card.append(&content);

    let visible: Vec<&Action> = spec
        .actions
        .iter()
        .filter(|a| a.key != "default" && a.key != INLINE_REPLY_KEY)
        .collect();
    if !visible.is_empty() {
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        row.add_css_class("notification-actions");
        row.set_halign(gtk4::Align::End);
        for action in visible {
            let btn = gtk4::Button::with_label(&action.label);
            btn.add_css_class("notification-action");
            let invoke = Invoke {
                conn: spec.conn.clone(),
                cards: spec.cards.clone(),
                cards_box: spec.cards_box.clone(),
                window: spec.window.clone(),
                id: spec.id,
            };
            let key = action.key.clone();
            btn.connect_clicked(move |_| {
                invoke_action(invoke.clone(), &key);
            });
            row.append(&btn);
        }
        card.append(&row);
    }

    if let Some(placeholder) = spec.inline_reply {
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        row.add_css_class("notification-reply");

        let entry = gtk4::Entry::new();
        entry.add_css_class("notification-reply-entry");
        entry.set_placeholder_text(Some(placeholder));
        entry.set_hexpand(true);

        let send_label = spec
            .actions
            .iter()
            .find(|a| a.key == INLINE_REPLY_KEY)
            .map(|a| a.label.as_str())
            .filter(|l| !l.is_empty())
            .unwrap_or("Send");
        let send = gtk4::Button::with_label(send_label);
        send.add_css_class("notification-action");

        let invoke = Invoke {
            conn: spec.conn.clone(),
            cards: spec.cards.clone(),
            cards_box: spec.cards_box.clone(),
            window: spec.window.clone(),
            id: spec.id,
        };
        let entry_for_btn = entry.clone();
        let invoke_btn = invoke.clone();
        send.connect_clicked(move |_| {
            submit_reply(&entry_for_btn, invoke_btn.clone());
        });
        entry.connect_activate(move |e| {
            submit_reply(e, invoke.clone());
        });

        row.append(&entry);
        row.append(&send);
        card.append(&row);
    }

    card
}

/// FDO `body-markup` is a small Pango-ish subset (`<b>`, `<i>`, `<u>`,
/// `<a href>`). Invalid markup falls back to plain text so a bad sender
/// doesn't blank the card.
fn apply_body_text(label: &gtk4::Label, body: &str) {
    if body.contains('<') && gtk4::pango::parse_markup(body, '\0').is_ok() {
        label.set_markup(body);
        return;
    }
    label.set_text(body);
}

#[derive(Clone)]
struct Invoke {
    conn: Option<zbus::Connection>,
    cards: Cards,
    cards_box: gtk4::Box,
    window: gtk4::Window,
    id: u32,
}

fn invoke_action(invoke: Invoke, key: &str) {
    let key = key.to_string();
    relm4::spawn_local(async move {
        emit_action(&invoke.conn, invoke.id, &key).await;
        if dismiss(&invoke.cards_box, &invoke.window, &invoke.cards, invoke.id) {
            emit_closed(&invoke.conn, invoke.id, close_reason::DISMISSED_BY_USER).await;
        }
    });
}

fn submit_reply(entry: &gtk4::Entry, invoke: Invoke) {
    let text = entry.text().to_string();
    if text.trim().is_empty() {
        return;
    }
    relm4::spawn_local(async move {
        emit_replied(&invoke.conn, invoke.id, &text).await;
        emit_action(&invoke.conn, invoke.id, INLINE_REPLY_KEY).await;
        if dismiss(&invoke.cards_box, &invoke.window, &invoke.cards, invoke.id) {
            emit_closed(&invoke.conn, invoke.id, close_reason::DISMISSED_BY_USER).await;
        }
    });
}

async fn emit_action(conn: &Option<zbus::Connection>, id: u32, action_key: &str) {
    let Some(conn) = conn else { return };
    let result = conn
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "ActionInvoked",
            &(id, action_key),
        )
        .await;
    if let Err(e) = result {
        eprintln!("breadbar: failed to emit ActionInvoked for {id}: {e}");
    }
}

/// GNOME/KDE (and clients such as Discord/Telegram) listen for this
/// non-spec signal on `org.freedesktop.Notifications` when the user
/// submits an inline reply. Signature: `NotificationReplied(u32 id, s text)`.
/// We also emit `ActionInvoked(id, "inline-reply")` so senders that only
/// watch the spec signal still see the send.
async fn emit_replied(conn: &Option<zbus::Connection>, id: u32, text: &str) {
    let Some(conn) = conn else { return };
    let result = conn
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "NotificationReplied",
            &(id, text),
        )
        .await;
    if let Err(e) = result {
        eprintln!("breadbar: failed to emit NotificationReplied for {id}: {e}");
    }
}

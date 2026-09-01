use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Instant};

use gtk4::glib::ControlFlow;
use gtk4::prelude::*;
use gtk4_layer_shell::{KeyboardMode, LayerShell};
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

    // NOTIFICATION INTERACTION #B: the surface (and `window.surface()`)
    // doesn't exist until map, same constraint `surface::click_through`
    // documents — this is the initial-region counterpart of that hook,
    // recomputing against whatever's already in `cards_box` at the moment
    // the toast becomes visible (empty on the very first map, but this
    // window remaps on every reappearance after being fully dismissed —
    // see `dismiss`'s `window.set_visible(false)` — and by the time a new
    // notification's `Show` handler calls `set_visible(true)` again, its
    // card is already a child of `cards_box`). `refresh_hit_region`'s own
    // per-event calls below are the steady-state path; this is the
    // just-in-case one for the map race itself.
    let cbox_for_map = cards_box.clone();
    window.connect_map(move |win| {
        apply_hit_region(win, &cbox_for_map);
    });

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
                // Re-pin to the focused output whenever the stack goes from
                // empty to shown — see `theme::pin_focused_output`. Skipped
                // while a toast is already up so an in-flight batch stays
                // put rather than re-pinning a live layer surface.
                if !window.is_visible() {
                    crate::theme::pin_focused_output(&window);
                }
                window.set_visible(true);
                // ANIMATION WORK #6: spring the new card's own height in
                // from 0 to its natural content height instead of it
                // appearing at full size in one frame — since it's
                // `prepend`ed (the vertical box's first child), the
                // existing cards below get pushed down smoothly as this
                // grows, rather than jumping straight to their new
                // position.
                spring_in_card(&card);
                refresh_hit_region(&window, &cards_box);
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
/// (not a no-op on an id that's already gone or was never shown). Every
/// caller (auto-expire, `CloseNotification`, an action/reply invocation,
/// and the card's own dismiss button) goes through this one function, so
/// this is also the one place that needs to recompute the hit region
/// (NOTIFICATION INTERACTION #B) on removal — a card gone from `cards_box`
/// but still counted in the input region would leave a dead click-through
/// hole where a live button used to be.
fn dismiss(cards_box: &gtk4::Box, window: &gtk4::Window, cards: &Cards, id: u32) -> bool {
    let removed = cards.borrow_mut().remove(&id);
    let Some(card) = removed else {
        return false;
    };
    cards_box.remove(&card);
    if cards.borrow().is_empty() {
        window.set_visible(false);
    }
    refresh_hit_region(window, cards_box);
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

/// ANIMATION WORK #6: springs `card`'s own height from 0 up to its natural
/// content height (`bread_theme::anim::spring_to` + `set_size_request`,
/// same "GTK4 CSS has no height transition" technique `main.rs`'s
/// `animate_drawer_height` already uses for the capsule drawer) instead of
/// it appearing at full size in one frame. Measured AFTER `card` is already
/// a child of `cards_box` (the caller's job), not before: an unparented
/// widget isn't rooted under this window's style provider chain yet, so its
/// `measure()` wouldn't see the real `.notification-card` padding/border —
/// only a widget that's actually in the tree gets an accurate natural size.
///
/// One-shot and self-contained — no cancellation bookkeeping, unlike
/// `animate_drawer_height`/`animate_osd_fill`'s own `Rc<RefCell<..>>`
/// tick-id storage: a card's entrance can't be interrupted by a second one,
/// since a same-id replacement tears the whole card down and builds a
/// fresh one (see the `Show` handler's `cards_box.remove(&old)`) rather
/// than reusing it.
const CARD_GROW_MS: f64 = 380.0;

fn spring_in_card(card: &gtk4::Box) {
    let (_, target_h, _, _) = card.measure(gtk4::Orientation::Vertical, -1);
    card.set_size_request(-1, 0);
    let target = card.clone();
    bread_theme::anim::spring_to(card, 0, target_h, CARD_GROW_MS, move |h| {
        target.set_size_request(-1, h.max(0));
    });
}

thread_local! {
    // NOTIFICATION INTERACTION #B: the tick callback that keeps
    // `refresh_hit_region` recomputing the toast's input region while a
    // card's entrance (`spring_in_card` above) or the stack's push-down
    // reflow could still be moving a button. One process-wide toast window
    // (this crate registers a single `org.freedesktop.Notifications` name),
    // so a thread-local — not a field threaded through every call site — is
    // enough, same reasoning as `theme::SHELL_THEME_MONITOR`.
    static HIT_TRACKER: RefCell<Option<gtk4::TickCallbackId>> = const { RefCell::new(None) };
}

/// How long after a card set/layout change to keep recomputing the hit
/// region every frame — long enough to cover both `spring_in_card`'s
/// `CARD_GROW_MS` and the CSS `notif-in` keyframe's 0.45s slide-in (see
/// `theme.rs`'s `.notification-card` rule), whichever finishes last.
const HIT_TRACK_MS: f64 = 700.0;

/// Recomputes the toast surface's clickable input region immediately, then
/// keeps recomputing it every frame for `HIT_TRACK_MS` — covering both a
/// newly-shown card's own entrance and the stack's push-down settle, either
/// of which can still be moving a button on the frame this is called.
/// Called any time the card set could have changed: shown, dismissed
/// (including via the new dismiss button — see `dismiss` above), or
/// expired. Cancels any previous tracking run first, so a rapid burst of
/// notifications doesn't accumulate overlapping tick callbacks.
fn refresh_hit_region(window: &gtk4::Window, cards_box: &gtk4::Box) {
    apply_hit_region(window, cards_box);

    if let Some(id) = HIT_TRACKER.with(|c| c.borrow_mut().take()) {
        id.remove();
    }
    let started = Instant::now();
    let win = window.clone();
    let cbox = cards_box.clone();
    let id = window.add_tick_callback(move |_, _| {
        apply_hit_region(&win, &cbox);
        if started.elapsed().as_secs_f64() * 1000.0 >= HIT_TRACK_MS {
            HIT_TRACKER.with(|c| c.borrow_mut().take());
            return ControlFlow::Break;
        }
        ControlFlow::Continue
    });
    HIT_TRACKER.with(|c| *c.borrow_mut() = Some(id));
}

/// One frame's worth of `refresh_hit_region`'s work: walk `cards_box` for
/// every currently-interactive widget (action/dismiss buttons, the
/// inline-reply entry) and hand their rectangles to
/// `surface::set_hit_region`. Split out from `refresh_hit_region` so the
/// initial immediate call and the tracking tick callback share the exact
/// same logic.
fn apply_hit_region(window: &gtk4::Window, cards_box: &gtk4::Box) {
    let mut widgets = Vec::new();
    collect_interactive(cards_box.upcast_ref::<gtk4::Widget>(), &mut widgets);
    crate::surface::set_hit_region(window, &widgets);
}

/// Depth-first walk of `root`'s widget tree collecting every `GtkButton`
/// (action buttons, the reply-send button, the dismiss button) and
/// `GtkEntry` (the inline-reply field) — the only things on a card a user
/// should ever be able to click into. Everything else (the summary/body
/// labels, the card's own background) stays click-through, same as the
/// blanket empty region did before NOTIFICATION INTERACTION #B. Walking
/// the real widget tree rather than tracking a flat list as cards/buttons
/// are built means this can't drift out of sync with `make_card`'s own
/// structure (e.g. the dismiss button living inside a `gtk4::Overlay`
/// rather than directly under `card`).
fn collect_interactive(root: &gtk4::Widget, out: &mut Vec<gtk4::Widget>) {
    let mut child = root.first_child();
    while let Some(w) = child {
        if w.is::<gtk4::Button>() || w.is::<gtk4::Entry>() {
            out.push(w.clone());
        }
        collect_interactive(&w, out);
        child = w.next_sibling();
    }
}

fn create_window() -> gtk4::Window {
    let window = gtk4::Window::new();
    window.add_css_class("breadbar-notification");
    window.init_layer_shell();
    window.set_namespace(Some("breadbar-notif"));
    crate::surface::apply(&window, "breadbar-notif");
    // Toasts are purely informational — they never grab keyboard focus,
    // full stop, regardless of what's clickable on them (KeyboardMode::None
    // stays; do NOT change this — see the NOTIFICATION INTERACTION #B task
    // note). Historically ("stop toast popups from stealing focus or
    // blocking clicks") that also meant a fully empty input region: every
    // pointer event passed straight through to whatever's underneath, but
    // that made `make_card`'s own action buttons, its inline-reply
    // `GtkEntry`, and the dismiss button below permanently unreachable too.
    // `crate::surface::set_hit_region` (called from `build_window`'s
    // `connect_map` and from `refresh_hit_region` below, any time the card
    // set or layout could have changed) replaces the old blanket
    // `surface::click_through` empty region with the union of just those
    // widgets' own rectangles — everywhere else on the surface stays
    // click-through, same as before. A toast that genuinely has none of
    // them yet (`cards_box` empty) still gets the same all-empty region
    // `click_through` set, since a rectangle union over zero widgets is
    // the empty region.
    window.set_keyboard_mode(KeyboardMode::None);
    crate::theme::bind_auto(&window);
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

    // NOTIFICATION INTERACTION #A: a direct dismiss control. Floated in
    // the card's top-right corner via an Overlay rather than a full extra
    // header row, so it doesn't add vertical bulk the approved demo's own
    // card never has (see the `.notification-dismiss` CSS in theme.rs).
    // `collect_interactive` (this module) picks it up the same way it
    // picks up the action/reply buttons below, by walking the real widget
    // tree — it doesn't need to know this button lives one level deeper,
    // inside the overlay, than they do.
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&content));

    let dismiss_btn = gtk4::Button::with_label("×");
    dismiss_btn.add_css_class("notification-dismiss");
    dismiss_btn.set_halign(gtk4::Align::End);
    dismiss_btn.set_valign(gtk4::Align::Start);
    let dismiss_invoke = Invoke {
        conn: spec.conn.clone(),
        cards: spec.cards.clone(),
        cards_box: spec.cards_box.clone(),
        window: spec.window.clone(),
        id: spec.id,
    };
    dismiss_btn.connect_clicked(move |_| {
        dismiss_card(dismiss_invoke.clone());
    });
    overlay.add_overlay(&dismiss_btn);

    card.append(&overlay);

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

/// NOTIFICATION INTERACTION #A: the card's own dismiss button. Unlike
/// `invoke_action`, this never emits `ActionInvoked` — there's no action
/// key here, the user just closed the toast unprompted — only the
/// spec-mandated `NotificationClosed(id, reason)`, with
/// `close_reason::DISMISSED_BY_USER` (freedesktop value 2, "dismissed by
/// the user") so clients are told properly, same reason code
/// `invoke_action` above and `submit_reply` below already use for their
/// own user-initiated dismissals.
fn dismiss_card(invoke: Invoke) {
    relm4::spawn_local(async move {
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

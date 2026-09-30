pub mod history;
pub mod popup;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use zbus::zvariant::OwnedValue;

/// Hint key used by `notify-send` and honored by notify-osd/dunst: senders
/// that fire off a new process per notification (so `replaces_id` is always
/// 0) tag related notifications with the same `(app_name, tag)` pair to mean
/// "replace whatever from this app is already showing." Without honoring
/// this, a fire-and-forget sender can never supersede an earlier
/// `Expire::Never` notification from itself (e.g. a critical hardware
/// warning); it just piles up a new card next to it forever.
const SYNCHRONOUS_HINT: &str = "x-canonical-private-synchronous";

/// Spec + GNOME/KDE reserved action id for an inline reply field. Hidden
/// from the button row; submitting the field emits `NotificationReplied`
/// (and `ActionInvoked` with this key). See `popup::emit_replied`.
pub const INLINE_REPLY_KEY: &str = "inline-reply";

/// KDE placeholder hint. Presence (or an `inline-reply` action) is enough
/// to show the reply field: Discord/Telegram use the action, Plasma often
/// only the hint.
const KDE_REPLY_PLACEHOLDER: &str = "x-kde-reply-placeholder";

/// Advertised `GetCapabilities` strings. `body` is the original set;
/// `actions` / `inline-reply` are this change; `body-markup` is the usual
/// companion so senders can ship `<b>`/`<i>` instead of stripping tags.
const CAPABILITIES: &[&str] = &["body", "body-markup", "actions", "inline-reply"];

/// One `(id, localized label)` pair from the Notify `actions` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub key: String,
    pub label: String,
}

/// How long a shown notification should stay up before auto-dismissing.
/// Distinct from `Option<Duration>` mainly for readability at call sites:
/// `Never` covers both the spec's `expire_timeout == 0` ("never expire")
/// and a critical-urgency notification with no explicit timeout, which
/// conventionally shouldn't auto-dismiss either.
#[derive(Debug, Clone, Copy)]
pub enum Expire {
    Never,
    After(Duration),
}

pub enum NotifEvent {
    Show {
        id: u32,
        app_name: String,
        summary: String,
        body: String,
        urgency: Urgency,
        expire: Expire,
        actions: Vec<Action>,
        /// Placeholder for the inline-reply field, if one should be shown.
        inline_reply: Option<String>,
    },
    Close(u32),
    ToggleHistory,
}

/// In-process handle to the notification event loop, handed back by
/// [`spawn`] so the bar can drive the history window directly instead of
/// going out over the `dev.breadway.Bar` D-Bus surface the way
/// `breadbar --history` does.
#[derive(Clone)]
pub struct NotifHandle {
    tx: mpsc::Sender<NotifEvent>,
}

impl NotifHandle {
    /// Show the history window if hidden, hide it if shown. A dropped send
    /// (loop gone, or the bounded channel briefly full) is not worth
    /// surfacing; the click just does nothing.
    pub fn toggle_history(&self) {
        let _ = self.tx.try_send(NotifEvent::ToggleHistory);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

impl Urgency {
    /// Spec: hints["urgency"] is a byte, 0=low, 1=normal, 2=critical (default normal).
    fn from_hint(hint: Option<&OwnedValue>) -> Self {
        match hint.and_then(|v| u8::try_from(v.clone()).ok()) {
            Some(0) => Urgency::Low,
            Some(2) => Urgency::Critical,
            _ => Urgency::Normal,
        }
    }

    /// CSS class suffix for `.notification-card.urgency-<kind>`.
    pub fn css_class(self) -> Option<&'static str> {
        match self {
            Urgency::Low => None,
            Urgency::Normal => Some("urgency-normal"),
            Urgency::Critical => Some("urgency-critical"),
        }
    }
}

/// Spec: `actions` is a flat list of pairs `(id, localized label)`. An
/// unpaired trailing id is ignored. Empty keys are dropped.
fn parse_actions(raw: &[String]) -> Vec<Action> {
    raw.as_chunks::<2>()
        .0
        .iter()
        .filter(|c| !c[0].is_empty())
        .map(|c| Action {
            key: c[0].clone(),
            label: c[1].clone(),
        })
        .collect()
}

/// Show an inline reply field when the sender asked for `inline-reply` or
/// sent the KDE placeholder hint. Placeholder text prefers the hint.
fn inline_reply_placeholder(
    actions: &[Action],
    hints: &HashMap<String, OwnedValue>,
) -> Option<String> {
    let from_hint = hints
        .get(KDE_REPLY_PLACEHOLDER)
        .and_then(|v| String::try_from(v.clone()).ok())
        .filter(|s| !s.is_empty());
    let has_action = actions.iter().any(|a| a.key == INLINE_REPLY_KEY);
    if has_action || from_hint.is_some() {
        Some(from_hint.unwrap_or_else(|| "Reply".into()))
    } else {
        None
    }
}

/// Maps a `Notify` call's `expire_timeout` (plus whether the `urgency` hint
/// was critical) to our internal `Expire`, per the freedesktop notification
/// spec: `0` always means never expire; a negative value means "server
/// picks a default" (5s here, except critical notifications, which
/// conventionally persist); any non-negative value is taken literally.
/// Pulled out of `NotifServer::notify` so this mapping is unit-testable
/// without a live D-Bus connection.
fn compute_expire(expire_timeout: i32, urgency_critical: bool) -> Expire {
    match expire_timeout {
        0 => Expire::Never,
        t if t < 0 => {
            if urgency_critical {
                Expire::Never
            } else {
                Expire::After(Duration::from_millis(5000))
            }
        }
        t => Expire::After(Duration::from_millis(t as u64)),
    }
}

struct NotifServer {
    tx: mpsc::Sender<NotifEvent>,
    next_id: AtomicU32,
    /// (app_name, synchronous-hint tag) -> id, for senders relying on
    /// `SYNCHRONOUS_HINT` instead of an explicit `replaces_id`.
    sync_tags: Mutex<HashMap<(String, String), u32>>,
    history: history::Store,
    /// Unit tests leave this off so `Notify` does not write `$XDG_STATE_HOME`.
    persist_history: bool,
}

/// `breadbar --pill toggle|show` (BAR-27 phase 1): sent over the same
/// `dev.breadway.Bar` D-Bus surface as `ToggleHistory`, but forwarded to the
/// App's own `ComponentSender` via a plain `Send` channel rather than the
/// notification event loop: the pill lives on the bar window, which
/// `notifications::spawn`'s caller (`main.rs`) owns, not this module.
#[derive(Debug, Clone, Copy)]
pub enum PillCmd {
    Toggle,
    Show,
}

/// `breadbar --stage toggle|show|hide` (BAR-30 phase 2): same D-Bus
/// forwarding shape as [`PillCmd`], and for the same reason (the stage
/// lives on the App's own `ComponentSender`, not this module's event loop).
#[derive(Debug, Clone, Copy)]
pub enum StageCmd {
    Toggle,
    Show,
    Hide,
}

/// Set while the active theme shows notifications on the spotlight stage:
/// `breadbar --history` then toggles the stage, whose notifications zone is
/// the history, instead of the separate history window.
static HISTORY_OPENS_STAGE: AtomicBool = AtomicBool::new(false);

pub fn set_history_opens_stage(on: bool) {
    HISTORY_OPENS_STAGE.store(on, Ordering::Relaxed);
}

/// The session-bus connection serving `org.freedesktop.Notifications`, once
/// claimed, so the stage can answer a notification's action buttons.
static CONNECTION: OnceLock<zbus::Connection> = OnceLock::new();

/// Runs notification `id`'s action `key` on the sender's behalf, from the
/// stage's action buttons: `ActionInvoked(id, key)`, then
/// `NotificationClosed(id, 2)` (dismissed by the user), as a popup's own
/// button does.
pub fn invoke_action(id: u32, key: String) {
    let Some(conn) = CONNECTION.get().cloned() else {
        return;
    };
    relm4::spawn(async move {
        for (member, result) in [
            (
                "ActionInvoked",
                conn.emit_signal(
                    None::<&str>,
                    "/org/freedesktop/Notifications",
                    "org.freedesktop.Notifications",
                    "ActionInvoked",
                    &(id, key.as_str()),
                )
                .await,
            ),
            (
                "NotificationClosed",
                conn.emit_signal(
                    None::<&str>,
                    "/org/freedesktop/Notifications",
                    "org.freedesktop.Notifications",
                    "NotificationClosed",
                    &(id, 2u32),
                )
                .await,
            ),
        ] {
            if let Err(e) = result {
                eprintln!("breadbar: failed to emit {member} for {id}: {e}");
            }
        }
    });
}

/// Private breadbar control surface on the same connection as
/// `org.freedesktop.Notifications`. `breadbar --history` is a one-shot
/// client of `ToggleHistory`; `breadbar --pill toggle|show` and
/// `breadbar --stage toggle|show|hide` are the other methods here.
struct BarService {
    tx: mpsc::Sender<NotifEvent>,
    pill_tx: mpsc::UnboundedSender<PillCmd>,
    stage_tx: mpsc::UnboundedSender<StageCmd>,
}

#[zbus::interface(name = "dev.breadway.Bar")]
impl BarService {
    async fn toggle_history(&self) {
        if HISTORY_OPENS_STAGE.load(Ordering::Relaxed) {
            let _ = self.stage_tx.send(StageCmd::Toggle);
        } else {
            let _ = self.tx.send(NotifEvent::ToggleHistory).await;
        }
    }

    async fn toggle_pill(&self) {
        let _ = self.pill_tx.send(PillCmd::Toggle);
    }

    async fn show_pill(&self) {
        let _ = self.pill_tx.send(PillCmd::Show);
    }

    async fn toggle_stage(&self) {
        let _ = self.stage_tx.send(StageCmd::Toggle);
    }

    async fn show_stage(&self) {
        let _ = self.stage_tx.send(StageCmd::Show);
    }

    async fn hide_stage(&self) {
        let _ = self.stage_tx.send(StageCmd::Hide);
    }
}

const BAR_DEST: &str = "org.freedesktop.Notifications";
const BAR_PATH: &str = "/dev/breadway/Bar";
const BAR_IFACE: &str = "dev.breadway.Bar";

/// Ask a running breadbar to toggle the history window. Used by
/// `breadbar --history`; does not start a second bar.
pub async fn toggle_history_remote() -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    conn.call_method(
        Some(BAR_DEST),
        BAR_PATH,
        Some(BAR_IFACE),
        "ToggleHistory",
        &(),
    )
    .await?;
    Ok(())
}

/// Ask a running breadbar to toggle its spotlight pill. Used by
/// `breadbar --pill toggle`; a no-op (server-side) on a theme with
/// `[bar.autohide].enabled = false`, since there's nothing to toggle.
pub async fn toggle_pill_remote() -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    conn.call_method(Some(BAR_DEST), BAR_PATH, Some(BAR_IFACE), "TogglePill", &())
        .await?;
    Ok(())
}

/// Ask a running breadbar to show its spotlight pill. Used by
/// `breadbar --pill show`.
pub async fn show_pill_remote() -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    conn.call_method(Some(BAR_DEST), BAR_PATH, Some(BAR_IFACE), "ShowPill", &())
        .await?;
    Ok(())
}

/// Ask a running breadbar to toggle its spotlight stage. Used by
/// `breadbar --stage toggle`; the Super+Space bind target under spotlight
/// (see the task report for the exact Hyprland `bind` line). A no-op
/// (server-side) on a theme with no stage wired up.
pub async fn toggle_stage_remote() -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    conn.call_method(
        Some(BAR_DEST),
        BAR_PATH,
        Some(BAR_IFACE),
        "ToggleStage",
        &(),
    )
    .await?;
    Ok(())
}

/// Ask a running breadbar to show its spotlight stage. Used by
/// `breadbar --stage show`.
pub async fn show_stage_remote() -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    conn.call_method(Some(BAR_DEST), BAR_PATH, Some(BAR_IFACE), "ShowStage", &())
        .await?;
    Ok(())
}

/// Ask a running breadbar to hide its spotlight stage. Used by
/// `breadbar --stage hide`.
pub async fn hide_stage_remote() -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    conn.call_method(Some(BAR_DEST), BAR_PATH, Some(BAR_IFACE), "HideStage", &())
        .await?;
    Ok(())
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl NotifServer {
    // The org.freedesktop.Notifications spec mandates exactly these 8 parameters.
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        _app_icon: &str,
        summary: &str,
        body: &str,
        actions: Vec<String>,
        hints: std::collections::HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let sync_tag = hints
            .get(SYNCHRONOUS_HINT)
            .and_then(|v| String::try_from(v.clone()).ok());

        let id = if replaces_id != 0 {
            if let Some(tag) = &sync_tag {
                self.sync_tags
                    .lock()
                    .unwrap()
                    .insert((app_name.to_string(), tag.clone()), replaces_id);
            }
            replaces_id
        } else if let Some(tag) = &sync_tag {
            let key = (app_name.to_string(), tag.clone());
            let mut sync_tags = self.sync_tags.lock().unwrap();
            *sync_tags
                .entry(key)
                .or_insert_with(|| self.next_id.fetch_add(1, Ordering::Relaxed))
        } else {
            self.next_id.fetch_add(1, Ordering::Relaxed)
        };
        // Per spec: 0 means "never expire", which used to be lumped in
        // with "-1: let the server pick a default" and coerced to a fixed
        // 5s, so a sender explicitly asking for a persistent notification
        // (e.g. a progress/error dialog) got auto-dismissed anyway.
        // Critical-urgency notifications conventionally persist too, even
        // when the sender left expire_timeout at the server-default (-1).
        let urgency = Urgency::from_hint(hints.get("urgency"));
        let expire = compute_expire(expire_timeout, urgency == Urgency::Critical);
        let actions = parse_actions(&actions);
        let inline_reply = inline_reply_placeholder(&actions, &hints);

        history::record(
            &self.history,
            history::Entry {
                id,
                app_name: app_name.to_string(),
                summary: summary.to_string(),
                body: body.to_string(),
                urgency,
                received: SystemTime::now(),
                actions: actions.clone(),
            },
        );
        if self.persist_history {
            history::persist(&self.history);
        }

        let _ = self
            .tx
            .send(NotifEvent::Show {
                id,
                app_name: app_name.to_string(),
                summary: summary.to_string(),
                body: body.to_string(),
                urgency,
                expire,
                actions,
                inline_reply,
            })
            .await;
        id
    }

    async fn close_notification(&self, id: u32) {
        let _ = self.tx.send(NotifEvent::Close(id)).await;
    }

    fn get_capabilities(&self) -> Vec<String> {
        CAPABILITIES.iter().map(|s| (*s).to_string()).collect()
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "breadbar".into(),
            "breadway".into(),
            env!("CARGO_PKG_VERSION").into(),
            "1.2".into(),
        )
    }
}

/// `spawn`'s `on_show` callback: `(app_name, summary, critical)`, called once per
/// `Show` event before the card (or, under `[notifications].style =
/// "surtitle"`, nothing) is built. Named so the type doesn't have to be
/// spelled out at every call site (`clippy::type_complexity`).
pub type OnShow = Box<dyn Fn(&str, &str, bool)>;

/// A fixed sample notification for `--screenshot notification`/
/// `notification-critical`; substitutes for a real `Notify` D-Bus call so a
/// capture doesn't depend on some external sender firing one at just the
/// right moment.
pub enum SampleKind {
    Normal,
    Critical,
}

impl SampleKind {
    fn sample_event(&self) -> NotifEvent {
        let urgency = match self {
            SampleKind::Normal => Urgency::Normal,
            SampleKind::Critical => Urgency::Critical,
        };
        NotifEvent::Show {
            id: 1,
            app_name: "Sample App".into(),
            summary: "Sample notification".into(),
            body: "This is what a notification card looks like.".into(),
            urgency,
            expire: Expire::Never,
            actions: vec![],
            inline_reply: None,
        }
    }
}

/// Builds the notification window synchronously (see
/// `popup::build_window`'s doc comment) and spawns the event loop that
/// shows/updates/hides it.
///
/// `sample`: `Some` skips real D-Bus registration entirely and seeds the
/// loop with one fixed sample event instead: screenshot mode only. Doing
/// the real `org.freedesktop.Notifications` registration in every
/// screenshot run would race the real breadbar (if running) for the same
/// well-known name for no benefit, since nothing needs to reach this
/// instance externally.
///
/// Returns the notification window, outside screenshot mode a
/// [`NotifHandle`] the bar keeps so a click on it can toggle the history
/// window without a D-Bus round-trip, and the history [`history::Store`]
/// itself (BAR-30 phase 4: the stage's own notifications zone reads the same
/// shared store directly, rather than duplicating a second history). In
/// screenshot mode there is no history UI wired up and the store is a fresh,
/// unpersisted one the caller can pre-seed with sample entries.
pub fn spawn(
    sample: Option<SampleKind>,
    on_show: Option<OnShow>,
    pill_tx: mpsc::UnboundedSender<PillCmd>,
    stage_tx: mpsc::UnboundedSender<StageCmd>,
) -> (gtk4::Window, Option<NotifHandle>, history::Store) {
    let (window, cards_box) = popup::build_window();
    let (tx, rx) = mpsc::channel(32);

    match sample {
        Some(kind) => {
            let store = history::new_store();
            let _ = tx.try_send(kind.sample_event());
            let window_for_loop = window.clone();
            relm4::spawn_local(async move {
                popup::run(window_for_loop, cards_box, rx, None, None, None).await;
            });
            (window, None, store)
        }
        None => {
            let handle = NotifHandle { tx: tx.clone() };
            let (conn_tx, conn_rx) = tokio::sync::oneshot::channel();
            let store = history::load_store();
            let next_id = history::next_id(&store);
            let history_ui = history::build_window(store.clone());
            let store_for_return = store.clone();

            relm4::spawn(async move {
                let server = NotifServer {
                    tx: tx.clone(),
                    next_id: AtomicU32::new(next_id),
                    sync_tags: Mutex::new(HashMap::new()),
                    history: store,
                    persist_history: true,
                };
                let bar = BarService {
                    tx,
                    pill_tx,
                    stage_tx,
                };
                // Builder failures here would only occur with invalid static strings; safe to unwrap.
                let conn = zbus::connection::Builder::session()
                    .unwrap()
                    .name("org.freedesktop.Notifications")
                    .unwrap()
                    .serve_at("/org/freedesktop/Notifications", server)
                    .unwrap()
                    .serve_at(BAR_PATH, bar)
                    .unwrap()
                    .build()
                    .await
                    .expect("failed to claim org.freedesktop.Notifications on D-Bus session bus");
                // Hand the connection to popup::run so it can emit `NotificationClosed`
                // (spec-mandated whenever a notification actually goes away); the
                // dismiss decisions all happen over there, not in this interface impl.
                let _ = CONNECTION.set(conn.clone());
                let _ = conn_tx.send(conn);
                std::future::pending::<()>().await
            });

            let window_for_loop = window.clone();
            relm4::spawn_local(async move {
                if let Ok(conn) = conn_rx.await {
                    popup::run(
                        window_for_loop,
                        cards_box,
                        rx,
                        Some(conn),
                        Some(history_ui),
                        on_show,
                    )
                    .await;
                }
            });

            (window, Some(handle), store_for_return)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_timeout_never_expires_regardless_of_urgency() {
        assert!(matches!(compute_expire(0, false), Expire::Never));
        assert!(matches!(compute_expire(0, true), Expire::Never));
    }

    #[test]
    fn negative_timeout_defaults_to_five_seconds_for_normal_urgency() {
        match compute_expire(-1, false) {
            Expire::After(d) => assert_eq!(d, Duration::from_millis(5000)),
            Expire::Never => panic!("expected a 5s default, got Never"),
        }
    }

    #[test]
    fn negative_timeout_persists_for_critical_urgency() {
        assert!(matches!(compute_expire(-1, true), Expire::Never));
    }

    #[test]
    fn positive_timeout_is_taken_literally() {
        match compute_expire(1500, false) {
            Expire::After(d) => assert_eq!(d, Duration::from_millis(1500)),
            Expire::Never => panic!("expected 1500ms, got Never"),
        }
        // Even for critical urgency, an explicit positive timeout is honored
        // rather than overridden to Never: "critical persists" is only the
        // *default* when the sender didn't specify one.
        match compute_expire(1500, true) {
            Expire::After(d) => assert_eq!(d, Duration::from_millis(1500)),
            Expire::Never => panic!("expected 1500ms, got Never"),
        }
    }

    fn test_server() -> (NotifServer, mpsc::Receiver<NotifEvent>) {
        let (tx, rx) = mpsc::channel(32);
        (
            NotifServer {
                tx,
                next_id: AtomicU32::new(1),
                sync_tags: Mutex::new(HashMap::new()),
                history: history::new_store(),
                persist_history: false,
            },
            rx,
        )
    }

    fn sync_hints(tag: &str) -> HashMap<String, OwnedValue> {
        let mut hints = HashMap::new();
        hints.insert(
            SYNCHRONOUS_HINT.to_string(),
            OwnedValue::try_from(zbus::zvariant::Value::from(tag)).unwrap(),
        );
        hints
    }

    #[tokio::test]
    async fn synchronous_hint_reuses_id_for_same_app_and_tag() {
        let (server, _rx) = test_server();
        let first = server
            .notify(
                "breadcrumbs",
                0,
                "",
                "no Wi-Fi adapter",
                "",
                vec![],
                sync_hints("breadcrumbs"),
                -1,
            )
            .await;
        let second = server
            .notify(
                "breadcrumbs",
                0,
                "",
                "back online",
                "",
                vec![],
                sync_hints("breadcrumbs"),
                -1,
            )
            .await;
        assert_eq!(
            first, second,
            "same app+tag should replace, not stack, a prior notification"
        );
    }

    #[tokio::test]
    async fn synchronous_hint_is_scoped_per_app_name() {
        let (server, _rx) = test_server();
        let first = server
            .notify(
                "breadcrumbs",
                0,
                "",
                "no Wi-Fi adapter",
                "",
                vec![],
                sync_hints("breadcrumbs"),
                -1,
            )
            .await;
        let second = server
            .notify(
                "other-app",
                0,
                "",
                "unrelated",
                "",
                vec![],
                sync_hints("breadcrumbs"),
                -1,
            )
            .await;
        assert_ne!(
            first, second,
            "same tag from a different app must not collide"
        );
    }

    #[tokio::test]
    async fn no_synchronous_hint_always_allocates_a_new_id() {
        let (server, _rx) = test_server();
        let first = server
            .notify("breadcrumbs", 0, "", "one", "", vec![], HashMap::new(), -1)
            .await;
        let second = server
            .notify("breadcrumbs", 0, "", "two", "", vec![], HashMap::new(), -1)
            .await;
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn notify_records_history_newest_first() {
        let (server, _rx) = test_server();
        server
            .notify(
                "app-a",
                0,
                "",
                "first",
                "body-a",
                vec![],
                HashMap::new(),
                -1,
            )
            .await;
        server
            .notify(
                "app-b",
                0,
                "",
                "second",
                "body-b",
                vec![],
                HashMap::new(),
                -1,
            )
            .await;
        let hist = server.history.lock().unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].summary, "second");
        assert_eq!(hist[0].app_name, "app-b");
        assert_eq!(hist[0].body, "body-b");
        assert_eq!(hist[1].summary, "first");
    }

    #[test]
    fn parse_actions_pairs_and_drops_trailing_id() {
        let parsed = parse_actions(&[
            "default".into(),
            "Open".into(),
            "snooze".into(),
            "Snooze".into(),
            "orphan".into(),
        ]);
        assert_eq!(
            parsed,
            vec![
                Action {
                    key: "default".into(),
                    label: "Open".into(),
                },
                Action {
                    key: "snooze".into(),
                    label: "Snooze".into(),
                },
            ]
        );
    }

    #[test]
    fn parse_actions_skips_empty_keys() {
        assert!(parse_actions(&["", "Nope"].map(String::from)).is_empty());
    }

    #[test]
    fn inline_reply_from_action_or_kde_hint() {
        let reply_action = vec![Action {
            key: INLINE_REPLY_KEY.into(),
            label: "Reply".into(),
        }];
        assert_eq!(
            inline_reply_placeholder(&reply_action, &HashMap::new()).as_deref(),
            Some("Reply")
        );
        assert!(inline_reply_placeholder(&[], &HashMap::new()).is_none());

        let mut hints = HashMap::new();
        hints.insert(
            KDE_REPLY_PLACEHOLDER.to_string(),
            OwnedValue::try_from(zbus::zvariant::Value::from("Write a reply…")).unwrap(),
        );
        assert_eq!(
            inline_reply_placeholder(&[], &hints).as_deref(),
            Some("Write a reply…")
        );
        // Hint wins over the generic default when both are present.
        assert_eq!(
            inline_reply_placeholder(&reply_action, &hints).as_deref(),
            Some("Write a reply…")
        );
    }

    #[test]
    fn get_capabilities_includes_actions_and_inline_reply() {
        let (server, _rx) = test_server();
        let caps = server.get_capabilities();
        for wanted in ["body", "body-markup", "actions", "inline-reply"] {
            assert!(
                caps.iter().any(|c| c == wanted),
                "missing capability {wanted}"
            );
        }
    }

    #[tokio::test]
    async fn notify_forwards_actions_and_inline_reply() {
        let (server, mut rx) = test_server();
        server
            .notify(
                "chat",
                0,
                "",
                "Alice",
                "hello",
                vec![
                    "default".into(),
                    "Open".into(),
                    INLINE_REPLY_KEY.into(),
                    "Reply".into(),
                ],
                HashMap::new(),
                -1,
            )
            .await;
        match rx.recv().await.expect("Show event") {
            NotifEvent::Show {
                actions,
                inline_reply,
                summary,
                ..
            } => {
                assert_eq!(summary, "Alice");
                assert_eq!(actions.len(), 2);
                assert_eq!(actions[0].key, "default");
                assert_eq!(actions[1].key, INLINE_REPLY_KEY);
                assert_eq!(inline_reply.as_deref(), Some("Reply"));
            }
            _ => panic!("expected Show, got a different event"),
        }
        // History persist path is unchanged: actions are UI-only, not stored.
        let hist = server.history.lock().unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].summary, "Alice");
        assert_eq!(hist[0].body, "hello");
    }
}

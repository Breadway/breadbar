pub mod history;
pub mod popup;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use zbus::zvariant::OwnedValue;

/// Hint key used by `notify-send` and honored by notify-osd/dunst: senders
/// that fire off a new process per notification (so `replaces_id` is always
/// 0) tag related notifications with the same `(app_name, tag)` pair to mean
/// "replace whatever from this app is already showing." Without honoring
/// this, a fire-and-forget sender can never supersede an earlier
/// `Expire::Never` notification from itself (e.g. a critical hardware
/// warning) — it just piles up a new card next to it forever.
const SYNCHRONOUS_HINT: &str = "x-canonical-private-synchronous";

/// How long a shown notification should stay up before auto-dismissing.
/// Distinct from `Option<Duration>` mainly for readability at call sites —
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
    },
    Close(u32),
    ToggleHistory,
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
}

/// Private breadbar control surface on the same connection as
/// `org.freedesktop.Notifications`. `breadbar --history` is a one-shot
/// client of `ToggleHistory` — there is no other IPC.
struct BarService {
    tx: mpsc::Sender<NotifEvent>,
}

#[zbus::interface(name = "dev.breadway.Bar")]
impl BarService {
    async fn toggle_history(&self) {
        let _ = self.tx.send(NotifEvent::ToggleHistory).await;
    }
}

const BAR_DEST: &str = "org.freedesktop.Notifications";
const BAR_PATH: &str = "/dev/breadway/Bar";
const BAR_IFACE: &str = "dev.breadway.Bar";

/// Ask a running breadbar to toggle the history window. Used by
/// `breadbar --history`; does not start a second bar.
pub async fn toggle_history_remote() -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    conn.call_method(Some(BAR_DEST), BAR_PATH, Some(BAR_IFACE), "ToggleHistory", &())
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
        _actions: Vec<String>,
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
        // Per spec: 0 means "never expire" — this used to be lumped in
        // with "-1: let the server pick a default" and coerced to a fixed
        // 5s, so a sender explicitly asking for a persistent notification
        // (e.g. a progress/error dialog) got auto-dismissed anyway.
        // Critical-urgency notifications conventionally persist too, even
        // when the sender left expire_timeout at the server-default (-1).
        let urgency = Urgency::from_hint(hints.get("urgency"));
        let expire = compute_expire(expire_timeout, urgency == Urgency::Critical);

        history::record(
            &self.history,
            history::Entry {
                id,
                app_name: app_name.to_string(),
                summary: summary.to_string(),
                body: body.to_string(),
                urgency,
                received: SystemTime::now(),
            },
        );

        let _ = self
            .tx
            .send(NotifEvent::Show {
                id,
                app_name: app_name.to_string(),
                summary: summary.to_string(),
                body: body.to_string(),
                urgency,
                expire,
            })
            .await;
        id
    }

    async fn close_notification(&self, id: u32) {
        let _ = self.tx.send(NotifEvent::Close(id)).await;
    }

    fn get_capabilities(&self) -> Vec<String> {
        vec!["body".to_string()]
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

/// A fixed sample notification for `--screenshot notification`/
/// `notification-critical` — substitutes for a real `Notify` D-Bus call so a
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
        }
    }
}

/// Builds the notification window synchronously (see
/// `popup::build_window`'s doc comment) and spawns the event loop that
/// shows/updates/hides it.
///
/// `sample`: `Some` skips real D-Bus registration entirely and seeds the
/// loop with one fixed sample event instead — screenshot mode only. Doing
/// the real `org.freedesktop.Notifications` registration in every
/// screenshot run would race the real breadbar (if running) for the same
/// well-known name for no benefit, since nothing needs to reach this
/// instance externally.
pub fn spawn(sample: Option<SampleKind>) -> gtk4::Window {
    let (window, cards_box) = popup::build_window();
    let (tx, rx) = mpsc::channel(32);

    match sample {
        Some(kind) => {
            let _ = tx.try_send(kind.sample_event());
            let window_for_loop = window.clone();
            relm4::spawn_local(async move {
                popup::run(window_for_loop, cards_box, rx, None, None).await;
            });
        }
        None => {
            let (conn_tx, conn_rx) = tokio::sync::oneshot::channel();
            let store = history::new_store();
            let history_ui = history::build_window(store.clone());

            relm4::spawn(async move {
                let server = NotifServer {
                    tx: tx.clone(),
                    next_id: AtomicU32::new(1),
                    sync_tags: Mutex::new(HashMap::new()),
                    history: store,
                };
                let bar = BarService { tx };
                // Builder failures here would only occur with invalid static strings — safe to unwrap.
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
                // (spec-mandated whenever a notification actually goes away) — the
                // dismiss decisions all happen over there, not in this interface impl.
                let _ = conn_tx.send(conn);
                std::future::pending::<()>().await
            });

            let window_for_loop = window.clone();
            relm4::spawn_local(async move {
                if let Ok(conn) = conn_rx.await {
                    popup::run(window_for_loop, cards_box, rx, Some(conn), Some(history_ui))
                        .await;
                }
            });
        }
    }

    window
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
        // rather than overridden to Never — "critical persists" is only the
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
            .notify("app-a", 0, "", "first", "body-a", vec![], HashMap::new(), -1)
            .await;
        server
            .notify("app-b", 0, "", "second", "body-b", vec![], HashMap::new(), -1)
            .await;
        let hist = server.history.lock().unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].summary, "second");
        assert_eq!(hist[0].app_name, "app-b");
        assert_eq!(hist[0].body, "body-b");
        assert_eq!(hist[1].summary, "first");
    }
}

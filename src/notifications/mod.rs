pub mod history;
pub mod popup;
pub mod preferences;

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
    HistoryChanged,
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

    /// Post a notification from inside the bar, without a D-Bus round trip.
    ///
    /// The card replaces any earlier card with the same id, which is how
    /// repeat senders group into one card instead of stacking. The history
    /// store is the caller's to record: this loop never sees the D-Bus
    /// server path that records for external senders.
    pub fn notify(&self, id: u32, app_name: &str, summary: &str, body: &str, urgency: Urgency) {
        let _ = self.tx.try_send(NotifEvent::Show {
            id,
            app_name: app_name.to_string(),
            summary: summary.to_string(),
            body: body.to_string(),
            urgency,
            expire: Expire::After(Duration::from_secs(8)),
            actions: Vec::new(),
            inline_reply: None,
        });
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

/// Whether the session is currently locked. breadlock is a lock process,
/// not a background daemon; lock implementations which publish logind's
/// session hint are supported too. Factored out of `notify` so the delivery
/// decision itself stays a pure function of the typed preferences.
async fn session_locked() -> bool {
    if tokio::process::Command::new("pgrep")
        .args(["-q", "-x", "breadlock"])
        .status()
        .await
        .is_ok_and(|s| s.success())
    {
        return true;
    }
    if let Ok(session) = std::env::var("XDG_SESSION_ID") {
        if let Ok(out) = tokio::process::Command::new("loginctl")
            .args(["show-session", &session, "--property=LockedHint", "--value"])
            .output()
            .await
        {
            return String::from_utf8_lossy(&out.stdout).trim() == "yes";
        }
    }
    false
}

/// Sound plays when the per-app policy and the global switch allow it
/// (`delivery.sound`) and the sender did not set the `suppress-sound` hint.
fn should_play_sound(eligible: bool, quiet: bool) -> bool {
    eligible && !quiet
}

/// The player invocation, factored out so tests can assert the exact command
/// without playing audio.
fn sound_command() -> std::process::Command {
    let mut cmd = std::process::Command::new("canberra-gtk-play");
    cmd.args(["-i", "message"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
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
    /// `breadbar --history` under the spotlight theme: the notification
    /// panel only, not the launcher dashboard.
    ToggleHistory,
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
    history: history::Store,
    tx: mpsc::Sender<NotifEvent>,
    pill_tx: mpsc::UnboundedSender<PillCmd>,
    stage_tx: mpsc::UnboundedSender<StageCmd>,
}

#[zbus::interface(name = "dev.breadway.Bar")]
impl BarService {
    fn get_notification_preferences(&self) -> String {
        let mut prefs = preferences::load();
        prefs
            .extra
            .insert("api_version".into(), serde_json::json!(1));
        prefs.extra.insert(
            "effective_dnd".into(),
            serde_json::json!(preferences::effective_dnd(
                &prefs,
                preferences::clock_minute()
            )),
        );
        prefs.extra.insert(
            "sound_available".into(),
            serde_json::json!(std::env::var_os("PATH").is_some_and(|paths| {
                std::env::split_paths(&paths).any(|p| p.join("canberra-gtk-play").is_file())
            })),
        );
        // Apps seen in history are reported separately so Settings can offer
        // them: merging them into `apps` here would make a later Set persist
        // policies the user never chose.
        let known = {
            let guard = self.history.lock().unwrap();
            let mut known: Vec<String> = guard.iter().map(|e| e.app_name.clone()).collect();
            known.sort();
            known.dedup();
            known
        };
        prefs
            .extra
            .insert("known_apps".into(), serde_json::json!(known));
        serde_json::to_string(&prefs).unwrap_or_default()
    }
    fn set_notification_preferences(&self, json: &str) -> zbus::fdo::Result<()> {
        if json.len() > 131072 {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Settings are too large".into(),
            ));
        }
        let mut prefs: preferences::Preferences =
            serde_json::from_str(json).map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        for key in ["api_version", "effective_dnd", "sound_available"] {
            prefs.extra.remove(key);
        }
        preferences::save(&prefs).map_err(zbus::fdo::Error::Failed)
    }

    async fn toggle_history(&self) {
        if HISTORY_OPENS_STAGE.load(Ordering::Relaxed) {
            let _ = self.stage_tx.send(StageCmd::ToggleHistory);
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

        let prefs = if self.persist_history {
            preferences::load()
        } else {
            preferences::Preferences {
                sound: false,
                ..Default::default()
            }
        };
        // Without a persisted store there is no lock-screen preview setting
        // to honor, so headless tests (and any future non-persisting caller)
        // skip live lock detection the same way they skip preference reads.
        let locked = if self.persist_history {
            session_locked().await
        } else {
            false
        };
        let quiet = hints
            .get("suppress-sound")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false);
        self.deliver(
            id,
            app_name,
            summary,
            body,
            urgency,
            expire,
            actions,
            inline_reply,
            &prefs,
            locked,
            preferences::clock_minute(),
            quiet,
        )
        .await
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

impl NotifServer {
    /// Shared core of [`NotifServer::notify`] and the headless tests: route
    /// one notification through the typed preferences. `locked` is the live
    /// [`session_locked`] result in production and an explicit value in
    /// tests; `now` is minutes since midnight. Delivery follows
    /// [`preferences::presentation`], the single source of truth both paths
    /// agree on: DND suppresses ordinary banners only, and critical alerts
    /// keep history plus banner even on a locked session.
    #[allow(clippy::too_many_arguments)]
    async fn deliver(
        &self,
        id: u32,
        app_name: &str,
        summary: &str,
        body: &str,
        urgency: Urgency,
        expire: Expire,
        actions: Vec<Action>,
        inline_reply: Option<String>,
        prefs: &preferences::Preferences,
        locked: bool,
        now: u32,
        quiet: bool,
    ) -> u32 {
        let delivery =
            preferences::presentation(prefs, app_name, urgency == Urgency::Critical, now, locked);
        if !delivery.retain {
            return id;
        }
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

        if !delivery.banner {
            let _ = self.tx.send(NotifEvent::HistoryChanged).await;
            return id;
        }
        if should_play_sound(delivery.sound, quiet) {
            let _ = sound_command().spawn();
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
    /// A failed breadram intervention, so the RAM card has a fixed visual
    /// without racing a real daemon in an isolated capture.
    Ram,
}

impl SampleKind {
    fn sample_event(&self) -> NotifEvent {
        let (urgency, id, app_name, summary, body) = match self {
            SampleKind::Normal => (
                Urgency::Normal,
                1,
                "Sample App".into(),
                "Sample notification".into(),
                "This is what a notification card looks like.".into(),
            ),
            SampleKind::Critical => (
                Urgency::Critical,
                1,
                "Sample App".into(),
                "Sample notification".into(),
                "This is what a notification card looks like.".into(),
            ),
            SampleKind::Ram => (
                Urgency::Critical,
                crate::bar::ram::RAM_NOTIF_ID,
                "RAM".into(),
                "Memory intervention failed".into(),
                "cgroup_soft_limit on unit:app.service: expected freed 20 MiB, measured freed 4 MiB: over budget"
                    .into(),
            ),
        };
        NotifEvent::Show {
            id,
            app_name,
            summary,
            body,
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
                    history: store.clone(),
                    persist_history: true,
                };
                let bar = BarService {
                    history: store,
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

    /// Run `test` with XDG config/state pointed at a fresh temp dir (and no
    /// session id, so live lock detection would report unlocked), holding
    /// the shared env lock and restoring every variable afterwards. All
    /// tests that touch process environment must go through here.
    use preferences::with_isolated_home;

    fn persist_server() -> (NotifServer, mpsc::Receiver<NotifEvent>) {
        let (tx, rx) = mpsc::channel(32);
        (
            NotifServer {
                tx,
                next_id: AtomicU32::new(1),
                sync_tags: Mutex::new(HashMap::new()),
                history: history::new_store(),
                persist_history: true,
            },
            rx,
        )
    }

    fn plain_hints() -> HashMap<String, OwnedValue> {
        HashMap::new()
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

    /// Per-app policy on the user-visible path: History records without a
    /// banner, Off drops the notification, and the default policy banners.
    /// Driven through `notify()` with persistence on so the stored
    /// preferences are actually loaded.
    #[test]
    fn notify_routes_history_banner_and_off_through_preferences() {
        with_isolated_home("policy", |_dir| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let mut prefs = preferences::Preferences::default();
                prefs
                    .apps
                    .insert("hist-app".into(), preferences::AppPolicy::History);
                prefs
                    .apps
                    .insert("off-app".into(), preferences::AppPolicy::Off);
                preferences::save(&prefs).unwrap();
                let (server, mut rx) = persist_server();

                server
                    .notify("hist-app", 0, "", "quiet", "", vec![], plain_hints(), -1)
                    .await;
                assert_eq!(server.history.lock().unwrap().len(), 1);
                assert!(matches!(
                    rx.recv().await.expect("HistoryChanged event"),
                    NotifEvent::HistoryChanged
                ));

                server
                    .notify("off-app", 0, "", "gone", "", vec![], plain_hints(), -1)
                    .await;
                assert_eq!(server.history.lock().unwrap().len(), 1);
                assert!(rx.try_recv().is_err(), "Off must emit no event");

                server
                    .notify("chat", 0, "", "hello", "", vec![], plain_hints(), -1)
                    .await;
                assert_eq!(server.history.lock().unwrap().len(), 2);
                assert!(matches!(
                    rx.recv().await.expect("Show event"),
                    NotifEvent::Show { summary, .. } if summary == "hello"
                ));
            });
        });
    }

    /// Twin of the `critical_under_dnd_and_lock_keeps_history_plus_banner`
    /// predicate test: a critical alert under manual DND on a locked session
    /// keeps history plus banner, while an ordinary one keeps history only.
    /// If `presentation()` and the live path disagree, one of the two fails.
    #[test]
    fn notify_critical_keeps_history_plus_banner_under_dnd_and_lock() {
        with_isolated_home("critical-lock", |_dir| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                for lock_screen in [false, true] {
                    let prefs = preferences::Preferences {
                        dnd: true,
                        lock_screen,
                        ..preferences::Preferences::default()
                    };
                    preferences::save(&prefs).unwrap();
                    let (server, mut rx) = persist_server();
                    let stored = preferences::load();

                    let mut critical_hints = HashMap::new();
                    critical_hints.insert(
                        "urgency".to_string(),
                        OwnedValue::try_from(zbus::zvariant::Value::from(2u8)).unwrap(),
                    );
                    let id = server
                        .deliver(
                            7,
                            "app",
                            "critical",
                            "",
                            Urgency::Critical,
                            Expire::Never,
                            vec![],
                            None,
                            &stored,
                            true,
                            12 * 60,
                            false,
                        )
                        .await;
                    assert_eq!(id, 7);
                    assert_eq!(server.history.lock().unwrap().len(), 1);
                    assert!(
                        matches!(
                            rx.recv().await.expect("Show event"),
                            NotifEvent::Show { summary, .. } if summary == "critical"
                        ),
                        "critical must banner under DND and lock (lock_screen={lock_screen})"
                    );

                    server
                        .deliver(
                            8,
                            "app",
                            "ordinary",
                            "",
                            Urgency::Normal,
                            Expire::Never,
                            vec![],
                            None,
                            &stored,
                            true,
                            12 * 60,
                            false,
                        )
                        .await;
                    assert_eq!(server.history.lock().unwrap().len(), 2);
                    assert!(
                        matches!(
                            rx.recv().await.expect("HistoryChanged event"),
                            NotifEvent::HistoryChanged
                        ),
                        "ordinary stays suppressed under DND and lock (lock_screen={lock_screen})"
                    );
                }
            });
        });
    }

    /// `persist_history: false` never reads the stored preferences and never
    /// touches disk: even with DND persisted on, `notify()` still banners,
    /// and no state file appears.
    #[test]
    fn notify_without_persistence_ignores_stored_preferences() {
        with_isolated_home("no-persist", |dir| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let prefs = preferences::Preferences {
                    dnd: true,
                    ..preferences::Preferences::default()
                };
                preferences::save(&prefs).unwrap();
                let (server, mut rx) = test_server();
                server
                    .notify("app", 0, "", "still shows", "", vec![], plain_hints(), -1)
                    .await;
                assert!(matches!(
                    rx.recv().await.expect("Show event"),
                    NotifEvent::Show { .. }
                ));
                assert_eq!(server.history.lock().unwrap().len(), 1);
                assert!(
                    !dir.join("state/breadbar/history.json").exists(),
                    "unpersisted server must not write history to disk"
                );
            });
        });
    }

    /// The provider half of the round trip: Set persists, Get reads back,
    /// unknown keys survive, a second write keeps a backup, bad writes are
    /// rejected without touching the file, and history apps are reported via
    /// `known_apps` without being absorbed into `apps`.
    #[test]
    fn provider_set_then_get_round_trip() {
        with_isolated_home("provider", |dir| {
            let (tx, _rx) = mpsc::channel(32);
            let (pill_tx, _) = mpsc::unbounded_channel();
            let (stage_tx, _) = mpsc::unbounded_channel();
            let store = history::new_store();
            let service = BarService {
                history: store.clone(),
                tx,
                pill_tx,
                stage_tx,
            };
            let path = dir.join("config/breadbar/notifications.json");

            service
                .set_notification_preferences(
                    r#"{"dnd": true, "apps": {"chat": "history"}, "mystery": 42}"#,
                )
                .unwrap();
            let got: serde_json::Value =
                serde_json::from_str(&service.get_notification_preferences()).unwrap();
            assert_eq!(got["dnd"], true);
            assert_eq!(got["apps"]["chat"], "history");
            assert_eq!(got["mystery"], 42, "unknown keys must be preserved");
            assert_eq!(got["api_version"], 1);
            assert!(got["effective_dnd"].is_boolean());
            assert!(got["sound_available"].is_boolean());
            assert_eq!(got["known_apps"], serde_json::json!([]));

            // A history app shows up for Settings without becoming policy.
            history::record(
                &store,
                history::Entry {
                    id: 1,
                    app_name: "seen-app".into(),
                    summary: "hi".into(),
                    body: String::new(),
                    urgency: Urgency::Normal,
                    received: std::time::SystemTime::now(),
                    actions: vec![],
                },
            );
            let got: serde_json::Value =
                serde_json::from_str(&service.get_notification_preferences()).unwrap();
            assert_eq!(got["known_apps"], serde_json::json!(["seen-app"]));
            assert!(
                got["apps"].get("seen-app").is_none(),
                "a read must not absorb history apps into persisted policy"
            );

            // Second write backs up the first; the unknown key rides along.
            service
                .set_notification_preferences(
                    r#"{"dnd": false, "apps": {"chat": "history"}, "mystery": 42}"#,
                )
                .unwrap();
            let backup = dir.join("config/breadbar/notifications.json.bak");
            assert!(backup.exists(), "second write must keep a backup");
            let old: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&backup).unwrap()).unwrap();
            assert_eq!(old["dnd"], true);
            let got: serde_json::Value =
                serde_json::from_str(&service.get_notification_preferences()).unwrap();
            assert_eq!(got["dnd"], false);
            assert_eq!(got["mystery"], 42);

            // Bad writes are rejected and leave the file alone.
            let before = std::fs::read_to_string(&path).unwrap();
            assert!(service.set_notification_preferences("not json").is_err());
            assert!(
                service
                    .set_notification_preferences(r#"{"start": "xx", "end": "yy"}"#)
                    .is_err()
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        });
    }

    /// The sound gate without playing audio: policy eligibility plus the
    /// sender hint decide, and the spawned command is exactly
    /// `canberra-gtk-play -i message` with silenced stdio.
    #[test]
    fn sound_gate_and_command_construction() {
        assert!(should_play_sound(true, false));
        assert!(!should_play_sound(true, true), "suppress-sound hint wins");
        assert!(
            !should_play_sound(false, false),
            "ineligible policy stays silent"
        );
        let cmd = sound_command();
        assert_eq!(cmd.get_program().to_string_lossy(), "canberra-gtk-play");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["-i".to_string(), "message".to_string()]);
    }
}

//! breadram memory-pressure status for the bar.
//!
//! breadram publishes everything this module shows over its local Unix socket
//! (`$XDG_RUNTIME_DIR/breadram.sock`): one `Diagnostics` call carries the
//! management mode, the pressure state, and host memory, and one `Actions`
//! call carries the plan plus the bounded action history with each entry's
//! expected and measured memory change. No bus is involved, so this works
//! whether or not breadd runs, and a missing daemon is an ordinary
//! unavailable state: the pill goes quiet and nothing notifies.
//!
//! The wire protocol is breadram's versioned IPC (major 1): `Hello` first,
//! then newline-delimited `{id, type, ...}` messages. Only the fields this
//! module reads are modeled, and everything is optional with a default, so a
//! newer daemon never breaks the parse and an older one fails the handshake
//! loudly rather than misreading.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::{App, AppInput};

/// Protocol major this client speaks. A daemon with another major is
/// refused, not guessed at.
const PROTOCOL_MAJOR: u16 = 1;
/// The newest minor this client understands. A newer daemon still decodes:
/// every field read here is optional.
const PROTOCOL_MINOR: u16 = 8;
/// How often the daemon is re-read. Pressure moves slowly; cheaper than the
/// 2 s stats poll and quick enough to catch an intervention.
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Successful interventions digest after this many polls with successes
/// pending (about ten minutes): repeated actions group into one quiet card
/// instead of one popup each. Failures always notify immediately.
pub const SUCCESS_DIGEST_POLLS: u32 = 120;
/// One socket round trip may take this long before the daemon counts as
/// unreachable for this pass. The next pass reconnects: every poll dials
/// fresh, so there is no stale connection to notice.
const ROUND_TRIP_TIMEOUT: Duration = Duration::from_secs(3);
/// Successful interventions digest only when due. Failures and pressure
/// transitions always notify immediately.
/// The notification id every RAM card replaces. Server-assigned ids start
/// at 1, so this never collides with a D-Bus notification.
pub const RAM_NOTIF_ID: u32 = 0;

fn socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("breadram.sock"),
        _ => std::env::temp_dir().join("breadram.sock"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pressure {
    Healthy,
    Pressured,
    Critical,
    #[default]
    Unknown,
}

impl Pressure {
    fn from_state(state: &str) -> Self {
        match state.to_lowercase().as_str() {
            "healthy" => Pressure::Healthy,
            "pressured" => Pressure::Pressured,
            "critical" => Pressure::Critical,
            _ => Pressure::Unknown,
        }
    }

    /// The pill text: short by design, details live in the tooltip.
    pub fn label(self) -> &'static str {
        match self {
            Pressure::Healthy => "OK",
            Pressure::Pressured => "BUSY",
            Pressure::Critical => "FULL",
            Pressure::Unknown => "—",
        }
    }

    /// The CSS class added to the RAM chip, alongside the shared `stat-pair`
    /// chrome every other chip already uses.
    pub fn css_class(self) -> &'static str {
        match self {
            Pressure::Healthy => "ram-ok",
            Pressure::Pressured => "ram-pressured",
            Pressure::Critical => "ram-critical",
            Pressure::Unknown => "ram-off",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RamMode {
    Automatic,
    Monitor,
    #[default]
    Unknown,
}

impl RamMode {
    fn from_str(mode: &str) -> Self {
        match mode.to_lowercase().as_str() {
            "automatic" => RamMode::Automatic,
            "monitor" => RamMode::Monitor,
            _ => RamMode::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RamMode::Automatic => "auto",
            RamMode::Monitor => "paused",
            RamMode::Unknown => "—",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RamAction {
    pub scope: String,
    pub kind: String,
    pub state: String,
    pub reason: String,
    pub requested_bytes: Option<i64>,
    pub observed_bytes: Option<i64>,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct RamSnapshot {
    /// The daemon answered. False means quietly unavailable: no pill state,
    /// no notification, no error card.
    pub present: bool,
    pub pressure: Pressure,
    pub mode: RamMode,
    pub available_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    /// Journal entries newest first, as the daemon reported them.
    pub history: Vec<RamAction>,
}

// --- Wire structs: only the fields this module reads, all optional. ---

#[derive(Deserialize)]
struct HelloReply {
    #[serde(rename = "type")]
    kind: String,
    major: u16,
    #[allow(dead_code)]
    minor: u16,
    #[allow(dead_code)]
    server: Option<String>,
}

#[derive(Deserialize, Default)]
struct PressureWire {
    #[serde(default)]
    state: String,
}

#[derive(Deserialize, Default)]
struct DiagnosticsWire {
    #[serde(default)]
    mode: String,
    #[serde(default)]
    pressure: PressureWire,
    #[serde(default)]
    memory: MemoryWire,
}

#[derive(Deserialize, Default)]
struct MemoryWire {
    #[serde(default)]
    ram_available_bytes: Option<u64>,
    #[serde(default)]
    ram_total_bytes: Option<u64>,
}

#[derive(Deserialize, Default)]
struct ActionWire {
    #[serde(default)]
    scope: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    state: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    requested_host_ram_delta: Option<i64>,
    #[serde(default)]
    observed_host_ram_delta: Option<i64>,
    #[serde(default)]
    at_ms: u64,
}

#[derive(Deserialize, Default)]
struct ActionsWire {
    #[serde(default)]
    history: Vec<ActionWire>,
}

struct Client {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    next_id: u64,
}

impl Client {
    async fn connect() -> std::io::Result<Self> {
        let stream = UnixStream::connect(socket_path()).await?;
        let (read, write) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(read),
            writer: write,
            next_id: 1,
        })
    }

    async fn call(&mut self, body: serde_json::Value) -> Option<String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut message = serde_json::json!({"id": id});
        if let Some(object) = body.as_object() {
            for (key, value) in object {
                message[key] = value.clone();
            }
        }
        let mut line = serde_json::to_string(&message).ok()?;
        line.push('\n');
        tokio::time::timeout(ROUND_TRIP_TIMEOUT, self.writer.write_all(line.as_bytes()))
            .await
            .ok()?
            .ok()?;
        let mut answer = String::new();
        tokio::time::timeout(ROUND_TRIP_TIMEOUT, self.reader.read_line(&mut answer))
            .await
            .ok()?
            .ok()?;
        if answer.trim().is_empty() {
            return None;
        }
        Some(answer)
    }

    async fn read_snapshot() -> RamSnapshot {
        let mut client = match Client::connect().await {
            Ok(client) => client,
            Err(_) => return RamSnapshot::default(),
        };
        let hello = serde_json::json!({
            "type": "hello",
            "major": PROTOCOL_MAJOR,
            "minor": PROTOCOL_MINOR,
            "client": "breadbar",
        });
        let reply: Option<HelloReply> = client
            .call(hello)
            .await
            .and_then(|line| serde_json::from_str(&line).ok());
        let Some(reply) = reply else {
            return RamSnapshot::default();
        };
        if reply.kind != "hello" || reply.major != PROTOCOL_MAJOR {
            return RamSnapshot::default();
        }
        let diagnostics: Option<DiagnosticsWire> = client
            .call(serde_json::json!({"type": "diagnostics"}))
            .await
            .and_then(|line| {
                serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|value| serde_json::from_value(value).ok())
            });
        let actions: Vec<RamAction> = client
            .call(serde_json::json!({"type": "actions"}))
            .await
            .and_then(|line| {
                serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|value| serde_json::from_value::<ActionsWire>(value).ok())
            })
            .map(|wire| {
                wire.history
                    .into_iter()
                    .map(|record| RamAction {
                        scope: record.scope,
                        kind: record.kind.unwrap_or_default(),
                        state: record.state,
                        reason: record.reason,
                        requested_bytes: record.requested_host_ram_delta,
                        observed_bytes: record.observed_host_ram_delta,
                        at_ms: record.at_ms,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let Some(diagnostics) = diagnostics else {
            return RamSnapshot::default();
        };
        RamSnapshot {
            present: true,
            pressure: Pressure::from_state(&diagnostics.pressure.state),
            mode: RamMode::from_str(&diagnostics.mode),
            available_bytes: diagnostics.memory.ram_available_bytes,
            total_bytes: diagnostics.memory.ram_total_bytes,
            history: actions,
        }
    }

    /// Switch the management mode without persisting it: a pause is
    /// temporary by construction, so a daemon restart restores the
    /// configured mode rather than the paused one.
    pub async fn set_mode(paused: bool) -> bool {
        let mut client = match Client::connect().await {
            Ok(client) => client,
            Err(_) => return false,
        };
        let hello = serde_json::json!({
            "type": "hello",
            "major": PROTOCOL_MAJOR,
            "minor": PROTOCOL_MINOR,
            "client": "breadbar",
        });
        let ok: bool = client
            .call(hello)
            .await
            .and_then(|line| serde_json::from_str::<HelloReply>(&line).ok())
            .is_some_and(|reply| reply.kind == "hello" && reply.major == PROTOCOL_MAJOR);
        if !ok {
            return false;
        }
        let mode = if paused { "monitor" } else { "automatic" };
        client
            .call(serde_json::json!({"type": "set_mode", "mode": mode}))
            .await
            .and_then(|line| serde_json::from_str::<serde_json::Value>(&line).ok())
            .is_some_and(|value| {
                value.get("type").and_then(|kind| kind.as_str()) == Some("applied")
            })
    }
}

pub fn spawn_poller(sender: relm4::ComponentSender<App>) {
    relm4::spawn(async move {
        loop {
            sender.input(AppInput::RamUpdate(Client::read_snapshot().await));
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
}

pub fn spawn_set_mode(paused: bool, sender: relm4::ComponentSender<App>) {
    relm4::spawn(async move {
        let ok = Client::set_mode(paused).await;
        // Refresh at once instead of waiting for the next poll so the
        // button answers the click.
        sender.input(AppInput::RamUpdate(Client::read_snapshot().await));
        sender.input(AppInput::RamModeSet { paused, ok });
    });
}

/// What the bar should tell the user about new journal entries.
///
/// Pure over the daemon's newest-first history: everything about grouping
/// and rate limits is unit-testable without a socket or a display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RamNotice {
    pub summary: String,
    pub body: String,
    pub critical: bool,
}

fn action_line(action: &RamAction) -> String {
    let effect = match (action.requested_bytes, action.observed_bytes) {
        (Some(expected), Some(measured)) => {
            format!(
                "expected {}, measured {}",
                signed_bytes(expected),
                signed_bytes(measured)
            )
        }
        (Some(expected), None) => format!("expected {}", signed_bytes(expected)),
        (None, Some(measured)) => format!("measured {}", signed_bytes(measured)),
        (None, None) => "no measured effect".to_string(),
    };
    let what = if action.kind.is_empty() {
        action.scope.clone()
    } else {
        format!("{} on {}", action.kind, action.scope)
    };
    format!("{what}: {effect}")
}

fn signed_bytes(delta: i64) -> String {
    // Relief is negative on the wire; users read "freed 12 MiB".
    let (word, bytes) = if delta < 0 {
        ("freed", delta.unsigned_abs())
    } else {
        ("used", delta as u64)
    };
    format!("{word} {}", bytes_text(bytes))
}

pub fn bytes_text(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.0} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.0} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Decide what to tell the user about journal entries newer than
/// `seen_at_ms`.
///
/// Failures notify immediately and critically, one card per entry, each
/// replacing the last RAM card. Successful confirmations accumulate into a
/// digest only when one is due (the caller counts polls: journal timestamps
/// are daemon-monotonic and unreadable here). Repeated actions group into
/// one quiet card instead of one popup each. Pressure transitions are the
/// caller's job (it knows the previous pill state); this function only
/// reads the journal.
pub fn decide_notices(
    history: &[RamAction],
    seen_at_ms: u64,
    digest_due: bool,
) -> (Vec<RamNotice>, u64, bool) {
    let mut notices = Vec::new();
    let mut newest = seen_at_ms;
    let mut successes = 0u32;
    let mut freed: i64 = 0;
    for action in history {
        if action.at_ms <= seen_at_ms {
            break;
        }
        newest = newest.max(action.at_ms);
        match action.state.as_str() {
            "confirmed" => {
                successes += 1;
                freed += action.observed_bytes.unwrap_or(0);
            }
            "failed" => notices.push(RamNotice {
                summary: "Memory intervention failed".to_string(),
                body: format!("{}: {}", action_line(action), action.reason),
                critical: true,
            }),
            _ => {}
        }
    }
    if successes > 0 && digest_due {
        let summary = if successes == 1 {
            "Memory intervention applied".to_string()
        } else {
            format!("{successes} memory interventions applied")
        };
        notices.push(RamNotice {
            summary,
            body: format!("observed {}", signed_bytes(freed)),
            critical: false,
        });
        return (notices, newest, true);
    }
    (notices, newest, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(at_ms: u64, state: &str, observed: Option<i64>) -> RamAction {
        RamAction {
            scope: "unit:app.service".to_string(),
            kind: "cgroup_soft_limit".to_string(),
            state: state.to_string(),
            reason: "over budget".to_string(),
            requested_bytes: Some(-20_000_000),
            observed_bytes: observed,
            at_ms,
        }
    }

    #[test]
    fn failures_notify_immediately_and_critically() {
        let history = vec![
            action(2_000, "failed", None),
            action(1_000, "confirmed", Some(-5_000_000)),
        ];
        let (notices, newest, _) = decide_notices(&history, 500, false);
        assert_eq!(newest, 2_000);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].critical);
        assert!(notices[0].summary.contains("failed"));
        assert!(notices[0].body.contains("expected"));
    }

    #[test]
    fn successes_digest_quietly_and_rate_limit() {
        let history = vec![
            action(2_000, "confirmed", Some(-5_000_000)),
            action(1_500, "confirmed", Some(-7_000_000)),
        ];
        let (notices, newest, digested) = decide_notices(&history, 500, true);
        assert_eq!(newest, 2_000);
        assert_eq!(notices.len(), 1);
        assert!(digested);
        assert!(!notices[0].critical);
        assert!(notices[0].summary.contains('2'));
        // Not due: the same entries produce nothing new.
        let (again, newest, digested) = decide_notices(&history, 500, false);
        assert!(again.is_empty());
        assert_eq!(newest, 2_000);
        assert!(!digested);
        // Seen: nothing new at all.
        let (seen, newest, _) = decide_notices(&history, newest, true);
        assert!(seen.is_empty());
        assert_eq!(newest, 2_000);
    }

    #[test]
    fn nothing_new_notifies_nothing() {
        let history = vec![action(1_000, "confirmed", Some(-5_000_000))];
        let (notices, newest, _) = decide_notices(&history, 5_000, true);
        assert!(notices.is_empty());
        assert_eq!(newest, 5_000);
    }

    #[test]
    fn unknown_states_are_ignored() {
        let history = vec![action(2_000, "skipped_duplicate", None)];
        let (notices, newest, _) = decide_notices(&history, 500, true);
        assert!(notices.is_empty());
        assert_eq!(newest, 2_000);
    }

    #[test]
    fn pressure_labels_stay_short() {
        assert_eq!(Pressure::Critical.label(), "FULL");
        assert_eq!(Pressure::Unknown.label(), "—");
        assert_eq!(Pressure::Pressured.css_class(), "ram-pressured");
    }
}

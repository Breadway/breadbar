//! Connects to breadd's IPC socket and keeps the bar's widget set in sync.
//!
//! breadbar is level-triggered here, not edge-triggered: `bread.widget.*`
//! events are used purely as a "something changed, go re-fetch" signal, not
//! applied as incremental patches. Every dirty signal (and the initial
//! connect) re-requests the complete widget list and hands it to `update()`
//! as one `AppInput::WidgetsUpdate`, which reconciles the bar's containers
//! from scratch. This sidesteps event-ordering/drop concerns entirely, and
//! widget registries are small enough that re-fetching the full list on
//! every change is not a real cost.

use crate::{App, AppInput};
use bread_shared::widget::WidgetSpec;
use bread_utils::bread_client::BreadClient;
use relm4::ComponentSender;
use std::time::Duration;

/// breadbar's own registered app id — already reserved in
/// `bread_shared::apps::KNOWN_APPS` (see `Documentation.md`'s Namespaces
/// section). Used both to fetch widgets and to publish click events.
pub const APP_ID: &str = "bar";

/// Safety-net poll interval. `bread.widget.cleared` (emitted once per
/// daemon reload, including a full restart — see breadd's `reload_internal`)
/// is meant to catch the case where a module stops registering widgets
/// without anything else re-triggering a fetch, but a *restart* (as opposed
/// to a live `bread reload`) drops the subscription entirely; if that one
/// event fires before `BreadClient::subscribe`'s reconnect-with-backoff
/// finishes re-establishing the stream, it's missed and there's no second
/// chance from the event side. This poll is the backstop for that race —
/// infrequent enough that it's not a real cost, frequent enough that a missed
/// event self-heals well within a session rather than needing a manual
/// breadbar restart to clear stale widgets.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Start the widget subsystem: an initial fetch, a live subscription that
/// re-fetches on every `bread.widget.*` change, and a low-frequency poll as
/// a backstop against the reconnect race described above. Call once from
/// `init`.
pub fn spawn(sender: ComponentSender<App>) {
    // BreadClient::request is blocking std I/O; run it off the tokio
    // runtime breadbar's other pollers rely on, same as the reasoning in
    // `BreadClient::subscribe`'s own background-thread design.
    let initial = sender.clone();
    std::thread::spawn(move || fetch_and_send(&initial));

    // `subscribe` already reconnects with backoff on its own background
    // thread for the lifetime of the process — there is no natural point to
    // stop it before the app exits, so the handle is intentionally leaked
    // rather than threaded through App just to be dropped at shutdown.
    let live = sender.clone();
    let client = BreadClient::connect(APP_ID);
    let subscription = client.subscribe("bread.widget.**", move |_event| {
        fetch_and_send(&live);
    });
    std::mem::forget(subscription);

    let polled = sender.clone();
    relm4::spawn(async move {
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let polled = polled.clone();
            std::thread::spawn(move || fetch_and_send(&polled));
        }
    });
}

fn fetch_and_send(sender: &ComponentSender<App>) {
    let client = BreadClient::connect(APP_ID);
    let Some(result) = client.request("widgets.list", serde_json::Value::Null) else {
        return;
    };
    // Decode element-wise rather than `Vec<WidgetSpec>` in one shot — one
    // malformed entry from any module (a bad `class`, an unknown enum value,
    // ...) must not blank out every other module's widgets.
    let raw: Vec<serde_json::Value> = serde_json::from_value(result).unwrap_or_default();
    let specs: Vec<WidgetSpec> = raw
        .into_iter()
        .filter_map(|v| {
            // `id`/`module` are read before the value is consumed by the
            // failed parse below, so a malformed spec still names itself in
            // the warning instead of just printing a bare serde error.
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("?").to_string();
            let module = v.get("module").and_then(|x| x.as_str()).unwrap_or("?").to_string();
            match serde_json::from_value::<WidgetSpec>(v) {
                Ok(spec) => Some(spec),
                Err(e) => {
                    eprintln!(
                        "breadbar: dropping malformed widget spec (id={id}, module={module}): {e}"
                    );
                    None
                }
            }
        })
        .collect();
    sender.input(AppInput::WidgetsUpdate(specs));
}

/// Publish a widget click back to breadd. `action` is whatever opaque value
/// the Lua module put in the clicked node's `on_click`.
pub fn emit_click(widget_id: &str, action: &serde_json::Value) {
    BreadClient::connect(APP_ID).emit(
        "bread.bar.widget_clicked",
        serde_json::json!({ "widget_id": widget_id, "action": action }),
    );
}

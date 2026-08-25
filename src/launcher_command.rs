//! Subscribes to `bread.command.box.open` and focuses/opens the capsule —
//! only under `[launcher] mode = "embedded"` (spotlight, THEME_SYSTEM_PLAN.md
//! §7 phase 6c). See `breadbox/EVENTS.md` for the command's existing
//! contract: it's honored today only while `breadbox listen` is running, and
//! is a silent no-op at the bus with no subscriber. breadbar becomes a
//! SECOND subscriber of the exact same verb here — under an embedded theme,
//! breadbox's own `main` (see its doc comment on `dispatch_embedded_open`)
//! redirects a direct launch to this same event instead of mapping its own
//! overlay window, specifically so this module can pick it up. `breadbox
//! listen`'s own handling of the same event is separately made a no-op
//! under an embedded theme (see its `handle_open`) — there is exactly one
//! real handler for this event at a time, whichever theme is active.
//!
//! Same connection pattern as `widgets::client` (this crate's other
//! `BreadClient::subscribe` user): a fire-and-forget connect, a background
//! subscription thread with its own reconnect/backoff, and the handle is
//! leaked rather than threaded through `App` — there's no natural point to
//! stop it before the process exits.

use crate::{App, AppInput};
use bread_theme::shell::LauncherMode;
use bread_utils::bread_client::BreadClient;
use relm4::ComponentSender;

/// Starts the subscription iff the active shell theme's launcher is
/// `Embedded`. A no-op call under every other theme — never connects to
/// breadd at all, matching the "effectively a no-op under every other
/// theme" pattern the rest of the capsule wiring already follows (main.rs's
/// `open_fn`/`close_fn` doc comment).
pub fn spawn(sender: ComponentSender<App>) {
    if crate::theme::shell_theme().launcher().mode != LauncherMode::Embedded {
        return;
    }
    let client = BreadClient::connect(crate::widgets::client::APP_ID);
    // Two verbs, deliberately.
    //
    // `bread.box.open_requested` is what breadbox actually emits when the
    // active theme is embedded: an app may only publish inside its own
    // `bread.<app_id>.*` namespace, so breadbox (app id `box`) cannot emit a
    // `bread.command.*` event at all — bread-client refuses it outright.
    //
    // `bread.command.box.open` is kept because it is the addressed-TO-an-app
    // command form, which is what an external trigger (the `bread` CLI, a
    // keybind, another app) would legitimately send. Honouring both means the
    // capsule opens whether it was asked directly or told by breadbox.
    for verb in ["bread.box.open_requested", "bread.command.box.open"] {
        let sender = sender.clone();
        let subscription = client.subscribe(verb, move |_event| {
            sender.input(AppInput::OpenLauncher);
        });
        std::mem::forget(subscription);
    }
}

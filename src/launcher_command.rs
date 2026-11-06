//! Subscribes to breadbox's launcher-open events and focuses/opens the
//! capsule, or toggles the spotlight stage for themes that have no capsule in
//! a bar slot. See `breadbox/EVENTS.md` for the command's existing contract:
//! it's honored today only while `breadbox listen` is running, and is a silent
//! no-op at the bus with no subscriber. breadbar becomes a SECOND subscriber
//! of the exact same verb here; under an embedded theme, breadbox's own `main`
//! (see its doc comment on `dispatch_embedded_open`) redirects a direct launch
//! to this same event instead of mapping its own overlay window, specifically
//! so this module can pick it up. `breadbox listen`'s own handling of the same
//! event is separately made a no-op under an embedded theme (see its
//! `handle_open`); there is exactly one real handler for this event at a time,
//! whichever theme is active.
//!
//! BAR-66: the subscription is process-lifetime and independent of the theme
//! that was active at startup. It used to be started only when the initial
//! theme's launcher was `Embedded`, so starting breadbar on any other theme
//! and then switching to spotlight at runtime left the process with no
//! subscription at all: breadbox's `bread.box.open_requested` had no
//! subscriber and the stage never opened. The theme is now consulted per
//! request ([`open_request_is_for_this_process`]), which also means a theme
//! switch must never start another subscription.
//!
//! Same connection pattern as `widgets::client` (this crate's other
//! `BreadClient::subscribe` user): a fire-and-forget connect and a background
//! subscription thread with its own reconnect/backoff. Unlike that module the
//! handles are owned, not leaked: [`LauncherSubscriptions::stop`] drops them
//! from `App::shutdown`, which stops the subscription threads.

use crate::{App, AppInput};
use bread_theme::shell::{LauncherMode, ShellTheme};
use bread_utils::bread_client::{BreadClient, Subscription};
use relm4::ComponentSender;

/// Both verbs, deliberately.
///
/// `bread.box.open_requested` is what breadbox actually emits when the active
/// theme is embedded: an app may only publish inside its own
/// `bread.<app_id>.*` namespace, so breadbox (app id `box`) cannot emit a
/// `bread.command.*` event at all; bread-client refuses it outright.
///
/// `bread.command.box.open` is kept because it is the addressed-TO-an-app
/// command form, which is what an external trigger (the `bread` CLI, a
/// keybind, another app) would legitimately send. Honouring both means the
/// capsule opens whether it was asked directly or told by breadbox.
pub const OPEN_VERBS: [&str; 2] = ["bread.box.open_requested", "bread.command.box.open"];

/// Owns this process's launcher-bus subscriptions.
///
/// Generic over the handle type so the lifecycle (start once, stop on
/// shutdown) is unit-testable against a fake bus: production instantiates it
/// with [`Subscription`].
pub struct LauncherSubscriptions<H = Subscription> {
    handles: Vec<H>,
}

impl<H> Default for LauncherSubscriptions<H> {
    fn default() -> Self {
        Self {
            handles: Vec::new(),
        }
    }
}

impl<H> LauncherSubscriptions<H> {
    /// Subscribes to every verb in [`OPEN_VERBS`] exactly once.
    ///
    /// Idempotent: once a set exists, a second call is a no-op. That keeps a
    /// reload/theme-switch path that happens to call [`spawn`] again from
    /// double-subscribing, which would deliver one bus event twice and toggle
    /// the stage twice.
    pub fn start_with(&mut self, mut subscribe: impl FnMut(&'static str) -> H) -> usize {
        if !self.handles.is_empty() {
            return self.handles.len();
        }
        for verb in OPEN_VERBS {
            self.handles.push(subscribe(verb));
        }
        self.handles.len()
    }

    /// Drops every handle, ending its subscription. `Subscription`'s `Drop`
    /// sets the thread's stop flag and shuts the live socket, so a thread
    /// blocked in a read wakes up and exits; it is deliberately not joined,
    /// matching the library's own best-effort shutdown. Called from
    /// `App::shutdown`.
    pub fn stop(&mut self) {
        self.handles.clear();
    }
}

/// Starts the launcher-bus subscription for the whole process, whatever the
/// initial theme is.
///
/// Returns the owned handles so `App` can stop them on shutdown. The current
/// theme is checked in the `AppInput::OpenLauncher` handler instead: a
/// subscription started under a non-embedded theme is inert until the theme
/// actually becomes embedded.
pub fn spawn(sender: ComponentSender<App>) -> LauncherSubscriptions {
    let client = BreadClient::connect(crate::widgets::client::APP_ID);
    let mut subscriptions = LauncherSubscriptions::default();
    subscriptions.start_with(|verb| {
        let sender = sender.clone();
        client.subscribe(verb, move |_event| {
            sender.input(AppInput::OpenLauncher);
        })
    });
    subscriptions
}

/// Whether a launcher-open request from the bus belongs to this process under
/// `theme`.
///
/// Only an `Embedded`-launcher theme routes Super+Space through breadbox to
/// this event at all. Every other theme keeps its own overlay launcher, so
/// the subscription has to stay silent (exactly as it was when it only
/// existed under an embedded initial theme).
pub fn open_request_is_for_this_process(theme: &ShellTheme) -> bool {
    theme.launcher().mode == LauncherMode::Embedded
}

#[cfg(test)]
mod tests {
    use super::{open_request_is_for_this_process, LauncherSubscriptions, OPEN_VERBS};
    use bread_theme::shell::load_named;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Fake bus: records each subscription and can deliver an event to it.
    /// Mirrors the real `BreadClient::subscribe` shape without a socket.
    #[derive(Default)]
    struct FakeBus {
        on_open: Vec<Rc<dyn Fn()>>,
    }

    impl FakeBus {
        fn subscribe(&mut self, on_open: Rc<dyn Fn()>) -> usize {
            self.on_open.push(on_open);
            self.on_open.len() - 1
        }

        fn deliver(&self) {
            for on_open in &self.on_open {
                on_open();
            }
        }
    }

    /// The production lifecycle plus the production theme gate, wired the
    /// same way `App` wires them around `AppInput::OpenLauncher`.
    struct Harness {
        bus: FakeBus,
        subscriptions: LauncherSubscriptions<usize>,
        capsule_opens: Rc<RefCell<usize>>,
        stage_opens: Rc<RefCell<usize>>,
        theme: bread_theme::shell::ShellTheme,
    }

    impl Harness {
        fn new(theme_id: &str) -> Self {
            let mut harness = Self {
                bus: FakeBus::default(),
                subscriptions: LauncherSubscriptions::default(),
                capsule_opens: Rc::new(RefCell::new(0)),
                stage_opens: Rc::new(RefCell::new(0)),
                theme: load_named(theme_id).unwrap(),
            };
            harness.start();
            harness
        }

        /// The process-start path: one subscription set.
        fn start(&mut self) {
            let capsule_opens = Rc::clone(&self.capsule_opens);
            self.subscriptions.start_with(|_verb| {
                let capsule_opens = Rc::clone(&capsule_opens);
                self.bus
                    .subscribe(Rc::new(move || *capsule_opens.borrow_mut() += 1))
            });
        }

        /// A live theme switch: theme state moves, and a (deliberately
        /// redundant) start attempt must not add another subscription.
        fn theme_changed(&mut self, theme_id: &str) {
            self.theme = load_named(theme_id).unwrap();
            self.start();
        }

        /// What `AppInput::OpenLauncher` does with one bus delivery under the
        /// current theme: ignore it, or open the stage when the theme has no
        /// capsule in a slot.
        fn deliver_box_open_requested(&mut self) {
            if !open_request_is_for_this_process(&self.theme) {
                return;
            }
            if crate::open_request_goes_to_stage(&self.theme) {
                *self.stage_opens.borrow_mut() += 1;
                return;
            }
            self.bus.deliver();
        }

        fn sub_count(&self) -> usize {
            self.bus.on_open.len()
        }
    }

    #[test]
    fn switching_to_spotlight_opens_the_stage_once() {
        let mut harness = Harness::new("liquid-motion");
        assert_eq!(harness.sub_count(), OPEN_VERBS.len());

        // Started on a non-embedded theme: the request is not ours.
        harness.deliver_box_open_requested();
        assert_eq!(*harness.stage_opens.borrow(), 0);
        assert_eq!(*harness.capsule_opens.borrow(), 0);

        harness.theme_changed("spotlight");
        harness.deliver_box_open_requested();
        assert_eq!(*harness.stage_opens.borrow(), 1);
        assert_eq!(*harness.capsule_opens.borrow(), 0);
    }

    #[test]
    fn switching_back_stops_opening_the_stage() {
        let mut harness = Harness::new("spotlight");
        harness.deliver_box_open_requested();
        assert_eq!(*harness.stage_opens.borrow(), 1);

        harness.theme_changed("daylight");
        harness.deliver_box_open_requested();
        assert_eq!(*harness.stage_opens.borrow(), 1);
    }

    #[test]
    fn repeated_theme_events_do_not_double_subscribe() {
        let mut harness = Harness::new("liquid-motion");
        let started = harness.sub_count();
        assert_eq!(started, OPEN_VERBS.len());

        harness.theme_changed("spotlight");
        harness.theme_changed("spotlight");
        assert_eq!(harness.sub_count(), started);

        // One event still means one open, not one per redundant start call.
        harness.deliver_box_open_requested();
        assert_eq!(*harness.stage_opens.borrow(), 1);
    }

    #[test]
    fn stop_drops_every_handle() {
        let mut harness = Harness::new("spotlight");
        assert!(!harness.subscriptions.handles.is_empty());
        harness.subscriptions.stop();
        assert!(harness.subscriptions.handles.is_empty());
    }

    #[test]
    fn only_embedded_launcher_themes_take_bus_requests() {
        // Liquid Motion is an overlay-launcher theme: not ours at all.
        let liquid = load_named("liquid-motion").unwrap();
        assert!(!open_request_is_for_this_process(&liquid));
        // Spotlight is embedded: the request is ours (and `main.rs` routes it
        // to the stage because the theme places no capsule in a slot).
        let spotlight = load_named("spotlight").unwrap();
        assert!(open_request_is_for_this_process(&spotlight));
    }
}

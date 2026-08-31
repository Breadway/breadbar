use std::{
    cell::{Cell, RefCell},
    process::Child,
    rc::Rc,
    sync::{Mutex, Once},
    time::Duration,
};

use gtk4::prelude::*;
use gtk4_layer_shell::LayerShell;
use tokio::sync::mpsc;

enum OsdEvent {
    Volume { pct: u8, muted: bool },
    Brightness { pct: u8 },
}

/// A fixed sample event for `--screenshot osd-volume`/`osd-brightness` —
/// substitutes for the real `pactl subscribe`/backlight-sysfs watchers so a
/// capture doesn't depend on this machine's actual volume/brightness at
/// capture time.
pub enum SampleKind {
    Volume,
    Brightness,
}

/// Live `pactl subscribe` children spawned by [`volume_watcher`]. Kept so a
/// best-effort cleanup can kill them when breadbar exits: `pactl subscribe`
/// blocks until the server connection dies, so without this every breadbar
/// restart orphaned one that kept its PulseAudio connection open — until
/// pipewire-pulse's client cap filled up and new clients (settings apps
/// included) were refused, showing "no devices". Also reaped here, so a
/// watcher that dies on its own never lingers as a zombie.
static WATCHER_CHILDREN: Mutex<Vec<Child>> = Mutex::new(Vec::new());
static REGISTER_EXIT_HOOK: Once = Once::new();

extern "C" {
    /// libc `atexit(3)`. Declared directly rather than pulling the libc
    /// crate in for a single function.
    fn atexit(cb: extern "C" fn()) -> i32;
}

extern "C" fn exit_cleanup() {
    kill_watchers();
}

fn register_exit_hook() {
    // Safety: `atexit` is provided by libc on every Linux target; the
    // callback is a `static` C-ABI fn valid for the whole process.
    unsafe {
        let _ = atexit(exit_cleanup);
    }
}

/// Kill any live `pactl subscribe` watcher children and reap them. Safe to
/// call more than once (an already-dead child is a no-op). Runs from the
/// process-exit hook and the SIGINT/SIGTERM handlers in `main`.
pub fn kill_watchers() {
    if let Ok(mut children) = WATCHER_CHILDREN.lock() {
        for mut child in children.drain(..) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl SampleKind {
    fn sample_event(&self) -> OsdEvent {
        match self {
            SampleKind::Volume => OsdEvent::Volume { pct: 65, muted: false },
            SampleKind::Brightness => OsdEvent::Brightness { pct: 80 },
        }
    }
}

/// Builds the OSD window synchronously (so a caller — screenshot mode, via
/// `sample`, in particular — has a real window to hook `connect_map` on
/// before the async event loop below ever runs) and spawns the event loop
/// that shows/updates/hides it.
///
/// `sample`: `Some` skips the real volume/brightness watchers entirely and
/// seeds the loop with one fixed sample event instead — screenshot mode
/// only, so a capture never depends on (or is disrupted by) this machine's
/// actual audio/backlight state.
pub fn spawn(sample: Option<SampleKind>) -> gtk4::Window {
    let (tx, rx) = mpsc::channel::<OsdEvent>(8);

    match sample {
        Some(kind) => {
            let _ = tx.try_send(kind.sample_event());
        }
        None => {
            let tx1 = tx.clone();
            std::thread::spawn(move || volume_watcher(tx1));
            std::thread::spawn(move || brightness_watcher(tx));
        }
    }

    let window = create_window();
    relm4::spawn_local(run_osd(window.clone(), rx));
    window
}

fn volume_watcher(tx: mpsc::Sender<OsdEvent>) {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let Ok(mut child) = Command::new("pactl")
        .args(["subscribe"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };

    let Some(stdout) = child.stdout.take() else { return };
    // The child is meant to outlive this reader loop (it blocks until
    // breadbar itself dies), so hand it to `kill_watchers` — the exit
    // hook plus the SIGINT/SIGTERM handlers in main — instead of letting
    // it orphan on restart.
    REGISTER_EXIT_HOOK.call_once(register_exit_hook);
    if let Ok(mut children) = WATCHER_CHILDREN.lock() {
        children.push(child);
    }
    let reader = BufReader::new(stdout);

    for line in reader.lines().map_while(Result::ok) {
        if line.contains("'change' on sink") {
            if let Some(evt) = query_volume() {
                let _ = tx.blocking_send(evt);
            }
        }
    }
}

fn query_volume() -> Option<OsdEvent> {
    use std::process::Command;

    let vol = Command::new("pactl")
        .args(["get-sink-volume", "@DEFAULT_SINK@"])
        .output()
        .ok()?;
    let mute = Command::new("pactl")
        .args(["get-sink-mute", "@DEFAULT_SINK@"])
        .output()
        .ok()?;

    let vol_str = String::from_utf8_lossy(&vol.stdout);
    let mute_str = String::from_utf8_lossy(&mute.stdout);

    // "Volume: front-left: 45875 /  70% / -8.58 dB, ..."
    let pct: u8 = vol_str
        .split('/')
        .nth(1)?
        .trim()
        .trim_end_matches('%')
        .trim()
        .parse()
        .ok()?;

    let muted = mute_str.contains(": yes");

    Some(OsdEvent::Volume { pct, muted })
}

fn brightness_watcher(tx: mpsc::Sender<OsdEvent>) {
    use std::fs;

    let base = match fs::read_dir("/sys/class/backlight")
        .ok()
        .and_then(|mut d| d.next())
        .and_then(|e| e.ok())
        .map(|e| e.path())
    {
        Some(p) => p,
        None => return,
    };

    let bright_path = base.join("brightness");
    let max_path = base.join("max_brightness");

    let max: u64 = match fs::read_to_string(&max_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
    {
        Some(v) if v > 0 => v,
        _ => return,
    };

    // Initialize to current value so startup doesn't trigger OSD.
    let mut last: u64 = fs::read_to_string(&bright_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(u64::MAX);

    loop {
        if let Some(val) = fs::read_to_string(&bright_path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
        {
            if val != last {
                last = val;
                let pct = ((val * 100) / max).min(100) as u8;
                let _ = tx.blocking_send(OsdEvent::Brightness { pct });
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

async fn run_osd(window: gtk4::Window, mut rx: mpsc::Receiver<OsdEvent>) {
    let container = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    container.set_margin_top(10);
    container.set_margin_bottom(10);
    container.set_margin_start(14);
    container.set_margin_end(14);
    window.set_child(Some(&container));

    let icon = crate::svg_image(crate::bar::stats::ICON_VOLUME);
    icon.add_css_class("osd-icon");
    container.append(&icon);

    let pbar = gtk4::ProgressBar::new();
    pbar.add_css_class("osd-bar");
    pbar.set_hexpand(true);
    pbar.set_valign(gtk4::Align::Center);
    container.append(&pbar);

    let dismiss_token = Rc::new(Cell::new(0u32));
    // OSD fill overshoot (ANIMATION WORK #5): current fraction (as a whole
    // percent, matching `anim::spring_to`'s `i32` interpolation), the
    // in-flight tick callback (so a fast double-tap of volume-up cancels the
    // previous run instead of fighting it), and a generation token so a
    // superseded run's queued second leg (see `animate_osd_fill` below)
    // never applies after a newer event has already taken over.
    let fill_pct = Rc::new(Cell::new(0i32));
    let fill_anim: Rc<RefCell<Option<gtk4::TickCallbackId>>> = Rc::new(RefCell::new(None));
    let fill_token = Rc::new(Cell::new(0u32));

    while let Some(event) = rx.recv().await {
        let (icon_svg, pct, muted) = match event {
            OsdEvent::Volume { pct, muted } => (crate::bar::stats::ICON_VOLUME, pct, muted),
            OsdEvent::Brightness { pct } => (crate::bar::stats::ICON_BRIGHTNESS, pct, false),
        };

        icon.set_paintable(Some(&crate::svg_texture(icon_svg)));
        crate::prepare_icon(&icon, crate::theme::shell_theme().tokens().icon_px() as i32);
        if muted {
            icon.add_css_class("osd-icon-muted");
        } else {
            icon.remove_css_class("osd-icon-muted");
        }
        animate_osd_fill(&pbar, &fill_pct, &fill_anim, &fill_token, pct);
        window.set_visible(true);

        let token = dismiss_token.get().wrapping_add(1);
        dismiss_token.set(token);
        let dtok = dismiss_token.clone();
        let win = window.clone();
        relm4::spawn_local(async move {
            gtk4::glib::timeout_future(Duration::from_millis(2000)).await;
            if dtok.get() == token {
                win.set_visible(false);
            }
        });
    }
}

/// How many percentage points the fill runs past its real target before
/// easing back — subtle, matching the rest of the theme's "spring" motion
/// rather than a dramatic bounce.
const OSD_OVERSHOOT_PCT: i32 = 4;
/// Leg 1 (toward the overshoot point) / leg 2 (settling back onto the real
/// target) durations. Two legs, not one: `anim::spring_to` clamps every
/// frame to `[min(from, to), max(from, to)]` (see its own doc comment and
/// tests) specifically so a caller like the capsule drawer can never end up
/// with a negative size request — which also means a single `spring_to`
/// call can *never* visibly overshoot `to`, no matter how much the
/// underlying curve wants to. Chaining two calls — first to a point past
/// the target, then back onto it — is what actually produces the overshoot.
const OSD_LEG1_MS: f64 = 160.0;
const OSD_LEG2_MS: f64 = 200.0;

/// Animates `pbar`'s fraction from wherever `current` says it currently is
/// to `target`, overshooting slightly past it and settling back — see the
/// constants above for why this takes two `spring_to` legs instead of one.
/// Cancels any run already in flight (a fast double volume-step must
/// continue from the current visual position, not fight or restart it) and
/// stamps a fresh generation token so a superseded run's leg 2, queued via
/// `timeout_add_local_once` below, is a no-op if it fires after a newer
/// call has already taken over.
fn animate_osd_fill(
    pbar: &gtk4::ProgressBar,
    current: &Rc<Cell<i32>>,
    anim: &Rc<RefCell<Option<gtk4::TickCallbackId>>>,
    token: &Rc<Cell<u32>>,
    target: u8,
) {
    if let Some(id) = anim.borrow_mut().take() {
        id.remove();
    }
    let my_token = token.get().wrapping_add(1);
    token.set(my_token);

    let from = current.get();
    let to = i32::from(target);
    if from == to {
        pbar.set_fraction(f64::from(to) / 100.0);
        return;
    }
    let overshoot = if to > from {
        (to + OSD_OVERSHOOT_PCT).min(100)
    } else {
        (to - OSD_OVERSHOOT_PCT).max(0)
    };

    let leg1_bar = pbar.clone();
    let leg1_current = current.clone();
    let id = bread_theme::anim::spring_to(pbar, from, overshoot, OSD_LEG1_MS, move |v| {
        leg1_current.set(v);
        leg1_bar.set_fraction(f64::from(v) / 100.0);
    });
    *anim.borrow_mut() = Some(id);

    let leg2_bar = pbar.clone();
    let leg2_current = current.clone();
    let leg2_anim = anim.clone();
    let leg2_token = token.clone();
    gtk4::glib::timeout_add_local_once(Duration::from_millis(OSD_LEG1_MS as u64), move || {
        if leg2_token.get() != my_token {
            return; // superseded by a newer OSD event before leg 1 finished
        }
        let inner_bar = leg2_bar.clone();
        let id = bread_theme::anim::spring_to(&leg2_bar, overshoot, to, OSD_LEG2_MS, move |v| {
            leg2_current.set(v);
            inner_bar.set_fraction(f64::from(v) / 100.0);
        });
        *leg2_anim.borrow_mut() = Some(id);
    });
}

fn create_window() -> gtk4::Window {
    let window = gtk4::Window::new();
    window.add_css_class("breadbar-osd");
    window.init_layer_shell();
    window.set_namespace(Some("breadbar-osd"));
    crate::surface::apply(&window, "breadbar-osd");
    crate::theme::bind_auto(&window);
    window
}

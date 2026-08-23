use std::{cell::Cell, rc::Rc, time::Duration};

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
        pbar.set_fraction(pct as f64 / 100.0);
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

fn create_window() -> gtk4::Window {
    let window = gtk4::Window::new();
    window.add_css_class("breadbar-osd");
    window.init_layer_shell();
    window.set_namespace(Some("breadbar-osd"));
    crate::surface::apply(&window, "breadbar-osd");
    crate::theme::bind_auto(&window);
    window
}

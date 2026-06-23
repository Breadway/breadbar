use std::{cell::Cell, rc::Rc, time::Duration};

use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use tokio::sync::mpsc;

enum OsdEvent {
    Volume { pct: u8, muted: bool },
    Brightness { pct: u8 },
}

pub fn spawn() {
    let (tx, rx) = mpsc::channel::<OsdEvent>(8);

    let tx1 = tx.clone();
    std::thread::spawn(move || volume_watcher(tx1));
    std::thread::spawn(move || brightness_watcher(tx));

    relm4::spawn_local(run_osd(rx));
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

async fn run_osd(mut rx: mpsc::Receiver<OsdEvent>) {
    let window = create_window();

    let container = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    container.set_margin_top(12);
    container.set_margin_bottom(12);
    container.set_margin_start(16);
    container.set_margin_end(16);
    window.set_child(Some(&container));

    let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    let kind_lbl = gtk4::Label::new(Some("Volume"));
    kind_lbl.add_css_class("osd-kind");
    kind_lbl.set_hexpand(true);
    kind_lbl.set_xalign(0.0);
    let pct_lbl = gtk4::Label::new(Some("0%"));
    pct_lbl.add_css_class("osd-pct");
    header.append(&kind_lbl);
    header.append(&pct_lbl);
    container.append(&header);

    let pbar = gtk4::ProgressBar::new();
    pbar.add_css_class("osd-bar");
    container.append(&pbar);

    let dismiss_token = Rc::new(Cell::new(0u32));

    while let Some(event) = rx.recv().await {
        let (kind, pct) = match event {
            OsdEvent::Volume { pct, muted } => {
                (if muted { "Volume (Muted)" } else { "Volume" }, pct)
            }
            OsdEvent::Brightness { pct } => ("Brightness", pct),
        };

        kind_lbl.set_label(kind);
        pct_lbl.set_label(&format!("{pct}%"));
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
    window.set_layer(Layer::Overlay);
    window.set_anchor(Edge::Bottom, true);
    window.set_margin(Edge::Bottom, 80);
    window.set_default_width(280);
    window
}

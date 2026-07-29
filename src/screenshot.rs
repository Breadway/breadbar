//! `--screenshot` CLI mode: render a specific view, capture it via
//! `bread-screenshots`, then exit — driven by `bread-ecosystem`'s
//! `bread-capture` orchestrator, or run standalone for one-off captures.
//!
//! Capture waits on GTK's `map` signal rather than a blind sleep before
//! grabbing pixels — the surface (or, for popover views, the popover itself)
//! genuinely isn't on screen yet before that fires, so a fixed delay would
//! either race a slow first paint or pad every fast one for nothing.
//!
//! breadbar is "a bar + the notification daemon + the OSD" (see its own
//! module docs), so its screenshot views span three separate top-level
//! surfaces, not just the bar: the bar itself and its popovers (this
//! module, anchored off `root`), plus the standalone notification and OSD
//! windows (`notifications::spawn`/`osd::spawn`, built and primed with
//! sample data by `main.rs` before `dispatch` runs — see [`Handles`]).

use clap::Parser;
use gtk4::prelude::*;
use std::path::PathBuf;
use std::time::Duration;

/// Extra settle time after `map` for the first frame to actually paint
/// before grim runs — `map` fires once the surface exists, not once
/// anything has been drawn into it.
const SETTLE_DELAY: Duration = Duration::from_millis(300);

/// Settle time for views whose content depends on `bar::stats::spawn_poller`'s
/// 2-second background loop (control-panel's CPU/RAM/PWR/GPU/network labels,
/// gated on popover visibility) or a similar live-data popover load
/// (connectivity's wifi/bluetooth scan) — capturing any sooner leaves
/// placeholder dashes/"Scanning…" instead of real content.
const LIVE_DATA_SETTLE_DELAY: Duration = Duration::from_millis(2_200);

/// Delay between the bar's own `map` and calling `popover.popup()`. Calling
/// `popup()` synchronously from inside the root window's `map` handler
/// produces a popover that reports itself `map`ped but never actually paints
/// (confirmed by an independent `grim` capture taken mid-sequence, showing no
/// popover at all) — presumably the parent widget's own allocation isn't
/// settled yet at that exact point. Giving the initial layout pass a beat to
/// finish first is what makes it actually render.
const PRE_POPUP_DELAY: Duration = Duration::from_millis(300);

const KNOWN_VIEWS: &[&str] = &[
    "bar",
    "control-panel",
    "connectivity-wifi",
    "connectivity-bluetooth",
    "media-popover",
    "notification",
    "notification-critical",
    "osd-volume",
    "osd-brightness",
    "wifi-add-dialog",
];

#[derive(Parser)]
#[command(name = "breadbar")]
pub struct Cli {
    /// Render the named view, capture it, then exit instead of running
    /// normally. See `screenshot::KNOWN_VIEWS` for the full list.
    #[arg(long)]
    pub screenshot: Option<String>,

    /// PNG path to write the capture to. Required together with --screenshot.
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Capture canvas width — matches the isolated compositor's output width
    /// (`bread-capture --isolate-width`) so the geometry passed to `grim`
    /// doesn't depend on querying anything at capture time.
    #[arg(long, default_value_t = 1920)]
    pub width: u32,

    /// Capture canvas height — see `width`.
    #[arg(long, default_value_t = 1080)]
    pub height: u32,
}

pub struct ScreenshotRequest {
    pub view: String,
    pub output: PathBuf,
    pub width: u32,
    pub height: u32,
}

impl Cli {
    /// `None` for a normal run. Exits the process with an error if
    /// `--screenshot` was given without `--output`, before any GTK/relm4
    /// setup happens.
    pub fn screenshot_request(&self) -> Option<ScreenshotRequest> {
        let view = self.screenshot.clone()?;
        let Some(output) = self.output.clone() else {
            eprintln!("breadbar: --screenshot requires --output");
            std::process::exit(1);
        };
        Some(ScreenshotRequest { view, output, width: self.width, height: self.height })
    }
}

/// Every widget/window `dispatch` might need, gathered by `main.rs`'s
/// `init()` — most of these are plain locals there that never otherwise
/// outlive `init()` (never stored on `App`), so they have to be cloned out
/// before dispatch time same as `control_popover` always was.
pub struct Handles {
    pub control_popover: gtk4::Popover,
    pub connectivity_popover: gtk4::Popover,
    pub wifi_tab_btn: gtk4::ToggleButton,
    pub bt_tab_btn: gtk4::ToggleButton,
    pub media_popover: gtk4::Popover,
    pub media_widget: gtk4::Box,
    pub media_track_lbl: gtk4::Label,
    /// Already built and primed with sample content by `main.rs` (via
    /// `notifications::spawn(Some(kind))`) when `req.view` calls for it —
    /// `None` otherwise.
    pub notification_window: Option<gtk4::Window>,
    /// Same deal as `notification_window`, via `osd::spawn(Some(kind))`.
    pub osd_window: Option<gtk4::Window>,
}

/// The bar's fixed height — matches `root.set_exclusive_zone(32)` /
/// `set_default_height: 32` in `main.rs`. Unlike the other views' full
/// canvas, this never varies with `--width`/`--height`.
const BAR_HEIGHT: i32 = 32;

pub fn dispatch(root: &gtk4::ApplicationWindow, req: ScreenshotRequest, handles: Handles) {
    let output = req.output;
    let (width, height) = (req.width as i32, req.height as i32);

    match req.view.as_str() {
        "bar" => {
            root.connect_map(move |_| {
                let output = output.clone();
                gtk4::glib::timeout_add_local_once(SETTLE_DELAY, move || {
                    finish(bread_screenshots::capture_region(0, 0, width, BAR_HEIGHT, &output));
                });
            });
        }
        "control-panel" => {
            open_popover_on_root_map(root, handles.control_popover, LIVE_DATA_SETTLE_DELAY, output, width, height);
        }
        "connectivity-wifi" => {
            handles.wifi_tab_btn.set_active(true);
            open_popover_on_root_map(root, handles.connectivity_popover, LIVE_DATA_SETTLE_DELAY, output, width, height);
        }
        "connectivity-bluetooth" => {
            handles.bt_tab_btn.set_active(true);
            open_popover_on_root_map(root, handles.connectivity_popover, LIVE_DATA_SETTLE_DELAY, output, width, height);
        }
        "media-popover" => {
            // Real media state only shows the widget/text when something's
            // actually playing (see AppInput::MediaUpdate) — an automated
            // run has nothing playing, so fake enough of it directly on the
            // widgets to get a representative capture.
            handles.media_widget.set_visible(true);
            handles.media_track_lbl.set_text("Sample Track — Sample Artist");
            open_popover_on_root_map(root, handles.media_popover, SETTLE_DELAY, output, width, height);
        }
        "notification" | "notification-critical" => {
            let Some(window) = handles.notification_window else {
                eprintln!("breadbar: internal error — no notification window built for '{}'", req.view);
                std::process::exit(1);
            };
            capture_standalone_window(window, output, width, height);
        }
        "osd-volume" | "osd-brightness" => {
            let Some(window) = handles.osd_window else {
                eprintln!("breadbar: internal error — no OSD window built for '{}'", req.view);
                std::process::exit(1);
            };
            capture_standalone_window(window, output, width, height);
        }
        "wifi-add-dialog" => {
            let anchor = handles.wifi_tab_btn;
            root.connect_map(move |_| {
                let output = output.clone();
                let anchor = anchor.clone();
                gtk4::glib::timeout_add_local_once(PRE_POPUP_DELAY, move || {
                    crate::show_add_network_dialog(&anchor, "Sample Network".to_string(), move |dialog| {
                        capture_standalone_window(dialog.clone(), output.clone(), width, height);
                    });
                });
            });
        }
        other => {
            eprintln!(
                "breadbar: unknown screenshot view '{other}' (known: {})",
                KNOWN_VIEWS.join(", ")
            );
            std::process::exit(1);
        }
    }
}

/// Shared shape for every popover view: force it open shortly after the bar
/// maps (autohide disabled — a programmatic `popup()` has no real input
/// event serial to grab the Wayland seat with), then capture the whole
/// canvas after `settle` once the popover itself maps.
fn open_popover_on_root_map(
    root: &gtk4::ApplicationWindow,
    popover: gtk4::Popover,
    settle: Duration,
    output: PathBuf,
    width: i32,
    height: i32,
) {
    let popover_to_open = popover.clone();
    root.connect_map(move |_| {
        popover_to_open.set_autohide(false);
        let popover_to_open = popover_to_open.clone();
        gtk4::glib::timeout_add_local_once(PRE_POPUP_DELAY, move || {
            popover_to_open.popup();
        });
    });
    popover.connect_map(move |_| {
        let output = output.clone();
        gtk4::glib::timeout_add_local_once(settle, move || {
            finish(bread_screenshots::capture_region(0, 0, width, height, &output));
        });
    });
}

/// Shared shape for the standalone notification/OSD windows and the wifi
/// add-network dialog: wait for `map`, settle, capture, exit. These are
/// already-visible-or-about-to-be windows by the time this is called (their
/// sample event is queued before `dispatch` even runs), so this is just the
/// capture half.
fn capture_standalone_window(window: gtk4::Window, output: PathBuf, width: i32, height: i32) {
    window.connect_map(move |_| {
        let output = output.clone();
        gtk4::glib::timeout_add_local_once(SETTLE_DELAY, move || {
            finish(bread_screenshots::capture_region(0, 0, width, height, &output));
        });
    });
}

fn finish(result: anyhow::Result<()>) {
    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("breadbar: screenshot capture failed: {e}");
            std::process::exit(1);
        }
    }
}

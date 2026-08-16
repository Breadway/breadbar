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

use bread_utils::screenshot_cli::{validate_pair, DEFAULT_HEIGHT, DEFAULT_WIDTH, SETTLE_DELAY};
use clap::Parser;
use gtk4::prelude::*;
use std::path::PathBuf;
use std::time::Duration;

/// Settle time for views whose content depends on a live-data popover load
/// (connectivity's wifi/bluetooth scan, control-panel sliders) — capturing
/// any sooner leaves placeholder dashes/"Scanning…" instead of real content.
const LIVE_DATA_SETTLE_DELAY: Duration = Duration::from_millis(2_200);

/// Delay between the bar's own `map` and calling `popover.popup()`. Calling
/// `popup()` synchronously from inside the root window's `map` handler
/// produces a popover that reports itself `map`ped but never actually paints
/// (confirmed by an independent `grim` capture taken mid-sequence, showing no
/// popover at all) — presumably the parent widget's own allocation isn't
/// settled yet at that exact point. Giving the initial layout pass a beat to
/// finish first is what makes it actually render.
const PRE_POPUP_DELAY: Duration = SETTLE_DELAY;

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
    #[arg(long, default_value_t = DEFAULT_WIDTH)]
    pub width: u32,

    /// Capture canvas height — see `width`.
    #[arg(long, default_value_t = DEFAULT_HEIGHT)]
    pub height: u32,

    /// Toggle the in-memory notification history on a running breadbar, then
    /// exit. Keybind-friendly; does not start a second instance.
    #[arg(long)]
    pub history: bool,
}

pub struct ScreenshotRequest {
    pub view: String,
    pub output: PathBuf,
    pub width: u32,
    pub height: u32,
}

impl Cli {
    /// `None` for a normal run. Exits the process with an error if the
    /// `--screenshot` / `--output` pair is incomplete, before any GTK/relm4
    /// setup happens.
    pub fn screenshot_request(&self) -> Option<ScreenshotRequest> {
        if let Err(e) = validate_pair(self.screenshot.as_deref(), self.output.as_deref()) {
            eprintln!("breadbar: {e}");
            std::process::exit(1);
        }
        Some(ScreenshotRequest {
            view: self.screenshot.clone()?,
            output: self.output.clone()?,
            width: self.width,
            height: self.height,
        })
    }
}

/// Every widget/window `dispatch` might need, gathered by `main.rs`'s
/// `init()` — most of these are plain locals there that never otherwise
/// outlive `init()` (never stored on `App`), so they have to be cloned out
/// before dispatch time same as `control_popover` always was.
pub struct Handles {
    pub control_panel: gtk4::Window,
    pub connectivity_panel: gtk4::Window,
    pub wifi_tab_btn: gtk4::ToggleButton,
    pub bt_tab_btn: gtk4::ToggleButton,
    pub media_panel: gtk4::Window,
    pub media_widget: gtk4::Box,
    pub media_track_lbl: gtk4::Label,
    /// Already built and primed with sample content by `main.rs` (via
    /// `notifications::spawn(Some(kind))`) when `req.view` calls for it —
    /// `None` otherwise.
    pub notification_window: Option<gtk4::Window>,
    /// Same deal as `notification_window`, via `osd::spawn(Some(kind))`.
    pub osd_window: Option<gtk4::Window>,
}

/// Capture height for the `bar` view: layer-shell top margin + widget
/// height (the exclusive zone). Unlike the other views' full canvas,
/// this never varies with `--width`/`--height`.
const BAR_HEIGHT: i32 = crate::BAR_HEIGHT + crate::BAR_MARGIN_TOP;

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
            open_panel_on_root_map(root, handles.control_panel, LIVE_DATA_SETTLE_DELAY, output, width, height);
        }
        "connectivity-wifi" => {
            handles.wifi_tab_btn.set_active(true);
            open_panel_on_root_map(root, handles.connectivity_panel, LIVE_DATA_SETTLE_DELAY, output, width, height);
        }
        "connectivity-bluetooth" => {
            handles.bt_tab_btn.set_active(true);
            open_panel_on_root_map(root, handles.connectivity_panel, LIVE_DATA_SETTLE_DELAY, output, width, height);
        }
        "media-popover" => {
            // Real media state only shows the widget/text when something's
            // actually playing (see AppInput::MediaUpdate) — an automated
            // run has nothing playing, so fake enough of it directly on the
            // widgets to get a representative capture.
            handles.media_widget.set_visible(true);
            handles.media_widget.add_css_class("playing");
            handles.media_track_lbl.set_text("Sample Track — Sample Artist");
            open_panel_on_root_map(root, handles.media_panel, SETTLE_DELAY, output, width, height);
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

/// Shared shape for panel views: present the standalone layer window after
/// the bar maps, then capture the canvas once the panel itself maps.
fn open_panel_on_root_map(
    root: &gtk4::ApplicationWindow,
    panel: gtk4::Window,
    settle: Duration,
    output: PathBuf,
    width: i32,
    height: i32,
) {
    let panel_to_open = panel.clone();
    root.connect_map(move |_| {
        let panel_to_open = panel_to_open.clone();
        gtk4::glib::timeout_add_local_once(PRE_POPUP_DELAY, move || {
            panel_to_open.set_visible(true);
            panel_to_open.present();
        });
    });
    panel.connect_map(move |_| {
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

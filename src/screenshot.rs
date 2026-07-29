//! `--screenshot` CLI mode: render a specific view, capture it via
//! `bread-screenshots`, then exit — driven by `bread-ecosystem`'s
//! `bread-capture` orchestrator, or run standalone for one-off captures.
//!
//! Capture waits on GTK's `map` signal rather than a blind sleep before
//! grabbing pixels — the surface (or, for popover views, the popover itself)
//! genuinely isn't on screen yet before that fires, so a fixed delay would
//! either race a slow first paint or pad every fast one for nothing.

use clap::Parser;
use gtk4::prelude::*;
use std::path::PathBuf;
use std::time::Duration;

/// Extra settle time after `map` for the first frame to actually paint
/// before grim runs — `map` fires once the surface exists, not once
/// anything has been drawn into it.
const SETTLE_DELAY: Duration = Duration::from_millis(300);

/// Settle time for the control-panel view specifically: longer than
/// [`SETTLE_DELAY`] because the CPU/RAM/PWR/GPU/network labels there aren't
/// populated by the popover's own load (that only covers volume/brightness/
/// sinks, see `bar::control::spawn_load`) — they're refreshed by
/// `bar::stats::spawn_poller`'s 2-second background loop, gated on
/// `control_popover.is_visible()` at each tick. Capturing any sooner than one
/// full poll interval after the popover opens leaves them at their initial
/// placeholder dashes.
const CONTROL_PANEL_SETTLE_DELAY: Duration = Duration::from_millis(2_200);

/// Delay between the bar's own `map` and calling `popover.popup()`. Calling
/// `popup()` synchronously from inside the root window's `map` handler
/// produces a popover that reports itself `map`ped but never actually paints
/// (confirmed by an independent `grim` capture taken mid-sequence, showing no
/// popover at all) — presumably the parent widget's own allocation isn't
/// settled yet at that exact point. Giving the initial layout pass a beat to
/// finish first is what makes it actually render.
const PRE_POPUP_DELAY: Duration = Duration::from_millis(300);

#[derive(Parser)]
#[command(name = "breadbar")]
pub struct Cli {
    /// Render the named view, capture it, then exit instead of running
    /// normally. Known views: "bar", "control-panel".
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

/// Wire up the given view's screenshot sequence. Called once from `init()`,
/// after the window and its popovers exist but before the component finishes
/// initializing — every path here ends by exiting the process, it never
/// returns control to the normal bar UI.
/// The bar's fixed height — matches `root.set_exclusive_zone(32)` /
/// `set_default_height: 32` in `main.rs`. Unlike the control-panel's full
/// canvas, this never varies with `--width`/`--height`.
const BAR_HEIGHT: i32 = 32;

pub fn dispatch(root: &gtk4::ApplicationWindow, req: ScreenshotRequest, control_popover: gtk4::Popover) {
    match req.view.as_str() {
        "bar" => {
            let output = req.output;
            let width = req.width as i32;
            root.connect_map(move |_| {
                let output = output.clone();
                gtk4::glib::timeout_add_local_once(SETTLE_DELAY, move || {
                    finish(bread_screenshots::capture_region(0, 0, width, BAR_HEIGHT, &output));
                });
            });
        }
        "control-panel" => {
            let output = req.output;
            let (width, height) = (req.width as i32, req.height as i32);
            let popover_to_open = control_popover.clone();
            root.connect_map(move |_| {
                // Autohide (the default) tries to grab the Wayland seat on
                // popup, keyed to a real input event's serial — a
                // programmatic popup() has no such event to grab with.
                // Screenshot mode never needs the popover to dismiss itself
                // anyway.
                popover_to_open.set_autohide(false);
                let popover_to_open = popover_to_open.clone();
                gtk4::glib::timeout_add_local_once(PRE_POPUP_DELAY, move || {
                    popover_to_open.popup();
                });
            });
            control_popover.connect_map(move |_| {
                let output = output.clone();
                gtk4::glib::timeout_add_local_once(CONTROL_PANEL_SETTLE_DELAY, move || {
                    finish(bread_screenshots::capture_region(0, 0, width, height, &output));
                });
            });
        }
        other => {
            eprintln!("breadbar: unknown screenshot view '{other}' (known: bar, control-panel)");
            std::process::exit(1);
        }
    }
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

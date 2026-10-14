macro_rules! asset {
    ($n:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/", $n))
    };
}

mod bar;
mod launcher_command;
mod motion;
mod notifications;
mod osd;
mod panel;
mod pill_autohide;
mod pill_hotzone;
mod screenshot;
mod spotlight_prefs;
mod stage;
mod surface;
mod surtitle;
mod theme;
mod theme_widgets;
mod widgets;

use bread_launcher::gtk::ResultsList;
use bread_theme::shell::{
    ClockStyle, Exclusive, Keyboard, Width, WindowOrientation, WorkspaceDisplay, WorkspaceStyle,
};
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use hyprland::data::Workspace;
use hyprland::shared::{Address, WorkspaceId};
use relm4::prelude::*;
use relm4::{Component, ComponentController, Controller};
use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;

/// The launched-app event's publisher id and event name: deliberately
/// matching breadbox's OWN constants (`breadbox/src/main.rs`: `APP_ID =
/// "box"`, `LAUNCHED_EVENT = "bread.box.launched"`), NOT breadbar's own
/// `widgets::client::APP_ID` ("bar"). Theme 04/spotlight's capsule IS the
/// launcher wearing a different shell (plan §7), sharing breadbox's cache
/// and history via `bread_launcher::LAUNCHER_APP`: it publishes under that
/// same launcher identity too, so anything downstream listening for "an app
/// was launched via the launcher" sees one event stream regardless of which
/// surface launched it.
const LAUNCHER_APP_ID: &str = "box";
const LAUNCHER_LAUNCHED_EVENT: &str = "bread.box.launched";

/// Diagnostic trace for the capsule's open/close/focus wiring, gated
/// behind an env var so it costs nothing by default. Three previous fixes
/// for the "opens itself at startup with no focus" bug all failed because
/// nothing could observe which path actually fired on a real desktop:
/// this makes that observable: `BREADBAR_CAPSULE_DEBUG=1 breadbar` prints
/// every call to `open_fn`/`close_fn` and every signal that could lead to
/// one, with enough context (source, open state) to tell a legitimate
/// user-driven open from an accidental one after the fact.
fn capsule_debug_enabled() -> bool {
    std::env::var_os("BREADBAR_CAPSULE_DEBUG").is_some()
}

macro_rules! capsule_trace {
    ($($arg:tt)*) => {
        if capsule_debug_enabled() {
            eprintln!("[capsule] {}", format!($($arg)*));
        }
    };
}

/// The drawer's own content-height ceiling (`04-spotlight.html`: `.searching
/// .results { max-height: 420px }`); see [`drawer_target_height`]. Also
/// used to size the click-away scrim's dead zone (see `open_fn` in `init`):
/// the scrim's clickable region never reaches higher than the capsule row
/// plus this much, so it can never overlap a real result row regardless of
/// how tall the drawer currently is.
const DRAWER_MAX_HEIGHT_PX: i32 = 420;

/// `bar.window.exclusive` resolved to a layer-shell exclusive-zone pixel
/// value (axis 2, daylight). `Exclusive::Auto` reserves `height` plus the
/// margin on whichever edge the bar is actually anchored to: `margin.top`
/// for every theme through spotlight (all top-anchored), `margin.bottom`
/// for daylight's bottom-anchored dock. Before daylight this always read
/// `margin.top` unconditionally, which happened to be correct for every
/// existing theme (a bottom-anchored bar had never shipped, so
/// `margin.bottom` was always 0 and never observed) but reserved 12px too
/// little for daylight (`height` alone, not `height + margin.bottom`):
/// tiled windows would have run up under the last 12px of the floating
/// dock. Extracted to a pure function so this is unit-testable without a
/// live layer-shell surface.
fn exclusive_zone_for(spec: &bread_theme::shell::WindowSpec) -> i32 {
    match spec.orientation() {
        WindowOrientation::Horizontal => {
            let anchor_margin = if spec.anchors.iter().any(|a| a == "bottom") {
                spec.margin.bottom
            } else {
                spec.margin.top
            };
            match spec.exclusive {
                Exclusive::Auto => spec.height + anchor_margin,
                Exclusive::None => -1,
                Exclusive::Px(px) => px,
            }
        }
        // Side dock: the reserved dimension is the bar's own thickness
        // (`width`, validated `Px`-only for a vertical anchor set, see
        // `resolve_window`) plus whichever side margin faces the desktop,
        // not `height`/top-or-bottom, which mean nothing on this axis.
        WindowOrientation::Vertical => {
            let anchor_margin = if spec.anchors.iter().any(|a| a == "right") {
                spec.margin.right
            } else {
                spec.margin.left
            };
            match spec.exclusive {
                Exclusive::Auto => {
                    let thickness = match spec.width {
                        Width::Px(px) => px,
                        Width::Fill => 0,
                    };
                    thickness + anchor_margin
                }
                Exclusive::None => -1,
                Exclusive::Px(px) => px,
            }
        }
    }
}

/// Apply a [`bread_theme::shell::WindowSpec`] to the bar's layer-shell surface:
/// anchors, margins, exclusive zone, width, keyboard mode, layer. `init`
/// calls this once; the live theme-switch path ([`App::rebuild_from_theme`])
/// calls it again. Every `gtk4-layer-shell` setter here is live-capable, so a
/// geometry-only theme change needs no restart. The bar-entrance animation is
/// deliberately not here: it is a first-paint effect owned by `init` alone.
fn apply_window_spec<W>(root: &W, spec: &bread_theme::shell::WindowSpec)
where
    W: IsA<gtk4::Window> + IsA<gtk4::Widget> + LayerShell,
{
    root.set_layer(if spec.layer == "overlay" {
        Layer::Overlay
    } else {
        Layer::Top
    });
    // Re-set every edge explicitly (true or false) so a switch from a 3-edge
    // bar to a 1-edge capsule actually releases the dropped edges.
    for (edge, name) in [
        (Edge::Top, "top"),
        (Edge::Bottom, "bottom"),
        (Edge::Left, "left"),
        (Edge::Right, "right"),
    ] {
        root.set_anchor(edge, spec.anchors.iter().any(|a| a == name));
    }
    root.set_margin(Edge::Top, spec.margin.top);
    root.set_margin(Edge::Left, spec.margin.left);
    root.set_margin(Edge::Right, spec.margin.right);
    root.set_margin(Edge::Bottom, spec.margin.bottom);
    match spec.width {
        Width::Px(px) => {
            root.set_default_width(px);
            root.set_size_request(px, -1);
        }
        // Release a previous capsule's pinned width so the surface stretches
        // to its anchored edges again.
        Width::Fill => root.set_size_request(-1, -1),
    }
    root.set_exclusive_zone(exclusive_zone_for(spec));
    root.set_keyboard_mode(match spec.keyboard {
        Keyboard::None => KeyboardMode::None,
        Keyboard::OnDemand => KeyboardMode::OnDemand,
        Keyboard::Exclusive => KeyboardMode::Exclusive,
    });
}

/// (Re)fill the bar's four slot containers from `[bar.slots]`. `init` runs
/// this once; the live theme-switch path runs it again (it clears each
/// container first). Module widgets are shared refs held by `modules`, so
/// emptying a container only unparents them; re-appending re-parents in the
/// new theme's order.
fn assemble_bar_slots(
    slots: &bread_theme::shell::Slots,
    modules: &bar::slots::ModuleRegistry,
    containers: &mut std::collections::HashMap<String, gtk4::Box>,
    workspace_row: &gtk4::Box,
    center_area: &gtk4::Box,
    stats_box: &gtk4::Box,
    drawer_box: &gtk4::Box,
) {
    for c in [workspace_row, center_area, stats_box, drawer_box] {
        while let Some(child) = c.first_child() {
            c.remove(&child);
        }
    }
    for (list, container) in [
        (&slots.left, workspace_row),
        (&slots.centre, center_area),
        (&slots.right, stats_box),
        (&slots.drawer, drawer_box),
    ] {
        modules.for_each_in_slot(
            list,
            |_, widget| container.append(widget),
            |key| container.append(&bar::slots::widget_slot_container(containers, key)),
        );
    }
}

#[cfg(test)]
mod exclusive_zone_tests {
    use super::exclusive_zone_for;
    use bread_theme::shell::{Exclusive, Margin, Width, WindowSpec};

    fn spec(anchors: &[&str], height: i32, margin: Margin, exclusive: Exclusive) -> WindowSpec {
        WindowSpec {
            anchors: anchors.iter().map(|s| s.to_string()).collect(),
            width: Width::Fill,
            height,
            margin,
            exclusive,
            keyboard: bread_theme::shell::Keyboard::None,
            layer: "top".to_string(),
        }
    }

    #[test]
    fn top_anchored_auto_reserves_height_plus_top_margin() {
        let s = spec(
            &["top", "left", "right"],
            44,
            Margin {
                top: 12,
                left: 16,
                right: 16,
                bottom: 0,
            },
            Exclusive::Auto,
        );
        assert_eq!(exclusive_zone_for(&s), 56);
    }

    #[test]
    fn bottom_anchored_auto_reserves_height_plus_bottom_margin_not_top() {
        // The daylight case: margin.top is 0 (nothing reserved there), the
        // real floating gap is margin.bottom.
        let s = spec(
            &["bottom", "left", "right"],
            40,
            Margin {
                top: 0,
                left: 0,
                right: 0,
                bottom: 12,
            },
            Exclusive::Auto,
        );
        assert_eq!(exclusive_zone_for(&s), 52);
    }

    #[test]
    fn none_is_no_reservation_regardless_of_anchor() {
        let s = spec(&["bottom"], 40, Margin::default(), Exclusive::None);
        assert_eq!(exclusive_zone_for(&s), -1);
    }

    #[test]
    fn explicit_px_wins_over_computed_auto_value() {
        let s = spec(
            &["bottom", "left", "right"],
            40,
            Margin {
                bottom: 12,
                ..Margin::default()
            },
            Exclusive::Px(99),
        );
        assert_eq!(exclusive_zone_for(&s), 99);
    }

    fn vertical_spec(
        anchors: &[&str],
        width_px: i32,
        margin: Margin,
        exclusive: Exclusive,
    ) -> WindowSpec {
        WindowSpec {
            anchors: anchors.iter().map(|s| s.to_string()).collect(),
            width: Width::Px(width_px),
            // Meaningless on this axis (see `WindowSpec::orientation`);
            // an arbitrary, deliberately-not-width value would catch a
            // regression that read the wrong field.
            height: 999,
            margin,
            exclusive,
            keyboard: bread_theme::shell::Keyboard::None,
            layer: "top".to_string(),
        }
    }

    #[test]
    fn left_docked_auto_reserves_thickness_plus_left_margin() {
        let s = vertical_spec(
            &["left", "top", "bottom"],
            56,
            Margin {
                top: 0,
                left: 10,
                right: 0,
                bottom: 0,
            },
            Exclusive::Auto,
        );
        assert_eq!(exclusive_zone_for(&s), 66);
    }

    #[test]
    fn right_docked_auto_reserves_thickness_plus_right_margin_not_left() {
        let s = vertical_spec(
            &["right", "top", "bottom"],
            48,
            Margin {
                top: 0,
                left: 12,
                right: 8,
                bottom: 0,
            },
            Exclusive::Auto,
        );
        assert_eq!(exclusive_zone_for(&s), 56);
    }

    #[test]
    fn vertical_none_is_no_reservation() {
        let s = vertical_spec(
            &["left", "top", "bottom"],
            56,
            Margin::default(),
            Exclusive::None,
        );
        assert_eq!(exclusive_zone_for(&s), -1);
    }

    #[test]
    fn vertical_explicit_px_wins_over_computed_auto_value() {
        let s = vertical_spec(
            &["left", "top", "bottom"],
            56,
            Margin {
                left: 10,
                ..Margin::default()
            },
            Exclusive::Px(99),
        );
        assert_eq!(exclusive_zone_for(&s), 99);
    }
}

pub struct BarInit {
    pub screenshot: Option<screenshot::ScreenshotRequest>,
    pub monitor: Option<String>,
    pub primary: bool,
    /// The primary's shared stage state, for a satellite; `None` for the
    /// primary itself, which creates it.
    pub shared: Option<stage::Shared>,
}

pub struct App {
    monitor: String,
    primary: bool,
    satellites: Vec<(String, Controller<App>)>,

    // ── Workspaces ────────────────────────────────────────────────────────
    workspaces: Vec<Workspace>,
    active_ws: WorkspaceId,
    workspace_box: gtk4::Box,
    workspace_trail: bar::workspaces::WorkspaceTrail,
    button_map: std::collections::HashMap<WorkspaceId, gtk4::Button>,

    // ── Clock ─────────────────────────────────────────────────────────────
    time_str: String,
    clock_digits: Vec<gtk4::Label>,
    date_lbl: gtk4::Label,
    // `modules.clock.style = "plain"` (glass-workbench, Phase 5): a plain
    // "HH:MM" label with no per-digit flip. Built alongside `clock_digits`
    // regardless of the active theme's style so switching styles needs no
    // recompile; only one of the two ever lands in a `[bar.slots]` module
    // registration (see the "Assemble" section).
    clock_plain_lbl: gtk4::Label,
    // `modules.clock.placeholder_clock` (spotlight, theme 04): when set,
    // `AppInput::ClockTick` writes the time into this entry's placeholder
    // text instead of (or alongside) any clock label; see that handler.
    launcher_entry: gtk4::Entry,
    // Whether the capsule's drawer is currently expanded: read by
    // `ClockTick` so a live search in progress never has its placeholder
    // text stomped (it wouldn't be visible anyway once there's real text,
    // but matches the demo's own `if (!open) q.placeholder = t;` guard).
    launcher_open: Rc<Cell<bool>>,
    // `AppInput::OpenLauncher`'s local-route arm needs to actually open the
    // capsule, not just move keyboard focus onto `launcher_entry`: focus
    // alone stopped opening it when `connect_enter`'s `open_fn()` call was
    // removed (see that handler's own comment for why: it was the
    // mechanism behind the startup-open bug). This is `init`'s `open_fn`,
    // stored so the hotkey/command path can call it directly, the same way
    // the click gesture and `connect_changed` do at their own call sites.
    launcher_open_fn: Rc<dyn Fn()>,

    // ── Stats bar ─────────────────────────────────────────────────────────
    // Island chrome matches the Liquid Motion demo: volume / wifi / battery
    // / hamburger. CPU/RAM/power live in the control panel, not on the bar.
    // Held to keep the control-panel sys-grid widgets alive for the App's
    // lifetime; their parent boxes own them, so they're never read back.
    #[allow(dead_code)]
    system_stats_box: gtk4::Box,
    system_sep: gtk4::Separator,
    #[allow(dead_code)]
    cpu_pair: gtk4::Box,
    #[allow(dead_code)]
    mem_pair: gtk4::Box,
    #[allow(dead_code)]
    pwr_pair: gtk4::Box,
    gpu_pair: gtk4::Box,
    cpu_lbl: gtk4::Label,
    mem_lbl: gtk4::Label,
    pwr_lbl: gtk4::Label,
    // `[bar.slots].right = [..., "cpu", "ram", ...]` (glass-workbench, Phase
    // 5): separate instances from `cpu_pair`/`mem_pair` above, which stay
    // parented in the control panel's sys-grid: a GTK widget can only have
    // one parent, so reusing those here would mean reparenting them out of
    // the panel, changing panel behaviour no theme asked to change. Fed by
    // the same `AppInput::StatsUpdate` data.
    bar_cpu_lbl: gtk4::Label,
    bar_ram_lbl: gtk4::Label,
    gpu_lbl: gtk4::Label,
    // ── breadram pressure ───────────────────────────────────────────────
    // The last daemon snapshot (or an absent one), plus the RAM rows in the
    // control panel's SYSTEM section. Updated by `AppInput::RamUpdate`;
    // never read back, only relabeled.
    ram_seen_at_ms: u64,
    ram_polls_since_digest: u32,
    ram_mem_base: String,
    ram_prev_pressure: bar::ram::Pressure,
    ram_prev_present: bool,
    /// Notifications (and the history writes behind them) only run live.
    /// Screenshot captures force sample snapshots instead, so a capture
    /// never tells the user anything and never pollutes their history.
    ram_live: bool,
    ram_notif: Option<notifications::NotifHandle>,
    ram_store: notifications::history::Store,
    ram_panel_status_lbl: gtk4::Label,
    ram_panel_mem_lbl: gtk4::Label,
    ram_pause_btn: gtk4::Button,
    ram_pause_lbl: gtk4::Label,
    /// Whether the daemon last reported monitor mode. Drives the
    /// pause/resume button label.
    ram_paused: bool,
    // Odometer-style digit chips (plan: "ODOMETER DIGITS FOR NUMERIC
    // CHIPS"), reuses the clock's `make_clock_digits`/`flip_clock_digits`
    // machinery (see `make_digit_chip`/`flip_digit_chip` below), so the
    // volume/battery numbers roll per-digit instead of snapping. `Rc<RefCell<..>>`,
    // not a plain `Vec`, because the label set is also mutated from the
    // control panel's volume-slider `connect_value_changed` closure (set up
    // in `init`, before `self` exists) as well as from `update`'s
    // `StatsUpdate` handler: both need to see and replace the same digit
    // set.
    vol_lbl: gtk4::Box,
    vol_digits: Rc<RefCell<Vec<gtk4::Label>>>,
    bat_lbl: gtk4::Box,
    bat_digits: Rc<RefCell<Vec<gtk4::Label>>>,
    bat_img: gtk4::Image,
    ac_img: gtk4::Image,
    bt_img: gtk4::Image,
    wifi_lbl: gtk4::Label,
    wifi_img: gtk4::Image,

    // ── WiFi popover ──────────────────────────────────────────────────────
    wifi_pane: gtk4::Box,
    crumbs_status: Option<bar::wifi::CrumbsStatus>,
    wifi_popover_data: Option<bar::wifi::WifiPopoverData>,
    wifi_profile: Option<String>,
    current_ssid: String,

    // ── Bluetooth popover ────────────────────────────────────────────────
    bt_pane: gtk4::Box,
    bt_popover_data: Option<bar::bluetooth::BtPopoverData>,

    // ── Media ─────────────────────────────────────────────────────────────
    media_widget: gtk4::Box,
    media_track_lbl: gtk4::Label,
    media_play_icon: gtk4::Image,
    media_last: Option<bar::media::MediaState>,
    media_paused_at: Option<std::time::Instant>,

    // ── Control panel ─────────────────────────────────────────────────────
    panel_vol_slider: gtk4::Scale,
    panel_bright_slider: gtk4::Scale,
    panel_loading: Rc<Cell<bool>>,
    sink_box: gtk4::Box,
    sink_section: gtk4::Box,

    // ── Tray ──────────────────────────────────────────────────────────────
    tray_section: gtk4::Box,
    tray_sep: gtk4::Separator,
    tray_box: gtk4::Box,
    tray_count_lbl: gtk4::Label,
    tray_items: std::collections::HashMap<String, gtk4::Button>,

    // ── Lua-declared widgets ─────────────────────────────────────────────
    // One container per `widget:<key>` slot entry (Phase 3b, see
    // bar::slots::ModuleRegistry and reconcile_widgets' routing below),
    // fully rebuilt on every AppInput::WidgetsUpdate; see widgets::client's
    // module doc for why that's simpler than incremental patching here.
    // Keyed by the slot entry's key: either a WidgetPlacement alias
    // (`right_of_workspaces`, `left_of_clock`, `right_of_clock`,
    // `left_of_stats`, `tray`) or a Lua module name. `bread_shared::widget`'s
    // `WidgetPlacement` itself never appears here: it's a wire type from
    // the bread daemon API and stays untouched.
    widget_containers: std::collections::HashMap<String, gtk4::Box>,
    /// The spotlight stage's settings menu (see `spotlight_prefs`); only
    /// consulted under the pill theme (`autohide_cfg.enabled`).
    prefs: spotlight_prefs::Prefs,
    /// Watches `spotlight.json` so a toggle on any monitor applies here.
    _prefs_monitor: Option<gtk4::gio::FileMonitor>,
    /// Widget ids already reported as undeliverable by `reconcile_widgets`, so
    /// the warning fires once per widget instead of once per reconcile.
    dropped_widget_warned: std::collections::HashSet<String>,
    widget_tray_section: gtk4::Box,
    widget_tray_sep: gtk4::Separator,

    // ── Widget sources (merged by `reconcile_all_widgets`) ───────────────
    /// Specs pushed by the `bread` daemon (`widgets/client.rs`).
    daemon_widget_specs: Vec<bread_shared::widget::WidgetSpec>,
    /// `[[bar.widget]]` from the active shell theme + each one's last poll.
    theme_widgets: Vec<bread_theme::shell::ThemeWidget>,
    theme_widget_values: std::collections::HashMap<String, String>,
    theme_widget_pollers: Vec<theme_widgets::PollHandle>,

    panels: panel::PanelSet,

    // ── Live theme switch ────────────────────────────────────────────────
    // Held so `AppInput::ShellThemeChanged` can re-apply the new theme's
    // `[bar.window]` geometry (`apply_window_spec`) and re-fill the slot
    // containers (`assemble_bar_slots`) without a restart. A workspace/clock
    // style or launcher-mode change still restarts: `theme::needs_restart`.
    root: gtk4::ApplicationWindow,
    bar_modules: bar::slots::ModuleRegistry,
    workspace_row: gtk4::Box,
    center_area: gtk4::Box,
    stats_box: gtk4::Box,
    drawer_box: gtk4::Box,

    // ── Spotlight pill (BAR-27 phase 1) ─────────────────────────────────
    /// `[bar.autohide]`, read once at `init` (a restart-gated table, same
    /// as the workspace/clock style: see `theme::needs_restart`).
    autohide_cfg: bread_theme::shell::AutoHide,
    /// The pointer is currently over the pill or its hot-zone.
    pill_hovering: bool,
    /// A timed peek (notification/level/workspace-switch) holds the pill
    /// down until this instant; `None` when nothing is pending.
    pill_hold_until: Option<std::time::Instant>,
    /// Bumped on every hover ENTER; a pending "leave" timeout (see
    /// `AppInput::PillHoverLeaveExpire`) captures the generation at the
    /// moment of the leave and only actually clears `pill_hovering` if
    /// nothing re-entered in between, the same debounce idiom `osd.rs`'s
    /// `dismiss_token` already uses.
    pill_hover_gen: u32,
    /// The pill's last-applied shown/tucked state, so `update_pill` only
    /// (re)starts the margin animation on an actual transition.
    pill_shown: bool,
    /// Stays down while true, regardless of hover/peek/timers (`bar.window`'s
    /// low-battery flag doubles as the auto-hide "stay down" signal).
    pill_low_battery: bool,
    /// The hot-zone/lip satellite window (see `pill_hotzone.rs`); `None`
    /// when `[bar.autohide].enabled` is false, so nothing extra is mapped
    /// for every builtin but spotlight. Held only to keep the window alive
    /// for the App's lifetime; never read back.
    #[allow(dead_code)]
    pill_hotzone: Option<gtk4::Window>,
    pill_lip: Option<gtk4::Widget>,
    /// `[bar.slots]`'s "flag"/"unread" modules: a low-battery flag and an
    /// unread-notification dot, both hidden by default and toggled by
    /// `AppInput::StatsUpdate`/a new notification respectively.
    pill_flag: gtk4::Box,
    pill_flag_label: gtk4::Label,
    pill_unread: gtk4::Box,
    /// BAR-38: the pill's now-playing title and do-not-disturb mark.
    pill_nowplaying: gtk4::Box,
    pill_nowplaying_title: gtk4::Label,
    pill_dnd: gtk4::Box,
    pill_battery: gtk4::Box,
    pill_battery_fill: gtk4::Box,
    pill_battery_pct: gtk4::Label,
    /// The last stats poll's battery: `None` on a machine without one.
    battery: Option<PillBattery>,
    /// The current player's `(title, playing)`, while there is one.
    nowplaying: Option<(String, bool)>,
    /// The workspaces/clock divider; collapses when `display = "none"`
    /// leaves the workspace strip empty.
    pill_sep: gtk4::Separator,
    /// The layer-shell top margin last applied (or mid-animation towards),
    /// so an interrupted show/tuck continues from where it visually is
    /// instead of jumping. `Rc<Cell<_>>` (not a plain field) because the
    /// running `motion::animate` tick callback also writes it every frame.
    pill_margin: Rc<Cell<i32>>,
    /// The in-flight margin animation, if any; cancelled before starting a
    /// new one, same pattern as `WorkspaceTrail::cancel`.
    pill_anim: Rc<RefCell<Option<gtk4::TickCallbackId>>>,
    /// BAR-30 phase 4: `[osd].style = "pill"`'s level row (a label, a bar,
    /// and a percentage), shown as an overlay covering the pill's own
    /// regular content for `bar.autohide.level_peek_ms`. `None` for every
    /// theme (built unconditionally like every other pill widget, but only
    /// ever shown when the active theme's `[osd].style` is "pill").
    pill_level_box: gtk4::Box,
    pill_level_name: gtk4::Label,
    pill_level_bar: gtk4::ProgressBar,
    pill_level_pct: gtk4::Label,
    /// Debounce token for the level display's auto-hide timer, same idiom
    /// as `osd.rs`'s `dismiss_token`.
    pill_level_token: Rc<Cell<u32>>,
    /// The level bar's in-flight width glide, if any.
    pill_level_anim: Rc<RefCell<Option<gtk4::TickCallbackId>>>,
    /// Pill items hidden while a level shows, to restore afterwards.
    pill_leveled: Rc<RefCell<Vec<gtk4::Widget>>>,

    // ── Spotlight stage (BAR-30 phases 2-4) ─────────────────────────────
    stage: Rc<stage::Stage>,
    /// BAR-30 phase 4: the surtitle caption window, shown instead of a
    /// toast when `[notifications].style = "surtitle"`.
    surtitle: surtitle::Surtitle,
    /// Do-not-disturb: shared with `Stage`'s own DND chip (same `Rc`, so a
    /// click there is immediately visible here with no message round-trip).
    /// Suppresses the surtitle/peek/unread-dot path for a new notification;
    /// the notification is still recorded to history regardless.
    dnd: Rc<Cell<bool>>,
    /// BAR-31: state shared by every bar instance (see `stage::Shared`).
    shared: stage::Shared,
}

#[derive(Debug)]
pub enum AppInput {
    WorkspaceSync {
        workspaces: Vec<Workspace>,
        actives: std::collections::HashMap<String, WorkspaceId>,
    },
    MonitorAdded(String),
    MonitorRemoved(String),
    ClockTick,
    StatsUpdate(bar::stats::Stats),
    TrayUpdate(bar::tray::TrayUpdate),
    CrumbsStatus(bar::wifi::CrumbsStatus),
    WifiPopoverData(bar::wifi::WifiPopoverData),
    SetProfile(String),
    BtPopoverData(bar::bluetooth::BtPopoverData),
    MediaUpdate(bar::media::MediaState),
    ControlPanelData(bar::control::ControlPanelData),
    /// A breadram status poll landed: pressure, mode, memory, and the
    /// newest-first action history.
    RamUpdate(bar::ram::RamSnapshot),
    /// A management pause or resume finished: whether it was asked for and
    /// whether the daemon accepted it.
    RamModeSet {
        paused: bool,
        ok: bool,
    },
    /// The control panel's pause/resume button was clicked. The handler
    /// reads the last reported mode and asks for the other one.
    RamPauseToggle,
    WidgetsUpdate(Vec<bread_shared::widget::WidgetSpec>),
    /// The primary's full widget list (daemon + theme widgets), forwarded to
    /// a satellite bar, which has no widget sources of its own.
    MirrorWidgets(Vec<bread_shared::widget::WidgetSpec>),
    /// One `[[bar.widget]]` poll landed: store its value and re-render.
    ThemeWidgetTick {
        id: String,
        value: String,
    },
    ReconcileMonitors,
    DismissPanels,
    /// The active shell theme changed on disk (`theme::watch_hot_reload`),
    /// and it is a CSS + geometry + slots change (a workspace/clock style or
    /// launcher-mode change restarts instead). Re-apply `[bar.window]` and
    /// re-fill the slot containers in place. The primary forwards this to
    /// every satellite.
    ShellThemeChanged(Box<bread_theme::shell::ShellTheme>),
    // `bread.command.box.open` (plan §7 phase 6c, `launcher_command`
    // module): only ever dispatched when the active theme's launcher is
    // `Embedded`: `launcher_command::spawn` never subscribes otherwise.
    // Focuses `launcher_entry` AND calls `self.launcher_open_fn()`
    // directly (its handler below): the hotkey/command itself is the
    // real-user-input signal, the same status a mouse click into the
    // entry has at its own call site. Deliberately does NOT rely on
    // `grab_focus()` triggering `EventControllerFocus::connect_enter` to
    // open it as a side effect any more; that indirection was the actual
    // mechanism behind the startup-open bug (see `connect_enter`'s own
    // comment in `init`).
    OpenLauncher,

    // ── Spotlight pill auto-hide (BAR-27 phase 1) ───────────────────────
    /// The pointer entered the pill or its hot-zone.
    PillHoverEnter,
    /// The pointer left the pill or its hot-zone; re-tucks after
    /// `bar.autohide.retuck_delay_ms` unless it re-enters first.
    PillHoverLeave,
    /// A pending hover-leave's debounce timer fired; `u32` is the
    /// generation it was scheduled under (see `pill_hover_gen`'s doc).
    PillHoverLeaveExpire(u32),
    /// Hold the pill down for `Duration` (a notification/level/workspace
    /// peek); re-evaluates immediately and again once it expires.
    PillPeek(std::time::Duration),
    /// A pending peek's expiry timer fired: just re-evaluate visibility
    /// (the peek itself already recorded `pill_hold_until`).
    PillReeval,
    /// `breadbar --pill show`: force a generous manual peek.
    PillShowCmd,
    /// `breadbar --pill toggle`: hide early if currently shown only via a
    /// live peek/hover-independent hold, otherwise show.
    PillToggleCmd,
    /// A notification arrived: lights the unread dot, peeks
    /// (`bar.autohide.notification_peek_ms`), and (BAR-30 phase 4, when
    /// `[notifications].style = "surtitle"` and DND is off) shows the
    /// surtitle caption. Always still recorded to history regardless of any
    /// of that (`NotifServer::notify` already did so before this fires).
    NotificationArrived {
        app: String,
        summary: String,
        /// Critical urgency: the surtitle stays up until dismissed.
        critical: bool,
    },
    /// A volume/brightness OSD event fired: peeks
    /// (`bar.autohide.level_peek_ms`) and, when `[osd].style = "pill"`,
    /// draws the level inside the pill instead of the standalone OSD
    /// window (BAR-30 phase 4).
    LevelChanged {
        kind: &'static str,
        pct: u8,
    },

    // ── Spotlight stage (BAR-30 phases 2-4) ─────────────────────────────
    /// The stage opened or closed: feeds `pill_autohide::State.open` (the
    /// pill stays down the whole time) and re-evaluates the pill.
    StageOpenChanged(bool),
    /// `spotlight.json` changed (the stage settings menu, or a hand edit).
    PrefsChanged(spotlight_prefs::Prefs),
    /// `breadbar --stage toggle` reaches the primary instance the same way
    /// `--pill toggle` does: forwarded from `notifications::StageCmd` over
    /// `dev.breadway.Bar`.
    StageToggleCmd,
    StageShowCmd,
    StageHideCmd,
    /// The pill's time was clicked: toggles this instance's own stage (the
    /// click already happened on this monitor, so no focus routing).
    StageClockClicked,
}

#[relm4::component(pub)]
impl SimpleComponent for App {
    type Init = BarInit;
    type Input = AppInput;
    type Output = ();

    view! {
        gtk::ApplicationWindow {
            add_css_class: "breadbar",
            set_title: Some("breadbar"),
            set_default_height: bar_height,

            // Root is a vbox (bar row + drawer), not a bare CenterBox, per
            // plan §2/§11: `drawer` is the only structural thing Capsule/
            // theme-04 adds over Island/Edge, and it's a slot below the bar
            // row, not a separate layout code path. `drawer_box` starts
            // empty and zero-height for every theme that never names a
            // module in `[bar.slots].drawer` (liquid-motion, glass-
            // workbench); see main.rs's "Assemble" section and
            // `theme.rs`'s `window.breadbar > box > centerbox` selector
            // update for why this is a no-op for both.
            #[name = "root_vbox"]
            gtk::Box {
                set_orientation: gtk4::Orientation::Vertical,

                #[name = "center_box"]
                gtk::CenterBox {
                    // Fill the bar's height. Without this the CenterBox takes
                    // only its natural height and sits at the TOP of the vbox,
                    // leaving the remainder as dead space along the bottom
                    // edge, so every valign:Center child centred within a
                    // short box rather than within the bar, and the whole row
                    // rode high. Regression from wrapping the bar row in a
                    // vbox to gain the drawer slot.
                    set_vexpand: true,
                },

                #[name = "drawer_box"]
                gtk::Box {
                    set_orientation: gtk4::Orientation::Vertical,
                },
            }
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let screenshot_req = init.screenshot;
        let monitor_name = init
            .monitor
            .clone()
            .or_else(primary_hypr_monitor)
            .unwrap_or_else(|| "eDP-1".into());

        // `bar.window` (plan §2/§6): window shape is data, not a closed
        // layout enum. Read once and reused below for the layer-shell setup
        // and (via `bar_height`, captured for the view! macro above) the
        // root window's initial GTK height. A vertical side dock has no
        // fixed height at all (it fills top-to-bottom via anchors, same as
        // a horizontal bar fills left-to-right). GTK's own default there
        // is fine as a first-paint hint; `apply_window_spec` below sets the
        // real fixed dimension (`width`, the dock's thickness) either way.
        let window_spec = theme::shell_theme().window().clone();
        // Read once, reused everywhere below that needs to pack the bar's
        // own row/segments top-to-bottom instead of left-to-right for a
        // side dock (`center_box` and the three slot containers). The
        // window shape and the widget layout inside it must always agree.
        let vertical_bar = window_spec.orientation() == WindowOrientation::Vertical;
        let bar_height = if vertical_bar { -1 } else { window_spec.height };

        root.init_layer_shell();
        root.set_namespace(Some("breadbar"));
        apply_window_spec(&root, &window_spec);
        // ANIMATION WORK #3: bar entrance on first map, Liquid Motion
        // only. The flush glass-workbench bar has no floating margin to
        // slide from (it sits edge-to-edge against the screen), so it
        // gets nothing; spotlight's capsule already has its own width/
        // drawer motion in flight during a real search, so stacking a
        // margin spring underneath that felt like fighting it rather than
        // adding to it: it gets a plain opacity fade instead (the
        // `bar-entrance` CSS class, added unconditionally by both arms
        // below; only liquid-motion also gets the margin spring).
        //
        // This animates the SURFACE's own layer-shell top margin via
        // `anim::spring_to`, not a CSS property, deliberately: `theme.rs`
        // has no `margin-top` transition/keyframe reachable from here
        // that wouldn't also require moving `root_vbox`'s or
        // `center_box`'s own CSS margin, which (unlike an outer
        // layer-shell margin, a compositor-level surface placement, not a
        // GTK box-model property at all) WOULD perturb every descendant
        // widget's own measured size, including the workspace row's.
        // `WorkspaceTrail::place()`'s single-shot initial geometry sample
        // is a documented previous crash site for exactly that: `ws-in`'s
        // margin-top keyframe on a first-build button raced that sample
        // and froze the trail pill low (see the `!is_first_build` guard
        // and its comment, a few hundred lines below, for the full
        // story). A layer-shell margin change repositions the whole
        // surface on screen without touching any widget's own box model,
        // so `compute_bounds(host)`'s host-local coordinates, the trail's
        // whole coordinate system, never see it move at all, by
        // construction rather than by luck.
        let theme_id = theme::shell_theme().id().to_string();
        if theme_id == "liquid-motion" {
            const BAR_ENTRANCE_DROP: i32 = 18;
            let target_top = window_spec.margin.top;
            let start_top = (target_top - BAR_ENTRANCE_DROP).max(0);
            root.set_margin(Edge::Top, start_top);
            root.add_css_class("bar-entrance");
            let played = Rc::new(Cell::new(false));
            root.connect_map(move |win| {
                // Guard against replaying on a later remap (e.g. a
                // satellite hidden/reshown; see `root.set_visible(false)`
                // a few lines below for the unbound-satellite case): this
                // is a first-paint entrance, not a reveal animation.
                if played.replace(true) {
                    return;
                }
                let target = win.clone();
                bread_theme::anim::spring_to(win, start_top, target_top, 420.0, move |v| {
                    target.set_margin(Edge::Top, v);
                });
            });
        } else if theme_id == "spotlight" {
            root.add_css_class("bar-entrance");
        }
        eprintln!(
            "breadbar: init monitor={monitor_name} primary={}",
            init.primary
        );
        if screenshot_req.is_none() && !bind_layer_monitor(&root, &monitor_name) {
            // Unbound satellites must not map on the compositor default
            // (that stacks a second exclusive-zone bar on the laptop).
            root.set_exclusive_zone(-1);
            if !init.primary {
                root.set_visible(false);
            }
        }

        // ── Spotlight pill: divider, low-battery flag, unread dot
        // (BAR-27 phase 1) ───────────────────────────────────────────────
        // Built unconditionally, same "registered but possibly unparented"
        // convention as every other bar module (`bar::slots::ModuleRegistry`'s
        // own doc comment): only spotlight's `[bar.slots]` currently names
        // any of these three, so they stay inert everywhere else.
        let pill_sep = gtk4::Separator::new(gtk4::Orientation::Vertical);
        pill_sep.add_css_class("bar-sep");
        // Hidden until `rebuild_buttons` finds something in the workspace
        // strip, so a bar with no workspace sync yet (or no Hyprland IPC)
        // doesn't open with a stray leading divider.
        pill_sep.set_visible(false);

        let pill_flag = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        pill_flag.add_css_class("pill-flag");
        pill_flag.set_valign(gtk4::Align::Center);
        pill_flag.set_visible(false);
        let pill_flag_dot = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        pill_flag_dot.add_css_class("pill-flag-dot");
        // Without this the dot fills the flag box's full height and draws as
        // a tall capsule instead of the mockup's 6px circle.
        pill_flag_dot.set_valign(gtk4::Align::Center);
        pill_flag.append(&pill_flag_dot);
        let pill_flag_label = gtk4::Label::new(None);
        pill_flag_label.add_css_class("pill-flag-label");
        pill_flag.append(&pill_flag_label);

        let pill_unread = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        pill_unread.add_css_class("pill-unread");
        pill_unread.set_valign(gtk4::Align::Center);
        pill_unread.set_visible(false);
        // BAR-38: a count of notifications since the stage was last open,
        // where the plain dot used to be (`stage::Shared` sets its text).
        let pill_unread_count = gtk4::Label::new(None);
        pill_unread_count.add_css_class("pill-unread-count");
        pill_unread_count.set_valign(gtk4::Align::Center);
        pill_unread.append(&pill_unread_count);

        // BAR-38: the pill's glance items. `nowplaying` is three bouncing
        // bars and the track title while a player plays; `dnd` is a crescent
        // while do-not-disturb is on (`stage::Shared::set_dnd` shows it).
        let pill_nowplaying = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        pill_nowplaying.add_css_class("pill-nowplaying");
        pill_nowplaying.set_valign(gtk4::Align::Center);
        pill_nowplaying.set_visible(false);
        let pill_eq = gtk4::Box::new(gtk4::Orientation::Horizontal, 2);
        pill_eq.add_css_class("pill-nowplaying-eq");
        pill_eq.set_valign(gtk4::Align::Center);
        for class in ["b1", "b2", "b3"] {
            let bar = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            bar.add_css_class("pill-eq-bar");
            bar.add_css_class(class);
            bar.set_valign(gtk4::Align::End);
            pill_eq.append(&bar);
        }
        pill_nowplaying.append(&pill_eq);
        let pill_nowplaying_title = gtk4::Label::new(None);
        pill_nowplaying_title.add_css_class("pill-nowplaying-title");
        pill_nowplaying_title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        pill_nowplaying_title.set_max_width_chars(18);
        pill_nowplaying.append(&pill_nowplaying_title);
        // The pill's battery: an outlined gauge whose fill tracks the charge
        // (sized in `refresh_battery`), its tip, and the percentage.
        let pill_battery = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        pill_battery.add_css_class("pill-battery");
        pill_battery.set_valign(gtk4::Align::Center);
        pill_battery.set_visible(false);
        let pill_battery_gauge = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        pill_battery_gauge.set_valign(gtk4::Align::Center);
        let pill_battery_shell = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        pill_battery_shell.add_css_class("pill-battery-shell");
        pill_battery_shell.set_valign(gtk4::Align::Center);
        let pill_battery_fill = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        pill_battery_fill.add_css_class("pill-battery-fill");
        pill_battery_fill.set_halign(gtk4::Align::Start);
        pill_battery_shell.append(&pill_battery_fill);
        pill_battery_gauge.append(&pill_battery_shell);
        let pill_battery_tip = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        pill_battery_tip.add_css_class("pill-battery-tip");
        pill_battery_tip.set_valign(gtk4::Align::Center);
        pill_battery_gauge.append(&pill_battery_tip);
        pill_battery.append(&pill_battery_gauge);
        let pill_battery_pct = gtk4::Label::new(None);
        pill_battery_pct.add_css_class("pill-battery-pct");
        pill_battery.append(&pill_battery_pct);
        let pill_dnd = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        pill_dnd.add_css_class("pill-dnd");
        pill_dnd.set_valign(gtk4::Align::Center);
        pill_dnd.set_tooltip_text(Some("Do not disturb is on"));
        pill_dnd.set_visible(false);

        // ── Spotlight pill: auto-hide (BAR-27 phase 1) ────────────────────
        // `[bar.autohide]`: off (`AutoHide::default()`) for every builtin but
        // spotlight, so the hover controller and hot-zone/lip satellite
        // below are simply never created there, and the pill stays at its
        // theme-configured margin forever (`update_pill` is a no-op while
        // `!autohide_cfg.enabled`).
        let autohide_cfg = *theme::shell_theme().autohide();
        if autohide_cfg.enabled {
            // BAR-31: the pill draws on the window's inner box inside a
            // transparent shadow margin (base.css `window.breadbar.pill`), so
            // the surface sits `SHADOW_TOP` above the theme's margin and only
            // the pill itself takes input.
            root.add_css_class("pill");
            root.set_margin(Edge::Top, pill_margins(&window_spec).0);
            root.connect_realize(|w| {
                if let Some(surface) = w.surface() {
                    surface.connect_layout(|surface, width, height| {
                        let (x, y, rw, rh) = pill_autohide::input_rect(width, height);
                        let rect = gtk4::cairo::RectangleInt::new(x, y, rw, rh);
                        surface
                            .set_input_region(Some(&gtk4::cairo::Region::create_rectangle(&rect)));
                    });
                }
            });
        }
        let (pill_hotzone, pill_lip, pill_hotzone_debug) = if autohide_cfg.enabled {
            let enter_sender = sender.clone();
            let leave_sender = sender.clone();
            let motion = gtk4::EventControllerMotion::new();
            motion.connect_enter(move |_, _, _| post(&enter_sender, AppInput::PillHoverEnter));
            motion.connect_leave(move |_| post(&leave_sender, AppInput::PillHoverLeave));
            root.add_controller(motion);

            let hz_enter = sender.clone();
            let hz_leave = sender.clone();
            let (hz_window, lip, debug) = pill_hotzone::spawn(
                autohide_cfg.hover_zone_width as i32,
                autohide_cfg.hover_zone_height as i32,
                move || post(&hz_enter, AppInput::PillHoverEnter),
                move || post(&hz_leave, AppInput::PillHoverLeave),
            );
            if screenshot_req.is_none() {
                bind_layer_monitor(&hz_window, &monitor_name);
            }
            hz_window.present();
            (Some(hz_window), Some(lip), Some(debug))
        } else {
            (None, None, None)
        };

        // ── Workspace row (left) ────────────────────────────────────────
        // Built imperatively (not via the view! macro) so a widget
        // container can sit as a plain sibling of workspace_box; see the
        // `widget:*` slot-entry handling in "Assemble" below. The Overlay
        // trail lives behind the buttons; rebuild_buttons only touches the
        // button box, never the trail host.
        let workspace_trail = bar::workspaces::WorkspaceTrail::new();
        let workspace_box = workspace_trail.buttons.clone();
        // A slot container's own internal packing direction (left-to-right
        // vs top-to-bottom) and cross-axis centring must follow the bar's
        // orientation, see `vertical_bar` above.
        let slot_orientation = if vertical_bar {
            gtk4::Orientation::Vertical
        } else {
            gtk4::Orientation::Horizontal
        };
        let workspace_row = gtk4::Box::new(slot_orientation, 0);
        // `.bar-segment` (axis 3, daylight): a no-op class under every other
        // theme (theme.rs only ever emits a `.bar-segment` CSS rule when
        // `bar_border == "segmented"`; see its `segment_css` local):
        // added unconditionally here, same as `center_area`/`stats_box`
        // below, so the Rust side never has to branch on the active theme
        // id to know which boxes are "the three segments".
        workspace_row.add_css_class("bar-segment");
        if vertical_bar {
            workspace_row.set_margin_top(8);
            workspace_row.set_halign(gtk4::Align::Center);
            workspace_row.set_hexpand(false);
        } else {
            workspace_row.set_margin_start(8);
            workspace_row.set_valign(gtk4::Align::Center);
            workspace_row.set_vexpand(false);
        }
        // `workspace_trail.overlay` is appended in the "Assemble" section
        // below, in the order `[bar.slots].left` names it, not here.

        // ── Lua-declared widget containers ──────────────────────────────
        // Phase 3b: a container per `widget:<key>` slot entry is created
        // on demand while walking `[bar.slots]` in the "Assemble" section
        // below (see `bar::slots::widget_slot_container`), so ANY slot can
        // host a Lua widget, not just the four fixed positions Phase 3a
        // shipped with. Populated by widgets::client's events.subscribe-
        // driven refresh loop, started at the end of init.

        // `tokens.icon_px` (plan §4): bar-chrome icon pixel size; reused
        // below for every `prepare_icon` call in this function.
        let icon_px = theme::shell_theme().tokens().icon_px() as i32;

        // ── SVG icon sets ────────────────────────────────────────────────
        // Battery / bluetooth glyphs swap by level/state at runtime via
        // `set_svg_icon` (which shares the process-wide `BAKED_TEXTURE_CACHE`
        // and keeps the rebake registry current); no per-App texture map.
        use bar::stats::{AC_POWER, BAT_MID, BT_OFF, ICON_VOLUME};
        // ── Stat labels ──────────────────────────────────────────────────
        let cpu_lbl = stat_label();
        let mem_lbl = stat_label();
        let pwr_lbl = stat_label();
        let gpu_lbl = stat_label();
        let vol_lbl = digit_chip_box();
        let vol_digits: Rc<RefCell<Vec<gtk4::Label>>> = Rc::new(RefCell::new(Vec::new()));
        let bat_lbl = digit_chip_box();
        let bat_digits: Rc<RefCell<Vec<gtk4::Label>>> = Rc::new(RefCell::new(Vec::new()));

        let vol_img = svg_image(ICON_VOLUME);
        vol_img.add_css_class("stat-icon");
        let bat_img = svg_image_sized(BAT_MID, icon_px as u32);
        let ac_img = svg_image(AC_POWER);
        ac_img.set_visible(false);
        let bt_img = svg_image_sized(BT_OFF, icon_px as u32);
        bt_img.set_visible(false);

        // ── WiFi pair + popover ──────────────────────────────────────────
        let wifi_lbl = gtk4::Label::new(None);
        wifi_lbl.add_css_class("stat-label");
        wifi_lbl.add_css_class("wifi-label");
        wifi_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        wifi_lbl.set_max_width_chars(28);
        wifi_lbl.set_xalign(0.0);
        // SSID lives in the popover + tooltip: a 20-char network name
        // crowding the tray is the opposite of a glass workbench bar.
        wifi_lbl.set_visible(false);
        let wifi_img = gtk4::Image::from_icon_name(bar::stats::WIFI_ICON_EXCELLENT);
        prepare_icon(&wifi_img, icon_px);
        wifi_img.add_css_class("stat-icon");

        // Content pane only: this becomes a tab inside the merged
        // connectivity popover built alongside the Bluetooth pane below,
        // once bt_img exists too. See "Connectivity popover" further down.
        let wifi_pane = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        let loading_lbl = gtk4::Label::new(Some("Scanning…"));
        loading_lbl.add_css_class("wifi-popover-loading");
        wifi_pane.append(&loading_lbl);

        // ── Media widget (center) ────────────────────────────────────────
        let media_widget = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        media_widget.add_css_class("media-widget");
        bar_chip(&media_widget);
        media_widget.set_visible(false);

        let media_eq = gtk4::Box::new(gtk4::Orientation::Horizontal, 3);
        media_eq.add_css_class("media-eq");
        media_eq.set_valign(gtk4::Align::Center);
        for _ in 0..4 {
            let bar = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            bar.add_css_class("media-eq-bar");
            bar.set_valign(gtk4::Align::End);
            media_eq.append(&bar);
        }

        let media_track_lbl = gtk4::Label::new(None);
        media_track_lbl.add_css_class("media-track-lbl");
        media_track_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        media_track_lbl.set_max_width_chars(42);
        media_track_lbl.set_xalign(0.0);

        media_widget.append(&media_eq);
        media_widget.append(&media_track_lbl);

        // Media controls popover
        let media_controls_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        media_controls_box.add_css_class("media-controls");
        media_controls_box.set_margin_top(4);
        media_controls_box.set_margin_bottom(4);
        media_controls_box.set_margin_start(4);
        media_controls_box.set_margin_end(4);

        let prev_btn = gtk4::Button::new();
        prev_btn.set_child(Some(&svg_image(asset!("Previous.svg"))));
        prev_btn.add_css_class("flat");
        prev_btn.add_css_class("media-btn");
        prev_btn.connect_clicked(|_| bar::media::spawn_cmd("previous"));

        let media_play_icon = svg_image(asset!("Pause.svg"));
        let media_play_btn = gtk4::Button::new();
        media_play_btn.set_child(Some(&media_play_icon));
        media_play_btn.add_css_class("flat");
        media_play_btn.add_css_class("media-btn");
        media_play_btn.add_css_class("media-play-btn");
        media_play_btn.connect_clicked(|_| bar::media::spawn_cmd("play-pause"));

        let next_btn = gtk4::Button::new();
        next_btn.set_child(Some(&svg_image(asset!("Next.svg"))));
        next_btn.add_css_class("flat");
        next_btn.add_css_class("media-btn");
        next_btn.connect_clicked(|_| bar::media::spawn_cmd("next"));

        media_controls_box.append(&prev_btn);
        media_controls_box.append(&media_play_btn);
        media_controls_box.append(&next_btn);

        // Clock: time is the hero, date sits beside it quieter.
        // Per-glyph labels so a minute rollover can flip only the digits
        // that changed, same motion as the Liquid Motion demo.
        let clock_time = bar::clock::time();
        let clock_digits = make_clock_digits(&clock_time);
        let clock_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        clock_box.add_css_class("clock-box");
        clock_box.add_css_class("clock-label");
        clock_box.set_valign(gtk4::Align::Center);
        clock_box.set_vexpand(false);
        // Varela Round's em box sits optically high in the 44px island.
        // No compensating margin: this +3 existed to nudge the clock down
        // against the mis-centred CenterBox above. With the row now filling
        // the bar height and centring properly, the same offset would push
        // the clock 3px BELOW everything else.
        clock_box.set_margin_top(0);
        for digit in &clock_digits {
            clock_box.append(digit);
        }
        let date_lbl = gtk4::Label::new(Some(&bar::clock::date()));
        date_lbl.add_css_class("date-label");
        date_lbl.set_visible(false);

        // `modules.clock.style = "plain"` (glass-workbench): date_lbl above
        // plus one plain "HH:MM" label, no per-digit flip markup at all.
        // Built unconditionally alongside the flip clock so a theme's style
        // choice is just which of the two gets registered into the "clock"
        // slot below; see "Assemble".
        let clock_plain_lbl = gtk4::Label::new(Some(&bar::clock::time()));
        clock_plain_lbl.add_css_class("clock-plain-time");
        clock_plain_lbl.set_valign(gtk4::Align::Center);
        clock_plain_lbl.set_vexpand(false);
        let clock_plain_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        clock_plain_box.add_css_class("clock-plain");
        clock_plain_box.set_valign(gtk4::Align::Center);
        clock_plain_box.set_vexpand(false);
        clock_plain_box.append(&date_lbl);
        clock_plain_box.append(&clock_plain_lbl);

        // BAR-30 phase 2: clicking the pill's time opens the stage (the
        // mockup's `#time` button). Only the auto-hiding pill themes have a
        // stage to open; on every other theme the click stays a no-op, as
        // before. Wired on both clock variants; only one is ever parented.
        for clock_widget in [
            clock_plain_box.upcast_ref::<gtk4::Widget>(),
            clock_box.upcast_ref(),
        ] {
            let click = gtk4::GestureClick::new();
            click.set_button(gtk4::gdk::BUTTON_PRIMARY);
            let s = sender.clone();
            click.connect_released(move |_, _, _, _| post(&s, AppInput::StageClockClicked));
            clock_widget.add_controller(click);
        }

        // ── Launcher entry + results (theme 04/spotlight, plan §7) ───────
        // Built unconditionally, exactly like `clock_plain_box` above:
        // placed in a slot only by a theme that names "launcher_entry"/
        // "launcher_results" (spotlight today; see "Assemble" below).
        // `bread_launcher::LAUNCHER_APP` ("breadbox") is the launcher's
        // shared identity: this reads/writes the SAME icon cache and
        // launch history breadbox's own overlay window does, so the
        // capsule and breadbox rank a user's apps identically instead of
        // forking into two histories just because a different theme
        // happens to be active (see that constant's own doc comment).
        let launcher_cfg = theme::shell_theme().launcher().clone();
        let launcher_manifest: std::collections::HashMap<String, std::path::PathBuf> =
            std::fs::read_to_string(bread_launcher::IconCache::manifest_path(
                bread_launcher::LAUNCHER_APP,
            ))
            .ok()
            .and_then(|s| {
                serde_json::from_str::<std::collections::HashMap<String, String>>(&s).ok()
            })
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| (k, std::path::PathBuf::from(v)))
            .collect();
        let launcher_history = Rc::new(RefCell::new(bread_launcher::LaunchHistory::load(
            bread_launcher::LAUNCHER_APP,
        )));
        // No per-workspace priority context here: that's breadbox's own
        // `Config`/`Context` format (breadbox-shared), not launcher
        // substance, and breadbar has no equivalent concept. The capsule
        // sorts by launch history then alphabetically, same fallback
        // ordering breadbox itself uses once a workspace has no configured
        // priority list at all.
        let launcher_entries = bread_launcher::load_sorted_entries(
            &launcher_manifest,
            &[],
            &launcher_history.borrow(),
        );
        let launcher_results = ResultsList::new(
            &launcher_entries,
            launcher_cfg.icon_px,
            Rc::clone(&launcher_history),
            launcher_cfg.sections,
        );
        launcher_results
            .scroller
            .add_css_class("bread-drawer-scroller");

        let launcher_entry = gtk4::Entry::new();
        launcher_entry.add_css_class("launcher-entry");
        launcher_entry.set_has_frame(false);
        launcher_entry.set_hexpand(true);
        // Starts non-focusable ("the spotlight theme starts with the
        // search open"). GTK4 auto-assigns keyboard focus to the first
        // can-focus widget in a window as it's first mapped/shown: with
        // nothing else focusable in the bar, that was always
        // `launcher_entry`, and `focus_ctrl`'s `connect_enter` below
        // (added unconditionally) turns "the entry gained focus" straight
        // into `open_fn()`, so the capsule opened itself at startup. A
        // widget that can't focus is skipped by BOTH that automatic
        // selection and an explicit `grab_focus()` call: see GTK's own
        // docs for `Widget::grab_focus`: "if widget is not focusable...
        // this function does nothing." Every place that later wants real
        // focus (the click gesture below, and `AppInput::OpenLauncher`'s
        // handler) flips this back to `true` immediately before grabbing.
        launcher_entry.set_can_focus(false);
        gtk4::prelude::EntryExt::set_alignment(&launcher_entry, 0.5);
        // `modules.clock.placeholder_clock` (spotlight): the entry's idle
        // placeholder IS the clock: no separate clock module renders at
        // all under `style = "none"`. Any other theme gets a plain "Search"
        // placeholder (never shown today: no other builtin slots
        // "launcher_entry" anywhere), so this still degrades sanely if a
        // future/user theme places it without also setting the flag.
        let placeholder_clock = theme::shell_theme().modules().clock.placeholder_clock;
        launcher_entry.set_placeholder_text(Some(
            if placeholder_clock {
                bar::clock::time()
            } else {
                "Search".to_string()
            }
            .as_str(),
        ));

        // Center area: [media_widget · widgets · clock · widgets]
        let center_area = gtk4::Box::new(slot_orientation, 12);
        center_area.add_css_class("center-area");
        center_area.add_css_class("bar-segment");
        if vertical_bar {
            center_area.set_halign(gtk4::Align::Center);
            center_area.set_hexpand(false);
        } else {
            center_area.set_valign(gtk4::Align::Center);
            center_area.set_vexpand(false);
        }
        // `media_widget`/`clock_box` and any `widget:*` entries interleaved
        // around them are all appended in "Assemble" below, in the exact
        // order `[bar.slots].centre` names them.

        // ── Stats box (right side, or trailing edge of a vertical dock) ────
        // Demo order: [vol 64] [wifi] [bat 83] [menu]
        let stats_box = gtk4::Box::new(slot_orientation, 2);
        stats_box.add_css_class("stats-box");
        stats_box.add_css_class("bar-segment");
        if vertical_bar {
            stats_box.set_margin_bottom(2);
            stats_box.set_halign(gtk4::Align::Center);
            stats_box.set_hexpand(false);
        } else {
            stats_box.set_margin_end(2);
            stats_box.set_valign(gtk4::Align::Center);
            stats_box.set_vexpand(false);
        }
        // Also appended in "Assemble": before the right slot's modules,
        // preserving its fixed position (left of the stats modules)
        // whatever `[bar.slots].right` contains.

        // CPU/RAM/power draw stay built (control panel + screenshots still
        // read the labels) but never mount on the island: the demo bar
        // does not show them.
        let cpu_pair = stat_pair(asset!("CPU.svg"), &cpu_lbl);
        let mem_pair = stat_pair(asset!("RAM Usage.svg"), &mem_lbl);
        let pwr_pair = stat_pair(asset!("Power Draw.svg"), &pwr_lbl);
        let gpu_pair = stat_pair(asset!("GPU.svg"), &gpu_lbl);
        for pair in [&cpu_pair, &mem_pair, &pwr_pair, &gpu_pair] {
            pair.add_css_class("sys-stat");
            pair.set_hexpand(true);
        }
        gpu_pair.set_visible(false);

        // `[bar.slots].right = [..., "cpu", "ram", ...]` (glass-workbench,
        // Phase 5): separate chip instances from `cpu_pair`/`mem_pair`
        // above, which stay parented in the control panel's sys-grid below:
        // reusing them here would mean reparenting them out of the panel.
        // Same icons, same `.stat-pair` chip styling every other bar chip
        // (volume/battery) already uses; fed by the same `StatsUpdate` data.
        let bar_cpu_lbl = stat_label();
        let bar_ram_lbl = stat_label();
        let bar_cpu_pair = stat_pair(asset!("CPU.svg"), &bar_cpu_lbl);
        let bar_ram_pair = stat_pair(asset!("RAM Usage.svg"), &bar_ram_lbl);

        let system_stats_box = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
        system_stats_box.add_css_class("sys-grid");
        let sys_row1 = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        sys_row1.append(&cpu_pair);
        sys_row1.append(&mem_pair);
        let sys_row2 = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        sys_row2.append(&gpu_pair);
        sys_row2.append(&pwr_pair);
        system_stats_box.append(&sys_row1);
        system_stats_box.append(&sys_row2);
        // breadram pressure row: daemon state plus a pause/resume button.
        // Same `.sys-stat` chip chrome as the rows above (its own label
        // instance: a widget has one parent, so `mem_lbl` cannot be
        // reused here without reparenting it out of its chip). The button
        // is plain text so it stays legible at panel widths.
        let ram_panel_mem_lbl = stat_label();
        ram_panel_mem_lbl.set_label("—");
        let ram_pair = stat_pair(asset!("RAM Usage.svg"), &ram_panel_mem_lbl);
        ram_pair.add_css_class("sys-stat");
        ram_pair.set_hexpand(true);
        let ram_panel_status_lbl = gtk4::Label::new(Some("breadram: —"));
        ram_panel_status_lbl.set_xalign(0.0);
        ram_panel_status_lbl.set_hexpand(true);
        let ram_pause_lbl = gtk4::Label::new(Some("Pause"));
        let ram_pause_btn = gtk4::Button::new();
        ram_pause_btn.add_css_class("flat");
        ram_pause_btn.set_child(Some(&ram_pause_lbl));
        ram_pause_btn.set_tooltip_text(Some(
            "Pause automatic memory management (temporary, not saved)",
        ));
        let sys_row3 = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        sys_row3.append(&ram_pair);
        sys_row3.append(&ram_panel_status_lbl);
        sys_row3.append(&ram_pause_btn);
        system_stats_box.append(&sys_row3);
        let system_sep = gtk4::Separator::new(gtk4::Orientation::Vertical);
        system_sep.add_css_class("bar-sep");
        system_sep.set_visible(false);

        let vol_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        vol_box.add_css_class("stat-pair");
        bar_chip(&vol_box);
        bar_chip(&vol_lbl);
        vol_box.append(&vol_img);
        vol_box.append(&vol_lbl);
        // Appended in "Assemble" below, per `[bar.slots].right`.

        let bat_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        bat_box.add_css_class("stat-pair");
        bar_chip(&bat_box);
        bat_img.add_css_class("stat-icon");
        bar_chip(&bat_lbl);
        ac_img.add_css_class("stat-icon");
        bat_box.append(&bat_img);
        bat_box.append(&bat_lbl);

        bt_img.add_css_class("bt-icon");

        // Content pane only: same deal as wifi_pane above.
        let bt_pane = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        let bt_loading_lbl = gtk4::Label::new(Some("Loading…"));
        bt_loading_lbl.add_css_class("wifi-popover-loading");
        bt_pane.append(&bt_loading_lbl);

        // ── Connectivity popover ─────────────────────────────────────────
        // WiFi and Bluetooth share one popover with one anatomy (same CSS
        // classes throughout the two panes) instead of two near-identical
        // popups behind two separate icons. A small tab row switches between
        // them; both fetches kick off on open so switching tabs afterward is
        // instant. The two panes live in a Stack rather than plain sibling
        // Boxes with manual visibility toggling, which left stale width
        // behind on reopen.
        //
        // Scrollport is a fixed 300px so tab-switch / first scan cannot
        // resize the xdg_popup (that vanish-on-grow bug). Nearby networks
        // live in the scroll, not a clipped 240px well.
        let content_stack = gtk4::Stack::new();
        content_stack.set_hhomogeneous(true);
        content_stack.set_vhomogeneous(false);
        content_stack.set_transition_type(gtk4::StackTransitionType::Crossfade);
        content_stack.set_transition_duration(220);
        content_stack.add_named(&wifi_pane, Some("wifi"));
        content_stack.add_named(&bt_pane, Some("bluetooth"));
        content_stack.set_visible_child_name("wifi");

        // Fixed scrollport so nearby networks stay reachable without
        // resizing the xdg_popup (that vanish-on-grow bug).
        let content_scroll = gtk4::ScrolledWindow::new();
        content_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        content_scroll.set_propagate_natural_width(true);
        content_scroll.set_min_content_height(300);
        content_scroll.set_max_content_height(300);
        content_scroll.set_child(Some(&content_stack));

        let wifi_tab_btn = popover_tab("Wi-Fi");
        wifi_tab_btn.set_active(true);
        let bt_tab_btn = popover_tab("Bluetooth");
        bt_tab_btn.set_group(Some(&wifi_tab_btn));

        let tab_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        tab_row.add_css_class("popover-tab-row");
        tab_row.set_homogeneous(true);
        tab_row.append(&wifi_tab_btn);
        tab_row.append(&bt_tab_btn);
        let wifi_caret = popover_caret();

        let stack_for_wifi = content_stack.clone();
        wifi_tab_btn.connect_toggled(move |btn| {
            if btn.is_active() {
                stack_for_wifi.set_visible_child_name("wifi");
            }
        });
        let stack_for_bt = content_stack.clone();
        bt_tab_btn.connect_toggled(move |btn| {
            if btn.is_active() {
                stack_for_bt.set_visible_child_name("bluetooth");
            }
        });

        let connectivity_inner = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        connectivity_inner.add_css_class("wifi-popover-inner");
        connectivity_inner.append(&tab_row);
        connectivity_inner.append(&wifi_caret);
        connectivity_inner.append(&content_scroll);

        let connectivity_pair = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        connectivity_pair.add_css_class("stat-pair");
        connectivity_pair.add_css_class("icon-only");
        bar_chip(&connectivity_pair);
        wifi_img.set_halign(gtk4::Align::Center);
        // `.stat-pair.icon-only` forces a 32px min-width on this box, wider
        // than the 24px icon's natural size. Without hexpand, a `gtk4::Box`
        // packs a non-expanding child at its natural size flush against the
        // start edge and leaves the leftover width trailing after it, so
        // `halign: Center` had nothing to center within and the glyph sat a
        // few pixels left of true center (reported: wifi icon not centered
        // on glass-workbench/liquid-motion). `bat_box` never showed this
        // because it isn't `icon-only`: no forced min-width wider than its
        // (icon + label) content, so there's no leftover space to
        // mis-place. hexpand(true) gives the icon a fillable cell spanning
        // the full 32px box, which `halign: Center` then centers within,
        // matching `bat_box`'s already-centered result.
        //
        // A bare `set_hexpand(true)` on the image is not enough on its
        // own: GTK4 computes a container's *effective* expand by OR-ing in
        // its children's hexpand whenever the container's own hexpand
        // hasn't been explicitly set, so the flag silently bubbles up
        // through `connectivity_pair` into the shared right-hand stats box
        // and from there into the centerbox's end slot, which then hands
        // that slot most of the bar's remaining width instead of its
        // normal packed size. The visible symptom was dramatic, not
        // subtle: the whole vol/wifi cluster jumped left to sit against
        // the clock, with a huge dead gap before battery/hamburger, in a
        // `--screenshot bar` capture. `connectivity_pair.set_hexpand(false)`
        // pins this box's own expand explicitly, which stops the
        // computation from climbing any further: the child can still
        // fill and center within this one box's fixed 32px cell.
        wifi_img.set_hexpand(true);
        connectivity_pair.set_hexpand(false);
        connectivity_pair.append(&wifi_img);
        // `connectivity_pair` and `bat_box` are appended in "Assemble"
        // below, per `[bar.slots].right`.

        // ── Control panel popover ────────────────────────────────────────
        // Liquid Motion chrome: CONTROL / volume / brightness / power tiles.
        // SNI tray + Lua tray widgets still mount here, but only as a
        // headerless icon row when something actually registers.
        let panel_inner = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        panel_inner.add_css_class("control-panel-inner");

        let panel_header = gtk4::Label::new(Some("CONTROL"));
        panel_header.add_css_class("control-panel-header");
        panel_header.set_xalign(0.0);
        panel_inner.append(&panel_header);
        panel_inner.append(&popover_caret());

        // Each control-panel section is built here in full (its handles stay
        // in scope for the signal wiring below), then appended to
        // `panel_inner` in the active theme's `[panel].sections` order: a
        // section whose name the theme drops is simply never parented. The
        // header + caret above are fixed.
        let vol_row = build_slider_row(bar::stats::ICON_VOLUME, "Volume", 0.0, 1.5, 0.02);
        let panel_vol_slider = vol_row.1.clone();

        let sink_section = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        sink_section.add_css_class("control-panel-section");
        let sink_header = gtk4::Label::new(Some("OUTPUT"));
        sink_header.add_css_class("control-panel-header");
        sink_header.set_xalign(0.0);
        let sink_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        sink_section.append(&sink_header);
        sink_section.append(&sink_box);
        sink_section.set_visible(false);

        let bright_row =
            build_slider_row(bar::stats::ICON_BRIGHTNESS, "Brightness", 0.0, 1.0, 0.02);
        let panel_bright_slider = bright_row.1.clone();

        // `system` section = its header + the stats box, wrapped so the pair
        // moves/hides as one unit under `[panel].sections`.
        let sys_section = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        sys_section.add_css_class("control-panel-section");
        let sys_header = gtk4::Label::new(Some("SYSTEM"));
        sys_header.add_css_class("control-panel-header");
        sys_header.set_xalign(0.0);
        sys_section.append(&sys_header);
        sys_section.append(&system_stats_box);

        // Equal-width icon tiles. Clicks are wired after `panels` exists
        // (see "Power actions" below) so an action can close the panel
        // first instead of leaving it open under the lock screen.
        let power_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        power_row.add_css_class("power-row");
        power_row.set_homogeneous(true);
        let power_btns: Vec<(gtk4::Button, gtk4::Label, PowerAction)> = POWER_ACTIONS
            .iter()
            .map(|action| {
                let btn = gtk4::Button::new();
                btn.add_css_class("flat");
                btn.add_css_class("power-btn");
                btn.set_tooltip_text(Some(action.tooltip));
                let tile = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
                tile.append(&svg_image(action.icon));
                let lbl = gtk4::Label::new(Some(action.label));
                lbl.add_css_class("power-btn-label");
                tile.append(&lbl);
                btn.set_child(Some(&tile));
                power_row.append(&btn);
                (btn, lbl, *action)
            })
            .collect();

        // SNI / Lua tray sit under the demo chrome so a single icon cannot
        // split the sliders from the power chips.
        // SNI icons are tucked behind a collapsed "APPS n" toggle: they are
        // rarely needed, and a loose row of mismatched third-party icons was
        // the noisiest thing in the panel. Expanded state lasts for the
        // session (the revealer is never rebuilt).
        let tray_section = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        tray_section.add_css_class("control-panel-section");
        let tray_toggle = gtk4::Button::new();
        tray_toggle.add_css_class("flat");
        tray_toggle.add_css_class("tray-toggle");
        tray_toggle.set_tooltip_text(Some("Show background apps"));
        let tray_toggle_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        let tray_toggle_lbl = gtk4::Label::new(Some("APPS"));
        tray_toggle_lbl.add_css_class("control-panel-header");
        let tray_count_lbl = gtk4::Label::new(None);
        tray_count_lbl.add_css_class("tray-count");
        let tray_chevron = svg_image_sized(asset!("Chevron Down.svg"), 14);
        tray_chevron.add_css_class("tray-chevron");
        tray_chevron.set_hexpand(true);
        tray_chevron.set_halign(gtk4::Align::End);
        tray_toggle_row.append(&tray_toggle_lbl);
        tray_toggle_row.append(&tray_count_lbl);
        tray_toggle_row.append(&tray_chevron);
        tray_toggle.set_child(Some(&tray_toggle_row));
        let tray_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        tray_box.add_css_class("tray-box");
        tray_box.set_halign(gtk4::Align::Start);
        let tray_revealer = gtk4::Revealer::new();
        tray_revealer.set_transition_type(gtk4::RevealerTransitionType::SlideDown);
        tray_revealer.set_transition_duration(180);
        tray_revealer.set_child(Some(&tray_box));
        tray_toggle.connect_clicked({
            let revealer = tray_revealer.clone();
            move |b| {
                let open = !revealer.reveals_child();
                revealer.set_reveal_child(open);
                if open {
                    b.add_css_class("open");
                    b.set_tooltip_text(Some("Hide background apps"));
                } else {
                    b.remove_css_class("open");
                    b.set_tooltip_text(Some("Show background apps"));
                }
            }
        });
        tray_section.append(&tray_toggle);
        tray_section.append(&tray_revealer);
        tray_section.set_visible(false);
        let tray_sep = gtk4::Separator::new(gtk4::Orientation::Horizontal);
        tray_sep.set_visible(false);

        let widget_tray_section = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        widget_tray_section.add_css_class("control-panel-section");
        let widget_tray_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        widget_tray_box.add_css_class("tray-box");
        widget_tray_box.set_halign(gtk4::Align::Center);
        widget_tray_section.append(&widget_tray_box);
        widget_tray_section.set_visible(false);
        let widget_tray_sep = gtk4::Separator::new(gtk4::Orientation::Horizontal);
        widget_tray_sep.set_visible(false);

        // `[panel].sections`: order + membership of the control-panel body.
        {
            use gtk4::Widget;
            let section_widgets: [(&str, Vec<Widget>); 6] = [
                ("volume", vec![vol_row.0.clone().upcast()]),
                ("output", vec![sink_section.clone().upcast()]),
                ("brightness", vec![bright_row.0.clone().upcast()]),
                ("system", vec![sys_section.clone().upcast()]),
                ("power", vec![power_row.clone().upcast()]),
                (
                    "tray",
                    vec![
                        tray_section.clone().upcast(),
                        widget_tray_section.clone().upcast(),
                    ],
                ),
            ];
            for name in &theme::shell_theme().panel().sections {
                if let Some((_, widgets)) = section_widgets.iter().find(|(n, _)| n == name) {
                    for w in widgets {
                        panel_inner.append(w);
                    }
                }
            }
        }

        // Hamburger button: same chip chrome as volume / wifi / battery.
        // A baked Lucide icon, not a "☰" text glyph: the glyph's weight and
        // size came from whatever font rendered it, so it never matched the
        // wifi/battery icons beside it.
        let hamburger_btn = gtk4::Button::new();
        hamburger_btn.set_child(Some(&svg_image(asset!("Menu.svg"))));
        hamburger_btn.set_tooltip_text(Some("Control panel"));
        hamburger_btn.add_css_class("flat");
        hamburger_btn.add_css_class("control-panel-btn");
        hamburger_btn.add_css_class("stat-pair");
        hamburger_btn.add_css_class("icon-only");
        bar_chip(&hamburger_btn);

        // Slider signals: use Rc<Cell<bool>> to suppress feedback during data load
        let panel_loading = Rc::new(Cell::new(false));

        let loading_v = panel_loading.clone();
        let vol_lbl_live = vol_lbl.clone();
        let vol_digits_live = vol_digits.clone();
        panel_vol_slider.connect_value_changed(move |s| {
            // No flip animation here, deliberately: a slider drag fires
            // this on every pointer-move tick, and re-playing the
            // digit-flip keyframe that fast would read as a flicker, not a
            // roll. `set_digit_chip` just swaps the label text/count in
            // place; `flip_digit_chip` (used by the `StatsUpdate` poll
            // below) is the one that actually animates.
            set_digit_chip(
                &vol_lbl_live,
                &mut vol_digits_live.borrow_mut(),
                &format!("{:.0}", s.value() * 100.0),
            );
            if loading_v.get() {
                return;
            }
            bar::control::spawn_set_volume(s.value());
        });

        let loading_b = panel_loading.clone();
        panel_bright_slider.connect_value_changed(move |s| {
            if loading_b.get() {
                return;
            }
            bar::control::spawn_set_brightness(s.value());
        });

        // `hamburger_btn` is appended in "Assemble" below, per
        // `[bar.slots].right`.

        // Standalone layer windows: below the island, slid in by Hyprland.
        let panels = panel::PanelSet::new(
            &monitor_name,
            &connectivity_inner,
            &panel_inner,
            &media_controls_box,
        );

        let sender_conn = sender.clone();
        panels.connectivity.connect_map(move |_| {
            bar::wifi::spawn_popover_load(sender_conn.clone());
            bar::bluetooth::spawn_popover_load(sender_conn.clone());
        });
        let sender_cp = sender.clone();
        panels.control.connect_map(move |_| {
            bar::control::spawn_load(sender_cp.clone());
        });

        {
            let panels = panels.clone();
            let win = panels.connectivity.clone();
            let gesture = gtk4::GestureClick::new();
            gesture.connect_released(move |_, _, _, _| {
                panels.toggle(&win);
            });
            connectivity_pair.add_controller(gesture);
        }
        {
            let panels = panels.clone();
            let win = panels.control.clone();
            hamburger_btn.connect_clicked(move |_| {
                panels.toggle(&win);
            });
        }
        // The RAM chip opens the control panel, where the SYSTEM section
        // carries the daemon state and the pause button: same one-click
        // reach as the connectivity chip above it.
        {
            let panels = panels.clone();
            let win = panels.control.clone();
            let gesture = gtk4::GestureClick::new();
            gesture.connect_released(move |_, _, _, _| {
                panels.toggle(&win);
            });
            bar_ram_pair.add_controller(gesture);
            bar_ram_pair.set_tooltip_text(Some("Memory: open control panel"));
        }
        // Power actions. Restart/off are one stray click from losing the
        // session, so they arm on the first click (label swaps to
        // "confirm") and only run on a second click within
        // POWER_CONFIRM_MS; closing the panel disarms them.
        {
            let armed: Rc<RefCell<Option<(gtk4::Button, gtk4::Label, PowerAction)>>> =
                Rc::new(RefCell::new(None));
            let disarm = {
                let armed = armed.clone();
                move || {
                    if let Some((btn, lbl, action)) = armed.borrow_mut().take() {
                        btn.remove_css_class("armed");
                        lbl.set_label(action.label);
                    }
                }
            };
            let generation = Rc::new(Cell::new(0u64));
            for (btn, lbl, action) in power_btns {
                let panels = panels.clone();
                let armed = armed.clone();
                let generation = generation.clone();
                let disarm = disarm.clone();
                btn.connect_clicked(move |b| {
                    let is_armed = armed
                        .borrow()
                        .as_ref()
                        .is_some_and(|(armed_btn, _, _)| armed_btn == b);
                    disarm();
                    if action.confirm && !is_armed {
                        b.add_css_class("armed");
                        lbl.set_label("confirm");
                        *armed.borrow_mut() = Some((b.clone(), lbl.clone(), action));
                        // Only the timer from the most recent arm may
                        // disarm: an older one must not cut a re-arm short.
                        generation.set(generation.get() + 1);
                        let armed_gen = generation.get();
                        let generation = generation.clone();
                        let disarm = disarm.clone();
                        gtk4::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(POWER_CONFIRM_MS),
                            move || {
                                if generation.get() == armed_gen {
                                    disarm();
                                }
                            },
                        );
                        return;
                    }
                    panels.hide_all();
                    let args = action.cmd;
                    relm4::spawn(async move {
                        let _ = tokio::process::Command::new(args[0])
                            .args(&args[1..])
                            .spawn();
                    });
                });
            }
            // breadram pause/resume: the handler reads the last reported
            // mode, so the click only says "the other one".
            {
                let sender = sender.clone();
                ram_pause_btn.connect_clicked(move |_| {
                    post(&sender, AppInput::RamPauseToggle);
                });
            }
            panels.control.connect_unmap(move |_| disarm());
        }
        {
            let panels = panels.clone();
            let win = panels.control.clone();
            let vol_gesture = gtk4::GestureClick::new();
            vol_gesture.connect_released(move |_, _, _, _| {
                panels.toggle(&win);
            });
            vol_box.add_controller(vol_gesture);
        }
        {
            let panels = panels.clone();
            let win = panels.media.clone();
            let mgesture = gtk4::GestureClick::new();
            mgesture.connect_released(move |_, _, _, _| {
                panels.toggle(&win);
            });
            media_widget.add_controller(mgesture);
        }

        // ── Assemble: slot-driven module + widget order (plan §11 Phase 3b) ──
        // Every module widget above is already fully built; only the ORDER
        // it lands in its container, and which of left/centre/right it
        // lands in, comes from the theme manifest's `[bar.slots]` now. A
        // `widget:<key>` slot entry gets (or creates) a Lua widget
        // container at that exact position: `<key>` is either a
        // `WidgetPlacement` alias or a Lua module name; see
        // `bar::slots::widget_slot_container` and `reconcile_widgets`'
        // routing below. This is how a Lua widget can land in ANY slot,
        // not just the four fixed positions Phase 3a shipped with.
        let bar_shell_theme = theme::shell_theme();
        let mut bar_modules = bar::slots::ModuleRegistry::new();
        bar_modules.register("workspaces", &workspace_trail.overlay);
        bar_modules.register("media", &media_widget);
        // `modules.clock.style`: "flip" uses the per-digit clock_box; "plain"
        // uses clock_plain_box and reveals date_lbl per `show_date`; "none"
        // registers neither. A clock-style change is restart-gated
        // (`theme::needs_restart`), so this is fixed for this App's lifetime:
        // the live slot re-assembly keeps whichever was registered here.
        match bar_shell_theme.modules().clock.style {
            ClockStyle::Plain => {
                date_lbl.set_visible(bar_shell_theme.modules().clock.show_date);
                bar_modules.register("clock", &clock_plain_box);
            }
            ClockStyle::Flip => bar_modules.register("clock", &clock_box),
            ClockStyle::None => {}
        }
        bar_modules.register("volume", &vol_box);
        bar_modules.register("wifi", &connectivity_pair);
        bar_modules.register("battery", &bat_box);
        bar_modules.register("control", &hamburger_btn);
        // Registered unconditionally; `assemble_bar_slots` only *places*
        // whichever the active `[bar.slots]` names, so an unnamed module
        // stays built-but-unparented (media under glass-workbench, cpu/ram
        // under liquid-motion, launcher_* outside spotlight).
        bar_modules.register("cpu", &bar_cpu_pair);
        bar_modules.register("ram", &bar_ram_pair);
        bar_modules.register("launcher_entry", &launcher_entry);
        bar_modules.register("launcher_results", &launcher_results.scroller);
        // BAR-27 phase 1 (spotlight pill): only spotlight's `[bar.slots]`
        // names any of these three.
        bar_modules.register("sep", &pill_sep);
        bar_modules.register("flag", &pill_flag);
        bar_modules.register("unread", &pill_unread);
        bar_modules.register("nowplaying", &pill_nowplaying);
        bar_modules.register("dnd", &pill_dnd);
        bar_modules.register("pill_battery", &pill_battery);

        // `tray` never appears in a bar slot: it lives in the control-panel
        // popover, but it's keyed here so `reconcile_widgets`' routing finds
        // it the same way as any slot-driven widget container.
        let mut widget_containers: std::collections::HashMap<String, gtk4::Box> =
            std::collections::HashMap::new();
        widget_containers.insert("tray".to_string(), widget_tray_box);

        // ── Pill level display (BAR-30 phase 4, BAR-31) ──────────────────
        // `[osd].style = "pill"` draws a volume/brightness change inside the
        // pill instead of the standalone OSD window. It is a regular slot
        // module ("level", placed just before the flag, where the mockup's
        // `.level` sits) that `show_pill_level` reveals while taking every
        // other pill item but the flag out of layout, the mockup's
        // `.wick.leveling>:not(.level):not(.flag){display:none}`. The pill
        // then resizes to fit the level row exactly as the browser does.
        let pill_level_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        pill_level_box.add_css_class("pill-level");
        pill_level_box.set_valign(gtk4::Align::Center);
        pill_level_box.set_visible(false);
        let pill_level_name = gtk4::Label::new(None);
        pill_level_name.add_css_class("pill-level-name");
        pill_level_box.append(&pill_level_name);
        let pill_level_bar = gtk4::ProgressBar::new();
        pill_level_bar.add_css_class("pill-level-bar");
        pill_level_bar.set_valign(gtk4::Align::Center);
        pill_level_box.append(&pill_level_bar);
        let pill_level_pct = gtk4::Label::new(None);
        pill_level_pct.add_css_class("pill-level-pct");
        pill_level_box.append(&pill_level_pct);
        bar_modules.register("level", &pill_level_box);

        // ── Assemble ─────────────────────────────────────────────────────
        let widgets = view_output!();
        // A side dock stacks start/center/end (workspaces/clock/stats)
        // top-to-bottom instead of left-to-right; `CenterBox` supports
        // both natively via `Orientable`, so this is the one place the
        // bar row itself needs to know its own orientation; every slot
        // container packed into it already picked `slot_orientation` above.
        if vertical_bar {
            widgets
                .center_box
                .set_orientation(gtk4::Orientation::Vertical);
        }
        widgets.center_box.set_start_widget(Some(&workspace_row));
        widgets.center_box.set_center_widget(Some(&center_area));
        widgets.center_box.set_end_widget(Some(&stats_box));
        widgets.drawer_box.add_css_class("bread-drawer");

        // Fill left/centre/right/drawer from `[bar.slots]`. Re-run verbatim
        // by `App::rebuild_from_theme` on a live theme switch.
        assemble_bar_slots(
            bar_shell_theme.slots(),
            &bar_modules,
            &mut widget_containers,
            &workspace_row,
            &center_area,
            &stats_box,
            &widgets.drawer_box,
        );
        // Collapsed by default; `Overflow::Hidden` clips the results list
        // while its allocated height is below its natural content height,
        // same as the demo's `.results { max-height: 0; overflow: hidden }`.
        widgets.drawer_box.set_overflow(gtk4::Overflow::Hidden);
        widgets.drawer_box.set_size_request(-1, 0);
        // set_size_request is a MINIMUM, not a maximum: GTK still allocates a
        // visible box its natural height, and this layer-shell surface has no
        // fixed height, so the window grew to fit the whole results list and
        // the capsule sat open-at-idle showing a stray row. A hidden widget
        // requests no size at all, which is what "collapsed" actually needs.
        // open_fn/close_fn toggle this back on/off around the height animation.
        widgets.drawer_box.set_visible(false);
        if autohide_cfg.enabled {
            // `.wick{gap:10px}`, `.ws{gap:2px}`, `.slot{gap:8px}`; the strip
            // starts collapsed until the first workspace sync fills it.
            center_area.set_spacing(10);
            workspace_box.set_spacing(2);
            for (key, slot) in &widget_containers {
                if key != "tray" {
                    slot.set_spacing(8);
                }
            }
            workspace_trail.overlay.set_visible(false);
            hide_empty_rows(&[&workspace_row, &stats_box]);
        }

        // ── Query-mode results (plan §7 phase 6c: `=` calc, `>` cmd, `.`
        // url) ───────────────────────────────────────────────────────────
        // A second, small list living alongside `launcher_results.scroller`
        // in the same drawer: only one of the two is ever visible at a
        // time (see `connect_changed` below). Built unconditionally, same
        // as `launcher_results` itself, but only ever APPENDED into
        // `drawer_box` for an embedded launcher: every other theme must
        // keep `drawer_box` exactly as childless as it already is (see the
        // comment above `bar_modules.for_each_in_slot(&bar_slots.drawer, ..)`).
        let mode_list = gtk4::ListBox::new();
        mode_list.set_selection_mode(gtk4::SelectionMode::Browse);
        mode_list.set_visible(false);
        if launcher_cfg.mode == bread_theme::shell::LauncherMode::Embedded {
            widgets.drawer_box.append(&mode_list);
        }

        // ── Capsule expand/collapse + search wiring (theme 04/spotlight) ──
        // Effectively a no-op under every other theme: `launcher_entry`
        // never receives focus if it's never in a slot, so `open_fn` is
        // simply never invoked. `results.set_query`/select_next`/`select_prev`
        // and launching all come straight from `bread-launcher`; only the
        // capsule shell (drawer height, entry placeholder/alignment,
        // keyboard focus) is this file's own.
        let anim: Rc<RefCell<Option<gtk4::TickCallbackId>>> = Rc::new(RefCell::new(None));
        // Search-state width (plan §7 phase 6c: `[launcher].search_width`,
        // `04-spotlight.html`'s `.searching .capsule { width: 520px }`);
        // a separate animation from the drawer's own height, both driven
        // by `bread_theme::anim::spring_to` independently.
        let anim_width: Rc<RefCell<Option<gtk4::TickCallbackId>>> = Rc::new(RefCell::new(None));
        let launcher_open: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        // Set around every PROGRAMMATIC `launcher_entry.set_text(...)` call
        // (close_fn's reset-to-empty today; any future one belongs here
        // too) so `connect_changed` below can tell "the user typed a
        // character" from "code changed the buffer" and only ever opens
        // the capsule for the former; see that handler's own comment.
        // Gates the SIGNAL itself rather than hunting every caller that
        // could fire it, so no future programmatic `set_text` can reopen
        // the capsule by accident, whether or not one does today.
        let programmatic_text_change: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        // The click-away scrim's dead zone (item B, see `DRAWER_MAX_HEIGHT_PX`'s
        // own doc comment): the capsule row's own theme-default dismiss
        // offset (52px: `bar.window.height` + `bar.window.margin.top`) plus
        // the drawer's own max content height, so the scrim's clickable
        // region can never reach up into a rendered result row.
        let capsule_dismiss_margin: i32 = theme::shell_theme()
            .surfaces()
            .get("breadbar-dismiss")
            .map(|s| s.offset.first().copied().unwrap_or(0.0) as i32)
            .unwrap_or(52)
            + DRAWER_MAX_HEIGHT_PX;

        let open_fn: Rc<dyn Fn()> = Rc::new({
            let drawer_box = widgets.drawer_box.clone();
            let anim = Rc::clone(&anim);
            let anim_width = Rc::clone(&anim_width);
            let launcher_open = Rc::clone(&launcher_open);
            let entry = launcher_entry.clone();
            let root = root.clone();
            let panels = panels.clone();
            let idle_width = launcher_cfg.width;
            let search_width = launcher_cfg.search_width;
            let monitor_name_for_dismiss = monitor_name.clone();
            move || {
                capsule_trace!("open_fn() called, was_open={}", launcher_open.get());
                if !launcher_open.get() {
                    launcher_open.set(true);
                    // Reveal before measuring/animating: while hidden the box
                    // reports no natural height, so the open animation would
                    // target 0 and nothing would appear.
                    drawer_box.set_visible(true);
                    entry.add_css_class("searching");
                    gtk4::prelude::EntryExt::set_alignment(&entry, 0.0);
                    // `.searching` on the root itself (plan §7 phase 6c):
                    // toggles `[launcher].search_radius` via CSS (see
                    // theme.rs's `window.breadbar.searching` rule) and
                    // marks the width animation's starting point below.
                    root.add_css_class("searching");
                    let current_width = root.width();
                    let from = if current_width > 0 {
                        current_width
                    } else {
                        idle_width
                    };
                    animate_capsule_width(&root, &anim_width, from, search_width);
                    // Item B: click-away scrim. Shown at a fixed dead-zone
                    // offset (`capsule_dismiss_margin`), never the live
                    // drawer height; see that constant's own doc comment
                    // for why a live-tracking offset would risk swallowing
                    // clicks meant for a result row. `hole` scopes that
                    // dead zone to the capsule's own column instead of the
                    // full screen width ("it only sometimes is dismissed
                    // when you click somewhere else"); see
                    // `capsule_dismiss_hole`'s doc comment. Falls back to
                    // the old full-width dead zone if the live geometry
                    // query fails for any reason.
                    let hole =
                        hypr_capsule_center_x(&monitor_name_for_dismiss).and_then(|center_x| {
                            let (origin_x, _) = hypr_monitor_origin(&monitor_name_for_dismiss)?;
                            Some(capsule_dismiss_hole(center_x, origin_x, search_width))
                        });
                    panels.show_capsule_dismiss(capsule_dismiss_margin, hole);
                }
                let target = drawer_target_height(&drawer_box);
                let current = drawer_box.size_request().1;
                animate_drawer_height(&drawer_box, &anim, current, target);
            }
        });
        let close_fn: Rc<dyn Fn()> = Rc::new({
            let drawer_box = widgets.drawer_box.clone();
            let anim = Rc::clone(&anim);
            let anim_width = Rc::clone(&anim_width);
            let launcher_open = Rc::clone(&launcher_open);
            let entry = launcher_entry.clone();
            let root_for_focus = root.clone();
            let root_for_width = root.clone();
            let panels = panels.clone();
            let idle_width = launcher_cfg.width;
            let programmatic_text_change = Rc::clone(&programmatic_text_change);
            move || {
                capsule_trace!("close_fn() called, was_open={}", launcher_open.get());
                if !launcher_open.get() {
                    return;
                }
                launcher_open.set(false);
                entry.remove_css_class("searching");
                gtk4::prelude::EntryExt::set_alignment(&entry, 0.5);
                // Resetting the buffer here is a PROGRAMMATIC change, not a
                // user keystroke: flagged so `connect_changed` below
                // doesn't treat "close_fn cleared the query" as "the user
                // typed", which would immediately reopen what this
                // function is in the middle of closing.
                programmatic_text_change.set(true);
                entry.set_text("");
                programmatic_text_change.set(false);
                if placeholder_clock {
                    entry.set_placeholder_text(Some(&bar::clock::time()));
                }
                root_for_width.remove_css_class("searching");
                let current_width = root_for_width.width();
                animate_capsule_width(&root_for_width, &anim_width, current_width, idle_width);
                panels.hide_dismiss();
                let current = drawer_box.size_request().1;
                animate_drawer_height(&drawer_box, &anim, current, 0);
                // `keyboard = "on_demand"` (plan §7) ties the layer-shell
                // surface's keyboard grab to GTK's own focus-widget state:
                // releasing focus here is what hands the keyboard back.
                gtk4::prelude::GtkWindowExt::set_focus(&root_for_focus, None::<&gtk4::Widget>);
            }
        });

        panels.set_on_dismiss({
            let close_fn = Rc::clone(&close_fn);
            move || close_fn()
        });

        // Which of the four query modes is currently driving `mode_list`
        // (plan §7 phase 6c): read by `key_ctrl` below to route Up/Down/
        // Return at the right list, and whether that mode's row is even
        // selectable (an info-only row, e.g. an empty calc expression,
        // never is). `launcher_cfg.modes` gates which prefixes actually
        // switch mode: a prefix this theme doesn't list in `modes` falls
        // through to a literal Apps query, prefix character and all.
        let active_mode: Rc<Cell<bread_launcher::QueryKind>> =
            Rc::new(Cell::new(bread_launcher::QueryKind::Apps));
        let mode_selectable: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let modes = launcher_cfg.modes.clone();

        {
            let results = launcher_results.clone();
            let mode_list = mode_list.clone();
            let open_fn = Rc::clone(&open_fn);
            let active_mode = Rc::clone(&active_mode);
            let mode_selectable = Rc::clone(&mode_selectable);
            let programmatic_text_change = Rc::clone(&programmatic_text_change);
            launcher_entry.connect_changed(move |entry| {
                // `changed` fires for EVERY buffer mutation, programmatic
                // ones included: `close_fn`'s own `entry.set_text("")`
                // among them. Without this guard that reset re-enters this
                // handler and its unconditional `open_fn()` below reopens
                // the capsule close_fn is still in the middle of closing.
                // This is the fix for "changed a moment ago". Real typing
                // never sets the flag, so it always reaches `open_fn()`.
                if programmatic_text_change.get() {
                    capsule_trace!("connect_changed: skipped (programmatic set_text)");
                    return;
                }
                let text = entry.text();
                capsule_trace!("connect_changed: user input, text={text:?}");
                let parsed = bread_launcher::parse_query(&text);
                let mode_name = match parsed.kind {
                    bread_launcher::QueryKind::Calc => "calc",
                    bread_launcher::QueryKind::Cmd => "cmd",
                    bread_launcher::QueryKind::Url => "url",
                    bread_launcher::QueryKind::Apps => "apps",
                };
                let kind = if modes.iter().any(|m| m == mode_name) {
                    parsed.kind
                } else {
                    bread_launcher::QueryKind::Apps
                };
                active_mode.set(kind);
                if kind == bread_launcher::QueryKind::Apps {
                    mode_list.set_visible(false);
                    results.scroller.set_visible(true);
                    mode_selectable.set(false);
                    results.set_query(text.as_str());
                } else {
                    results.scroller.set_visible(false);
                    let selectable = populate_mode_list(&mode_list, &parsed);
                    mode_selectable.set(selectable);
                    mode_list.set_visible(true);
                }
                // Reached only for a real keystroke (the guard above
                // already returned for a programmatic reset): matches the
                // reference model's `q.addEventListener("input", () => {
                // if (!open) setOpen(true); ... })`: typing is itself a
                // real-user-input signal that opens the capsule.
                open_fn();
            });
        }
        {
            // Deliberately does NOT call `open_fn()`. It used to: that
            // was the actual mechanism behind "opens itself with no
            // focus": ANYTHING that moved keyboard focus onto
            // `launcher_entry`, including GTK's own auto-focus-on-map of
            // the first focusable widget in a window, opened the capsule
            // as a side effect. `set_can_focus(false)` (constructor,
            // above) already closes that specific hole, but leaving this
            // handler wired to `open_fn` kept the same footgun loaded for
            // the next path that calls `grab_focus()` on this entry for
            // any reason. Both real-input paths that DO want to open now
            // call `open_fn()` themselves at their own call site (the
            // click gesture below, `AppInput::OpenLauncher`'s handler):
            // opening is a direct consequence of the user's action, not a
            // side effect of a focus-change event that could have come
            // from anywhere. What's left here is pure diagnostics: under
            // `BREADBAR_CAPSULE_DEBUG=1`, seeing "focus entered" with no
            // preceding "open_fn() called" trace is exactly the signature
            // a future accidental-open regression would leave.
            let focus_ctrl = gtk4::EventControllerFocus::new();
            focus_ctrl.connect_enter(move |_| {
                capsule_trace!("focus_ctrl: entry gained keyboard focus");
            });
            launcher_entry.add_controller(focus_ctrl);
        }
        // "you can't close it using escape unless you are focused on the
        // UI" / "it doesn't grab keyboard for typing": both are the same
        // root cause as the startup-open bug above, from the other side.
        // `launcher_entry.set_can_focus(false)` means nothing (startup or
        // otherwise) can silently steal GTK's own notion of focus any
        // more, so it now needs a real, explicit grab. This click gesture
        // is that grab for the mouse path: a `GestureClick` press is a
        // genuine user-originated pointer event delivered through the
        // compositor, which is also exactly the kind of interaction
        // `KeyboardMode::OnDemand` (gtk4-layer-shell/wlr-layer-shell) is
        // documented to react to: the protocol spec (wlr-layer-shell-
        // unstable-v1.xml) leaves *when* an on-demand surface gets the
        // compositor's keyboard focus as "implementation-defined", but a
        // literal click on the surface is the universal, minimum-common-
        // denominator trigger every compositor implementation reacts to
        // (it's the same interaction wofi/fuzzel/rofi-wayland rely on).
        // Flipping `can_focus` back on right before `grab_focus()` mirrors
        // `AppInput::OpenLauncher`'s handler below, which needs the exact
        // same two-liner for the hotkey/command path.
        {
            let entry_for_click = launcher_entry.clone();
            let open_fn = Rc::clone(&open_fn);
            let click = gtk4::GestureClick::new();
            click.connect_pressed(move |_, _, _, _| {
                capsule_trace!("click gesture: pressed");
                entry_for_click.set_can_focus(true);
                entry_for_click.grab_focus();
                // Called directly here rather than left to fire as a side
                // effect of `grab_focus()` moving keyboard focus (that was
                // `focus_ctrl.connect_enter`'s job until this same pass
                // removed it): a real click is itself the user-input
                // signal that should open the capsule, not merely a way to
                // produce a focus-change event that then does.
                open_fn();
            });
            launcher_entry.add_controller(click);
        }
        {
            let results = launcher_results.clone();
            let mode_list = mode_list.clone();
            let close_fn = Rc::clone(&close_fn);
            let active_mode = Rc::clone(&active_mode);
            let mode_selectable = Rc::clone(&mode_selectable);
            let key_ctrl = gtk4::EventControllerKey::new();
            // CAPTURE, not the default BUBBLE. A GtkEntry handles Return in the
            // target phase itself: it emits `activate` and returns TRUE, which
            // stops propagation before a bubble-phase controller ever runs, so
            // Enter silently did nothing and the selected app never launched.
            // Capturing puts this controller ahead of the entry's own handling
            // for every key it cares about (Return/Up/Down/Escape), and keys it
            // doesn't claim still Proceed to the entry for normal text input.
            key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
            key_ctrl.connect_key_pressed(move |_, key, _, _| {
                use gtk4::gdk::Key;
                if active_mode.get() != bread_launcher::QueryKind::Apps {
                    // Calc/Cmd/Url (plan §7 phase 6c): Up/Down move the
                    // (possibly single-row) `mode_list` selection; Return
                    // runs whatever `mode_row_action` finds on the
                    // selected row: nothing, for a calc result or an
                    // empty prompt, since those rows carry none.
                    return match key {
                        Key::Escape => {
                            close_fn();
                            gtk4::glib::Propagation::Stop
                        }
                        Key::Down if mode_selectable.get() => {
                            listbox_select_next(&mode_list);
                            gtk4::glib::Propagation::Stop
                        }
                        Key::Up if mode_selectable.get() => {
                            listbox_select_prev(&mode_list);
                            gtk4::glib::Propagation::Stop
                        }
                        Key::Return | Key::KP_Enter => {
                            if let Some(row) = mode_list.selected_row() {
                                if let Some(action) = mode_row_action(&row) {
                                    run_mode_action(&action);
                                    close_fn();
                                }
                            }
                            gtk4::glib::Propagation::Stop
                        }
                        _ => gtk4::glib::Propagation::Proceed,
                    };
                }
                match key {
                    Key::Escape => {
                        close_fn();
                        gtk4::glib::Propagation::Stop
                    }
                    Key::Down => {
                        results.select_next();
                        gtk4::glib::Propagation::Stop
                    }
                    Key::Up => {
                        results.select_prev();
                        gtk4::glib::Propagation::Stop
                    }
                    Key::Return | Key::KP_Enter => {
                        if let Some(entry) = results.selected_entry() {
                            results.record_launch(&entry);
                            bread_launcher::do_launch(
                                &entry,
                                LAUNCHER_APP_ID,
                                LAUNCHER_LAUNCHED_EVENT,
                            );
                        }
                        close_fn();
                        gtk4::glib::Propagation::Stop
                    }
                    _ => gtk4::glib::Propagation::Proceed,
                }
            });
            launcher_entry.add_controller(key_ctrl);
        }
        // Click on a mode_list row (a `>`-mode command or the `.`-mode
        // "open this URL" prompt) acts too, same as a result row's click.
        {
            let close_fn = Rc::clone(&close_fn);
            mode_list.connect_row_activated(move |_, row| {
                if let Some(action) = mode_row_action(row) {
                    run_mode_action(&action);
                    close_fn();
                }
            });
        }
        // Row click launches too, same as breadbox's own overlay.
        {
            let results = launcher_results.clone();
            let close_fn = Rc::clone(&close_fn);
            launcher_results.list.connect_row_activated(move |_, row| {
                if let Some(entry) = bread_launcher::gtk::row_entry(row) {
                    results.record_launch(&entry);
                    bread_launcher::do_launch(&entry, LAUNCHER_APP_ID, LAUNCHER_LAUNCHED_EVENT);
                }
                close_fn();
            });
        }

        // Captured before these move into `model` (or are otherwise dropped
        // as bare locals, never stored on `App` at all): needed by the
        // screenshot dispatch just before this function returns.
        let control_panel_for_screenshot = panels.control.clone();
        let connectivity_panel_for_screenshot = panels.connectivity.clone();
        let wifi_tab_btn_for_screenshot = wifi_tab_btn.clone();
        let bt_tab_btn_for_screenshot = bt_tab_btn.clone();
        let media_panel_for_screenshot = panels.media.clone();
        let media_widget_for_screenshot = media_widget.clone();
        let media_track_lbl_for_screenshot = media_track_lbl.clone();
        // Theme 04/spotlight's capsule (plan §6b): `launcher_entry` moves
        // into `model` below, `drawer_box` lives only in `widgets`: both
        // need a clone out here for the same reason every other
        // `_for_screenshot` handle above does.
        let launcher_entry_for_screenshot = launcher_entry.clone();
        let drawer_box_for_screenshot = widgets.drawer_box.clone();

        // Never launch sibling App windows from inside this init: RelmApp
        // is still in GApplication activate, and a same-type launch here
        // creates a second primary on the laptop. Idle reconcile after.
        let satellites = Vec::new();
        if init.primary && screenshot_req.is_none() {
            let later = sender.clone();
            gtk4::glib::idle_add_local_once(move || {
                post(&later, AppInput::ReconcileMonitors);
            });
        }

        let notif_sample = screenshot_req.as_ref().and_then(|r| match r.view.as_str() {
            "notification" => Some(notifications::SampleKind::Normal),
            "notification-critical" => Some(notifications::SampleKind::Critical),
            "notification-ram" => Some(notifications::SampleKind::Ram),
            _ => None,
        });
        // A screenshot run must never touch the real session bus: zbus's
        // default name flags are AllowReplacement|ReplaceExisting, so a
        // capture process claiming org.freedesktop.Notifications would
        // silently take the name from the live breadbar, and the live one
        // (DoNotQueue) never gets it back once the capture exits.
        // bread-capture isolates Wayland but not D-Bus, so only the
        // notification views (whose sample path never connects to D-Bus)
        // build the notification server here.
        let claim_notifications = screenshot_req.is_none() || notif_sample.is_some();
        let (notification_window, notif_handle, notif_store) =
            if init.primary && claim_notifications {
                // BAR-27 phase 1: `breadbar --pill toggle|show` reaches this
                // instance over `dev.breadway.Bar` (same surface as
                // `--history`), forwarded here rather than through the
                // notification event loop, which owns that D-Bus connection but
                // has nothing to do with the pill itself. BAR-30 phase 2:
                // `breadbar --stage toggle|show|hide` are forwarded the same way.
                let (pill_cmd_tx, mut pill_cmd_rx) =
                    tokio::sync::mpsc::unbounded_channel::<notifications::PillCmd>();
                let pill_cmd_sender = sender.clone();
                relm4::spawn_local(async move {
                    while let Some(cmd) = pill_cmd_rx.recv().await {
                        match cmd {
                            notifications::PillCmd::Toggle => {
                                post(&pill_cmd_sender, AppInput::PillToggleCmd)
                            }
                            notifications::PillCmd::Show => {
                                post(&pill_cmd_sender, AppInput::PillShowCmd)
                            }
                        }
                    }
                });
                let (stage_cmd_tx, mut stage_cmd_rx) =
                    tokio::sync::mpsc::unbounded_channel::<notifications::StageCmd>();
                let stage_cmd_sender = sender.clone();
                relm4::spawn_local(async move {
                    while let Some(cmd) = stage_cmd_rx.recv().await {
                        match cmd {
                            notifications::StageCmd::Toggle => {
                                post(&stage_cmd_sender, AppInput::StageToggleCmd)
                            }
                            notifications::StageCmd::Show => {
                                post(&stage_cmd_sender, AppInput::StageShowCmd)
                            }
                            notifications::StageCmd::Hide => {
                                post(&stage_cmd_sender, AppInput::StageHideCmd)
                            }
                        }
                    }
                });
                let notify_peek_sender = sender.clone();
                let on_show: Option<notifications::OnShow> =
                    Some(Box::new(move |app, summary, critical| {
                        post(
                            &notify_peek_sender,
                            AppInput::NotificationArrived {
                                app: app.to_string(),
                                summary: summary.to_string(),
                                critical,
                            },
                        );
                    }));
                let (win, handle, store) =
                    notifications::spawn(notif_sample, on_show, pill_cmd_tx, stage_cmd_tx);
                (Some(win), handle, store)
            } else {
                (None, None, notifications::history::new_store())
            };

        // Right-click the hamburger toggles the notification-history window
        // (left-click still opens the control panel, wired above). This is
        // the no-terminal equivalent of `breadbar --history`; it reaches
        // the same event loop in-process rather than over `dev.breadway.Bar`.
        if let Some(handle) = &notif_handle {
            let secondary = gtk4::GestureClick::new();
            secondary.set_button(gtk4::gdk::BUTTON_SECONDARY);
            let handle = handle.clone();
            secondary.connect_released(move |g, _, _, _| {
                g.set_state(gtk4::EventSequenceState::Claimed);
                handle.toggle_history();
            });
            hamburger_btn.add_controller(secondary);
            hamburger_btn.set_tooltip_text(Some("Control panel (right-click: notifications)"));
        }
        let osd_sample = screenshot_req.as_ref().and_then(|r| match r.view.as_str() {
            "osd-volume" => Some(osd::SampleKind::Volume),
            "osd-brightness" => Some(osd::SampleKind::Brightness),
            _ => None,
        });
        let osd_window = if init.primary {
            let osd_cfg = theme::shell_theme().osd().clone();
            let level_peek_sender = sender.clone();
            let on_change: Option<Box<dyn Fn(&'static str, u8)>> =
                Some(Box::new(move |kind, pct| {
                    post(&level_peek_sender, AppInput::LevelChanged { kind, pct });
                }));
            Some(osd::spawn(
                osd_sample,
                &osd_cfg.enabled,
                osd_cfg.dismiss_ms.max(1) as u64,
                on_change,
            ))
        } else {
            None
        };

        // BAR-30 phases 2-4: the stage and (phase 4) the surtitle caption.
        // Built unconditionally on every instance (primary and satellite
        // alike), same as `panel::PanelSet`/`pill_hotzone`: `resolve_
        // launcher_route` (see `AppInput::StageToggleCmd` etc.) is what
        // decides which one's window actually opens. BAR-31: every instance
        // shares the primary's `stage::Shared` (history store, DND, which
        // stage is open, every pill's unread dot), handed to satellites
        // through `BarInit`, so a satellite's stage shows the real history.
        let shared = init
            .shared
            .clone()
            .unwrap_or_else(|| stage::Shared::new(notif_store.clone()));
        if init.primary && screenshot_req.is_none() {
            let prefs = notifications::preferences::load();
            shared.paint_dnd(notifications::preferences::effective_dnd(
                &prefs,
                notifications::preferences::clock_minute(),
            ));
            let provider = gtk4::CssProvider::new();
            provider.load_from_string(&notifications::preferences::text_css());
            if let Some(display) = gtk4::gdk::Display::default() {
                gtk4::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk4::STYLE_PROVIDER_PRIORITY_USER,
                );
            }
            let mut previous_css = notifications::preferences::text_css();
            let shared_policy = shared.clone();
            gtk4::glib::timeout_add_seconds_local(2, move || {
                let css = notifications::preferences::text_css();
                if css != previous_css {
                    provider.load_from_string(&css);
                    previous_css = css;
                }
                let prefs = notifications::preferences::load();
                shared_policy.refresh_all_notifications();
                shared_policy.paint_dnd(notifications::preferences::effective_dnd(
                    &prefs,
                    notifications::preferences::clock_minute(),
                ));
                gtk4::glib::ControlFlow::Continue
            });
        }
        let dnd = shared.dnd.clone();
        let stage = stage::build(sender.clone(), shared.clone());
        let surtitle = surtitle::spawn();
        if init.primary {
            *shared.surtitle.borrow_mut() = Some(surtitle.clone());
        }
        shared.register_unread_dot(pill_unread.upcast_ref());
        shared.register_dnd_mark(pill_dnd.upcast_ref());
        // Under spotlight, `breadbar --history` opens the stage, whose
        // notifications zone is the history (BAR-45).
        if init.primary && screenshot_req.is_none() {
            notifications::set_history_opens_stage(
                theme::shell_theme().notifications().style == "surtitle",
            );
        }

        let mut model = App {
            monitor: monitor_name,
            primary: init.primary,
            satellites,
            workspaces: vec![],
            active_ws: 1,
            workspace_box,
            workspace_trail,
            button_map: std::collections::HashMap::new(),
            time_str: bar::clock::current(),
            clock_digits,
            date_lbl,
            clock_plain_lbl,
            launcher_entry,
            launcher_open,
            launcher_open_fn: open_fn,
            system_stats_box,
            system_sep,
            cpu_pair,
            mem_pair,
            pwr_pair,
            gpu_pair,
            cpu_lbl,
            mem_lbl,
            pwr_lbl,
            bar_cpu_lbl,
            bar_ram_lbl,
            gpu_lbl,
            ram_seen_at_ms: 0,
            ram_polls_since_digest: 0,
            ram_mem_base: String::new(),
            ram_prev_pressure: bar::ram::Pressure::Unknown,
            ram_prev_present: false,
            ram_live: screenshot_req.is_none(),
            ram_notif: notif_handle.clone(),
            ram_store: notif_store,
            ram_panel_status_lbl,
            ram_panel_mem_lbl,
            ram_pause_btn,
            ram_pause_lbl,
            ram_paused: false,
            vol_lbl,
            vol_digits,
            bat_lbl,
            bat_digits,
            bat_img,
            ac_img,
            bt_img,
            wifi_lbl,
            wifi_img,
            wifi_pane,
            crumbs_status: None,
            wifi_popover_data: None,
            wifi_profile: None,
            current_ssid: "—".to_string(),
            bt_pane,
            bt_popover_data: None,
            media_widget,
            media_track_lbl,
            media_play_icon,
            media_last: None,
            media_paused_at: None,
            panel_vol_slider,
            panel_bright_slider,
            panel_loading,
            sink_box,
            sink_section,
            tray_section,
            tray_sep,
            tray_box,
            tray_count_lbl,
            tray_items: std::collections::HashMap::new(),
            widget_containers,
            prefs: if autohide_cfg.enabled {
                spotlight_prefs::load()
            } else {
                spotlight_prefs::Prefs::default()
            },
            _prefs_monitor: if autohide_cfg.enabled && screenshot_req.is_none() {
                watch_prefs(&sender)
            } else {
                None
            },
            dropped_widget_warned: std::collections::HashSet::new(),
            widget_tray_section,
            widget_tray_sep,
            daemon_widget_specs: Vec::new(),
            theme_widgets: Vec::new(),
            theme_widget_values: std::collections::HashMap::new(),
            theme_widget_pollers: Vec::new(),
            panels,
            root: root.clone(),
            bar_modules,
            workspace_row: workspace_row.clone(),
            center_area: center_area.clone(),
            stats_box: stats_box.clone(),
            drawer_box: widgets.drawer_box.clone(),
            autohide_cfg,
            pill_hovering: false,
            pill_hold_until: None,
            pill_hover_gen: 0,
            // Assumed shown at construction (the window is placed at its
            // theme-configured margin by `apply_window_spec` above); the
            // `update_pill` call right after this literal reconciles it
            // against the real state immediately, animating a tuck-in on
            // startup if nothing keeps it down.
            pill_shown: true,
            pill_low_battery: false,
            pill_hotzone,
            pill_lip,
            pill_flag,
            pill_flag_label,
            pill_unread,
            pill_nowplaying,
            pill_nowplaying_title,
            pill_dnd,
            pill_battery,
            pill_battery_fill,
            pill_battery_pct,
            battery: None,
            nowplaying: None,
            pill_sep,
            pill_margin: Rc::new(Cell::new(if autohide_cfg.enabled {
                pill_margins(&window_spec).0
            } else {
                window_spec.margin.top
            })),
            pill_anim: Rc::new(RefCell::new(None)),
            pill_level_box,
            pill_level_name,
            pill_level_bar,
            pill_level_pct,
            pill_level_token: Rc::new(Cell::new(0)),
            pill_level_anim: Rc::new(RefCell::new(None)),
            pill_leveled: Rc::new(RefCell::new(Vec::new())),
            stage,
            surtitle,
            dnd,
            shared,
        };
        // Saved stage settings (auto-hide, widgets, workspace view); this also
        // runs the initial `update_pill`.
        if model.autohide_cfg.enabled {
            model.apply_prefs();
        } else {
            model.update_pill();
        }

        theme::apply();
        theme::bind_output(&root, &model.monitor);
        // Skipped for "pill-ws-*": those views mock `self.workspaces`
        // directly below (the isolated capture session has no Hyprland IPC
        // for a real sync to report anyway, but its own empty-result
        // `AppInput::WorkspaceSync` would still land after the mock and
        // wipe it, same race as the widgets/battery views above).
        if !screenshot_req
            .as_ref()
            .is_some_and(|r| r.view.starts_with("pill-ws-") || r.view.starts_with("stage-"))
        {
            bar::workspaces::spawn_watcher(sender.clone());
        }
        bar::clock::spawn_ticker(sender.clone());
        // Skipped for "pill-low-battery": that view forces the pill's flag
        // state directly (there's no `Option<SampleKind>`-style bypass for
        // the real `/sys/class/power_supply` poll the way notifications/OSD
        // have), and a real battery reading racing in during the capture's
        // settle delay would otherwise clobber the forced state right back
        // to whatever this machine's actual battery/AC status is.
        // BAR-31: likewise skipped for the stage and surtitle views, whose
        // sample battery, network and media data (the mockup's own) a real
        // reading would otherwise overwrite mid-capture.
        let sampled_view = screenshot_req.as_ref().is_some_and(|r| {
            r.view == "pill-low-battery"
                || r.view.starts_with("stage-")
                || r.view.starts_with("surtitle")
        });
        if !sampled_view {
            bar::stats::spawn_poller(sender.clone());
            bar::wifi::spawn_status_poller(sender.clone());
            bar::media::spawn_poller(sender.clone());
        }
        // The RAM poller never runs in a screenshot capture: it would dial
        // the live daemon socket, racing real state into the capture (and
        // touching the session the capture must leave alone). RAM views
        // force their snapshot below instead.
        if screenshot_req.is_none() {
            bar::ram::spawn_poller(sender.clone());
        }
        if init.primary {
            // Same D-Bus name-stealing hazard as the notification server
            // above: a screenshot run must not take
            // org.kde.StatusNotifierWatcher from the live bar.
            if screenshot_req.is_none() {
                bar::tray::spawn_watcher(sender.clone());
            }
            // Skipped for "pill-widgets-on": that view mocks its own
            // sample widgets directly into the left_of_clock/right_of_clock
            // containers below (there's no `Option<SampleKind>`-style
            // bypass for the real Lua-widget daemon connection the way
            // notifications/OSD have); a real daemon happening to be
            // reachable from the capture session would otherwise race
            // that mock and wipe it the moment its own (empty, for
            // spotlight's placements) update arrived.
            if !screenshot_req
                .as_ref()
                .is_some_and(|r| r.view == "pill-widgets-on" || r.view.starts_with("stage-"))
            {
                widgets::client::spawn(sender.clone());
            }
            // `[[bar.widget]]`: theme-declared live widgets. Primary only,
            // matching the Lua-widget path (`widgets::client::spawn` above);
            // the containers exist on every bar from `assemble_bar_slots`,
            // but only the primary polls and renders.
            model.theme_widgets = theme::shell_theme().widgets().to_vec();
            model.theme_widget_pollers =
                theme_widgets::spawn_pollers(&model.theme_widgets, &sender);
            model.reconcile_all_widgets();
            // `bread.command.box.open` (plan §7 phase 6c): one subscriber,
            // same reasoning as `widgets::client::spawn` above: a keybind
            // should focus ONE capsule, not every satellite monitor's.
            // A no-op call under every theme but spotlight (see the
            // module's own doc comment).
            launcher_command::spawn(sender.clone());
            // Live theme.toml/extra.css/`active` reload (plan §10). One watch
            // per process, so only the primary arms it; a CSS/geometry/slots
            // change routes back here as `ShellThemeChanged` (the primary
            // forwards it to every satellite).
            let watch_sender = sender.clone();
            theme::watch_hot_reload(move |theme| {
                post(&watch_sender, AppInput::ShellThemeChanged(Box::new(theme)));
            });
        }

        if let Some(req) = screenshot_req {
            let notification_window = notification_window.filter(|_| {
                matches!(
                    req.view.as_str(),
                    "notification" | "notification-critical" | "notification-ram"
                )
            });
            let osd_window =
                osd_window.filter(|_| matches!(req.view.as_str(), "osd-volume" | "osd-brightness"));
            // BAR-27 phase 1: force the spotlight pill into the exact state
            // each named view wants, deterministically, rather than racing
            // the real hover/timer machinery in an isolated capture with no
            // real pointer. Every `pill-*` view except "pill-tucked" itself
            // wants the pill visibly down (there's nothing to see of the
            // workspace/widget/flag states otherwise); "hovering" is the
            // mechanism, since there is no real pointer here to trigger the
            // mockup's own `.wick:hover` background shade, so phase 1's
            // capture of "hovering" and "shown some other way" render
            // pixel-identical anyway (see the task report).
            if (req.view.starts_with("pill-")
                && req.view != "pill-tucked"
                && req.view != "pill-tucked-input")
                || req.view == "ram-bar"
            {
                model.pill_hovering = true;
            }
            if req.view.as_str() == "pill-tucked-input" {
                if let Some(debug) = &pill_hotzone_debug {
                    pill_hotzone::set_debug_visible(debug, true);
                }
            }
            // RAM views force a pressured snapshot through the same
            // handler live polls take: no daemon is dialed in an
            // isolated capture, and notifications stay off (`ram_live`
            // is false) so nothing pops up or pollutes history.
            if req.view.as_str() == "ram-bar" || req.view.as_str() == "ram-panel" {
                let paused = req.view.as_str() == "ram-panel";
                model.apply_ram_snapshot(bar::ram::RamSnapshot {
                    present: true,
                    pressure: bar::ram::Pressure::Pressured,
                    mode: if paused {
                        bar::ram::RamMode::Monitor
                    } else {
                        bar::ram::RamMode::Automatic
                    },
                    available_bytes: Some(3_221_225_472),
                    total_bytes: Some(16_106_127_360),
                    history: vec![bar::ram::RamAction {
                        scope: "unit:app.service".to_string(),
                        kind: "cgroup_soft_limit".to_string(),
                        state: "confirmed".to_string(),
                        reason: "over budget".to_string(),
                        requested_bytes: Some(-20_971_520),
                        observed_bytes: Some(-4_194_304),
                        at_ms: 1_000,
                    }],
                });
            }
            if req.view.as_str() == "pill-low-battery" {
                model.pill_low_battery = true;
                model.pill_flag.set_visible(true);
                model.pill_flag_label.set_label("12%");
                model.battery = Some(PillBattery {
                    pct: 12,
                    charging: false,
                    low: true,
                    minutes_left: Some(25),
                });
                model.refresh_battery();
            }
            if req.view.starts_with("pill-ws-") || req.view.starts_with("stage-") {
                // The isolated capture session has no Hyprland IPC (same
                // gap `bar::workspaces::dot_width_index`'s own doc comment
                // notes for window counts), so `self.workspaces` would
                // otherwise stay empty forever and every `display` mode
                // would render identically ("nothing"). Mock a small,
                // mixed occupied/empty set (matching the mockup's own
                // sample data shape: an active workspace, an empty one,
                // and another occupied one) so the three modes actually
                // differ in the capture.
                let mon = model.monitor.clone();
                let fake = |id: WorkspaceId, windows: u16| Workspace {
                    id,
                    name: id.to_string(),
                    monitor: mon.clone(),
                    monitor_id: None,
                    windows,
                    fullscreen: false,
                    last_window: Address::new("0"),
                    last_window_title: String::new(),
                };
                // The mockup's own `WS=[3,1,0,2,1]` with the first active.
                model.workspaces = vec![fake(1, 3), fake(2, 1), fake(3, 0), fake(4, 2), fake(5, 1)];
                model.active_ws = 1;
                model.rebuild_buttons(false);
            }
            if req.view.as_str() == "pill-widgets-on" {
                // The bread daemon has no widgets in an isolated capture;
                // the mockup's two examples stand in, through the same
                // reconcile path real widgets take.
                model.daemon_widget_specs = screenshot_sample_widgets();
                model.reconcile_all_widgets();
            }
            model.update_pill();
            if req.view.as_str() == "pill-level" {
                // Through the real level path, once the main loop runs
                // (the pill row is only complete then); the capture fires
                // 300ms after map, well inside the 1.4s level peek.
                let s = sender.clone();
                gtk4::glib::idle_add_local_once(move || {
                    post(
                        &s,
                        AppInput::LevelChanged {
                            kind: "VOL",
                            pct: 65,
                        },
                    );
                });
            }
            // BAR-30 phases 2-4: force the stage/surtitle into the exact
            // state each named view wants. Deferred to an idle callback
            // scheduled AFTER `screenshot::dispatch` below registers its own
            // `connect_map` watcher: unlike `root` (whose first real map is
            // deferred by relm4 itself until after `init()` returns) or the
            // notification/OSD windows (whose sample `Show`/OSD event only
            // reaches `popup::run`/`run_osd`'s loop once the async executor
            // actually starts pumping, likewise post-`init()`), the stage
            // and surtitle windows are plain `gtk4::Window`s this code calls
            // `present()`/`show()` on directly. Doing that SYNCHRONOUSLY
            // inside `init()`, before `dispatch` had a chance to attach its
            // `connect_map` handler, let GTK map (and therefore emit `map`
            // on) the surface before anything was listening: every
            // `stage-*`/"surtitle" capture hung to `bread-capture`'s 10s
            // timeout with no error. Queuing the actual open/show here,
            // after `dispatch` runs, closes that race. Sample notification/
            // window/media data is still set eagerly (no visibility change,
            // so no race), matching `Stage::open_for_screenshot`'s own
            // contract of never kicking off the real pollers itself.
            let view = req.view.clone();
            if view.starts_with("stage-") {
                // The mockup's own sample data, so the capture compares
                // against it like for like. Every real poller that could
                // overwrite it is skipped for these views (see the poller
                // spawns above).
                model.stage.set_media(&bar::media::MediaState {
                    title: "Sample Track".into(),
                    artist: "Example Artist".into(),
                    playing: true,
                    has_player: true,
                    player: "Spotify".into(),
                    art: None,
                });
                model
                    .stage
                    .set_control_data(&bar::control::ControlPanelData {
                        volume: 0.6,
                        brightness: 0.8,
                        sinks: vec![
                            bar::control::AudioSink {
                                name: "sample-buds".into(),
                                description: "All Buds".into(),
                                is_default: true,
                            },
                            bar::control::AudioSink {
                                name: "sample-speakers".into(),
                                description: "Speakers".into(),
                                is_default: false,
                            },
                        ],
                    });
                model.stage.set_wifi_radio(true);
                model.stage.set_bluetooth(&bar::bluetooth::BtPopoverData {
                    powered: false,
                    devices: vec![],
                });
                model.stage.set_windows(vec![
                    stage::pure::OpenWindow {
                        address: "0x1".into(),
                        title: "breadboard.breadway.dev: ECO board".into(),
                        app: "firefox".into(),
                        workspace: 1,
                    },
                    stage::pure::OpenWindow {
                        address: "0x2".into(),
                        title: "~/Projects/breadbar".into(),
                        app: "kitty".into(),
                        workspace: 1,
                    },
                    stage::pure::OpenWindow {
                        address: "0x3".into(),
                        title: "main.rs: breadbar".into(),
                        app: "code".into(),
                        workspace: 4,
                    },
                    stage::pure::OpenWindow {
                        address: "0x4".into(),
                        title: "Sample Track \u{00b7} Example Artist".into(),
                        app: "spotify".into(),
                        workspace: 2,
                    },
                    stage::pure::OpenWindow {
                        address: "0x5".into(),
                        title: "Arch Wiki: Hyprland".into(),
                        app: "firefox".into(),
                        workspace: 1,
                    },
                ]);
                model.stage.seed_sample_apps();
                model
                    .stage
                    .set_status(Some(83), false, Some(300), "Home Wi-Fi");
                model.stage.set_time(&bar::clock::time());
                model.stage.seed_sample_notifications();
                // The pill carries the mockup's two example widgets, which
                // the reconcile also mirrors into the stage's cards.
                model.daemon_widget_specs = screenshot_sample_widgets();
                model.reconcile_all_widgets();
                // The pill stays down (as it does whenever a stage is open)
                // rather than racing its tuck animation into the capture.
                model.pill_hovering = true;
                model.update_pill();
            }
            let surtitle_sender = sender.clone();
            if req.view.as_str() == "pill-hover" {
                // No real pointer in the capture: put the pill in its hover
                // state, as `.wick:hover` would.
                if let Some(pill) = root.child() {
                    pill.set_state_flags(gtk4::StateFlags::PRELIGHT, false);
                }
            }
            let stage_for_screenshot = model.stage.clone();
            let surtitle_for_screenshot = model.surtitle.clone();
            screenshot::dispatch(
                &root,
                req,
                screenshot::Handles {
                    control_panel: control_panel_for_screenshot,
                    connectivity_panel: connectivity_panel_for_screenshot,
                    wifi_tab_btn: wifi_tab_btn_for_screenshot,
                    bt_tab_btn: bt_tab_btn_for_screenshot,
                    media_panel: media_panel_for_screenshot,
                    media_widget: media_widget_for_screenshot,
                    media_track_lbl: media_track_lbl_for_screenshot,
                    notification_window,
                    osd_window,
                    launcher_entry: launcher_entry_for_screenshot,
                    drawer_box: drawer_box_for_screenshot,
                    stage_window: model.stage.window.clone(),
                    surtitle_window: model.surtitle.window_for_screenshot(),
                },
            );
            if view.starts_with("stage-") || view.starts_with("surtitle") {
                gtk4::glib::idle_add_local_once(move || match view.as_str() {
                    "stage-results" => {
                        stage_for_screenshot.open_for_screenshot();
                        stage_for_screenshot.force_query("fire");
                    }
                    "stage-commands" => {
                        stage_for_screenshot.open_for_screenshot();
                        stage_for_screenshot.force_query(">");
                    }
                    "stage-calc" => {
                        stage_for_screenshot.open_for_screenshot();
                        stage_for_screenshot.force_query("=2+2");
                    }
                    "stage-power-armed" => {
                        stage_for_screenshot.open_for_screenshot();
                        stage_for_screenshot.force_power_press("restart");
                    }
                    "stage-narrow" => {
                        stage_for_screenshot.open_for_screenshot();
                        stage_for_screenshot.force_narrow(true);
                    }
                    "stage-settings" => {
                        stage_for_screenshot.open_for_screenshot();
                        stage_for_screenshot.force_settings_open();
                    }
                    "stage-empty" => {
                        stage_for_screenshot.open_for_screenshot();
                        stage_for_screenshot.force_query("zzz");
                    }
                    "stage-note-hover" => {
                        stage_for_screenshot.open_for_screenshot();
                        stage_for_screenshot.force_note_hover();
                    }
                    "stage-idle" => stage_for_screenshot.open_for_screenshot(),
                    // Through the real arrival path, so the pill peeks and
                    // lights its unread dot as it does live.
                    "surtitle" => post(
                        &surtitle_sender,
                        AppInput::NotificationArrived {
                            app: "kitty".into(),
                            summary: "Task complete".into(),
                            critical: false,
                        },
                    ),
                    "surtitle-critical" => surtitle_for_screenshot.show(
                        "power",
                        "Battery at 5%. Plug in now or the machine will suspend.",
                        true,
                        std::time::Duration::from_secs(30),
                    ),
                    _ => {}
                });
            }
        }

        ComponentParts { model, widgets }
    }

    /// Runs when this bar's `Controller` is dropped (a satellite whose monitor
    /// went away, see `drop_satellite`). Only the root bar window is hidden
    /// there; the satellite's other layer surfaces would otherwise outlive it
    /// with signal handlers still aimed at the shut-down runtime.
    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        if let Some(hz) = &self.pill_hotzone {
            hz.destroy();
        }
        self.shared
            .unregister_unread_dot(self.pill_unread.upcast_ref());
        self.shared.unregister_dnd_mark(self.pill_dnd.upcast_ref());
        self.stage.window.destroy();
        self.surtitle.destroy();
        self.panels.destroy();
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AppInput::WorkspaceSync {
                workspaces,
                actives,
            } => {
                let mut sorted = workspaces;
                sorted.sort_by_key(|w| w.id);
                let new_active = actives
                    .get(&self.monitor)
                    .copied()
                    .unwrap_or(self.active_ws);
                // Workspace also carries last-window title/address, which
                // change constantly. Only the visible row (id/name/monitor/
                // occupied) should rebuild the pills: otherwise a title
                // flicker on switch cancels the trail mid-stretch.
                let rows_changed = visible_ws_rows(&sorted, &self.monitor, new_active)
                    != visible_ws_rows(&self.workspaces, &self.monitor, self.active_ws);
                let active_changed = new_active != self.active_ws;
                // BAR-27 phase 1: a workspace switch peeks the pill down
                // (`bar.autohide.workspace_peek_ms`); a no-op while
                // auto-hide is off.
                if active_changed {
                    let ms = self.autohide_cfg.workspace_peek_ms.max(0) as u64;
                    self.peek(std::time::Duration::from_millis(ms), &sender);
                }
                self.workspaces = sorted;
                if self.primary {
                    self.reconcile_satellites();
                }
                if rows_changed {
                    self.active_ws = new_active;
                    self.rebuild_buttons(active_changed);
                } else if active_changed {
                    let from = self.button_map.get(&self.active_ws).cloned();
                    if let Some(old) = &from {
                        old.remove_css_class("active");
                    }
                    self.active_ws = new_active;
                    if let Some(btn) = self.button_map.get(&self.active_ws).cloned() {
                        btn.add_css_class("active");
                        // Trail style only: pill/dots never call place()/
                        // stretch() at all: the "active" CSS class above is
                        // the whole of their active-workspace treatment
                        // (solid accent fill, no trail overlay).
                        if theme::shell_theme().modules().workspaces.style == WorkspaceStyle::Trail
                        {
                            self.workspace_trail.stretch(from.as_ref(), &btn);
                        }
                    }
                }
            }
            AppInput::MonitorAdded(name) => {
                if !self.primary || name == self.monitor {
                    return;
                }
                if self.satellites.iter().any(|(n, _)| n == &name) {
                    return;
                }
                if let Some(ctrl) = spawn_satellite(&name, self.shared.clone()) {
                    ctrl.sender()
                        .emit(AppInput::MirrorWidgets(self.mirrored_specs()));
                    self.satellites.push((name, ctrl));
                }
            }
            AppInput::MonitorRemoved(name) => {
                drop_satellite(&mut self.satellites, &name);
            }
            AppInput::ClockTick => {
                self.time_str = bar::clock::current();
                self.date_lbl.set_label(&bar::clock::date());
                self.stage.set_time(&bar::clock::time());
                let clock_module = theme::shell_theme().modules().clock.clone();
                match clock_module.style {
                    // Plain (glass-workbench): one label, no flip animation:
                    // `flip_clock_digits` would just be wasted work (and
                    // a pointless 450ms `play_once` timer) on digits that
                    // are never on screen.
                    ClockStyle::Plain => {
                        self.clock_plain_lbl
                            .set_label(&bar::clock::formatted(&clock_module.format));
                    }
                    // Flip (default, liquid-motion) and None both keep
                    // exactly today's per-digit-flip update: None has no
                    // module in a slot to display it, but there's no reason
                    // to special-case skipping the (cheap, idempotent) work.
                    ClockStyle::Flip | ClockStyle::None => {
                        flip_clock_digits(&self.clock_digits, &bar::clock::time());
                    }
                }
                // `modules.clock.placeholder_clock` (spotlight): the
                // capsule's entry IS the clock until focused: matches the
                // demo's own `if (!open) q.placeholder = t;` guard so a
                // live search in progress never has its placeholder text
                // (invisibly, since real text covers it) stomped mid-type.
                if clock_module.placeholder_clock && !self.launcher_open.get() {
                    self.launcher_entry
                        .set_placeholder_text(Some(&bar::clock::time()));
                }
            }
            AppInput::StatsUpdate(stats) => {
                self.stage.set_system(&stats);
                let cpu = match stats.cpu_temp {
                    Some(t) => format!("{} · {:.0}°", stats.cpu, t),
                    None => stats.cpu,
                };
                self.cpu_lbl.set_label(&cpu);
                self.mem_lbl.set_label(&stats.mem);
                self.pwr_lbl.set_label(&stats.power);
                // `[bar.slots].right = [..., "cpu", "ram", ...]` (glass-
                // workbench): same formatted text, separate chip instances
                // (see the App struct field docs for why).
                self.bar_cpu_lbl.set_label(&cpu);
                self.bar_ram_lbl.set_label(&stats.mem);
                self.ram_mem_base = stats.mem.clone();
                self.sync_ram_chip();
                match stats.gpu_usage {
                    Some(g) => {
                        let gpu = match stats.gpu_temp {
                            Some(t) => format!("{g}% · {t:.0}°"),
                            None => format!("{g}%"),
                        };
                        self.gpu_lbl.set_label(&gpu);
                        self.gpu_pair.set_visible(true);
                    }
                    None => self.gpu_pair.set_visible(false),
                }
                self.system_sep.set_visible(false);

                flip_digit_chip(
                    &self.vol_lbl,
                    &mut self.vol_digits.borrow_mut(),
                    &stats.volume_pct.to_string(),
                );
                self.vol_lbl
                    .set_tooltip_text(Some(&format!("volume {}%", stats.volume_pct)));
                flip_digit_chip(&self.bat_lbl, &mut self.bat_digits.borrow_mut(), &stats.bat);
                set_svg_icon(&self.bat_img, stats.bat_icon);
                let bat_tip = if stats.ac_connected {
                    format!("{}% · charging", stats.bat)
                } else {
                    format!("{}%", stats.bat)
                };
                self.bat_img.set_tooltip_text(Some(&bat_tip));
                self.ac_img.set_visible(false);

                // BAR-27 phase 1: the spotlight pill's low-battery flag.
                // Charging counts as "not low": the flag is a "plug in
                // soon" warning, meaningless with a charger already
                // connected.
                let low = !stats.ac_connected && bar::stats::is_low_battery(stats.bat_pct);
                self.battery = stats.bat_pct.map(|pct| PillBattery {
                    pct,
                    charging: stats.ac_connected,
                    low,
                    minutes_left: stats.bat_minutes_left,
                });
                // The battery item, when shown, carries the low warning
                // itself; the flag is for when it is off. Visibility and
                // label both live in `sync_pill_flag`, which also carries
                // the pressure flag, so neither poller fights the other.
                self.refresh_battery();
                if low != self.pill_low_battery {
                    self.pill_low_battery = low;
                    self.update_pill();
                }
                set_svg_icon(&self.bt_img, stats.bt_icon);
                self.current_ssid = stats.wifi_ssid.clone();
                if stats.wifi_profile.is_some() {
                    self.wifi_profile = stats.wifi_profile;
                }
                self.apply_wifi_label();
                self.stage.set_status(
                    stats.bat_pct,
                    low,
                    stats.bat_minutes_left,
                    &self.current_ssid,
                );
                self.stage.set_wifi_radio(stats.wifi_radio);
                let internet_ok = self
                    .crumbs_status
                    .as_ref()
                    .map(|s| s.internet && !s.captive_portal)
                    .unwrap_or(true);
                let icon = if !internet_ok && stats.wifi_ssid != "—" {
                    bar::stats::WIFI_ICON_OFF
                } else {
                    stats.wifi_icon
                };
                self.wifi_img.set_icon_name(Some(icon));
            }
            AppInput::RamUpdate(snapshot) => {
                self.apply_ram_snapshot(snapshot);
            }
            AppInput::RamPauseToggle => {
                let paused = !self.ram_paused;
                bar::ram::spawn_set_mode(paused, sender.clone());
            }
            AppInput::RamModeSet { paused, ok } => {
                if !ok {
                    self.ram_panel_status_lbl
                        .set_label("breadram: pause refused");
                    return;
                }
                self.ram_paused = paused;
                self.sync_ram_pause_button();
            }
            AppInput::TrayUpdate(bar::tray::TrayUpdate::Add { id, icon, title }) => {
                // Only the primary watches the tray; every monitor's stage
                // shows it as Background apps.
                if self.primary {
                    for (_, ctrl) in &self.satellites {
                        ctrl.sender()
                            .emit(AppInput::TrayUpdate(bar::tray::TrayUpdate::Add {
                                id: id.clone(),
                                icon: icon.clone(),
                                title: title.clone(),
                            }));
                    }
                }
                self.stage.tray_add(&id, icon.as_ref(), &title);
                if self.tray_items.contains_key(&id) {
                    return;
                }
                let btn = gtk4::Button::new();
                btn.add_css_class("flat");
                btn.add_css_class("tray-btn");
                btn.set_child(Some(&bar::tray::make_tray_image(icon.as_ref())));
                if !title.is_empty() {
                    btn.set_tooltip_text(Some(&title));
                }
                let id_click = id.clone();
                btn.connect_clicked(move |_| bar::tray::spawn_activate(id_click.clone()));
                self.tray_box.append(&btn);
                self.tray_items.insert(id, btn);
                self.tray_count_lbl
                    .set_label(&self.tray_items.len().to_string());
                self.tray_section.set_visible(true);
                self.tray_sep.set_visible(true);
            }
            AppInput::TrayUpdate(bar::tray::TrayUpdate::Remove { id }) => {
                if self.primary {
                    for (_, ctrl) in &self.satellites {
                        ctrl.sender()
                            .emit(AppInput::TrayUpdate(bar::tray::TrayUpdate::Remove {
                                id: id.clone(),
                            }));
                    }
                }
                self.stage.tray_remove(&id);
                if let Some(btn) = self.tray_items.remove(&id) {
                    self.tray_box.remove(&btn);
                }
                self.tray_count_lbl
                    .set_label(&self.tray_items.len().to_string());
                let has_items = !self.tray_items.is_empty();
                self.tray_section.set_visible(has_items);
                self.tray_sep.set_visible(has_items);
            }
            AppInput::CrumbsStatus(status) => {
                self.stage.set_wifi_details(Some(&status), None);
                self.crumbs_status = Some(status);
                // CrumbsStatus and WifiPopoverData arrive independently (different
                // pollers); whichever lands second must still repaint the header:
                // otherwise it only shows up if CrumbsStatus happens to win the race.
                if self.wifi_popover_data.is_some() {
                    self.rebuild_wifi_popover(&sender);
                }
            }
            AppInput::WifiPopoverData(data) => {
                self.stage.set_wifi_details(None, Some(&data));
                self.wifi_popover_data = Some(data);
                self.rebuild_wifi_popover(&sender);
            }
            AppInput::BtPopoverData(data) => {
                self.stage.set_bluetooth(&data);
                self.bt_popover_data = Some(data);
                self.rebuild_bt_popover();
            }
            AppInput::SetProfile(name) => {
                self.wifi_profile = Some(name);
                self.apply_wifi_label();
            }
            AppInput::MediaUpdate(state) => {
                self.stage.set_media(&state);
                let nowplaying = state
                    .has_player
                    .then(|| (state.title.clone(), state.playing));
                if nowplaying != self.nowplaying {
                    self.nowplaying = nowplaying;
                    self.refresh_nowplaying();
                }
                if state.has_player {
                    let label = if state.artist.is_empty() {
                        state.title.clone()
                    } else {
                        format!("{} · {}", state.artist, state.title)
                    };
                    self.media_track_lbl.set_label(&label);
                    let icon_svg = if state.playing {
                        asset!("Pause.svg")
                    } else {
                        asset!("Play.svg")
                    };
                    set_svg_icon(&self.media_play_icon, icon_svg);
                    if state.playing {
                        self.media_widget.add_css_class("playing");
                    } else {
                        self.media_widget.remove_css_class("playing");
                    }

                    if state.playing {
                        self.media_paused_at = None;
                    } else if self.media_paused_at.is_none() {
                        self.media_paused_at = Some(std::time::Instant::now());
                    }

                    let within_linger = self
                        .media_paused_at
                        .is_none_or(|t| t.elapsed().as_secs() < 30 * 60);
                    self.media_last = Some(state);
                    reveal_media(&self.media_widget, within_linger);
                } else {
                    // Player gone: honour linger from last pause
                    self.media_widget.remove_css_class("playing");
                    if let Some(paused_at) = self.media_paused_at {
                        if paused_at.elapsed().as_secs() < 30 * 60 {
                            reveal_media(&self.media_widget, true);
                        } else {
                            reveal_media(&self.media_widget, false);
                            self.media_last = None;
                            self.media_paused_at = None;
                        }
                    } else {
                        reveal_media(&self.media_widget, false);
                        self.media_last = None;
                    }
                }
            }
            AppInput::ControlPanelData(data) => {
                self.stage.set_control_data(&data);
                // Suppress slider value-changed signals during programmatic update
                self.panel_loading.set(true);
                self.panel_vol_slider.set_value(data.volume);
                self.panel_bright_slider.set_value(data.brightness);
                self.panel_loading.set(false);
                self.rebuild_sinks(&data.sinks, &sender);
            }
            AppInput::WidgetsUpdate(specs) => {
                self.daemon_widget_specs = specs;
                self.reconcile_all_widgets();
            }
            AppInput::MirrorWidgets(specs) => {
                self.daemon_widget_specs = specs;
                self.reconcile_all_widgets();
            }
            AppInput::ThemeWidgetTick { id, value } => {
                let changed = self.theme_widget_values.get(&id) != Some(&value);
                self.theme_widget_values.insert(id, value);
                if changed {
                    self.reconcile_all_widgets();
                }
            }
            AppInput::ReconcileMonitors => {
                if self.primary {
                    self.reconcile_satellites();
                }
            }
            AppInput::DismissPanels => {
                self.panels.hide_all();
            }
            AppInput::ShellThemeChanged(theme) => {
                if self.primary {
                    for (_, ctrl) in &self.satellites {
                        ctrl.sender()
                            .emit(AppInput::ShellThemeChanged(theme.clone()));
                    }
                }
                self.rebuild_from_theme(&theme, &sender);
            }
            AppInput::OpenLauncher => {
                // Themes without the embedded capsule in any slot (spotlight
                // since BAR-30) launch from the stage instead: breadbox still
                // forwards Super+Space here because the theme's launcher mode
                // is "embedded", and without this the request was dropped.
                // Toggle, so a second press closes it like breadbox would.
                if open_request_goes_to_stage(&theme::shell_theme()) {
                    post(&sender, AppInput::StageToggleCmd);
                    return;
                }
                // Only the primary instance subscribes to the open command
                // (`launcher_command::spawn`), but `self.monitor` here is
                // whichever output was focused ONCE, at this instance's own
                // `init()`: baked in at process start, not re-resolved on
                // every keybind press. If the user has since moved focus to
                // a different monitor, blindly grabbing focus on `self`
                // would open the capsule on the wrong screen. Re-resolve
                // the focused monitor now and route to whichever instance
                // actually owns it.
                let satellite_names: Vec<&str> =
                    self.satellites.iter().map(|(n, _)| n.as_str()).collect();
                let focused = primary_hypr_monitor();
                match resolve_launcher_route(focused.as_deref(), &self.monitor, &satellite_names) {
                    LauncherRoute::Satellite(name) => {
                        // resolve_launcher_route only returns a name present in
                        // satellite_names, so this lookup cannot miss.
                        if let Some((_, ctrl)) = self.satellites.iter().find(|(n, _)| *n == name) {
                            ctrl.sender().emit(AppInput::OpenLauncher);
                        }
                    }
                    LauncherRoute::Local => {
                        // See `launcher_entry.set_can_focus(false)`'s own
                        // comment above: a hotkey/command-triggered open
                        // has no pointer click to flip this back on, so
                        // this path has to do it itself or `grab_focus()`
                        // below is a silent no-op.
                        self.launcher_entry.set_can_focus(true);
                        self.launcher_entry.grab_focus();
                        // ...and then actually OPEN it. Focus alone no longer
                        // opens the capsule: `connect_enter` used to call
                        // `open_fn()`, which is precisely what made the capsule
                        // open itself during window construction, so that path
                        // was deliberately removed. Without this call the
                        // keybind focuses the entry and leaves the drawer shut.
                        (self.launcher_open_fn)();
                    }
                }
            }

            // ── Spotlight pill auto-hide (BAR-27 phase 1) ───────────────
            AppInput::PillHoverEnter => {
                self.pill_hover_gen = self.pill_hover_gen.wrapping_add(1);
                self.pill_hovering = true;
                // Hovering does not clear the unread dot: only opening the
                // stage does (the mockup's `hoverOn` leaves `.unread` alone).
                self.update_pill();
            }
            AppInput::PillHoverLeave => {
                let gen = self.pill_hover_gen;
                let delay = self.autohide_cfg.retuck_delay_ms.max(0) as u64;
                let s = sender.clone();
                gtk4::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(delay),
                    move || post(&s, AppInput::PillHoverLeaveExpire(gen)),
                );
            }
            AppInput::PillHoverLeaveExpire(gen) => {
                if gen == self.pill_hover_gen {
                    self.pill_hovering = false;
                    self.update_pill();
                }
            }
            AppInput::PillPeek(dur) => self.peek(dur, &sender),
            AppInput::PillReeval => self.update_pill(),
            AppInput::PillShowCmd => self.peek(std::time::Duration::from_millis(3000), &sender),
            AppInput::PillToggleCmd => {
                let now = std::time::Instant::now();
                let visible_now = pill_autohide::State {
                    enabled: self.autohide_cfg.enabled && self.prefs.autohide_on(),
                    hovering: self.pill_hovering,
                    low_battery: self.pill_low_battery,
                    hold_until: self.pill_hold_until,
                    open: self.stage.is_open(),
                }
                .visible(now);
                if !visible_now {
                    self.peek(std::time::Duration::from_millis(3000), &sender);
                } else if !self.pill_hovering && !self.pill_low_battery {
                    // Only visible via a live hold (or auto-hide is off);
                    // clear the hold so it can tuck again. Hovering/low-
                    // battery keep it down regardless, so toggling then is
                    // a no-op, matching the mockup's own always-on
                    // `hoverOn`/`lowBat` precedence.
                    self.pill_hold_until = None;
                    self.update_pill();
                }
            }
            AppInput::NotificationArrived {
                app,
                summary,
                critical,
            } => {
                // The mockup's `notify()`: the history list updates first
                // (live, on whichever monitor has the stage open), then DND
                // suppresses the surtitle, the peek and the unread dot. The
                // surtitle shows even over an open stage (`z-index:39` above
                // the stage's 30); the dot only lights while no stage is
                // open. Peeks and the dot reach every monitor's pill.
                self.shared.refresh_open_notifications();
                if self.dnd.get() && !critical {
                    return;
                }
                if self.shared.open_stage().is_none() {
                    self.shared.set_unread(true);
                }
                let ms = self.autohide_cfg.notification_peek_ms.max(0) as u64;
                let dur = std::time::Duration::from_millis(ms);
                self.peek(dur, &sender);
                for (_, ctrl) in &self.satellites {
                    ctrl.sender().emit(AppInput::PillPeek(dur));
                }
                if theme::shell_theme().notifications().style == "surtitle" {
                    self.surtitle.show(&app, &summary, critical, dur);
                }
            }
            AppInput::LevelChanged { kind, pct } => {
                // Every monitor's pill plays the level, not only the
                // primary's (satellites have no OSD source of their own).
                for (_, ctrl) in &self.satellites {
                    ctrl.sender().emit(AppInput::LevelChanged { kind, pct });
                }
                let ms = self.autohide_cfg.level_peek_ms.max(0) as u64;
                self.peek(std::time::Duration::from_millis(ms), &sender);
                if theme::shell_theme().osd().style == "pill" {
                    self.show_pill_level(kind, pct, ms);
                }
            }

            // ── Spotlight stage (BAR-30 phases 2-4) ─────────────────────
            AppInput::PrefsChanged(prefs) => {
                if prefs != self.prefs {
                    self.prefs = prefs;
                    self.apply_prefs();
                }
            }
            AppInput::StageOpenChanged(open) => {
                tracing::debug!(
                    target: "breadbar::stage",
                    open,
                    autohide = self.autohide_cfg.enabled,
                    layer = ?self.root.layer(),
                    mapped = self.root.is_mapped(),
                    "stage open changed"
                );
                // The mockup draws the pill (`z-index:40`) above the stage
                // (30). Both are layer surfaces here, so the pill moves up to
                // the Overlay layer while this monitor's stage is open,
                // mapped above it, and back to its theme layer once the
                // stage has faded out.
                if open {
                    if self.autohide_cfg.enabled {
                        // Deferred a moment: on the stage's first open its own
                        // layer surface is created in the same instant, and
                        // Hyprland dropped the pill's layer move sent alongside
                        // it, leaving the pill under the stage (BAR-47). The
                        // stage's .28s fade covers the delay.
                        let root = self.root.clone();
                        let stage = self.stage.clone();
                        gtk4::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(60),
                            move || {
                                if stage.is_open() {
                                    root.set_layer(Layer::Overlay);
                                }
                            },
                        );
                    }
                } else {
                    let root = self.root.clone();
                    let stage = self.stage.clone();
                    let theme_layer = if theme::shell_theme().window().layer == "overlay" {
                        Layer::Overlay
                    } else {
                        Layer::Top
                    };
                    gtk4::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(320),
                        move || {
                            if !stage.is_open() {
                                root.set_layer(theme_layer);
                            }
                        },
                    );
                }
                self.update_pill();
            }
            // `breadbar --stage` (and Super+Space) should open on the focused
            // monitor, same reasoning and the same `resolve_launcher_route`
            // mechanism as `AppInput::OpenLauncher` above. BAR-31: a toggle
            // while a stage is open on ANY monitor closes that one instead of
            // opening a second.
            AppInput::StageToggleCmd => {
                if let Some(open) = self.shared.open_stage() {
                    open.close();
                    return;
                }
                let satellite_names: Vec<&str> =
                    self.satellites.iter().map(|(n, _)| n.as_str()).collect();
                let focused = primary_hypr_monitor();
                match resolve_launcher_route(focused.as_deref(), &self.monitor, &satellite_names) {
                    LauncherRoute::Satellite(name) => {
                        if let Some((_, ctrl)) = self.satellites.iter().find(|(n, _)| *n == name) {
                            ctrl.sender().emit(AppInput::StageToggleCmd);
                        }
                    }
                    LauncherRoute::Local => self.stage.toggle(),
                }
            }
            AppInput::StageShowCmd => {
                if self.shared.open_stage().is_some() {
                    return;
                }
                let satellite_names: Vec<&str> =
                    self.satellites.iter().map(|(n, _)| n.as_str()).collect();
                let focused = primary_hypr_monitor();
                match resolve_launcher_route(focused.as_deref(), &self.monitor, &satellite_names) {
                    LauncherRoute::Satellite(name) => {
                        if let Some((_, ctrl)) = self.satellites.iter().find(|(n, _)| *n == name) {
                            ctrl.sender().emit(AppInput::StageShowCmd);
                        }
                    }
                    LauncherRoute::Local => self.stage.open(),
                }
            }
            AppInput::StageClockClicked => {
                if self.autohide_cfg.enabled {
                    // A stage open on another monitor closes first: only one
                    // is ever up.
                    if let Some(open) = self.shared.open_stage() {
                        if !Rc::ptr_eq(&open, &self.stage) {
                            open.close();
                        }
                    }
                    self.stage.toggle();
                }
            }
            AppInput::StageHideCmd => {
                if let Some(open) = self.shared.open_stage() {
                    open.close();
                }
            }
        }
    }
}

/// The `widget:<key>` alias `for_each_in_slot` recognizes for each
/// `WidgetPlacement` variant: the fallback a `WidgetSpec` routes through
/// when no `widget:<module>` container claims its module name specifically.
/// Kept in one place since both the builtin manifest's slot lists and
/// `reconcile_widgets`' routing below must agree on these names.
/// The spotlight pill's layer-shell top margin when shown and when tucked
/// (see `pill_autohide::margins`: the surface carries a transparent shadow
/// margin above the visible pill, and tucking travels the mockup's 54px).
fn pill_margins(spec: &bread_theme::shell::WindowSpec) -> (i32, i32) {
    pill_autohide::margins(spec.margin.top)
}

/// Hides an empty side row of the pill's CenterBox: an empty but visible row
/// still adds its own margin to both sides of the centred content.
fn hide_empty_rows(rows: &[&gtk4::Box]) {
    for row in rows {
        row.set_visible(row.first_child().is_some());
    }
}

/// The mockup's two example widgets (a flight and a usage meter), for the
/// screenshot views that show them in the pill and as stage cards.
fn screenshot_sample_widgets() -> Vec<bread_shared::widget::WidgetSpec> {
    use bread_shared::widget::{
        FontWeight, Orientation, WidgetNode, WidgetPlacement, WidgetSpec, WidgetStyle,
    };
    let label = |text: &str, bold: bool| WidgetNode::Label {
        text: text.into(),
        class: None,
        style: bold.then(|| WidgetStyle {
            weight: Some(FontWeight::Bold),
            ..WidgetStyle::default()
        }),
        on_click: None,
    };
    let row = |children: Vec<WidgetNode>| WidgetNode::Box {
        orientation: Orientation::Horizontal,
        spacing: Some(6),
        class: None,
        style: None,
        on_click: None,
        children,
    };
    let progress = |value: f64| WidgetNode::Progress {
        value,
        class: None,
        style: None,
        on_click: None,
    };
    let spec = |id: &str, placement, tooltip: &str, root| WidgetSpec {
        id: id.into(),
        module: "screenshot".into(),
        placement,
        order: 0,
        visible: true,
        tooltip: Some(tooltip.into()),
        root,
        updated_at: 0,
    };
    vec![
        spec(
            "screenshot.build_timer",
            WidgetPlacement::LeftOfClock,
            "Example build timer",
            row(vec![label("Build", false), label("12 min", true)]),
        ),
        spec(
            "screenshot.disk_usage",
            WidgetPlacement::RightOfClock,
            "Example disk usage",
            row(vec![
                label("Disk", false),
                progress(0.62),
                label("62%", true),
            ]),
        ),
    ]
}

/// Takes every pill item except the level readout and the low-battery flag
/// out of layout while a level shows (`leveling`), or puts back exactly the
/// ones it took out (`hidden` remembers them), the mockup's
/// `.wick.leveling>:not(.level):not(.flag){display:none}`. Hiding (not just
/// child-visibility) is what drops them from the row's width and spacing,
/// so the pill shrinks to the level row as the browser's does.
/// The pill battery gauge's inner width in px (`.pill-battery-shell`'s
/// `min-width` in base.css).
const PILL_BATTERY_PX: i32 = 18;

/// The pill's battery reading, from the stats poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PillBattery {
    pct: u8,
    charging: bool,
    low: bool,
    minutes_left: Option<u32>,
}

/// Shrinks the spotlight pill's surface to its content. The pill is anchored
/// to the top edge only, so its width is its content's; GTK grows a window
/// whose content needs more room, but never gives space back on its own, so
/// hiding the workspaces or widgets left a wide, mostly empty pill until
/// something else (the stage closing, which moves the pill between layers)
/// forced a fresh configure. Setting the default width to the natural width
/// makes the next layout shrink it. Deferred a main-loop turn so it measures
/// the content after the visibility changes that prompted it.
fn fit_pill_width(root: &gtk4::ApplicationWindow) {
    let root = root.clone();
    gtk4::glib::idle_add_local_once(move || {
        let (_, natural, _, _) = root.measure(gtk4::Orientation::Horizontal, -1);
        if natural > 0 && root.default_width() != natural {
            root.set_default_width(natural);
        }
    });
}

fn set_leveling(
    row: &gtk4::Box,
    level: &gtk4::Box,
    flag: &gtk4::Box,
    hidden: &RefCell<Vec<gtk4::Widget>>,
    leveling: bool,
) {
    let mut hidden = hidden.borrow_mut();
    if leveling {
        if !hidden.is_empty() {
            return;
        }
        let mut child = row.first_child();
        while let Some(w) = child {
            let keep =
                &w == level.upcast_ref::<gtk4::Widget>() || &w == flag.upcast_ref::<gtk4::Widget>();
            if !keep && w.is_visible() {
                w.set_visible(false);
                hidden.push(w.clone());
            }
            child = w.next_sibling();
        }
    } else {
        for w in hidden.drain(..) {
            w.set_visible(true);
        }
    }
}

fn placement_alias(placement: bread_shared::widget::WidgetPlacement) -> &'static str {
    use bread_shared::widget::WidgetPlacement::*;
    match placement {
        Tray => "tray",
        LeftOfClock => "left_of_clock",
        RightOfClock => "right_of_clock",
        RightOfWorkspaces => "right_of_workspaces",
        LeftOfStats => "left_of_stats",
    }
}

impl App {
    // ── Spotlight pill: auto-hide (BAR-27 phase 1) ────────────────────────

    /// Recomputes [`pill_autohide::State::visible`] from the current
    /// hover/hold/battery state and, on an actual shown<->tucked
    /// transition, animates the layer-shell top margin between the theme's
    /// configured margin and a fully-off-screen value, toggling the lip's
    /// visibility to match. A no-op while `!autohide_cfg.enabled` (the
    /// pill just stays at its configured margin forever, same as every
    /// builtin before this table existed).
    /// Applies `self.prefs` (the stage settings menu) to the pill: auto-hide,
    /// the workspace view and widgets. Pill theme only.
    fn apply_prefs(&mut self) {
        if !self.autohide_cfg.enabled {
            return;
        }
        if let Some(hz) = &self.pill_hotzone {
            hz.set_visible(self.prefs.autohide_on());
        }
        self.update_pill();
        self.rebuild_buttons(false);
        self.reconcile_all_widgets();
        self.refresh_nowplaying();
        self.refresh_battery();
        fit_pill_width(&self.root);
        self.stage.set_prefs(
            &self.prefs,
            self.prefs
                .workspaces_or(theme::shell_theme().modules().workspaces.display),
        );
    }

    /// Shows or hides one pill item. While the pill is showing a level, the
    /// item stays hidden and is queued to come back (or not) when the level
    /// ends: showing it now would put it beside the level row (BAR-40).
    fn set_pill_item_visible(&self, item: &gtk4::Widget, visible: bool) {
        let mut leveled = self.pill_leveled.borrow_mut();
        if leveled.is_empty() {
            item.set_visible(visible);
            return;
        }
        item.set_visible(false);
        leveled.retain(|w| w != item);
        if visible {
            leveled.push(item.clone());
        }
    }

    fn battery_item_on(&self) -> bool {
        self.autohide_cfg.enabled && self.prefs.battery_on() && self.battery.is_some()
    }

    /// The pill's battery: the gauge filled to the charge, the percentage,
    /// red when low and accent-filled while charging. Hidden when the
    /// setting is off or the machine has no battery.
    fn refresh_battery(&self) {
        if !self.autohide_cfg.enabled {
            return;
        }
        let shown = self.battery_item_on();
        if let Some(b) = &self.battery {
            // The shell's content is `PILL_BATTERY_PX` wide; the fill keeps
            // at least a sliver so an empty battery still reads as a gauge.
            let width = (f64::from(PILL_BATTERY_PX) * f64::from(b.pct.min(100)) / 100.0)
                .round()
                .max(1.0) as i32;
            self.pill_battery_fill.set_size_request(width, -1);
            self.pill_battery_pct.set_label(&format!("{}%", b.pct));
            for (class, on) in [("charging", b.charging), ("low", b.low)] {
                if on {
                    self.pill_battery.add_css_class(class);
                } else {
                    self.pill_battery.remove_css_class(class);
                }
            }
            let mut tip = format!("Battery {}%", b.pct);
            if b.charging {
                tip.push_str(", charging");
            } else if let Some(m) = b.minutes_left {
                tip.push_str(&format!(", {}", stage::pure::format_time_left(m)));
            }
            self.pill_battery.set_tooltip_text(Some(&tip));
            self.sync_pill_flag();
        }
        self.set_pill_item_visible(self.pill_battery.upcast_ref(), shown);
        fit_pill_width(&self.root);
    }

    /// breadram pressure state: the bar chip, the control panel's SYSTEM
    /// rows, and intervention notifications.
    ///
    /// The pill chip keeps the local memory readout and gains a short state
    /// marker only while the daemon reports worse than healthy; the tooltip
    /// always carries the full sentence (pressure, available memory, mode),
    /// which is also the accessible description. An absent daemon is
    /// ordinary: the pill stays exactly as the stats poll left it, the
    /// panel shows the same em dash every other missing reading uses, and
    /// nothing notifies.
    fn apply_ram_snapshot(&mut self, snapshot: bar::ram::RamSnapshot) {
        use bar::ram::{Pressure, RamMode};
        if snapshot.present {
            let previously = self.ram_prev_pressure;
            self.ram_prev_pressure = snapshot.pressure;
            self.ram_prev_present = true;
            // The stats poll owns the number; the marker is ours. Suffixing
            // keeps one chip instead of growing the bar at narrow widths.
            // The base text is remembered from the last stats poll because
            // the two pollers run on different ticks.
            self.sync_ram_chip();
            let available = snapshot
                .available_bytes
                .map(bar::ram::bytes_text)
                .unwrap_or_else(|| "unknown".to_string());
            // Spotlight has no RAM chip: the pill flag carries pressure
            // there, through the same sync the battery warning uses, so
            // the two pollers never fight over the box.
            self.sync_pill_flag();
            self.bar_ram_lbl.set_tooltip_text(Some(&format!(
                "breadram {} · {} available · {}",
                match snapshot.pressure {
                    Pressure::Healthy => "healthy",
                    Pressure::Pressured => "pressured",
                    Pressure::Critical => "critical",
                    Pressure::Unknown => "unknown",
                },
                available,
                match snapshot.mode {
                    RamMode::Automatic => "automatic",
                    RamMode::Monitor => "management paused",
                    RamMode::Unknown => "mode unknown",
                }
            )));
            self.ram_panel_mem_lbl
                .set_label(&format!("{available} free"));
            self.ram_panel_status_lbl.set_label(&format!(
                "breadram: {} · {}",
                match snapshot.pressure {
                    Pressure::Healthy => "healthy",
                    Pressure::Pressured => "pressured",
                    Pressure::Critical => "critical",
                    Pressure::Unknown => "unknown",
                },
                match snapshot.mode {
                    RamMode::Automatic => "auto",
                    RamMode::Monitor => "paused",
                    RamMode::Unknown => "—",
                }
            ));
            self.ram_paused = snapshot.mode == RamMode::Monitor;
            // Pressure transitions notify (cooled down by change, not by
            // time): a newly pressured or critical host deserves one card.
            if self.ram_live
                && snapshot.pressure != previously
                && snapshot.pressure != Pressure::Healthy
                && snapshot.pressure != Pressure::Unknown
            {
                self.post_ram_notice(
                    if snapshot.pressure == Pressure::Critical {
                        "Memory pressure critical"
                    } else {
                        "Memory pressure rising"
                    },
                    &format!("breadram reports {available} available"),
                    snapshot.pressure == Pressure::Critical,
                );
            }
            // Journal entries newer than the last poll: failures now,
            // successes as a rate-limited digest.
            self.ram_polls_since_digest += 1;
            if self.ram_live {
                let digest_due = self.ram_polls_since_digest >= bar::ram::SUCCESS_DIGEST_POLLS;
                let (notices, newest, digested) =
                    bar::ram::decide_notices(&snapshot.history, self.ram_seen_at_ms, digest_due);
                self.ram_seen_at_ms = newest;
                if digested {
                    self.ram_polls_since_digest = 0;
                }
                for notice in notices {
                    self.post_ram_notice(&notice.summary, &notice.body, notice.critical);
                }
            }
        } else {
            // Absent is ordinary: leave the stats readout alone, show the
            // same em dash every other missing reading uses, and never
            // notify.
            self.ram_panel_mem_lbl.set_label("—");
            self.ram_panel_status_lbl.set_label("breadram: —");
            self.ram_prev_present = false;
            self.sync_pill_flag();
        }
        self.sync_ram_pause_button();
    }

    /// Post one RAM card internally and record it to history.
    ///
    /// Internal, never over D-Bus: a capture or test run must not page the
    /// live bar, and the live bar must not page anyone else. History is
    /// recorded alongside the card so `--history` shows what popped up.
    fn post_ram_notice(&self, summary: &str, body: &str, critical: bool) {
        use notifications::{history, Urgency};
        let Some(handle) = &self.ram_notif else {
            return;
        };
        handle.notify(
            bar::ram::RAM_NOTIF_ID,
            "RAM",
            summary,
            body,
            if critical {
                Urgency::Critical
            } else {
                Urgency::Normal
            },
        );
        history::record(
            &self.ram_store,
            history::Entry {
                id: bar::ram::RAM_NOTIF_ID,
                app_name: "RAM".to_string(),
                summary: summary.to_string(),
                body: body.to_string(),
                urgency: if critical {
                    Urgency::Critical
                } else {
                    Urgency::Normal
                },
                received: std::time::SystemTime::now(),
                actions: Vec::new(),
            },
        );
    }

    /// The bar chip shows the stats poll's number plus our pressure marker.
    ///
    /// Both pollers write this label on their own ticks (stats every 2 s,
    /// RAM every 5 s), so each one re-applies the full text: whichever runs
    /// second never wipes the other's half.
    fn sync_ram_chip(&self) {
        if !self.ram_prev_present {
            return;
        }
        let marker = match self.ram_prev_pressure {
            bar::ram::Pressure::Healthy => String::new(),
            bar::ram::Pressure::Pressured => " BUSY".to_string(),
            bar::ram::Pressure::Critical => " FULL".to_string(),
            bar::ram::Pressure::Unknown => String::new(),
        };
        self.bar_ram_lbl
            .set_label(&format!("{}{marker}", self.ram_mem_base));
    }

    /// The pill flag shows at most one warning: a low battery outranks
    /// memory pressure, and pressure outranks silence. Both pollers call
    /// this after their own updates, so whichever runs second converges on
    /// the same box instead of wiping the other's half.
    fn sync_pill_flag(&self) {
        use bar::ram::Pressure;
        if self.pill_low_battery && !self.battery_item_on() && self.pill_leveled.borrow().is_empty()
        {
            if let Some(battery) = &self.battery {
                self.pill_flag_label.set_label(&format!("{}%", battery.pct));
            }
            self.pill_flag.set_visible(true);
            return;
        }
        if self.ram_prev_present
            && matches!(
                self.ram_prev_pressure,
                Pressure::Pressured | Pressure::Critical
            )
        {
            let critical = self.ram_prev_pressure == Pressure::Critical;
            self.pill_flag_label
                .set_label(if critical { "MEM FULL" } else { "MEM BUSY" });
            self.pill_flag.set_visible(true);
        } else {
            self.pill_flag.set_visible(false);
        }
    }

    /// The pause/resume button follows the last reported mode.
    fn sync_ram_pause_button(&self) {
        if self.ram_paused {
            self.ram_pause_lbl.set_label("Resume");
            self.ram_pause_btn
                .set_tooltip_text(Some("Resume automatic memory management"));
        } else {
            self.ram_pause_lbl.set_label("Pause");
            self.ram_pause_btn.set_tooltip_text(Some(
                "Pause automatic memory management (temporary, not saved)",
            ));
        }
    }

    /// The pill's now-playing title: shown while a player plays (and the
    /// setting is on), bars still while it is paused, gone when it stops.
    fn refresh_nowplaying(&self) {
        if !self.autohide_cfg.enabled {
            return;
        }
        let shown = match &self.nowplaying {
            Some((title, playing)) if self.prefs.nowplaying_on() && !title.is_empty() => {
                self.pill_nowplaying_title.set_label(title);
                self.pill_nowplaying.set_tooltip_text(Some(title));
                if *playing {
                    self.pill_nowplaying.remove_css_class("paused");
                } else {
                    self.pill_nowplaying.add_css_class("paused");
                }
                *playing
            }
            _ => false,
        };
        self.set_pill_item_visible(self.pill_nowplaying.upcast_ref(), shown);
        fit_pill_width(&self.root);
    }

    fn update_pill(&mut self) {
        if !self.autohide_cfg.enabled {
            return;
        }
        let now = std::time::Instant::now();
        let state = pill_autohide::State {
            enabled: self.autohide_cfg.enabled && self.prefs.autohide_on(),
            hovering: self.pill_hovering,
            low_battery: self.pill_low_battery,
            hold_until: self.pill_hold_until,
            open: self.stage.is_open(),
        };
        let shown = state.visible(now);
        if shown == self.pill_shown {
            return;
        }
        self.pill_shown = shown;
        // `.lip.gone` and `.wick.tucked`: both fade on CSS transitions.
        if let Some(lip) = &self.pill_lip {
            if shown {
                lip.add_css_class("gone");
            } else {
                lip.remove_css_class("gone");
            }
        }
        if shown {
            self.root.remove_css_class("tucked");
        } else {
            self.root.add_css_class("tucked");
        }
        // Phase-1 review must-fix: keep the hot zone's input region at a thin
        // top-edge strip (never its full footprint), so it stops swallowing
        // clicks meant for whatever window is underneath. Re-applied on
        // every transition in case the surface was re-created. See
        // `pill_hotzone::set_input_state` for why it is not emptied while
        // the pill is shown.
        if let Some(hz) = &self.pill_hotzone {
            pill_hotzone::set_input_state(hz, self.autohide_cfg.hover_zone_width as i32);
        }

        let (shown_margin, tucked_margin) = pill_margins(theme::shell_theme().window());
        let target = if shown { shown_margin } else { tucked_margin };

        if let Some(id) = self.pill_anim.borrow_mut().take() {
            id.remove();
        }
        // `.wick{transition:transform .42s var(--spring)}`: the real curve,
        // overshoot included.
        let from = f64::from(self.pill_margin.get());
        let win = self.root.clone();
        let margin_cell = self.pill_margin.clone();
        *self.pill_anim.borrow_mut() =
            motion::animate(&self.root, 420.0, 0.0, motion::SPRING, move |p| {
                let v = motion::lerp(from, f64::from(target), p).round() as i32;
                margin_cell.set(v);
                win.set_margin(Edge::Top, v);
            });
    }

    /// BAR-30 phase 4: `[osd].style = "pill"` draws a volume/brightness
    /// change inside the pill instead of the standalone OSD window, for
    /// `bar.autohide.level_peek_ms` (`ms`), matching the mockup's own
    /// `level(name,pct)`: shows the level row, hides it again after `ms`
    /// unless a newer level event supersedes it first (the same debounce
    /// idiom `osd.rs`'s `dismiss_token` and `pill_hover_gen` both use).
    fn show_pill_level(&mut self, kind: &'static str, pct: u8, ms: u64) {
        self.pill_level_name.set_label(kind);
        // The mockup's readout is the bare number (`levelPct.textContent=pct`).
        self.pill_level_pct.set_label(&pct.to_string());
        // `.level .bar i{transition:width .18s}`: glide from the previous
        // level instead of jumping, unless the row was hidden (a fresh show
        // starts at the new value, as the browser's hidden element does).
        let target = f64::from(pct.min(100)) / 100.0;
        if let Some(id) = self.pill_level_anim.borrow_mut().take() {
            id.remove();
        }
        if self.pill_level_box.is_visible() {
            let from = self.pill_level_bar.fraction();
            let bar = self.pill_level_bar.clone();
            *self.pill_level_anim.borrow_mut() =
                motion::animate(&self.pill_level_bar, 180.0, 0.0, motion::EASE, move |p| {
                    bar.set_fraction(motion::lerp(from, target, p));
                });
        } else {
            self.pill_level_bar.set_fraction(target);
        }
        self.pill_level_box.set_visible(true);
        set_leveling(
            &self.center_area,
            &self.pill_level_box,
            &self.pill_flag,
            &self.pill_leveled,
            true,
        );
        fit_pill_width(&self.root);

        let token = self.pill_level_token.get().wrapping_add(1);
        self.pill_level_token.set(token);
        let tok_cell = self.pill_level_token.clone();
        let level_box = self.pill_level_box.clone();
        let center_area = self.center_area.clone();
        let flag = self.pill_flag.clone();
        let leveled = self.pill_leveled.clone();
        let root = self.root.clone();
        gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || {
            if tok_cell.get() == token {
                level_box.set_visible(false);
                set_leveling(&center_area, &level_box, &flag, &leveled, false);
                fit_pill_width(&root);
            }
        });
    }

    /// Holds the pill down for at least `dur` (extending, never shortening,
    /// an already-longer pending hold), re-evaluates immediately, and
    /// schedules one more re-evaluation just after `dur` expires, mirroring
    /// the mockup's own `peek(ms)`: `holdUntil=Math.max(holdUntil,
    /// Date.now()+ms); wickState(); setTimeout(wickState, ms+20)`.
    fn peek(&mut self, dur: std::time::Duration, sender: &ComponentSender<Self>) {
        let candidate = std::time::Instant::now() + dur;
        self.pill_hold_until = Some(match self.pill_hold_until {
            Some(existing) if existing > candidate => existing,
            _ => candidate,
        });
        self.update_pill();
        let s = sender.clone();
        gtk4::glib::timeout_add_local_once(dur + std::time::Duration::from_millis(20), move || {
            post(&s, AppInput::PillReeval)
        });
    }

    /// Merge the daemon's widget specs with the active theme's
    /// `[[bar.widget]]` specs (each rendered from its last poll value) and
    /// reconcile the containers from that combined list.
    /// Daemon + theme widget specs, as `reconcile_all_widgets` builds them.
    fn mirrored_specs(&self) -> Vec<bread_shared::widget::WidgetSpec> {
        let mut specs = self.daemon_widget_specs.clone();
        for tw in &self.theme_widgets {
            let value = self
                .theme_widget_values
                .get(&tw.id)
                .map(String::as_str)
                .unwrap_or("");
            specs.push(theme_widgets::to_spec(tw, value));
        }
        specs
    }

    fn reconcile_all_widgets(&mut self) {
        let specs = self.mirrored_specs();
        // Only the primary subscribes to breadd's widgets and polls theme
        // widgets, so it hands the finished list to every other monitor's
        // bar. Without this, widgets showed only on whichever monitor was
        // focused when breadbar started.
        if self.primary {
            for (_, ctrl) in &self.satellites {
                ctrl.sender().emit(AppInput::MirrorWidgets(specs.clone()));
            }
        }
        self.reconcile_widgets(specs);
    }

    fn reconcile_widgets(&mut self, specs: Vec<bread_shared::widget::WidgetSpec>) {
        // A widget that went away can leave the pill wider than its content
        // (satellites land here straight from `MirrorWidgets`).
        if self.autohide_cfg.enabled {
            fit_pill_width(&self.root);
        }
        for container in self.widget_containers.values() {
            while let Some(child) = container.first_child() {
                container.remove(&child);
            }
        }

        // Route each spec to a widget_containers entry: a `widget:<module>`
        // slot entry (keyed by WidgetSpec::module) takes priority over the
        // spec's placement alias, so a theme can retarget one Lua module's
        // widgets without moving every widget that shares its placement.
        // A spec whose module AND placement alias both lack a container
        // (e.g. a theme's slots omit that placement's widget: entry
        // entirely) is logged and dropped rather than silently vanishing:
        // WidgetPlacement itself never changes; only which container (if
        // any) each spec lands in does.
        let mut by_container: std::collections::HashMap<
            String,
            Vec<&bread_shared::widget::WidgetSpec>,
        > = std::collections::HashMap::new();
        for spec in &specs {
            let key = if self.widget_containers.contains_key(&spec.module) {
                spec.module.clone()
            } else {
                placement_alias(spec.placement).to_string()
            };
            if self.widget_containers.contains_key(&key) {
                by_container.entry(key).or_default().push(spec);
            } else if self.dropped_widget_warned.insert(spec.id.clone()) {
                // Warn ONCE per widget id, not once per reconcile: breadd
                // re-pushes every spec on each update (a widget on a timer,
                // like a git-branch poller, reconciles continuously), which
                // turned a legitimate one-off diagnostic into unbounded log
                // spam. The set is only added to, so a spec that starts
                // resolving again after a theme switch stays quiet: the
                // message is about a theme lacking the slot, and repeating it
                // every tick tells the reader nothing new.
                eprintln!(
                    "breadbar: widget '{}' (module '{}', placement {:?}) has no matching \
                     [bar.slots] widget: container; dropping",
                    spec.id, spec.module, spec.placement
                );
            }
        }

        let mut has_tray_widgets = false;
        for (key, mut group) in by_container {
            let container = &self.widget_containers[&key];
            group.sort_by_key(|s| s.order);
            for spec in group {
                if !spec.visible {
                    continue;
                }
                if key == "tray" {
                    has_tray_widgets = true;
                }
                let node = widgets::build_node(&spec.root, &spec.id);
                if let Some(tooltip) = &spec.tooltip {
                    node.set_tooltip_text(Some(tooltip));
                }
                container.append(&node);
            }
        }

        // The "tray" container has its own section/separator (handled
        // below, same as the existing SNI tray items): an empty inline
        // slot has no such wrapper, so it must hide itself to stop
        // contributing to its parent box's `spacing` gap.
        // The stage settings menu can switch the pill's widgets off.
        let widgets_shown = !self.autohide_cfg.enabled || self.prefs.widgets_on();
        for (key, container) in &self.widget_containers {
            if key == "tray" {
                continue;
            }
            self.set_pill_item_visible(
                container.upcast_ref(),
                widgets_shown && container.first_child().is_some(),
            );
        }

        self.widget_tray_section.set_visible(has_tray_widgets);
        self.widget_tray_sep.set_visible(has_tray_widgets);

        // BAR-31: the stage's centre-zone cards mirror the widgets that sit
        // in the pill's own row (the mockup's `.cards`, off with widgets).
        let centre: &gtk4::Widget = self.center_area.upcast_ref();
        let mut in_pill: Vec<&bread_shared::widget::WidgetSpec> = specs
            .iter()
            .filter(|spec| spec.visible)
            .filter(|spec| {
                let key = if self.widget_containers.contains_key(&spec.module) {
                    spec.module.clone()
                } else {
                    placement_alias(spec.placement).to_string()
                };
                self.widget_containers
                    .get(&key)
                    .and_then(|c| c.parent())
                    .is_some_and(|p| &p == centre)
            })
            .collect();
        in_pill.sort_by_key(|s| s.order);
        // Tray-placement widgets (the control panel's cards on other themes)
        // are the stage's real cards; a pill widget only gets a derived card
        // when its module registered no tray card of its own.
        let mut tray: Vec<&bread_shared::widget::WidgetSpec> = specs
            .iter()
            .filter(|spec| {
                spec.visible
                    && !self.widget_containers.contains_key(&spec.module)
                    && placement_alias(spec.placement) == "tray"
            })
            .collect();
        tray.sort_by_key(|s| s.order);
        let derived: Vec<&bread_shared::widget::WidgetSpec> = in_pill
            .into_iter()
            .filter(|p| !tray.iter().any(|t| t.module == p.module))
            .collect();
        if widgets_shown {
            self.stage.set_cards(&tray, &derived);
        } else {
            self.stage.set_cards(&[], &[]);
        }
    }

    fn reconcile_satellites(&mut self) {
        let live: Vec<String> = hypr_monitor_names()
            .into_iter()
            .filter(|n| n != &self.monitor)
            .collect();
        let stale: Vec<String> = self
            .satellites
            .iter()
            .filter_map(|(n, _)| {
                if live.contains(n) {
                    None
                } else {
                    Some(n.clone())
                }
            })
            .collect();
        for name in stale {
            drop_satellite(&mut self.satellites, &name);
        }
        for name in live {
            if self.satellites.iter().any(|(n, _)| n == &name) {
                continue;
            }
            if let Some(ctrl) = spawn_satellite(&name, self.shared.clone()) {
                ctrl.sender()
                    .emit(AppInput::MirrorWidgets(self.mirrored_specs()));
                self.satellites.push((name, ctrl));
            }
        }
    }

    fn rebuild_sinks(&mut self, sinks: &[bar::control::AudioSink], sender: &ComponentSender<Self>) {
        while let Some(child) = self.sink_box.first_child() {
            self.sink_box.remove(&child);
        }
        self.sink_section.set_visible(!sinks.is_empty());
        for (i, sink) in sinks.iter().enumerate() {
            let row = gtk4::Button::new();
            row.add_css_class("flat");
            row.add_css_class("wifi-popover-row");
            row.add_css_class("sink-row");
            if sink.is_default {
                row.add_css_class("wifi-popover-row-active");
            }
            stagger_row(&row, i);
            let lbl = gtk4::Label::new(Some(&sink.description));
            lbl.set_xalign(0.0);
            lbl.set_hexpand(true);
            lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            lbl.set_max_width_chars(22);
            lbl.set_valign(gtk4::Align::Center);
            row.set_child(Some(&lbl));
            let name = sink.name.clone();
            let sender = sender.clone();
            row.connect_clicked(move |_| {
                bar::control::spawn_set_sink(name.clone(), sender.clone());
            });
            self.sink_box.append(&row);
        }
    }

    fn apply_wifi_label(&self) {
        let label = match &self.wifi_profile {
            Some(p) => format!("{p} · {}", self.current_ssid),
            None => self.current_ssid.clone(),
        };
        self.wifi_lbl.set_label(&label);
        self.wifi_img.set_tooltip_text(Some(&label));
    }

    /// Apply a CSS + geometry + slots-only shell-theme change in place:
    /// `AppInput::ShellThemeChanged`'s handler. `theme::shell_theme()` has
    /// already been swapped by the watch; `theme` is the same value, passed
    /// for the geometry read.
    fn rebuild_from_theme(
        &mut self,
        theme: &bread_theme::shell::ShellTheme,
        sender: &ComponentSender<Self>,
    ) {
        // Any in-flight trail animation touches `workspace_row`, which
        // `assemble_bar_slots` is about to empty.
        self.workspace_trail.cancel();

        apply_window_spec(&self.root, theme.window());
        // `apply_window_spec` just reset the top margin to the theme's shown
        // value; while auto-hide has the pill tucked (or mid-animation) that
        // would leave it drawn down with `pill_shown == false`, and the next
        // show would then animate from a stale `pill_margin`. Snap back to
        // where the auto-hide state says it belongs.
        if self.autohide_cfg.enabled {
            if let Some(id) = self.pill_anim.borrow_mut().take() {
                id.remove();
            }
            let (shown_margin, tucked_margin) = pill_margins(theme.window());
            let m = if self.pill_shown {
                shown_margin
            } else {
                tucked_margin
            };
            self.pill_margin.set(m);
            self.root.set_margin(Edge::Top, m);
        }

        assemble_bar_slots(
            theme.slots(),
            &self.bar_modules,
            &mut self.widget_containers,
            &self.workspace_row,
            &self.center_area,
            &self.stats_box,
            &self.drawer_box,
        );
        if self.autohide_cfg.enabled {
            hide_empty_rows(&[&self.workspace_row, &self.stats_box]);
        }

        // Colours, radii, spring curves, the `light` axis + icon textures.
        theme::reload();

        // `[[bar.widget]]` set may have changed: drop the old pollers
        // (aborted on `Drop`), re-read the theme's widgets, restart the
        // pollers, and re-render. Primary only, same as `init`.
        if self.primary {
            self.theme_widget_pollers.clear();
            self.theme_widgets = theme.widgets().to_vec();
            self.theme_widget_values
                .retain(|id, _| self.theme_widgets.iter().any(|w| &w.id == id));
            self.theme_widget_pollers = theme_widgets::spawn_pollers(&self.theme_widgets, sender);
            self.reconcile_all_widgets();
        }

        // Workspace buttons re-read chip height / dot widths on rebuild; the
        // style itself is fixed for this App (a style change is restart-gated).
        // Not an animated rebuild: a theme switch is not a workspace switch.
        self.rebuild_buttons(false);
    }

    fn rebuild_buttons(&mut self, animate: bool) {
        // A closed workspace can leave the pill wider than its content.
        if self.autohide_cfg.enabled {
            fit_pill_width(&self.root);
        }
        self.workspace_trail.cancel();
        let prev: std::collections::HashSet<WorkspaceId> =
            self.button_map.keys().copied().collect();
        // `button_map` only starts empty once: the very first call this
        // App instance ever makes, before any workspace has ever been
        // synced from Hyprland. A fully-emptied bar never happens after
        // that (the active workspace's own row is always kept), so this
        // doubles as a clean "is this the initial paint" signal without a
        // dedicated flag; see its one use below.
        let is_first_build = prev.is_empty();
        while let Some(child) = self.workspace_box.first_child() {
            self.workspace_box.remove(&child);
        }
        self.button_map.clear();
        let modules = theme::shell_theme().modules().clone();
        let ws_style = modules.workspaces.style;
        let show_empty = modules.workspaces.show_empty;
        // BAR-27 phase 1: `modules.workspaces.display`. `All` (every
        // builtin but spotlight, and spotlight's own default) is a pure
        // no-op filter, so the show_empty/style branching below runs
        // exactly as it always did; `None` skips every workspace (the
        // strip renders empty, and `pill_sep` collapses with it below);
        // `Current` renders one compact "WS n" indicator for the active
        // workspace instead of a per-workspace button.
        let display = if self.autohide_cfg.enabled {
            self.prefs.workspaces_or(modules.workspaces.display)
        } else {
            modules.workspaces.display
        };
        for ws in &self.workspaces {
            if ws.monitor != self.monitor {
                continue;
            }
            if !bar::workspaces::workspace_visible(ws.id, self.active_ws, display) {
                continue;
            }
            if display == WorkspaceDisplay::Current {
                let indicator = bar::workspaces::make_current_indicator(&ws.name);
                self.workspace_box.append(&indicator);
                // No `button_map` entry: `display = "current"` shows only
                // the (already) active workspace, so there is nothing a
                // click on it would need to switch to.
                continue;
            }
            let empty = ws.windows == 0 && ws.id != self.active_ws;
            if empty {
                match ws_style {
                    // Trail (default, liquid-motion): unconditionally off
                    // the bar, exactly as before this change, regardless
                    // of `show_empty`, which liquid-motion's own manifest
                    // declares `true` but this style has never consumed.
                    // Changing that now would be a real, undesired
                    // liquid-motion regression, not a Phase 5 fix.
                    WorkspaceStyle::Trail => continue,
                    // Pill/Dots: honour `show_empty` for real: demo 02's
                    // pills render an unoccupied, non-active workspace at
                    // reduced opacity via the `.workspace-btn:not(.occupied)
                    // :not(.active)` CSS rule rather than hiding it.
                    _ => {
                        if !show_empty {
                            continue;
                        }
                    }
                }
            }
            let btn = match ws_style {
                WorkspaceStyle::Dots => bar::workspaces::make_dot_button(
                    ws.id,
                    self.active_ws,
                    ws.windows as i32,
                    modules.workspaces.dot_widths,
                ),
                WorkspaceStyle::Trail | WorkspaceStyle::Pill => {
                    bar::workspaces::make_button(ws.id, &ws.name, self.active_ws, ws.windows > 0)
                }
            };
            // Never on the very first build (bug: "the [Trail] row sits
            // ~5px low on first paint and only corrects after the first
            // switch"). Root cause: `ws-in`'s `row-in` keyframe animates
            // `margin-top` 8px → 0 over 320ms; `WorkspaceTrail::place`
            // (called once, synchronously-ish, right after this loop for
            // the initial row) samples each button's geometry via a
            // single-shot tick callback that can fire while that margin
            // is still mid-animation, freezing the trail pill a few px
            // low until the next `place`/`stretch` call (the first real
            // workspace switch) re-samples the by-then-settled layout.
            // The demo itself never animates the initial row in at all
            // (`OCC.forEach` builds plainly, only `place(0)` runs): only
            // *subsequently added* workspaces should ever play this.
            if !is_first_build && !prev.contains(&ws.id) {
                play_once(&btn, "ws-in", 360);
            }
            self.workspace_box.append(&btn);
            self.button_map.insert(ws.id, btn);
        }
        // BAR-27 phase 1: the divider between the workspace strip and
        // whatever follows it collapses along with an empty strip
        // (`display = "none"`, or simply no workspaces synced yet).
        self.pill_sep
            .set_visible(self.workspace_box.first_child().is_some());
        // `display = "none"` never has anything in the strip, but the empty
        // (still visible) strip keeps its slot container's box spacing, which
        // pushed the clock off-centre inside the pill. Hide the whole module
        // then (the mockup's `.ws:empty{display:none}`). Gated on `None` so
        // every theme on the default `All` keeps its layout untouched.
        // Under the pill an empty strip also collapses (`.ws:empty
        // {display:none}`), so a bar with nothing synced yet keeps the time
        // centred; every other theme keeps its layout untouched.
        let has_items = self.workspace_box.first_child().is_some();
        self.workspace_trail.overlay.set_visible(
            display != WorkspaceDisplay::None && (has_items || !self.autohide_cfg.enabled),
        );
        // Trail style only: pill/dots never call place()/stretch()/clear()
        // at all: the "active" CSS class `make_button` already applies is
        // the whole of their active-workspace treatment. Also skipped
        // outside `display = "all"`: `Current`/`None` never populate
        // `button_map`, so the trail pill has nothing of its own to track.
        if ws_style == WorkspaceStyle::Trail && display == WorkspaceDisplay::All {
            match self.button_map.get(&self.active_ws).cloned() {
                Some(btn) if animate => self.workspace_trail.stretch(None, &btn),
                Some(btn) => self.workspace_trail.place(&btn),
                None => self.workspace_trail.clear(),
            }
        } else {
            self.workspace_trail.clear();
        }
    }

    fn rebuild_wifi_popover(&mut self, sender: &ComponentSender<Self>) {
        let panels = self.panels.clone();
        while let Some(child) = self.wifi_pane.first_child() {
            self.wifi_pane.remove(&child);
        }

        if let Some(st) = &self.crumbs_status {
            let header = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
            header.add_css_class("wifi-popover-header");
            header.set_margin_bottom(6);

            let ssid_str = st.ssid.as_deref().filter(|s| !s.is_empty()).unwrap_or("—");
            let ssid_lbl = gtk4::Label::new(Some(ssid_str));
            ssid_lbl.add_css_class("wifi-popover-ssid");
            ssid_lbl.set_xalign(0.0);
            header.append(&ssid_lbl);

            if let Some(ip) = &st.ip {
                let ip_lbl = gtk4::Label::new(Some(ip.as_str()));
                ip_lbl.add_css_class("wifi-popover-ip");
                ip_lbl.set_xalign(0.0);
                header.append(&ip_lbl);
            }

            let mut parts = Vec::new();
            if st.captive_portal {
                parts.push("captive portal");
            } else if st.internet {
                parts.push("internet ✓");
            } else {
                parts.push("internet ✗");
            }
            if st.tailscale_required {
                parts.push(if st.tailscale_ok {
                    "tailscale ✓"
                } else {
                    "tailscale ✗"
                });
            }
            let status_lbl = gtk4::Label::new(Some(&parts.join("   ")));
            status_lbl.add_css_class("wifi-popover-status");
            status_lbl.set_xalign(0.0);
            header.append(&status_lbl);

            self.wifi_pane.append(&header);
            self.wifi_pane
                .append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
        }

        let nh = gtk4::Label::new(Some("NETWORKS"));
        nh.add_css_class("wifi-popover-section");
        nh.set_xalign(0.0);
        nh.set_margin_top(2);
        nh.set_margin_bottom(4);
        self.wifi_pane.append(&nh);

        let data = self.wifi_popover_data.as_ref();
        let scan_ready = data.map(|d| d.scan_ready).unwrap_or(false);
        let scan = data.map(|d| d.scan.as_slice()).unwrap_or(&[]);
        let profiles = data
            .map(|d| unique_profiles(&d.profiles))
            .unwrap_or_default();

        if !scan_ready {
            let lbl = gtk4::Label::new(Some("Scanning…"));
            lbl.add_css_class("wifi-popover-loading");
            lbl.set_xalign(0.0);
            self.wifi_pane.append(&lbl);
        } else if scan.is_empty() {
            let lbl = gtk4::Label::new(Some("No networks found"));
            lbl.add_css_class("wifi-popover-loading");
            lbl.set_xalign(0.0);
            self.wifi_pane.append(&lbl);
        } else {
            for (i, entry) in scan.iter().enumerate() {
                let row = gtk4::Button::new();
                row.add_css_class("flat");
                row.add_css_class("wifi-popover-row");
                stagger_row(&row, i);
                if !entry.saved {
                    row.add_css_class("wifi-popover-row-unsaved");
                }
                let is_current = entry.ssid == self.current_ssid;
                if is_current {
                    row.add_css_class("wifi-popover-row-active");
                }

                let row_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
                let img = gtk4::Image::from_icon_name(wifi_icon_for_signal(entry.signal));
                prepare_icon(&img, 18);
                img.add_css_class("stat-icon");
                row_box.append(&img);
                let lbl = gtk4::Label::new(Some(&entry.ssid));
                lbl.set_xalign(0.0);
                lbl.set_hexpand(true);
                lbl.set_valign(gtk4::Align::Center);
                row_box.append(&lbl);
                row.set_child(Some(&row_box));

                let ssid_clone = entry.ssid.clone();
                let saved = entry.saved;
                let panels = panels.clone();
                row.connect_clicked(move |btn| {
                    if saved {
                        bar::wifi::spawn_join(ssid_clone.clone());
                    } else {
                        show_add_network_dialog(btn, ssid_clone.clone(), |_| {});
                    }
                    panels.hide_all();
                });
                self.wifi_pane.append(&row);
            }
        }

        if !profiles.is_empty() {
            let ph = gtk4::Label::new(Some("PROFILES"));
            ph.add_css_class("wifi-popover-section");
            ph.set_xalign(0.0);
            ph.set_margin_top(10);
            ph.set_margin_bottom(4);
            self.wifi_pane.append(&ph);

            for (i, (name, active)) in profiles.into_iter().enumerate() {
                let row = gtk4::Button::new();
                row.add_css_class("flat");
                row.add_css_class("wifi-popover-row");
                stagger_row(&row, i);
                if active {
                    row.add_css_class("wifi-popover-row-active");
                }
                let lbl = gtk4::Label::new(Some(&name));
                lbl.set_xalign(0.0);
                lbl.set_valign(gtk4::Align::Center);
                row.set_child(Some(&lbl));

                let name_clone = name.clone();
                let sender_clone = sender.clone();
                let panels = panels.clone();
                row.connect_clicked(move |_| {
                    post(&sender_clone, AppInput::SetProfile(name_clone.clone()));
                    bar::wifi::spawn_profile_set(name_clone.clone());
                    panels.hide_all();
                });
                self.wifi_pane.append(&row);
            }
        }

        self.wifi_pane
            .append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
        let settings_row = gtk4::Button::new();
        settings_row.add_css_class("flat");
        settings_row.add_css_class("wifi-popover-row");
        let settings_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        let settings_icon = svg_image(bar::stats::ICON_BT_SETTINGS);
        settings_icon.add_css_class("stat-icon");
        settings_box.append(&settings_icon);
        settings_box.append(&gtk4::Label::new(Some("Wi-Fi settings")));
        settings_row.set_child(Some(&settings_box));
        settings_row.connect_clicked(|_| {
            relm4::spawn(async {
                let _ = tokio::process::Command::new("bos-settings")
                    .args(["--page", "network"])
                    .spawn();
            });
        });
        self.wifi_pane.append(&settings_row);
    }

    fn rebuild_bt_popover(&mut self) {
        while let Some(child) = self.bt_pane.first_child() {
            self.bt_pane.remove(&child);
        }

        let Some(data) = &self.bt_popover_data else {
            let lbl = gtk4::Label::new(Some("Loading…"));
            lbl.add_css_class("wifi-popover-loading");
            self.bt_pane.append(&lbl);
            return;
        };

        // Power toggle row
        let toggle_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        toggle_row.set_margin_bottom(4);
        let toggle_lbl = gtk4::Label::new(Some("Bluetooth"));
        toggle_lbl.add_css_class("wifi-popover-ssid");
        toggle_lbl.set_hexpand(true);
        toggle_lbl.set_xalign(0.0);
        let toggle_switch = gtk4::Switch::new();
        toggle_switch.add_css_class("bt-switch");
        toggle_switch.set_active(data.powered);
        toggle_switch.set_valign(gtk4::Align::Center);
        toggle_switch.connect_state_set(|_, on| {
            bar::bluetooth::spawn_set_powered(on);
            gtk4::glib::Propagation::Proceed
        });
        toggle_row.append(&toggle_lbl);
        toggle_row.append(&toggle_switch);
        self.bt_pane.append(&toggle_row);
        self.bt_pane
            .append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));

        if !data.powered {
            let lbl = gtk4::Label::new(Some("Bluetooth is off"));
            lbl.add_css_class("wifi-popover-loading");
            self.bt_pane.append(&lbl);
        } else if data.devices.is_empty() {
            let lbl = gtk4::Label::new(Some("No paired devices"));
            lbl.add_css_class("wifi-popover-loading");
            self.bt_pane.append(&lbl);
        } else {
            let dh = gtk4::Label::new(Some("PAIRED"));
            dh.add_css_class("wifi-popover-section");
            dh.set_xalign(0.0);
            dh.set_margin_top(2);
            dh.set_margin_bottom(4);
            self.bt_pane.append(&dh);

            for (i, dev) in data.devices.iter().enumerate() {
                let row = gtk4::Button::new();
                row.add_css_class("flat");
                row.add_css_class("wifi-popover-row");
                stagger_row(&row, i);
                if dev.connected {
                    row.add_css_class("wifi-popover-row-active");
                }
                let lbl = gtk4::Label::new(Some(&dev.name));
                lbl.set_xalign(0.0);
                lbl.set_valign(gtk4::Align::Center);
                row.set_child(Some(&lbl));

                let address = dev.address.clone();
                let connected = dev.connected;
                row.connect_clicked(move |_| {
                    if connected {
                        bar::bluetooth::spawn_disconnect(address.clone());
                    } else {
                        bar::bluetooth::spawn_connect(address.clone());
                    }
                });
                self.bt_pane.append(&row);
            }
        }

        self.bt_pane
            .append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
        let settings_row = gtk4::Button::new();
        settings_row.add_css_class("flat");
        settings_row.add_css_class("wifi-popover-row");
        let settings_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        let settings_icon = svg_image(bar::stats::ICON_BT_SETTINGS);
        settings_icon.add_css_class("stat-icon");
        settings_box.append(&settings_icon);
        settings_box.append(&gtk4::Label::new(Some("Bluetooth settings")));
        settings_row.set_child(Some(&settings_box));
        settings_row.connect_clicked(|_| {
            relm4::spawn(async {
                let _ = tokio::process::Command::new("bos-settings")
                    .args(["--page", "bluetooth"])
                    .spawn();
            });
        });
        self.bt_pane.append(&settings_row);
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// One tile in the control panel's power row. `pub(crate)` (BAR-30 phase 2):
/// the stage's own power row (`stage::mod`) reuses this same table rather
/// than declaring a second one, so the two never drift apart on which
/// actions exist or what they run.
#[derive(Clone, Copy)]
pub(crate) struct PowerAction {
    pub(crate) label: &'static str,
    pub(crate) tooltip: &'static str,
    pub(crate) icon: &'static str,
    pub(crate) cmd: &'static [&'static str],
    /// Needs a second click within [`POWER_CONFIRM_MS`] before it runs.
    pub(crate) confirm: bool,
}

pub(crate) const POWER_CONFIRM_MS: u64 = 3_000;

pub(crate) const POWER_ACTIONS: &[PowerAction] = &[
    // breadlock is the ecosystem's own screen locker: hyprlock is the thing
    // it was built to replace; the bar shouldn't still be pointing at it.
    PowerAction {
        label: "lock",
        tooltip: "Lock screen",
        icon: bar::stats::ICON_LOCK,
        cmd: &["breadlock"],
        confirm: false,
    },
    PowerAction {
        label: "sleep",
        tooltip: "Suspend",
        icon: bar::stats::ICON_SLEEP,
        cmd: &["systemctl", "suspend"],
        confirm: false,
    },
    PowerAction {
        label: "restart",
        tooltip: "Restart (click twice)",
        icon: bar::stats::ICON_RESTART,
        cmd: &["systemctl", "reboot"],
        confirm: true,
    },
    PowerAction {
        label: "off",
        tooltip: "Power off (click twice)",
        icon: bar::stats::ICON_SHUTDOWN,
        cmd: &["systemctl", "poweroff"],
        confirm: true,
    },
];

/// Icon + slider + live percentage readout. The readout tracks the scale
/// itself (not the `ControlPanelData` poll), so it follows a drag frame by
/// frame and shows >100% on the volume row's 0..1.5 range.
fn build_slider_row(
    icon_svg: &'static str,
    tooltip: &str,
    min: f64,
    max: f64,
    step: f64,
) -> (gtk4::Box, gtk4::Scale) {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    row.add_css_class("control-panel-row");

    let icon = svg_image(icon_svg);
    icon.add_css_class("control-panel-row-icon");
    icon.set_tooltip_text(Some(tooltip));

    let slider = gtk4::Scale::with_range(gtk4::Orientation::Horizontal, min, max, step);
    slider.set_draw_value(false);
    slider.set_hexpand(true);
    slider.set_width_request(160);
    slider.add_css_class("control-panel-slider");

    let value = gtk4::Label::new(Some("0%"));
    value.add_css_class("control-panel-row-value");
    value.set_xalign(1.0);
    value.set_width_chars(4);
    slider.connect_value_changed({
        let value = value.clone();
        move |s| value.set_label(&format!("{:.0}%", s.value() * 100.0))
    });

    row.append(&icon);
    row.append(&slider);
    row.append(&value);
    (row, slider)
}

fn wifi_icon_for_signal(pct: u8) -> &'static str {
    use bar::stats::{
        WIFI_ICON_EXCELLENT, WIFI_ICON_GOOD, WIFI_ICON_OFF, WIFI_ICON_OK, WIFI_ICON_WEAK,
    };
    match pct {
        75..=100 => WIFI_ICON_EXCELLENT,
        50..=74 => WIFI_ICON_GOOD,
        25..=49 => WIFI_ICON_OK,
        1..=24 => WIFI_ICON_WEAK,
        _ => WIFI_ICON_OFF,
    }
}

/// Small modal prompting for a password, then saves + joins the network via
/// `breadcrumbs add` + `breadcrumbs join`. `on_build` runs on the freshly
/// built dialog *before* it's presented: screenshot mode's only hook point,
/// since `connect_map` registered any later would miss a map that already
/// happened. The real call site passes a no-op.
fn show_add_network_dialog(
    anchor: &impl IsA<gtk4::Widget>,
    ssid: String,
    on_build: impl FnOnce(&gtk4::Window),
) {
    let dialog = gtk4::Window::new();
    dialog.set_title(Some(&format!("Add “{ssid}”")));
    dialog.set_resizable(false);
    dialog.add_css_class("wifi-add-dialog");
    if let Some(output) = bread_theme::gtk::output_for_widget(anchor) {
        theme::bind_output(&dialog, &output);
    } else {
        theme::bind_auto(&dialog);
    }
    // A bare gtk4::Window with no titlebar set falls back to GTK's own
    // minimal CSD: a flat bar with plain system-font title text and no
    // rounding: which is what actually made this look like a stray window
    // from a different decade next to the rest of the (rounded, borderless,
    // shadowed) ecosystem. A real HeaderBar picks up the window's own title
    // automatically and gets the same `.wifi-add-dialog` theming below.
    let header = gtk4::HeaderBar::new();
    header.set_show_title_buttons(true);
    dialog.set_titlebar(Some(&header));
    if let Some(root) = anchor.root() {
        if let Ok(win) = root.downcast::<gtk4::Window>() {
            dialog.set_transient_for(Some(&win));
            dialog.set_modal(true);
        }
    }

    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);

    let lbl = gtk4::Label::new(Some(&format!("Password for {ssid}")));
    lbl.set_xalign(0.0);
    body.append(&lbl);

    let entry = gtk4::PasswordEntry::new();
    entry.set_show_peek_icon(true);
    body.append(&entry);

    let btn_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    btn_row.set_halign(gtk4::Align::End);
    let cancel_btn = gtk4::Button::with_label("Cancel");
    let connect_btn = gtk4::Button::with_label("Connect");
    // Not "suggested-action": GTK4's own bundled theme special-cases that
    // class for a newer native OS-accent-colour feature that isn't a normal
    // CSS rule at all, and simply doesn't lose to any `background-color`
    // override this stylesheet adds, however specific the selector (already
    // confirmed empirically: adding a much more specific override rule had
    // zero effect). "confirm-button" is the same ecosystem-wide accent
    // button convention breadman/breadpad already use successfully.
    connect_btn.add_css_class("confirm-button");
    btn_row.append(&cancel_btn);
    btn_row.append(&connect_btn);
    body.append(&btn_row);

    dialog.set_child(Some(&body));

    let d = dialog.clone();
    cancel_btn.connect_clicked(move |_| d.close());

    let dialog_for_connect = dialog.clone();
    let entry_for_connect = entry.clone();
    let ssid_for_connect = ssid.clone();
    connect_btn.connect_clicked(move |_| {
        let password = entry_for_connect.text().to_string();
        if password.is_empty() {
            return;
        }
        bar::wifi::spawn_add_and_join(ssid_for_connect.clone(), password);
        dialog_for_connect.close();
    });

    let dialog_for_activate = dialog.clone();
    let entry_for_activate = entry.clone();
    let ssid_for_activate = ssid.clone();
    entry.connect_activate(move |_| {
        let password = entry_for_activate.text().to_string();
        if password.is_empty() {
            return;
        }
        bar::wifi::spawn_add_and_join(ssid_for_activate.clone(), password);
        dialog_for_activate.close();
    });

    on_build(&dialog);
    dialog.present();
    entry.grab_focus();
}

fn stat_pair(icon_svg: &'static str, label: &gtk4::Label) -> gtk4::Box {
    let pair = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    pair.add_css_class("stat-pair");
    bar_chip(&pair);
    let img = svg_image(icon_svg);
    img.add_css_class("stat-icon");
    pair.append(&img);
    pair.append(label);
    pair
}

/// Identity of the pills this bar actually draws. Ignores last-window
/// title/address so a tab title change cannot rebuild the row.
fn visible_ws_rows(
    workspaces: &[Workspace],
    monitor: &str,
    active: WorkspaceId,
) -> Vec<(WorkspaceId, String, bool)> {
    workspaces
        .iter()
        .filter(|w| w.monitor == monitor && (w.windows > 0 || w.id == active))
        .map(|w| (w.id, w.name.clone(), w.windows > 0))
        .collect()
}

fn bar_chip(widget: &impl IsA<gtk4::Widget>) {
    widget.set_valign(gtk4::Align::Center);
    widget.set_vexpand(false);
}

fn popover_caret() -> gtk4::Box {
    let caret = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    caret.add_css_class("popover-caret");
    caret.set_hexpand(true);
    caret
}

fn stagger_row(widget: &impl IsA<gtk4::Widget>, i: usize) {
    widget.add_css_class("row-in");
    widget.add_css_class(&format!("stagger-{}", i.min(11)));
}

fn play_once(widget: &impl IsA<gtk4::Widget>, class: &str, ms: u64) {
    widget.remove_css_class(class);
    widget.add_css_class(class);
    let w = widget.as_ref().clone();
    let class = class.to_string();
    gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || {
        w.remove_css_class(&class);
    });
}

fn make_clock_digits(time: &str) -> Vec<gtk4::Label> {
    time.chars()
        .map(|ch| {
            let lbl = gtk4::Label::new(Some(&ch.to_string()));
            lbl.add_css_class("clock-digit");
            if ch == ':' {
                lbl.add_css_class("clock-colon");
            }
            lbl.set_valign(gtk4::Align::Center);
            lbl.set_vexpand(false);
            lbl.set_yalign(0.5);
            lbl
        })
        .collect()
}

fn flip_clock_digits(digits: &[gtk4::Label], time: &str) {
    let chars: Vec<char> = time.chars().collect();
    for (i, lbl) in digits.iter().enumerate() {
        let next = chars.get(i).copied().unwrap_or(' ');
        let next_s = next.to_string();
        if lbl.label().as_str() == next_s {
            continue;
        }
        lbl.set_label(&next_s);
        if next != ':' {
            play_once(lbl, "flip", 450);
        }
    }
}

/// Container for a [`make_digit_chip`]/[`flip_digit_chip`]-driven numeric
/// chip (volume, battery): a plain horizontal box, same shape as
/// `vol_box`/`bat_box`'s existing icon+label chips, that gets one
/// `.stat-digit` label per character instead of a single `gtk4::Label`.
fn digit_chip_box() -> gtk4::Box {
    gtk4::Box::new(gtk4::Orientation::Horizontal, 0)
}

/// One `.stat-digit` label per character of `text`: the `stat-label`
/// styled counterpart of `make_clock_digits`, reused so volume/battery
/// chips can roll per-digit the same way the clock does (plan: "ODOMETER
/// DIGITS FOR NUMERIC CHIPS"). Not the clock's own `.clock-digit` class:
/// that carries the clock's much larger `font-size`, wrong for a bar chip.
fn make_digit_chip(text: &str) -> Vec<gtk4::Label> {
    text.chars()
        .map(|ch| {
            let lbl = gtk4::Label::new(Some(&ch.to_string()));
            lbl.add_css_class("stat-label");
            lbl.add_css_class("stat-digit");
            lbl.set_valign(gtk4::Align::Center);
            lbl.set_vexpand(false);
            lbl.set_yalign(0.5);
            lbl
        })
        .collect()
}

/// Tears down `container`'s current digit labels and rebuilds them for
/// `text` from scratch: the shared fallback [`set_digit_chip`] and
/// [`flip_digit_chip`] both take when the character count changes (`9` ->
/// `10`), including the very first call, when `digits` starts empty. No
/// per-position diff makes sense across different lengths, so this never
/// animates.
fn rebuild_digit_chip(container: &gtk4::Box, digits: &mut Vec<gtk4::Label>, text: &str) {
    for lbl in digits.drain(..) {
        container.remove(&lbl);
    }
    *digits = make_digit_chip(text);
    for lbl in digits.iter() {
        container.append(lbl);
    }
}

/// Replaces `container`'s digit labels with `text`'s, with NO animation:
/// used for the volume slider's live drag feedback (`connect_value_changed`
/// fires on every pointer-move tick; replaying the flip keyframe that fast
/// would read as a flicker). [`flip_digit_chip`] below is the animated
/// counterpart, driven by the `StatsUpdate` poll instead.
fn set_digit_chip(container: &gtk4::Box, digits: &mut Vec<gtk4::Label>, text: &str) {
    let chars: Vec<char> = text.chars().collect();
    if digits.len() != chars.len() {
        rebuild_digit_chip(container, digits, text);
        return;
    }
    for (lbl, ch) in digits.iter().zip(chars.iter()) {
        let next = ch.to_string();
        if lbl.label().as_str() != next {
            lbl.set_label(&next);
        }
    }
}

/// The animated counterpart of [`set_digit_chip`]: same rebuild-on-length-
/// change fallback, but on a same-length update it plays the `flip`
/// keyframe (`digit-flip`, the same one the clock uses) only on the
/// characters that actually changed, same convention as
/// `flip_clock_digits`.
fn flip_digit_chip(container: &gtk4::Box, digits: &mut Vec<gtk4::Label>, text: &str) {
    let chars: Vec<char> = text.chars().collect();
    if digits.len() != chars.len() {
        rebuild_digit_chip(container, digits, text);
        return;
    }
    for (lbl, ch) in digits.iter().zip(chars.iter()) {
        let next = ch.to_string();
        if lbl.label().as_str() == next {
            continue;
        }
        lbl.set_label(&next);
        play_once(lbl, "flip", 350);
    }
}

fn reveal_media(widget: &gtk4::Box, show: bool) {
    if show && !widget.is_visible() {
        play_once(widget, "media-in", 420);
    }
    widget.set_visible(show);
}

/// Drives `drawer_box`'s height from `from` to `to` over 360ms via
/// `bread_theme::anim::spring_to` (plan §7: GTK4 has no CSS height
/// transition on a widget, so the capsule's `.results { max-height: 0 →
/// 420px }` becomes a `set_size_request` interpolation on the frame clock
/// instead). Cancels any run already in flight first: reopening mid-close
/// (or vice versa) must restart from the CURRENT height, not fight a
/// leftover callback still walking toward the old target.
fn animate_drawer_height(
    drawer_box: &gtk4::Box,
    anim: &Rc<std::cell::RefCell<Option<gtk4::TickCallbackId>>>,
    from: i32,
    to: i32,
) {
    if let Some(id) = anim.borrow_mut().take() {
        id.remove();
    }
    let target = drawer_box.clone();
    let id = bread_theme::anim::spring_to(drawer_box, from, to, 360.0, move |h| {
        // `spring_ease` deliberately overshoots past t=1.0: that bounce is the
        // point on expand, but on a collapse (from=content height, to=0) the
        // same overshoot carries the interpolated value BELOW zero, and
        // `set_size_request` hard-asserts `height >= -1` (GTK-CRITICAL, once
        // per frame at 60fps). -1 is GTK's "use natural height" sentinel, not a
        // valid animation frame, so clamp to 0 rather than -1: a drawer mid-
        // collapse wants zero height, never its natural height.
        target.set_size_request(-1, h.max(0));
        // Collapse finished: hide the box so it stops claiming natural height.
        // set_size_request is only a minimum, so a visible-but-zero-request
        // drawer still gets allocated its children's full height and holds the
        // capsule open. Hiding here rather than in close_fn keeps the collapse
        // animated instead of snapping shut on the first frame.
        if to == 0 && h <= 0 {
            target.set_visible(false);
        }
    });
    *anim.borrow_mut() = Some(id);
}

/// Drives the capsule's own window width from `from` to `to` over 360ms
/// (plan §7 phase 6c: `[launcher].search_width`, `04-spotlight.html`'s
/// `.searching .capsule { width: 520px }`): the same `spring_to` +
/// `set_size_request` technique `animate_drawer_height` uses for the
/// drawer's height, applied to the root window itself instead of a child
/// box. Unlike a drawer collapse, width never animates toward a negative
/// target (idle/search widths are both positive theme values), so there is
/// no analogous "clamp to 0" concern here.
fn animate_capsule_width(
    root: &gtk4::ApplicationWindow,
    anim: &Rc<std::cell::RefCell<Option<gtk4::TickCallbackId>>>,
    from: i32,
    to: i32,
) {
    if let Some(id) = anim.borrow_mut().take() {
        id.remove();
    }
    let target = root.clone();
    let id = bread_theme::anim::spring_to(root, from, to, 360.0, move |w| {
        target.set_size_request(w.max(0), -1);
    });
    *anim.borrow_mut() = Some(id);
}

/// The drawer's natural content height right now, capped at the demo's own
/// 420px (`04-spotlight.html`: `.searching .results { max-height: 420px }`):
/// `ResultsList`'s scroller already self-caps at 480px
/// (`max_content_height`), shared with breadbox, so this is a tighter,
/// spotlight-specific ceiling on top of that shared one, not a replacement
/// for it.
fn drawer_target_height(drawer_box: &gtk4::Box) -> i32 {
    // Deliberately never measures `drawer_box` itself. `animate_drawer_height`'s
    // tick callback calls `drawer_box.set_size_request(-1, h)` on every
    // frame, and GTK clamps a widget's own `measure()` result up to at
    // least its own explicit size request, so once an animation has run
    // even one frame, `drawer_box.measure()` reports that frame's forced
    // height (or the spring's overshoot past it), not whatever its
    // children actually need next. This bit spotlight's new query-mode
    // rows directly: switching from the (tall) app list to a one-row calc
    // result measured "437" instead of "~33", because the PREVIOUS
    // animation frame had already forced `drawer_box` to 437px.
    //
    // Summing each currently-visible child's own natural height instead
    // sidesteps this entirely: `launcher_results.scroller` and
    // `mode_list` never get an explicit size request of their own, so
    // their `measure()` always reflects their actual current content.
    let mut total = 0;
    let mut child = drawer_box.first_child();
    while let Some(c) = child {
        if c.is_visible() {
            let (_, natural, _, _) = c.measure(gtk4::Orientation::Vertical, -1);
            total += natural;
        }
        child = c.next_sibling();
    }
    total.min(DRAWER_MAX_HEIGHT_PX)
}

// ── Query-mode rows (plan §7 phase 6c) ──────────────────────────────────
//
// `mode_list`'s rows are NOT `bread_launcher::DesktopEntry`-backed
// (`bread_launcher::gtk::row_entry` returns `None` for every one of
// these), so they're built/read here rather than through that crate.

/// A single-line, non-interactive row: the calc result, or a "nothing
/// typed yet" placeholder. Reuses `.app-name` so it inherits the same
/// `.bread-drawer row` typography `bread-launcher`'s own rows get.
fn mode_info_row(text: &str) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(false);
    let lbl = gtk4::Label::new(Some(text));
    lbl.add_css_class("app-name");
    lbl.set_xalign(0.0);
    row.set_child(Some(&lbl));
    row
}

/// A single-line, actionable row (a `>`-mode command, or the `.`-mode
/// "open this URL" prompt): Enter/click spawns `action` once resolved by
/// [`run_mode_action`].
fn mode_action_row(text: &str, action: ModeAction) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    let lbl = gtk4::Label::new(Some(text));
    lbl.add_css_class("app-name");
    lbl.set_xalign(0.0);
    row.set_child(Some(&lbl));
    unsafe { row.set_data("mode_action", action) };
    row
}

/// What Enter/click on a [`mode_action_row`] does. Two variants, not one
/// shell-command string, so a `.`-mode URL (arbitrary user-typed text)
/// never passes through `bash -c` at all: only a `>`-mode command's own
/// fixed, trusted `exec` string does.
#[derive(Clone)]
enum ModeAction {
    RunShell(&'static str),
    OpenUrl(String),
}

fn mode_row_action(row: &gtk4::ListBoxRow) -> Option<ModeAction> {
    unsafe {
        row.data::<ModeAction>("mode_action")
            .map(|p| p.as_ref().clone())
    }
}

/// Pure half of the `.`-mode URL action: adds a scheme when the user typed
/// a bare host (`example.com` -> `https://example.com`), leaves anything
/// that already looks like `scheme://...` untouched.
fn url_open_target(url: &str) -> String {
    if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    }
}

fn run_mode_action(action: &ModeAction) {
    match action {
        ModeAction::RunShell(cmd) => {
            if let Err(e) = std::process::Command::new("bash")
                .args(["-c", cmd])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                eprintln!("breadbar: failed to run mode command {cmd:?}: {e}");
            }
        }
        ModeAction::OpenUrl(url) => {
            // No scheme-adding shell involved: `xdg-open` gets the raw
            // argument, so nothing in a `.`-mode query is ever parsed as
            // shell syntax.
            let target = url_open_target(url);
            if let Err(e) = std::process::Command::new("xdg-open")
                .arg(&target)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                eprintln!("breadbar: failed to open url {target:?}: {e}");
            }
        }
    }
}

/// Moves `list`'s selection to the next/previous row: unlike
/// `bread_launcher::gtk::ResultsList::select_next`/`select_prev`, `mode_list`
/// never has hidden rows to skip (it's cleared and rebuilt from scratch on
/// every query change), so this is the plain, un-filtered version.
fn listbox_select_next(list: &gtk4::ListBox) {
    let cur = list.selected_row().map(|r| r.index()).unwrap_or(-1);
    if let Some(row) = list.row_at_index(cur + 1) {
        list.select_row(Some(&row));
    }
}

fn listbox_select_prev(list: &gtk4::ListBox) {
    let cur = list.selected_row().map(|r| r.index()).unwrap_or(0);
    if cur > 0 {
        if let Some(row) = list.row_at_index(cur - 1) {
            list.select_row(Some(&row));
        }
    }
}

/// Clears `mode_list` and rebuilds it for `parsed`: the calc result,
/// filtered `>`-mode commands, or the `.`-mode "open this URL" prompt.
/// Returns whether anything is now selectable (a real command/URL row, not
/// just an info row) so the caller knows whether Return has anything to do.
fn populate_mode_list(mode_list: &gtk4::ListBox, parsed: &bread_launcher::ParsedQuery) -> bool {
    while let Some(row) = mode_list.row_at_index(0) {
        mode_list.remove(&row);
    }
    match parsed.kind {
        bread_launcher::QueryKind::Calc => {
            match bread_launcher::eval_calc(&parsed.value) {
                Some(result) => mode_list.append(&mode_info_row(&format!("= {result}"))),
                None => mode_list.append(&mode_info_row("=")),
            }
            false
        }
        bread_launcher::QueryKind::Cmd => {
            let matches =
                bread_launcher::filter_commands(&parsed.value, bread_launcher::builtin_commands());
            if matches.is_empty() {
                mode_list.append(&mode_info_row("No matching commands"));
                false
            } else {
                for cmd in &matches {
                    mode_list.append(&mode_action_row(cmd.name, ModeAction::RunShell(cmd.exec)));
                }
                if let Some(first) = mode_list.row_at_index(0) {
                    mode_list.select_row(Some(&first));
                }
                true
            }
        }
        bread_launcher::QueryKind::Url => {
            if parsed.value.is_empty() {
                mode_list.append(&mode_info_row("."));
                false
            } else {
                mode_list.append(&mode_action_row(
                    &format!("Open {}", parsed.value),
                    ModeAction::OpenUrl(parsed.value.clone()),
                ));
                if let Some(first) = mode_list.row_at_index(0) {
                    mode_list.select_row(Some(&first));
                }
                true
            }
        }
        bread_launcher::QueryKind::Apps => false,
    }
}

fn popover_tab(label: &str) -> gtk4::ToggleButton {
    let btn = gtk4::ToggleButton::with_label(label);
    btn.add_css_class("popover-tab");
    btn.set_hexpand(true);
    btn.set_valign(gtk4::Align::Center);
    btn.set_vexpand(false);
    btn.set_size_request(-1, theme::shell_theme().tokens().chip_height() as i32);
    if let Some(child) = btn.child() {
        child.set_halign(gtk4::Align::Center);
        child.set_valign(gtk4::Align::Center);
        child.set_hexpand(true);
        child.set_vexpand(false);
        if let Ok(lbl) = child.downcast::<gtk4::Label>() {
            lbl.set_xalign(0.5);
            lbl.set_yalign(0.5);
        }
    }
    btn
}

/// breadcrumbs can list the same profile twice under different case
/// (`Home` / `home`). Keep the active spelling when there is one.
fn unique_profiles(profiles: &[(String, bool)]) -> Vec<(String, bool)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (name, active) in profiles {
        if *active {
            seen.insert(name.to_ascii_lowercase());
            out.push((name.clone(), true));
        }
    }
    for (name, active) in profiles {
        if seen.insert(name.to_ascii_lowercase()) {
            out.push((name.clone(), *active));
        }
    }
    out
}

pub(crate) fn prepare_icon(img: &gtk4::Image, px: i32) {
    img.set_pixel_size(px);
    img.set_valign(gtk4::Align::Center);
    img.set_vexpand(false);
}

// ── Baked SVG icon textures ─────────────────────────────────────────────
//
// Icon glyphs are recoloured by baking `theme::fg_color()` into the SVG
// source before resvg rasterises it (`svg_texture_sized`). That tint lives
// in the texture, entirely outside GTK CSS, so a `bread-theme reload` that
// flips the `light` axis recolours the CSS chrome but leaves every already-
// rendered glyph its old ink (near-white glyphs, invisible on daylight's
// paper pills). Two pieces keep them in step:
//
//   * `BAKED_TEXTURE_CACHE` memoises `(src, px) -> Texture` so a poller
//     calling `set_svg_icon` every couple of seconds isn't re-rasterising;
//     `rebake_icons` clears it so the next bake picks up the new ink.
//   * `BAKED_ICONS` weak-tracks every `Image` handed out here plus the
//     source it currently shows, so `rebake_icons` can push a freshly baked
//     texture into each live one.
//
// `theme::reload` drives `rebake_icons` on every SIGHUP / `theme.toml` edit.
thread_local! {
    static BAKED_TEXTURE_CACHE:
        RefCell<std::collections::HashMap<(usize, u32), gtk4::gdk::Texture>> =
        RefCell::new(std::collections::HashMap::new());
    static BAKED_ICONS: RefCell<Vec<BakedIcon>> = const { RefCell::new(Vec::new()) };
}

struct BakedIcon {
    img: gtk4::glib::WeakRef<gtk4::Image>,
    src: Cell<&'static str>,
    px: u32,
}

fn baked_texture(src: &'static str, px: u32) -> gtk4::gdk::Texture {
    let key = (src.as_ptr() as usize, px);
    BAKED_TEXTURE_CACHE.with(|c| {
        c.borrow_mut()
            .entry(key)
            .or_insert_with(|| svg_texture_sized(src, px))
            .clone()
    })
}

fn register_baked_icon(img: &gtk4::Image, src: &'static str, px: u32) {
    BAKED_ICONS.with(|v| {
        let mut v = v.borrow_mut();
        v.retain(|b| b.img.upgrade().is_some());
        v.push(BakedIcon {
            img: img.downgrade(),
            src: Cell::new(src),
            px,
        });
    });
}

/// Swap the glyph shown by an `Image` created via [`svg_image`] /
/// [`svg_image_sized`], keeping the baked-icon registry in step so a later
/// [`rebake_icons`] re-tints the *current* glyph, not the one it was born
/// with. Use this instead of `img.set_paintable(...)` for any icon that
/// changes at runtime (battery level, bluetooth state, media play/pause,
/// OSD volume/brightness).
pub(crate) fn set_svg_icon(img: &gtk4::Image, src: &'static str) {
    let px = BAKED_ICONS.with(|v| {
        v.borrow().iter().find_map(|b| {
            b.img.upgrade().filter(|u| u == img).map(|_| {
                b.src.set(src);
                b.px
            })
        })
    });
    let px = px.unwrap_or_else(|| theme::shell_theme().tokens().icon_px() as u32);
    img.set_paintable(Some(&baked_texture(src, px)));
    prepare_icon(img, px as i32);
}

/// Re-tint every live baked icon against the current `theme::fg_color()`.
/// Called by [`theme::reload`] (SIGHUP / `theme.toml` hot-reload).
pub(crate) fn rebake_icons() {
    BAKED_TEXTURE_CACHE.with(|c| c.borrow_mut().clear());
    BAKED_ICONS.with(|v| {
        v.borrow_mut().retain(|b| match b.img.upgrade() {
            Some(img) => {
                img.set_paintable(Some(&baked_texture(b.src.get(), b.px)));
                prepare_icon(&img, b.px as i32);
                true
            }
            None => false,
        });
    });
}

pub(crate) fn svg_image(svg_src: &'static str) -> gtk4::Image {
    svg_image_sized(svg_src, theme::shell_theme().tokens().icon_px() as u32)
}

pub(crate) fn svg_image_sized(svg_src: &'static str, px: u32) -> gtk4::Image {
    let img = gtk4::Image::from_paintable(Some(&baked_texture(svg_src, px)));
    prepare_icon(&img, px as i32);
    register_baked_icon(&img, svg_src, px);
    img
}

/// Rasterise at 2× the display size so Lucide strokes stay sharp when GTK
/// displays the texture at `px` via `Image::set_pixel_size`.
pub(crate) fn svg_texture_sized(svg_src: &str, px: u32) -> gtk4::gdk::Texture {
    use resvg::{tiny_skia, usvg};
    let raster = px.saturating_mul(2).max(1);
    let fg = theme::fg_color();
    let dim = format!(r#"width="{raster}" height="{raster}""#);
    let svg = svg_src
        .replace("currentColor", &fg)
        .replace(r#"stroke-width="2""#, r#"stroke-width="2.35""#)
        .replace(r#"width="24" height="24""#, &dim);
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).expect("parse svg");
    let size = tree.size().to_int_size();
    let (w, h) = (size.width(), size.height());
    let mut pixmap = tiny_skia::Pixmap::new(w, h).expect("alloc pixmap");
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let bytes = gtk4::glib::Bytes::from_owned(pixmap.take());
    gtk4::gdk::MemoryTexture::new(
        w as i32,
        h as i32,
        gtk4::gdk::MemoryFormat::R8g8b8a8Premultiplied,
        &bytes,
        (w * 4) as usize,
    )
    .upcast()
}

fn stat_label() -> gtk4::Label {
    let lbl = gtk4::Label::new(None);
    lbl.add_css_class("stat-label");
    lbl.set_xalign(0.0);
    lbl
}

fn main() {
    use clap::Parser;

    // Route `tracing` events (this crate's per-output palette-fallback
    // warning, plus bread-theme's own shell/theme fallback logs) to stderr.
    // Quiet by default; `BREADBAR_LOG=debug` (or standard `RUST_LOG`) opens
    // it up.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("BREADBAR_LOG")
                .or_else(|_| tracing_subscriber::EnvFilter::try_from_default_env())
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = screenshot::Cli::parse();
    if cli.history {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        if let Err(e) = rt.block_on(notifications::toggle_history_remote()) {
            eprintln!("breadbar: could not toggle history (is breadbar running?): {e}");
            std::process::exit(1);
        }
        return;
    }
    if let Some(mode) = &cli.pill {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let result = match mode.as_str() {
            "toggle" => rt.block_on(notifications::toggle_pill_remote()),
            // `Cli::pill`'s `value_parser` already restricts this to
            // "toggle"/"show"; anything else is unreachable.
            _ => rt.block_on(notifications::show_pill_remote()),
        };
        if let Err(e) = result {
            eprintln!("breadbar: could not {mode} the pill (is breadbar running?): {e}");
            std::process::exit(1);
        }
        return;
    }
    if let Some(mode) = &cli.stage {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let result = match mode.as_str() {
            "toggle" => rt.block_on(notifications::toggle_stage_remote()),
            "show" => rt.block_on(notifications::show_stage_remote()),
            // `Cli::stage`'s `value_parser` already restricts this to
            // "toggle"/"show"/"hide"; anything else is unreachable.
            _ => rt.block_on(notifications::hide_stage_remote()),
        };
        if let Err(e) = result {
            eprintln!("breadbar: could not {mode} the stage (is breadbar running?): {e}");
            std::process::exit(1);
        }
        return;
    }
    let screenshot_req = cli.screenshot_request();

    relm4::spawn(async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut stream = signal(SignalKind::hangup()).expect("SIGHUP handler");
        loop {
            stream.recv().await;
            // Full refresh, not just `theme::apply`: re-bind the persistent
            // bars/panels and re-bake icon textures too (see theme::reload).
            gtk4::glib::MainContext::default().invoke(theme::reload);
        }
    });

    // Best-effort cleanup when killed (Ctrl+C in the terminal, a dev-loop
    // `kill`, systemd stop): kill the OSD's `pactl subscribe` children
    // before exiting; the exit hook in osd.rs covers normal exits and
    // panics. Without either, each breadbar restart orphaned a `pactl
    // subscribe` holding its PulseAudio connection open until
    // pipewire-pulse's client cap filled up and new clients, settings
    // apps among them, were refused ("no devices in settings").
    relm4::spawn(async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        let mut intr = signal(SignalKind::interrupt()).expect("SIGINT handler");
        tokio::select! {
            _ = term.recv() => {}
            _ = intr.recv() => {}
        }
        crate::osd::kill_watchers();
        std::process::exit(0);
    });

    // `with_args(vec![])` stops relm4 from handing our own --screenshot/
    // --output flags to GLib's option parser (`app.run()`'s default), which
    // would otherwise reject them as unrecognized before Cli::parse() above
    // ever sees argv. allow_multiple_instances is needed for screenshot runs
    // specifically: GApplication is single-instance by default, and a normal
    // breadbar is typically already running, so without this a screenshot
    // invocation would just activate that existing instance instead of
    // starting a fresh one whose `init()` receives the request at all.
    let app = RelmApp::new("sh.breadway.breadbar").with_args(vec![]);
    if screenshot_req.is_some() {
        app.allow_multiple_instances(true);
    }
    app.run::<App>(BarInit {
        screenshot: screenshot_req,
        monitor: None,
        primary: true,
        shared: None,
    });
}

/// Live outputs only. `hyprctl monitors all` (and the hyprland crate's
/// `Monitors::get`) keep ghost connectors after a rename: `DVI-I-1` stayed
/// at 0×0 with `disabled=false` after the panel became `DVI-I-2`, and a
/// geometry fallback then stacked a second bar on the laptop.
#[derive(Debug, Clone, serde::Deserialize)]
struct HyprMon {
    name: String,
    x: i32,
    y: i32,
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    disabled: bool,
}

fn hypr_monitors_live() -> Vec<HyprMon> {
    let output = match std::process::Command::new("hyprctl")
        .args(["monitors", "-j"])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    serde_json::from_slice::<Vec<HyprMon>>(&output)
        .unwrap_or_default()
        .into_iter()
        .filter(|m| !m.disabled)
        .collect()
}

fn primary_hypr_monitor() -> Option<String> {
    let mons = hypr_monitors_live();
    mons.iter()
        .find(|m| m.focused)
        .or_else(|| mons.first())
        .map(|m| m.name.clone())
}

/// Whether a launcher open request (breadbox's `bread.box.open_requested`
/// redirect, or `bread.command.box.open`) should toggle the stage: only for an
/// embedded-launcher theme that places the capsule (`launcher_entry`) in no
/// bar slot, where the capsule is built but never shown. Overlay-launcher
/// themes keep the existing capsule route untouched.
fn open_request_goes_to_stage(theme: &bread_theme::shell::ShellTheme) -> bool {
    let slots = theme.slots();
    let capsule_placed = [&slots.left, &slots.centre, &slots.right, &slots.drawer]
        .iter()
        .any(|slot| slot.iter().any(|m| m == "launcher_entry"));
    theme.launcher().mode == bread_theme::shell::LauncherMode::Embedded && !capsule_placed
}

/// Where `AppInput::OpenLauncher` should be actually handled: locally (this
/// instance grabs its own capsule's focus), or forwarded to a specific
/// satellite instance. Pure decision logic, split out of the `update` match
/// arm so it's unit-testable without a live `App`/GTK/Hyprland stack.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LauncherRoute {
    Local,
    Satellite(String),
}

/// `focused`: the currently-focused Hyprland monitor, re-queried at
/// keybind-fire time (`None` if Hyprland's monitor query failed, e.g.
/// screenshot mode). `own`: this instance's own monitor, fixed at `init()`.
/// `satellites`: names of monitors this (necessarily primary) instance
/// tracks a `Controller<App>` for.
///
/// Falls back to `Local` whenever forwarding isn't possible or isn't
/// needed, so a caller can always make forward progress: no focused
/// monitor, the focused monitor is this instance's own, or the focused
/// monitor has no tracked satellite yet.
fn resolve_launcher_route(focused: Option<&str>, own: &str, satellites: &[&str]) -> LauncherRoute {
    match focused {
        Some(name) if name != own && satellites.contains(&name) => {
            LauncherRoute::Satellite(name.to_string())
        }
        _ => LauncherRoute::Local,
    }
}

fn hypr_monitor_names() -> Vec<String> {
    hypr_monitors_live().into_iter().map(|m| m.name).collect()
}

fn hypr_monitor_origin(name: &str) -> Option<(i32, i32)> {
    hypr_monitors_live()
        .into_iter()
        .find(|m| m.name == name)
        .map(|m| (m.x, m.y))
}

/// The bar/capsule's own layer-surface geometry for `monitor`, straight
/// from the compositor (`hyprctl layers -j`, ground truth, not derived
/// from anything GTK/gtk4-layer-shell reports client-side, since the
/// wlr-layer-shell protocol never hands a client its own assigned x/y back;
/// only width/height come through `configure`). Matched by `namespace`
/// ("breadbar", set via `root.set_namespace` above), which is unique per
/// output since each monitor gets its own bound `App` instance/window.
/// Returns the surface's horizontal center in Hyprland's global coordinate
/// space. `None` on any parse/lookup failure: callers must fall back to
/// the pre-existing, safe-but-broader dead-zone behaviour rather than
/// guess.
fn hypr_capsule_center_x(monitor: &str) -> Option<i32> {
    let output = std::process::Command::new("hyprctl")
        .args(["layers", "-j"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let levels = root.get(monitor)?.get("levels")?.as_object()?;
    for arr in levels.values() {
        let Some(items) = arr.as_array() else {
            continue;
        };
        for item in items {
            if item.get("namespace").and_then(|v| v.as_str()) != Some("breadbar") {
                continue;
            }
            let x = item.get("x")?.as_i64()? as i32;
            let w = item.get("w")?.as_i64()? as i32;
            return Some(x + w / 2);
        }
    }
    None
}

/// The click-away scrim's capsule-column hole, in coordinates local to the
/// `breadbar-dismiss` surface (see `PanelSet::show_capsule_dismiss`):
/// pure arithmetic, split out for unit testing. `capsule_center_global` and
/// `monitor_origin_x` are both in Hyprland's global compositor space
/// (`hypr_capsule_center_x`/`hypr_monitor_origin`); `column_width` is the
/// capsule's own *configured* search-state width
/// (`[launcher].search_width`), not a live-queried one: this fires right
/// as `open_fn` starts the width-animation from idle to search width, so a
/// live query at that exact instant would catch it mid-transition. Using
/// the wider, settled target here (like `DRAWER_MAX_HEIGHT_PX` already does
/// for the vertical bound) means the hole is never narrower than the
/// capsule ever actually gets while the scrim is showing.
fn capsule_dismiss_hole(
    capsule_center_global: i32,
    monitor_origin_x: i32,
    column_width: i32,
) -> (i32, i32) {
    let local_center = capsule_center_global - monitor_origin_x;
    (local_center - column_width / 2, column_width)
}

/// Hyprland connector names and GDK connector names can disagree after a
/// hotplug (`DVI-I-1` vs `DVI-I-2`). Match the connector first, then the
/// output's origin: transform swaps width/height so size is not reliable.
/// Never steal a GDK output whose connector is already a live Hyprland name.
fn gdk_monitor_for_hypr(name: &str) -> Option<gtk4::gdk::Monitor> {
    use gtk4::gdk::prelude::MonitorExt;
    use gtk4::gio::prelude::ListModelExt;
    let display = gtk4::gdk::Display::default()?;
    let list = display.monitors();
    for i in 0..list.n_items() {
        let Some(mon) = list.item(i).and_downcast::<gtk4::gdk::Monitor>() else {
            continue;
        };
        if mon.connector().as_deref() == Some(name) {
            return Some(mon);
        }
    }
    let (hx, hy) = hypr_monitor_origin(name)?;
    let live = hypr_monitor_names();
    for i in 0..list.n_items() {
        let Some(mon) = list.item(i).and_downcast::<gtk4::gdk::Monitor>() else {
            continue;
        };
        let g = mon.geometry();
        if g.x() != hx || g.y() != hy {
            continue;
        }
        if let Some(conn) = mon.connector() {
            if live.iter().any(|n| n != name && n == conn.as_str()) {
                return None;
            }
        }
        return Some(mon);
    }
    None
}

pub(crate) fn bind_layer_monitor(window: &impl LayerShell, name: &str) -> bool {
    match gdk_monitor_for_hypr(name) {
        Some(mon) => {
            window.set_monitor(Some(&mon));
            true
        }
        None => {
            eprintln!("breadbar: no GDK monitor for {name}");
            false
        }
    }
}

fn spawn_satellite(name: &str, shared: stage::Shared) -> Option<Controller<App>> {
    if gdk_monitor_for_hypr(name).is_none() {
        eprintln!("breadbar: skip bar on {name}: no matching GDK output");
        return None;
    }
    let ctrl = App::builder()
        .launch(BarInit {
            screenshot: None,
            monitor: Some(name.to_string()),
            primary: false,
            shared: Some(shared),
        })
        .detach();
    ctrl.widget().present();
    Some(ctrl)
}

/// Watches `spotlight.json` (creating its directory first, since GIO cannot
/// watch a path under a missing one) and posts [`AppInput::PrefsChanged`]
/// on every change.
fn watch_prefs(sender: &ComponentSender<App>) -> Option<gtk4::gio::FileMonitor> {
    let path = spotlight_prefs::path()?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let monitor = gtk4::gio::File::for_path(&path)
        .monitor_file(
            gtk4::gio::FileMonitorFlags::WATCH_MOVES,
            gtk4::gio::Cancellable::NONE,
        )
        .ok()?;
    let s = sender.clone();
    monitor.connect_changed(move |_, _, _, _| {
        post(&s, AppInput::PrefsChanged(spotlight_prefs::load()));
    });
    Some(monitor)
}

/// Queue `msg` for this bar instance without panicking if it is gone.
///
/// A satellite bar's `Controller` is dropped when its monitor disconnects
/// (`drop_satellite`), which shuts its runtime down. relm4's `input()` panics
/// on a shut-down runtime, and a panic inside a GTK signal handler or glib
/// timer cannot unwind, so the whole process aborted: an orphaned pill hot
/// zone firing `PillHoverEnter` after a dock replug killed breadbar. Dropping
/// the message is correct, since there is no bar left to act on it.
fn post(sender: &ComponentSender<App>, msg: AppInput) {
    let _ = sender.input_sender().send(msg);
}

fn drop_satellite(satellites: &mut Vec<(String, Controller<App>)>, name: &str) {
    satellites.retain(|(n, ctrl)| {
        if n == name {
            ctrl.widget().set_visible(false);
            false
        } else {
            true
        }
    });
}

#[cfg(test)]
mod open_request_goes_to_stage_tests {
    use super::open_request_goes_to_stage;
    use bread_theme::shell::load_named;

    #[test]
    fn spotlight_sends_open_requests_to_the_stage() {
        assert!(open_request_goes_to_stage(
            &load_named("spotlight").unwrap()
        ));
    }

    #[test]
    fn overlay_launcher_themes_are_unaffected() {
        for id in ["liquid-motion", "glass-workbench", "daylight", "loaf"] {
            assert!(
                !open_request_goes_to_stage(&load_named(id).unwrap()),
                "{id}"
            );
        }
    }
}

#[cfg(test)]
mod launcher_route_tests {
    use super::{resolve_launcher_route, LauncherRoute};

    #[test]
    fn focused_monitor_is_own_stays_local() {
        assert_eq!(
            resolve_launcher_route(Some("eDP-1"), "eDP-1", &["DVI-I-1"]),
            LauncherRoute::Local
        );
    }

    #[test]
    fn focused_monitor_is_tracked_satellite_forwards() {
        assert_eq!(
            resolve_launcher_route(Some("DVI-I-1"), "eDP-1", &["DVI-I-1"]),
            LauncherRoute::Satellite("DVI-I-1".to_string())
        );
    }

    #[test]
    fn focused_monitor_with_no_tracked_satellite_falls_back_local() {
        // e.g. reconcile hasn't caught up with a very recent hotplug yet.
        assert_eq!(
            resolve_launcher_route(Some("HDMI-A-1"), "eDP-1", &["DVI-I-1"]),
            LauncherRoute::Local
        );
    }

    #[test]
    fn no_focused_monitor_falls_back_local() {
        // Hyprland's monitor query failed (screenshot mode, hyprctl missing).
        assert_eq!(
            resolve_launcher_route(None, "eDP-1", &["DVI-I-1"]),
            LauncherRoute::Local
        );
    }
}

#[cfg(test)]
mod capsule_dismiss_hole_tests {
    use super::capsule_dismiss_hole;

    #[test]
    fn centered_capsule_on_primary_monitor_at_origin() {
        // A 520px-wide capsule centered on a 1920px-wide monitor at global
        // origin (0,0): global center x = 960, monitor origin x = 0.
        let (x, w) = capsule_dismiss_hole(960, 0, 520);
        assert_eq!((x, w), (960 - 260, 520));
    }

    #[test]
    fn negative_origin_secondary_monitor_converts_to_local() {
        // This machine's own DVI-I-1 (`hyprctl layers -j`, quoted in this
        // module's doc comments): monitor origin x = -1080. A capsule
        // centered on that output's own 1080px-wide span sits at global
        // center x = -1080 + 540 = -540.
        let (x, w) = capsule_dismiss_hole(-540, -1080, 520);
        // Local center is 540 (origin subtracted back out); hole starts
        // 260px to either side of it, independent of the monitor's sign.
        assert_eq!((x, w), (540 - 260, 520));
    }

    #[test]
    fn hole_width_always_matches_requested_column_width() {
        let (_, w) = capsule_dismiss_hole(100, 0, 480);
        assert_eq!(w, 480);
    }
}

#[cfg(test)]
mod url_open_target_tests {
    use super::url_open_target;

    #[test]
    fn bare_host_gets_https_scheme() {
        assert_eq!(url_open_target("example.com"), "https://example.com");
    }

    #[test]
    fn existing_scheme_is_left_untouched() {
        assert_eq!(url_open_target("http://example.com"), "http://example.com");
        assert_eq!(
            url_open_target("ftp://example.com/file"),
            "ftp://example.com/file"
        );
    }
}

macro_rules! asset {
    ($n:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/", $n))
    };
}

mod bar;
mod launcher_command;
mod notifications;
mod osd;
mod panel;
mod screenshot;
mod surface;
mod theme;
mod widgets;

use bread_launcher::gtk::ResultsList;
use bread_theme::shell::{ClockStyle, Exclusive, Keyboard, Width, WorkspaceStyle};
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use hyprland::data::Workspace;
use hyprland::shared::WorkspaceId;
use relm4::prelude::*;
use relm4::{Component, ComponentController, Controller};
use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;

/// The launched-app event's publisher id and event name — deliberately
/// matching breadbox's OWN constants (`breadbox/src/main.rs`: `APP_ID =
/// "box"`, `LAUNCHED_EVENT = "bread.box.launched"`), NOT breadbar's own
/// `widgets::client::APP_ID` ("bar"). Theme 04/spotlight's capsule IS the
/// launcher wearing a different shell (plan §7), sharing breadbox's cache
/// and history via `bread_launcher::LAUNCHER_APP` — it publishes under that
/// same launcher identity too, so anything downstream listening for "an app
/// was launched via the launcher" sees one event stream regardless of which
/// surface launched it.
const LAUNCHER_APP_ID: &str = "box";
const LAUNCHER_LAUNCHED_EVENT: &str = "bread.box.launched";

/// Diagnostic trace for the capsule's open/close/focus wiring, gated
/// behind an env var so it costs nothing by default. Three previous fixes
/// for the "opens itself at startup with no focus" bug all failed because
/// nothing could observe which path actually fired on a real desktop —
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
/// .results { max-height: 420px }`) — see [`drawer_target_height`]. Also
/// used to size the click-away scrim's dead zone (see `open_fn` in `init`):
/// the scrim's clickable region never reaches higher than the capsule row
/// plus this much, so it can never overlap a real result row regardless of
/// how tall the drawer currently is.
const DRAWER_MAX_HEIGHT_PX: i32 = 420;

pub struct BarInit {
    pub screenshot: Option<screenshot::ScreenshotRequest>,
    pub monitor: Option<String>,
    pub primary: bool,
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
    // text instead of (or alongside) any clock label — see that handler.
    launcher_entry: gtk4::Entry,
    // Whether the capsule's drawer is currently expanded — read by
    // `ClockTick` so a live search in progress never has its placeholder
    // text stomped (it wouldn't be visible anyway once there's real text,
    // but matches the demo's own `if (!open) q.placeholder = t;` guard).
    launcher_open: Rc<Cell<bool>>,
    // `AppInput::OpenLauncher`'s local-route arm needs to actually open the
    // capsule, not just move keyboard focus onto `launcher_entry` — focus
    // alone stopped opening it when `connect_enter`'s `open_fn()` call was
    // removed (see that handler's own comment for why: it was the
    // mechanism behind the startup-open bug). This is `init`'s `open_fn`,
    // stored so the hotkey/command path can call it directly, the same way
    // the click gesture and `connect_changed` do at their own call sites.
    launcher_open_fn: Rc<dyn Fn()>,

    // ── Stats bar ─────────────────────────────────────────────────────────
    // Island chrome matches the Liquid Motion demo: volume / wifi / battery
    // / hamburger. CPU/RAM/power live in the control panel, not on the bar.
    system_stats_box: gtk4::Box,
    system_sep: gtk4::Separator,
    cpu_pair: gtk4::Box,
    mem_pair: gtk4::Box,
    pwr_pair: gtk4::Box,
    gpu_pair: gtk4::Box,
    cpu_lbl: gtk4::Label,
    mem_lbl: gtk4::Label,
    pwr_lbl: gtk4::Label,
    // `[bar.slots].right = [..., "cpu", "ram", ...]` (glass-workbench, Phase
    // 5): separate instances from `cpu_pair`/`mem_pair` above, which stay
    // parented in the control panel's sys-grid — a GTK widget can only have
    // one parent, so reusing those here would mean reparenting them out of
    // the panel, changing panel behaviour no theme asked to change. Fed by
    // the same `AppInput::StatsUpdate` data.
    bar_cpu_lbl: gtk4::Label,
    bar_ram_lbl: gtk4::Label,
    gpu_lbl: gtk4::Label,
    // Odometer-style digit chips (plan: "ODOMETER DIGITS FOR NUMERIC
    // CHIPS") — reuses the clock's `make_clock_digits`/`flip_clock_digits`
    // machinery (see `make_digit_chip`/`flip_digit_chip` below), so the
    // volume/battery numbers roll per-digit instead of snapping. `Rc<RefCell<..>>`,
    // not a plain `Vec`, because the label set is also mutated from the
    // control panel's volume-slider `connect_value_changed` closure (set up
    // in `init`, before `self` exists) as well as from `update`'s
    // `StatsUpdate` handler — both need to see and replace the same digit
    // set.
    vol_lbl: gtk4::Box,
    vol_digits: Rc<RefCell<Vec<gtk4::Label>>>,
    bat_lbl: gtk4::Box,
    bat_digits: Rc<RefCell<Vec<gtk4::Label>>>,
    bat_img: gtk4::Image,
    bat_textures: std::collections::HashMap<usize, gtk4::gdk::Texture>,
    ac_img: gtk4::Image,
    bt_img: gtk4::Image,
    bt_textures: std::collections::HashMap<usize, gtk4::gdk::Texture>,
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
    tray_items: std::collections::HashMap<String, gtk4::Button>,

    // ── Lua-declared widgets ─────────────────────────────────────────────
    // One container per `widget:<key>` slot entry (Phase 3b — see
    // bar::slots::ModuleRegistry and reconcile_widgets' routing below),
    // fully rebuilt on every AppInput::WidgetsUpdate — see widgets::client's
    // module doc for why that's simpler than incremental patching here.
    // Keyed by the slot entry's key: either a WidgetPlacement alias
    // (`right_of_workspaces`, `left_of_clock`, `right_of_clock`,
    // `left_of_stats`, `tray`) or a Lua module name. `bread_shared::widget`'s
    // `WidgetPlacement` itself never appears here — it's a wire type from
    // the bread daemon API and stays untouched.
    widget_containers: std::collections::HashMap<String, gtk4::Box>,
    /// Widget ids already reported as undeliverable by `reconcile_widgets`, so
    /// the warning fires once per widget instead of once per reconcile.
    dropped_widget_warned: std::collections::HashSet<String>,
    widget_tray_section: gtk4::Box,
    widget_tray_sep: gtk4::Separator,

    panels: panel::PanelSet,
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
    WidgetsUpdate(Vec<bread_shared::widget::WidgetSpec>),
    ReconcileMonitors,
    DismissPanels,
    // `bread.command.box.open` (plan §7 phase 6c, `launcher_command`
    // module): only ever dispatched when the active theme's launcher is
    // `Embedded` — `launcher_command::spawn` never subscribes otherwise.
    // Focuses `launcher_entry` AND calls `self.launcher_open_fn()`
    // directly (its handler below) — the hotkey/command itself is the
    // real-user-input signal, the same status a mouse click into the
    // entry has at its own call site. Deliberately does NOT rely on
    // `grab_focus()` triggering `EventControllerFocus::connect_enter` to
    // open it as a side effect any more; that indirection was the actual
    // mechanism behind the startup-open bug (see `connect_enter`'s own
    // comment in `init`).
    OpenLauncher,
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
            // workbench) — see main.rs's "Assemble" section and
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
                    // edge — so every valign:Center child centred within a
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

        // `bar.window` (plan §2/§6) — window shape is data, not a closed
        // layout enum. Read once and reused below for the layer-shell setup
        // and (via `bar_height`, captured for the view! macro above) the
        // root window's initial GTK height.
        let window_spec = theme::shell_theme().window().clone();
        let bar_height = window_spec.height;

        root.init_layer_shell();
        root.set_namespace(Some("breadbar"));
        root.set_layer(if window_spec.layer == "overlay" {
            Layer::Overlay
        } else {
            Layer::Top
        });
        for anchor in &window_spec.anchors {
            match anchor.as_str() {
                "top" => root.set_anchor(Edge::Top, true),
                "bottom" => root.set_anchor(Edge::Bottom, true),
                "left" => root.set_anchor(Edge::Left, true),
                "right" => root.set_anchor(Edge::Right, true),
                other => eprintln!(
                    "breadbar: bar.window.anchors entry \"{other}\" is not top|bottom|left|right, ignoring"
                ),
            }
        }
        root.set_margin(Edge::Top, window_spec.margin.top);
        root.set_margin(Edge::Left, window_spec.margin.left);
        root.set_margin(Edge::Right, window_spec.margin.right);
        // `Width::Fill` (Island/Edge): unset, exactly as before this
        // change — the surface stretches to the anchored left/right edges
        // on its own, with no explicit width request needed. `Width::Px`
        // (the capsule, anchored top-only): gtk4-layer-shell has nothing to
        // stretch it TO, so without this it would size to its natural
        // content width instead of the theme's requested 480px — this was
        // a schema key declared but never consumed before theme 04 needed
        // a real value out of it.
        if let Width::Px(px) = window_spec.width {
            root.set_default_width(px);
            // set_default_width alone is only a preference — a wide child (the
            // results list, whose natural width is its longest app name plus
            // icon and wm-class) overrides it, so the capsule rendered far
            // wider than the theme's 480px and stopped reading as a pill.
            // Pinning the request keeps the surface at the theme's width
            // regardless of what the app catalog contains.
            root.set_size_request(px, -1);
        }
        // "auto" reserves height + top margin so tiled clients sit below the
        // gap — see WindowSpec::exclusive's doc comment (bread-theme).
        let exclusive_zone = match window_spec.exclusive {
            Exclusive::Auto => window_spec.height + window_spec.margin.top,
            Exclusive::None => -1,
            Exclusive::Px(px) => px,
        };
        root.set_exclusive_zone(exclusive_zone);
        // breadbar never called `set_keyboard_mode` before Phase 2 — it
        // relied on gtk4-layer-shell's own default (`KeyboardMode::None`),
        // which is exactly what the builtin manifest's `keyboard = "none"`
        // resolves to. Same behaviour, no longer implicit.
        root.set_keyboard_mode(match window_spec.keyboard {
            Keyboard::None => KeyboardMode::None,
            Keyboard::OnDemand => KeyboardMode::OnDemand,
            Keyboard::Exclusive => KeyboardMode::Exclusive,
        });
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

        // ── Workspace row (left) ────────────────────────────────────────
        // Built imperatively (not via the view! macro) so a widget
        // container can sit as a plain sibling of workspace_box — see the
        // `widget:*` slot-entry handling in "Assemble" below. The Overlay
        // trail lives behind the buttons; rebuild_buttons only touches the
        // button box, never the trail host.
        let workspace_trail = bar::workspaces::WorkspaceTrail::new();
        let workspace_box = workspace_trail.buttons.clone();
        let workspace_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        workspace_row.set_margin_start(8);
        workspace_row.set_valign(gtk4::Align::Center);
        workspace_row.set_vexpand(false);
        // `workspace_trail.overlay` is appended in the "Assemble" section
        // below, in the order `[bar.slots].left` names it — not here.

        // ── Lua-declared widget containers ──────────────────────────────
        // Phase 3b: a container per `widget:<key>` slot entry is created
        // on demand while walking `[bar.slots]` in the "Assemble" section
        // below (see `bar::slots::widget_slot_container`), so ANY slot can
        // host a Lua widget — not just the four fixed positions Phase 3a
        // shipped with. Populated by widgets::client's events.subscribe-
        // driven refresh loop, started at the end of init.

        // `tokens.icon_px` (plan §4) — bar-chrome icon pixel size; reused
        // below for every `prepare_icon` call in this function.
        let icon_px = theme::shell_theme().tokens().icon_px() as i32;

        // ── SVG icon sets ────────────────────────────────────────────────
        use bar::stats::{
            AC_POWER, BAT_HIGH, BAT_LOW, BAT_MID, BT_CONNECTED, BT_OFF, BT_ON, ICON_VOLUME,
        };
        let bat_textures: std::collections::HashMap<usize, gtk4::gdk::Texture> =
            [BAT_HIGH, BAT_MID, BAT_LOW]
                .into_iter()
                .map(|p| (p.as_ptr() as usize, svg_texture(p)))
                .collect();
        let bt_textures: std::collections::HashMap<usize, gtk4::gdk::Texture> =
            [BT_OFF, BT_ON, BT_CONNECTED]
                .into_iter()
                .map(|p| (p.as_ptr() as usize, svg_texture(p)))
                .collect();
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
        let bat_img = gtk4::Image::from_paintable(Some(
            bat_textures.get(&(BAT_MID.as_ptr() as usize)).unwrap(),
        ));
        prepare_icon(&bat_img, icon_px);
        let ac_img = svg_image(AC_POWER);
        ac_img.set_visible(false);
        let bt_img = gtk4::Image::from_paintable(Some(
            bt_textures.get(&(BT_OFF.as_ptr() as usize)).unwrap(),
        ));
        prepare_icon(&bt_img, icon_px);
        bt_img.set_visible(false);

        // ── WiFi pair + popover ──────────────────────────────────────────
        let wifi_lbl = gtk4::Label::new(None);
        wifi_lbl.add_css_class("stat-label");
        wifi_lbl.add_css_class("wifi-label");
        wifi_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        wifi_lbl.set_max_width_chars(28);
        wifi_lbl.set_xalign(0.0);
        // SSID lives in the popover + tooltip — a 20-char network name
        // crowding the tray is the opposite of a glass workbench bar.
        wifi_lbl.set_visible(false);
        let wifi_img = gtk4::Image::from_icon_name(bar::stats::WIFI_ICON_EXCELLENT);
        prepare_icon(&wifi_img, icon_px);
        wifi_img.add_css_class("stat-icon");

        // Content pane only — this becomes a tab inside the merged
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
        // that changed — same motion as the Liquid Motion demo.
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
        // slot below — see "Assemble".
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

        // ── Launcher entry + results (theme 04/spotlight, plan §7) ───────
        // Built unconditionally, exactly like `clock_plain_box` above —
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
            .and_then(|s| serde_json::from_str::<std::collections::HashMap<String, String>>(&s).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| (k, std::path::PathBuf::from(v)))
            .collect();
        let launcher_history = Rc::new(RefCell::new(bread_launcher::LaunchHistory::load(
            bread_launcher::LAUNCHER_APP,
        )));
        // No per-workspace priority context here — that's breadbox's own
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
        launcher_results.scroller.add_css_class("bread-drawer-scroller");

        let launcher_entry = gtk4::Entry::new();
        launcher_entry.add_css_class("launcher-entry");
        launcher_entry.set_has_frame(false);
        launcher_entry.set_hexpand(true);
        // Starts non-focusable ("the spotlight theme starts with the
        // search open"). GTK4 auto-assigns keyboard focus to the first
        // can-focus widget in a window as it's first mapped/shown — with
        // nothing else focusable in the bar, that was always
        // `launcher_entry`, and `focus_ctrl`'s `connect_enter` below
        // (added unconditionally) turns "the entry gained focus" straight
        // into `open_fn()`, so the capsule opened itself at startup. A
        // widget that can't focus is skipped by BOTH that automatic
        // selection and an explicit `grab_focus()` call — see GTK's own
        // docs for `Widget::grab_focus`: "if widget is not focusable...
        // this function does nothing." Every place that later wants real
        // focus (the click gesture below, and `AppInput::OpenLauncher`'s
        // handler) flips this back to `true` immediately before grabbing.
        launcher_entry.set_can_focus(false);
        gtk4::prelude::EntryExt::set_alignment(&launcher_entry, 0.5);
        // `modules.clock.placeholder_clock` (spotlight): the entry's idle
        // placeholder IS the clock — no separate clock module renders at
        // all under `style = "none"`. Any other theme gets a plain "Search"
        // placeholder (never shown today: no other builtin slots
        // "launcher_entry" anywhere), so this still degrades sanely if a
        // future/user theme places it without also setting the flag.
        let placeholder_clock = theme::shell_theme().modules().clock.placeholder_clock;
        launcher_entry.set_placeholder_text(Some(if placeholder_clock {
            bar::clock::time()
        } else {
            "Search".to_string()
        }.as_str()));

        // Center area: [media_widget · widgets · clock · widgets]
        let center_area = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
        center_area.add_css_class("center-area");
        center_area.set_valign(gtk4::Align::Center);
        center_area.set_vexpand(false);
        // `media_widget`/`clock_box` and any `widget:*` entries interleaved
        // around them are all appended in "Assemble" below, in the exact
        // order `[bar.slots].centre` names them.

        // ── Stats box (right side) ───────────────────────────────────────
        // Demo order: [vol 64] [wifi] [bat 83] [☰]
        let stats_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 2);
        stats_box.add_css_class("stats-box");
        stats_box.set_margin_end(2);
        stats_box.set_valign(gtk4::Align::Center);
        stats_box.set_vexpand(false);
        // Also appended in "Assemble" — before the right slot's modules,
        // preserving its fixed position (left of the stats modules)
        // whatever `[bar.slots].right` contains.

        // CPU/RAM/power draw stay built (control panel + screenshots still
        // read the labels) but never mount on the island — the demo bar
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
        // above, which stay parented in the control panel's sys-grid below
        // — reusing them here would mean reparenting them out of the panel.
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

        // Content pane only — same deal as wifi_pane above.
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
        // start edge and leaves the leftover width trailing after it — so
        // `halign: Center` had nothing to center within and the glyph sat a
        // few pixels left of true center (reported: wifi icon not centered
        // on glass-workbench/liquid-motion). `bat_box` never showed this
        // because it isn't `icon-only` — no forced min-width wider than its
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
        // and from there into the centerbox's end slot — which then hands
        // that slot most of the bar's remaining width instead of its
        // normal packed size. The visible symptom was dramatic, not
        // subtle: the whole vol/wifi cluster jumped left to sit against
        // the clock, with a huge dead gap before battery/hamburger, in a
        // `--screenshot bar` capture. `connectivity_pair.set_hexpand(false)`
        // pins this box's own expand explicitly, which stops the
        // computation from climbing any further — the child can still
        // fill and center within this one box's fixed 32px cell.
        wifi_img.set_hexpand(true);
        connectivity_pair.set_hexpand(false);
        connectivity_pair.append(&wifi_img);
        // `connectivity_pair` and `bat_box` are appended in "Assemble"
        // below, per `[bar.slots].right`.

        // ── Control panel popover ────────────────────────────────────────
        // Liquid Motion chrome: CONTROL / vol / bl / lock·sleep·off.
        // SNI tray + Lua tray widgets still mount here, but only as a
        // headerless icon row when something actually registers.
        let panel_inner = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        panel_inner.add_css_class("control-panel-inner");

        let panel_header = gtk4::Label::new(Some("CONTROL"));
        panel_header.add_css_class("control-panel-header");
        panel_header.set_xalign(0.0);
        panel_inner.append(&panel_header);
        panel_inner.append(&popover_caret());

        let vol_row = build_slider_row("vol", 0.0, 1.5, 0.02);
        let panel_vol_slider = vol_row.1.clone();
        panel_inner.append(&vol_row.0);

        let sink_section = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        sink_section.add_css_class("control-panel-section");
        let sink_header = gtk4::Label::new(Some("OUTPUT"));
        sink_header.add_css_class("control-panel-header");
        sink_header.set_xalign(0.0);
        sink_header.set_margin_top(6);
        let sink_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        sink_section.append(&sink_header);
        sink_section.append(&sink_box);
        sink_section.set_visible(false);
        panel_inner.append(&sink_section);

        let bright_row = build_slider_row("bl", 0.0, 1.0, 0.02);
        let panel_bright_slider = bright_row.1.clone();
        panel_inner.append(&bright_row.0);

        let sys_header = gtk4::Label::new(Some("SYSTEM"));
        sys_header.add_css_class("control-panel-header");
        sys_header.set_xalign(0.0);
        sys_header.set_margin_top(10);
        panel_inner.append(&sys_header);
        panel_inner.append(&system_stats_box);

        let power_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        power_row.add_css_class("power-row");
        power_row.set_halign(gtk4::Align::Center);
        for (label, cmd) in [
            // breadlock is the ecosystem's own screen locker — hyprlock is
            // the thing it was built to replace; the bar shouldn't still
            // be pointing at it.
            ("lock", vec!["breadlock"]),
            ("sleep", vec!["systemctl", "suspend"]),
            ("off", vec!["systemctl", "poweroff"]),
        ] {
            let btn = gtk4::Button::with_label(label);
            btn.add_css_class("flat");
            btn.add_css_class("power-btn");
            btn.connect_clicked(move |_| {
                let args = cmd.to_vec();
                relm4::spawn(async move {
                    let _ = tokio::process::Command::new(args[0])
                        .args(&args[1..])
                        .spawn();
                });
            });
            power_row.append(&btn);
        }
        panel_inner.append(&power_row);

        // SNI / Lua tray sit under the demo chrome so a single icon cannot
        // split the sliders from the power chips.
        let tray_section = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        tray_section.add_css_class("control-panel-section");
        let tray_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        tray_box.add_css_class("tray-box");
        tray_box.set_halign(gtk4::Align::Center);
        tray_section.append(&tray_box);
        tray_section.set_visible(false);
        panel_inner.append(&tray_section);
        let tray_sep = gtk4::Separator::new(gtk4::Orientation::Horizontal);
        tray_sep.set_visible(false);

        let widget_tray_section = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        widget_tray_section.add_css_class("control-panel-section");
        let widget_tray_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        widget_tray_box.add_css_class("tray-box");
        widget_tray_box.set_halign(gtk4::Align::Center);
        widget_tray_section.append(&widget_tray_box);
        widget_tray_section.set_visible(false);
        panel_inner.append(&widget_tray_section);
        let widget_tray_sep = gtk4::Separator::new(gtk4::Orientation::Horizontal);
        widget_tray_sep.set_visible(false);

        // Hamburger button — same chip chrome as volume / wifi / battery.
        let hamburger_btn = gtk4::Button::with_label("☰");
        hamburger_btn.add_css_class("flat");
        hamburger_btn.add_css_class("control-panel-btn");
        hamburger_btn.add_css_class("stat-pair");
        hamburger_btn.add_css_class("icon-only");
        bar_chip(&hamburger_btn);

        // Slider signals — use Rc<Cell<bool>> to suppress feedback during data load
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

        // Standalone layer windows — below the island, slid in by Hyprland.
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
        // container at that exact position — `<key>` is either a
        // `WidgetPlacement` alias or a Lua module name; see
        // `bar::slots::widget_slot_container` and `reconcile_widgets`'
        // routing below. This is how a Lua widget can land in ANY slot,
        // not just the four fixed positions Phase 3a shipped with.
        let bar_shell_theme = theme::shell_theme();
        let bar_slots = bar_shell_theme.slots();
        let mut bar_modules = bar::slots::ModuleRegistry::new();
        bar_modules.register("workspaces", &workspace_trail.overlay);
        bar_modules.register("media", &media_widget);
        // `modules.clock.style`: "flip" (default, liquid-motion) registers
        // the existing per-digit clock_box unchanged; "plain" (glass-
        // workbench) registers clock_plain_box instead and reveals date_lbl
        // per `show_date` — clock_box/clock_digits are still fully built in
        // that case, just never placed in any slot. "none" (Phase 6+,
        // unused today) registers neither.
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
        // `[bar.slots].right = [..., "cpu", "ram", ...]` (glass-workbench):
        // registered unconditionally, same as every other module — a theme
        // that never names "cpu"/"ram" in a slot (liquid-motion) just never
        // walks these entries in `for_each_in_slot` below, so they stay
        // built but unparented, exactly like `media_widget` does for
        // glass-workbench (which omits "media" entirely).
        bar_modules.register("cpu", &bar_cpu_pair);
        bar_modules.register("ram", &bar_ram_pair);
        // Theme 04/spotlight (plan §7): unconditional, same reasoning —
        // liquid-motion/glass-workbench never name either in a slot, so
        // both stay built but unparented for them.
        bar_modules.register("launcher_entry", &launcher_entry);
        bar_modules.register("launcher_results", &launcher_results.scroller);

        // `tray` never appears in a bar slot — it stays inside the
        // control-panel popover (built above, next to the SNI tray) — but
        // it's keyed here so `reconcile_widgets`' routing finds it the same
        // way as any slot-driven widget container.
        let mut widget_containers: std::collections::HashMap<String, gtk4::Box> =
            std::collections::HashMap::new();
        widget_containers.insert("tray".to_string(), widget_tray_box);

        bar_modules.for_each_in_slot(
            &bar_slots.left,
            |_, widget| workspace_row.append(widget),
            |key| workspace_row.append(&bar::slots::widget_slot_container(&mut widget_containers, key)),
        );
        bar_modules.for_each_in_slot(
            &bar_slots.centre,
            |_, widget| center_area.append(widget),
            |key| center_area.append(&bar::slots::widget_slot_container(&mut widget_containers, key)),
        );
        bar_modules.for_each_in_slot(
            &bar_slots.right,
            |_, widget| stats_box.append(widget),
            |key| stats_box.append(&bar::slots::widget_slot_container(&mut widget_containers, key)),
        );

        // ── Assemble ─────────────────────────────────────────────────────
        let widgets = view_output!();
        widgets.center_box.set_start_widget(Some(&workspace_row));
        widgets.center_box.set_center_widget(Some(&center_area));
        widgets.center_box.set_end_widget(Some(&stats_box));

        // `drawer` slot (plan §2/§7/§11 Phase 6): the only slot list that
        // isn't left/centre/right of the CenterBox — appended into the vbox
        // row below it instead. Empty for every theme but spotlight, so
        // `drawer_box` stays a childless, zero-height box for them (see the
        // `window.breadbar > box > centerbox` selector note in theme.rs for
        // why the vbox wrapper itself is safe for those two themes too).
        widgets.drawer_box.add_css_class("bread-drawer");
        bar_modules.for_each_in_slot(
            &bar_slots.drawer,
            |_, widget| widgets.drawer_box.append(widget),
            |key| {
                widgets
                    .drawer_box
                    .append(&bar::slots::widget_slot_container(&mut widget_containers, key))
            },
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

        // ── Query-mode results (plan §7 phase 6c: `=` calc, `>` cmd, `.`
        // url) ───────────────────────────────────────────────────────────
        // A second, small list living alongside `launcher_results.scroller`
        // in the same drawer — only one of the two is ever visible at a
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
        // `04-spotlight.html`'s `.searching .capsule { width: 520px }`) —
        // a separate animation from the drawer's own height, both driven
        // by `bread_theme::anim::spring_to` independently.
        let anim_width: Rc<RefCell<Option<gtk4::TickCallbackId>>> = Rc::new(RefCell::new(None));
        let launcher_open: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        // Set around every PROGRAMMATIC `launcher_entry.set_text(...)` call
        // (close_fn's reset-to-empty today; any future one belongs here
        // too) so `connect_changed` below can tell "the user typed a
        // character" from "code changed the buffer" and only ever opens
        // the capsule for the former — see that handler's own comment.
        // Gates the SIGNAL itself rather than hunting every caller that
        // could fire it, so no future programmatic `set_text` can reopen
        // the capsule by accident, whether or not one does today.
        let programmatic_text_change: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        // The click-away scrim's dead zone (item B, see `DRAWER_MAX_HEIGHT_PX`'s
        // own doc comment) — the capsule row's own theme-default dismiss
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
                    let from = if current_width > 0 { current_width } else { idle_width };
                    animate_capsule_width(&root, &anim_width, from, search_width);
                    // Item B: click-away scrim. Shown at a fixed dead-zone
                    // offset (`capsule_dismiss_margin`), never the live
                    // drawer height — see that constant's own doc comment
                    // for why a live-tracking offset would risk swallowing
                    // clicks meant for a result row. `hole` scopes that
                    // dead zone to the capsule's own column instead of the
                    // full screen width ("it only sometimes is dismissed
                    // when you click somewhere else") — see
                    // `capsule_dismiss_hole`'s doc comment. Falls back to
                    // the old full-width dead zone if the live geometry
                    // query fails for any reason.
                    let hole = hypr_capsule_center_x(&monitor_name_for_dismiss).and_then(
                        |center_x| {
                            let (origin_x, _) = hypr_monitor_origin(&monitor_name_for_dismiss)?;
                            Some(capsule_dismiss_hole(center_x, origin_x, search_width))
                        },
                    );
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
                // user keystroke — flagged so `connect_changed` below
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
                // surface's keyboard grab to GTK's own focus-widget state —
                // releasing focus here is what hands the keyboard back.
                gtk4::prelude::GtkWindowExt::set_focus(&root_for_focus, None::<&gtk4::Widget>);
            }
        });

        panels.set_on_dismiss({
            let close_fn = Rc::clone(&close_fn);
            move || close_fn()
        });

        // Which of the four query modes is currently driving `mode_list`
        // (plan §7 phase 6c) — read by `key_ctrl` below to route Up/Down/
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
                // ones included — `close_fn`'s own `entry.set_text("")`
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
                // already returned for a programmatic reset) — matches the
                // reference model's `q.addEventListener("input", () => {
                // if (!open) setOpen(true); ... })`: typing is itself a
                // real-user-input signal that opens the capsule.
                open_fn();
            });
        }
        {
            // Deliberately does NOT call `open_fn()`. It used to — that
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
            // click gesture below, `AppInput::OpenLauncher`'s handler) —
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
        // more — so it now needs a real, explicit grab. This click gesture
        // is that grab for the mouse path: a `GestureClick` press is a
        // genuine user-originated pointer event delivered through the
        // compositor, which is also exactly the kind of interaction
        // `KeyboardMode::OnDemand` (gtk4-layer-shell/wlr-layer-shell) is
        // documented to react to — the protocol spec (wlr-layer-shell-
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
                // removed it) — a real click is itself the user-input
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
            // target phase itself — it emits `activate` and returns TRUE, which
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
                    // selected row — nothing, for a calc result or an
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
        // as bare locals, never stored on `App` at all) — needed by the
        // screenshot dispatch just before this function returns.
        let control_panel_for_screenshot = panels.control.clone();
        let connectivity_panel_for_screenshot = panels.connectivity.clone();
        let wifi_tab_btn_for_screenshot = wifi_tab_btn.clone();
        let bt_tab_btn_for_screenshot = bt_tab_btn.clone();
        let media_panel_for_screenshot = panels.media.clone();
        let media_widget_for_screenshot = media_widget.clone();
        let media_track_lbl_for_screenshot = media_track_lbl.clone();
        // Theme 04/spotlight's capsule (plan §6b): `launcher_entry` moves
        // into `model` below, `drawer_box` lives only in `widgets` — both
        // need a clone out here for the same reason every other
        // `_for_screenshot` handle above does.
        let launcher_entry_for_screenshot = launcher_entry.clone();
        let drawer_box_for_screenshot = widgets.drawer_box.clone();

        // Never launch sibling App windows from inside this init — RelmApp
        // is still in GApplication activate, and a same-type launch here
        // creates a second primary on the laptop. Idle reconcile after.
        let satellites = Vec::new();
        if init.primary && screenshot_req.is_none() {
            let later = sender.clone();
            gtk4::glib::idle_add_local_once(move || {
                later.input(AppInput::ReconcileMonitors);
            });
        }

        let model = App {
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
            vol_lbl,
            vol_digits,
            bat_lbl,
            bat_digits,
            bat_img,
            bat_textures,
            ac_img,
            bt_img,
            bt_textures,
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
            tray_items: std::collections::HashMap::new(),
            widget_containers,
            dropped_widget_warned: std::collections::HashSet::new(),
            widget_tray_section,
            widget_tray_sep,
            panels,
        };

        theme::apply();
        theme::bind_output(&root, &model.monitor);
        bar::workspaces::spawn_watcher(sender.clone());
        bar::clock::spawn_ticker(sender.clone());
        bar::stats::spawn_poller(sender.clone());
        bar::wifi::spawn_status_poller(sender.clone());
        bar::media::spawn_poller(sender.clone());
        if init.primary {
            bar::tray::spawn_watcher(sender.clone());
            widgets::client::spawn(sender.clone());
            // `bread.command.box.open` (plan §7 phase 6c): one subscriber,
            // same reasoning as `widgets::client::spawn` above — a keybind
            // should focus ONE capsule, not every satellite monitor's.
            // A no-op call under every theme but spotlight (see the
            // module's own doc comment).
            launcher_command::spawn(sender.clone());
            // Optional (plan §10, Phase 2 item 6): live theme.toml/extra.css
            // token reload, the same way a pywal palette change already
            // hot-reloads via `apply_app_css`. One watch per process, so
            // only the primary instance arms it.
            theme::watch_hot_reload();
        }

        // Screenshot mode primes these with sample content instead of the
        // real D-Bus/pactl/backlight sources — see notifications::SampleKind
        // and osd::SampleKind's doc comments.
        let notif_sample = screenshot_req.as_ref().and_then(|r| match r.view.as_str() {
            "notification" => Some(notifications::SampleKind::Normal),
            "notification-critical" => Some(notifications::SampleKind::Critical),
            _ => None,
        });
        let notification_window = if init.primary {
            Some(notifications::spawn(notif_sample))
        } else {
            None
        };
        let osd_sample = screenshot_req.as_ref().and_then(|r| match r.view.as_str() {
            "osd-volume" => Some(osd::SampleKind::Volume),
            "osd-brightness" => Some(osd::SampleKind::Brightness),
            _ => None,
        });
        let osd_window = if init.primary {
            Some(osd::spawn(osd_sample))
        } else {
            None
        };

        if let Some(req) = screenshot_req {
            let notification_window = notification_window.filter(|_| {
                matches!(req.view.as_str(), "notification" | "notification-critical")
            });
            let osd_window =
                osd_window.filter(|_| matches!(req.view.as_str(), "osd-volume" | "osd-brightness"));
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
                },
            );
        }

        ComponentParts { model, widgets }
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
                // occupied) should rebuild the pills — otherwise a title
                // flicker on switch cancels the trail mid-stretch.
                let rows_changed = visible_ws_rows(&sorted, &self.monitor, new_active)
                    != visible_ws_rows(&self.workspaces, &self.monitor, self.active_ws);
                let active_changed = new_active != self.active_ws;
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
                        // stretch() at all — the "active" CSS class above is
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
                if let Some(ctrl) = spawn_satellite(&name) {
                    self.satellites.push((name, ctrl));
                }
            }
            AppInput::MonitorRemoved(name) => {
                drop_satellite(&mut self.satellites, &name);
            }
            AppInput::ClockTick => {
                self.time_str = bar::clock::current();
                self.date_lbl.set_label(&bar::clock::date());
                let clock_module = theme::shell_theme().modules().clock.clone();
                match clock_module.style {
                    // Plain (glass-workbench): one label, no flip animation
                    // — `flip_clock_digits` would just be wasted work (and
                    // a pointless 450ms `play_once` timer) on digits that
                    // are never on screen.
                    ClockStyle::Plain => {
                        self.clock_plain_lbl
                            .set_label(&bar::clock::formatted(&clock_module.format));
                    }
                    // Flip (default, liquid-motion) and None both keep
                    // exactly today's per-digit-flip update — None has no
                    // module in a slot to display it, but there's no reason
                    // to special-case skipping the (cheap, idempotent) work.
                    ClockStyle::Flip | ClockStyle::None => {
                        flip_clock_digits(&self.clock_digits, &bar::clock::time());
                    }
                }
                // `modules.clock.placeholder_clock` (spotlight): the
                // capsule's entry IS the clock until focused — matches the
                // demo's own `if (!open) q.placeholder = t;` guard so a
                // live search in progress never has its placeholder text
                // (invisibly, since real text covers it) stomped mid-type.
                if clock_module.placeholder_clock && !self.launcher_open.get() {
                    self.launcher_entry
                        .set_placeholder_text(Some(&bar::clock::time()));
                }
            }
            AppInput::StatsUpdate(stats) => {
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
                self.vol_lbl.set_tooltip_text(Some(&format!("volume {}%", stats.volume_pct)));
                flip_digit_chip(&self.bat_lbl, &mut self.bat_digits.borrow_mut(), &stats.bat);
                if let Some(tex) = self.bat_textures.get(&(stats.bat_icon.as_ptr() as usize)) {
                    self.bat_img.set_paintable(Some(tex));
                }
                let bat_tip = if stats.ac_connected {
                    format!("{}% · charging", stats.bat)
                } else {
                    format!("{}%", stats.bat)
                };
                self.bat_img.set_tooltip_text(Some(&bat_tip));
                self.ac_img.set_visible(false);
                if let Some(tex) = self.bt_textures.get(&(stats.bt_icon.as_ptr() as usize)) {
                    self.bt_img.set_paintable(Some(tex));
                }
                self.current_ssid = stats.wifi_ssid.clone();
                if stats.wifi_profile.is_some() {
                    self.wifi_profile = stats.wifi_profile;
                }
                self.apply_wifi_label();
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
            AppInput::TrayUpdate(bar::tray::TrayUpdate::Add { id, icon, title }) => {
                if self.tray_items.contains_key(&id) {
                    return;
                }
                let btn = gtk4::Button::new();
                btn.add_css_class("tray-btn");
                btn.set_child(Some(&bar::tray::make_tray_image(icon.as_ref())));
                if !title.is_empty() {
                    btn.set_tooltip_text(Some(&title));
                }
                let id_click = id.clone();
                btn.connect_clicked(move |_| bar::tray::spawn_activate(id_click.clone()));
                self.tray_box.append(&btn);
                self.tray_items.insert(id, btn);
                self.tray_section.set_visible(true);
                self.tray_sep.set_visible(true);
            }
            AppInput::TrayUpdate(bar::tray::TrayUpdate::Remove { id }) => {
                if let Some(btn) = self.tray_items.remove(&id) {
                    self.tray_box.remove(&btn);
                }
                let has_items = !self.tray_items.is_empty();
                self.tray_section.set_visible(has_items);
                self.tray_sep.set_visible(has_items);
            }
            AppInput::CrumbsStatus(status) => {
                self.crumbs_status = Some(status);
                // CrumbsStatus and WifiPopoverData arrive independently (different
                // pollers); whichever lands second must still repaint the header —
                // otherwise it only shows up if CrumbsStatus happens to win the race.
                if self.wifi_popover_data.is_some() {
                    self.rebuild_wifi_popover(&sender);
                }
            }
            AppInput::WifiPopoverData(data) => {
                self.wifi_popover_data = Some(data);
                self.rebuild_wifi_popover(&sender);
            }
            AppInput::BtPopoverData(data) => {
                self.bt_popover_data = Some(data);
                self.rebuild_bt_popover();
            }
            AppInput::SetProfile(name) => {
                self.wifi_profile = Some(name);
                self.apply_wifi_label();
            }
            AppInput::MediaUpdate(state) => {
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
                    self.media_play_icon
                        .set_paintable(Some(&svg_texture(icon_svg)));
                    prepare_icon(
                        &self.media_play_icon,
                        theme::shell_theme().tokens().icon_px() as i32,
                    );
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
                    // Player gone — honour linger from last pause
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
                // Suppress slider value-changed signals during programmatic update
                self.panel_loading.set(true);
                self.panel_vol_slider.set_value(data.volume);
                self.panel_bright_slider.set_value(data.brightness);
                self.panel_loading.set(false);
                self.rebuild_sinks(&data.sinks, &sender);
            }
            AppInput::WidgetsUpdate(specs) => {
                self.reconcile_widgets(specs);
            }
            AppInput::ReconcileMonitors => {
                if self.primary {
                    self.reconcile_satellites();
                }
            }
            AppInput::DismissPanels => {
                self.panels.hide_all();
            }
            AppInput::OpenLauncher => {
                // Only the primary instance subscribes to the open command
                // (`launcher_command::spawn`), but `self.monitor` here is
                // whichever output was focused ONCE, at this instance's own
                // `init()` — baked in at process start, not re-resolved on
                // every keybind press. If the user has since moved focus to
                // a different monitor, blindly grabbing focus on `self`
                // would open the capsule on the wrong screen. Re-resolve
                // the focused monitor now and route to whichever instance
                // actually owns it.
                let satellite_names: Vec<&str> =
                    self.satellites.iter().map(|(n, _)| n.as_str()).collect();
                let focused = primary_hypr_monitor();
                match resolve_launcher_route(focused.as_deref(), &self.monitor, &satellite_names)
                {
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
        }
    }
}

/// The `widget:<key>` alias `for_each_in_slot` recognizes for each
/// `WidgetPlacement` variant — the fallback a `WidgetSpec` routes through
/// when no `widget:<module>` container claims its module name specifically.
/// Kept in one place since both the builtin manifest's slot lists and
/// `reconcile_widgets`' routing below must agree on these names.
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
    fn reconcile_widgets(&mut self, specs: Vec<bread_shared::widget::WidgetSpec>) {
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
        // entirely) is logged and dropped rather than silently vanishing —
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
                // resolving again after a theme switch stays quiet — the
                // message is about a theme lacking the slot, and repeating it
                // every tick tells the reader nothing new.
                eprintln!(
                    "breadbar: widget '{}' (module '{}', placement {:?}) has no matching \
                     [bar.slots] widget: container — dropping",
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
        // below, same as the existing SNI tray items) — an empty inline
        // slot has no such wrapper, so it must hide itself to stop
        // contributing to its parent box's `spacing` gap.
        for (key, container) in &self.widget_containers {
            if key == "tray" {
                continue;
            }
            container.set_visible(container.first_child().is_some());
        }

        self.widget_tray_section.set_visible(has_tray_widgets);
        self.widget_tray_sep.set_visible(has_tray_widgets);
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
            if let Some(ctrl) = spawn_satellite(&name) {
                self.satellites.push((name, ctrl));
            }
        }
    }

    fn rebuild_sinks(
        &mut self,
        sinks: &[bar::control::AudioSink],
        sender: &ComponentSender<Self>,
    ) {
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

    fn rebuild_buttons(&mut self, animate: bool) {
        self.workspace_trail.cancel();
        let prev: std::collections::HashSet<WorkspaceId> =
            self.button_map.keys().copied().collect();
        // `button_map` only starts empty once — the very first call this
        // App instance ever makes, before any workspace has ever been
        // synced from Hyprland. A fully-emptied bar never happens after
        // that (the active workspace's own row is always kept), so this
        // doubles as a clean "is this the initial paint" signal without a
        // dedicated flag — see its one use below.
        let is_first_build = prev.is_empty();
        while let Some(child) = self.workspace_box.first_child() {
            self.workspace_box.remove(&child);
        }
        self.button_map.clear();
        let modules = theme::shell_theme().modules().clone();
        let ws_style = modules.workspaces.style;
        let show_empty = modules.workspaces.show_empty;
        for ws in &self.workspaces {
            if ws.monitor != self.monitor {
                continue;
            }
            let empty = ws.windows == 0 && ws.id != self.active_ws;
            if empty {
                match ws_style {
                    // Trail (default, liquid-motion): unconditionally off
                    // the bar, exactly as before this change — regardless
                    // of `show_empty`, which liquid-motion's own manifest
                    // declares `true` but this style has never consumed.
                    // Changing that now would be a real, undesired
                    // liquid-motion regression, not a Phase 5 fix.
                    WorkspaceStyle::Trail => continue,
                    // Pill/Dots: honour `show_empty` for real — demo 02's
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
            // (`OCC.forEach` builds plainly, only `place(0)` runs) — only
            // *subsequently added* workspaces should ever play this.
            if !is_first_build && !prev.contains(&ws.id) {
                play_once(&btn, "ws-in", 360);
            }
            self.workspace_box.append(&btn);
            self.button_map.insert(ws.id, btn);
        }
        // Trail style only: pill/dots never call place()/stretch()/clear()
        // at all — the "active" CSS class `make_button` already applies is
        // the whole of their active-workspace treatment.
        if ws_style == WorkspaceStyle::Trail {
            match self.button_map.get(&self.active_ws).cloned() {
                Some(btn) if animate => self.workspace_trail.stretch(None, &btn),
                Some(btn) => self.workspace_trail.place(&btn),
                None => self.workspace_trail.clear(),
            }
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
                    sender_clone.input(AppInput::SetProfile(name_clone.clone()));
                    bar::wifi::spawn_profile_set(name_clone.clone());
                    panels.hide_all();
                });
                self.wifi_pane.append(&row);
            }
        }
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
                let _ = tokio::process::Command::new("blueman-manager").spawn();
            });
        });
        self.bt_pane.append(&settings_row);
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn build_slider_row(label: &str, min: f64, max: f64, step: f64) -> (gtk4::Box, gtk4::Scale) {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.add_css_class("control-panel-row");

    let lbl = gtk4::Label::new(Some(label));
    lbl.add_css_class("control-panel-row-label");
    lbl.set_xalign(0.0);
    lbl.set_width_chars(3);

    let slider = gtk4::Scale::with_range(gtk4::Orientation::Horizontal, min, max, step);
    slider.set_draw_value(false);
    slider.set_hexpand(true);
    slider.set_width_request(160);
    slider.add_css_class("control-panel-slider");

    row.append(&lbl);
    row.append(&slider);
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
/// built dialog *before* it's presented — screenshot mode's only hook point,
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
    // rounding — which is what actually made this look like a stray window
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
    // Not "suggested-action" — GTK4's own bundled theme special-cases that
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

fn stat_pair(icon_svg: &str, label: &gtk4::Label) -> gtk4::Box {
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
/// chip (volume, battery) — a plain horizontal box, same shape as
/// `vol_box`/`bat_box`'s existing icon+label chips, that gets one
/// `.stat-digit` label per character instead of a single `gtk4::Label`.
fn digit_chip_box() -> gtk4::Box {
    gtk4::Box::new(gtk4::Orientation::Horizontal, 0)
}

/// One `.stat-digit` label per character of `text` — the `stat-label`
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
/// `text` from scratch — the shared fallback [`set_digit_chip`] and
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

/// Replaces `container`'s digit labels with `text`'s, with NO animation —
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
/// characters that actually changed — same convention as
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
/// instead). Cancels any run already in flight first — reopening mid-close
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
        // `spring_ease` deliberately overshoots past t=1.0 — that bounce is the
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
/// `.searching .capsule { width: 520px }`) — the same `spring_to` +
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
/// 420px (`04-spotlight.html`: `.searching .results { max-height: 420px }`)
/// — `ResultsList`'s scroller already self-caps at 480px
/// (`max_content_height`), shared with breadbox, so this is a tighter,
/// spotlight-specific ceiling on top of that shared one, not a replacement
/// for it.
fn drawer_target_height(drawer_box: &gtk4::Box) -> i32 {
    // Deliberately never measures `drawer_box` itself. `animate_drawer_height`'s
    // tick callback calls `drawer_box.set_size_request(-1, h)` on every
    // frame, and GTK clamps a widget's own `measure()` result up to at
    // least its own explicit size request — so once an animation has run
    // even one frame, `drawer_box.measure()` reports that frame's forced
    // height (or the spring's overshoot past it), not whatever its
    // children actually need next. This bit spotlight's new query-mode
    // rows directly: switching from the (tall) app list to a one-row calc
    // result measured "437" instead of "~33", because the PREVIOUS
    // animation frame had already forced `drawer_box` to 437px.
    //
    // Summing each currently-visible child's own natural height instead
    // sidesteps this entirely — `launcher_results.scroller` and
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

/// A single-line, non-interactive row — the calc result, or a "nothing
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
/// "open this URL" prompt) — Enter/click spawns `action` once resolved by
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
/// never passes through `bash -c` at all — only a `>`-mode command's own
/// fixed, trusted `exec` string does.
#[derive(Clone)]
enum ModeAction {
    RunShell(&'static str),
    OpenUrl(String),
}

fn mode_row_action(row: &gtk4::ListBoxRow) -> Option<ModeAction> {
    unsafe { row.data::<ModeAction>("mode_action").map(|p| p.as_ref().clone()) }
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
            // No scheme-adding shell involved — `xdg-open` gets the raw
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

/// Moves `list`'s selection to the next/previous row — unlike
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

/// Clears `mode_list` and rebuilds it for `parsed` — the calc result,
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
            let matches = bread_launcher::filter_commands(
                &parsed.value,
                bread_launcher::builtin_commands(),
            );
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

pub(crate) fn svg_image(svg_src: &str) -> gtk4::Image {
    svg_image_sized(svg_src, theme::shell_theme().tokens().icon_px() as u32)
}

pub(crate) fn svg_image_sized(svg_src: &str, px: u32) -> gtk4::Image {
    let img = gtk4::Image::from_paintable(Some(&svg_texture_sized(svg_src, px)));
    prepare_icon(&img, px as i32);
    img
}

pub(crate) fn svg_texture(svg_src: &str) -> gtk4::gdk::Texture {
    svg_texture_sized(svg_src, theme::shell_theme().tokens().icon_px() as u32)
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
    let screenshot_req = cli.screenshot_request();

    relm4::spawn(async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut stream = signal(SignalKind::hangup()).expect("SIGHUP handler");
        loop {
            stream.recv().await;
            gtk4::glib::MainContext::default().invoke(theme::apply);
        }
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
    });
}

/// Live outputs only. `hyprctl monitors all` (and the hyprland crate's
/// `Monitors::get`) keep ghost connectors after a rename — `DVI-I-1` stayed
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
    hypr_monitors_live()
        .into_iter()
        .map(|m| m.name)
        .collect()
}

fn hypr_monitor_origin(name: &str) -> Option<(i32, i32)> {
    hypr_monitors_live()
        .into_iter()
        .find(|m| m.name == name)
        .map(|m| (m.x, m.y))
}

/// The bar/capsule's own layer-surface geometry for `monitor`, straight
/// from the compositor (`hyprctl layers -j`, ground truth — not derived
/// from anything GTK/gtk4-layer-shell reports client-side, since the
/// wlr-layer-shell protocol never hands a client its own assigned x/y back;
/// only width/height come through `configure`). Matched by `namespace`
/// ("breadbar", set via `root.set_namespace` above), which is unique per
/// output since each monitor gets its own bound `App` instance/window.
/// Returns the surface's horizontal center in Hyprland's global coordinate
/// space. `None` on any parse/lookup failure — callers must fall back to
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
/// `breadbar-dismiss` surface (see `PanelSet::show_capsule_dismiss`) —
/// pure arithmetic, split out for unit testing. `capsule_center_global` and
/// `monitor_origin_x` are both in Hyprland's global compositor space
/// (`hypr_capsule_center_x`/`hypr_monitor_origin`); `column_width` is the
/// capsule's own *configured* search-state width
/// (`[launcher].search_width`), not a live-queried one — this fires right
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
/// output's origin — transform swaps width/height so size is not reliable.
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

fn spawn_satellite(name: &str) -> Option<Controller<App>> {
    if gdk_monitor_for_hypr(name).is_none() {
        eprintln!("breadbar: skip bar on {name}: no matching GDK output");
        return None;
    }
    let ctrl = App::builder()
        .launch(BarInit {
            screenshot: None,
            monitor: Some(name.to_string()),
            primary: false,
        })
        .detach();
    ctrl.widget().present();
    Some(ctrl)
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

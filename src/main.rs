macro_rules! asset {
    ($n:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/", $n))
    };
}

mod bar;
mod notifications;
mod osd;
mod panel;
mod screenshot;
mod surface;
mod theme;
mod widgets;

use bread_theme::shell::{Exclusive, Keyboard};
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use hyprland::data::Workspace;
use hyprland::shared::WorkspaceId;
use relm4::prelude::*;
use relm4::{Component, ComponentController, Controller};
use std::cell::Cell;
use std::rc::Rc;

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
    gpu_lbl: gtk4::Label,
    vol_lbl: gtk4::Label,
    bat_lbl: gtk4::Label,
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
    // One container per WidgetPlacement (see bread_shared::widget), fully
    // rebuilt on every AppInput::WidgetsUpdate — see widgets::client's
    // module doc for why that's simpler than incremental patching here.
    widget_containers: std::collections::HashMap<bread_shared::widget::WidgetPlacement, gtk4::Box>,
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

            #[name = "center_box"]
            gtk::CenterBox {
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
        // container can sit as a plain sibling of workspace_box — see
        // WidgetPlacement::RightOfWorkspaces below. The Overlay trail
        // lives behind the buttons; rebuild_buttons only touches the
        // button box, never the trail host.
        let workspace_trail = bar::workspaces::WorkspaceTrail::new();
        let workspace_box = workspace_trail.buttons.clone();
        let workspace_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        workspace_row.set_margin_start(8);
        workspace_row.set_valign(gtk4::Align::Center);
        workspace_row.set_vexpand(false);
        workspace_row.append(&workspace_trail.overlay);

        // ── Lua-declared widget containers ──────────────────────────────
        // One per WidgetPlacement; positioned into the layout below as each
        // surrounding section (workspace row / center area / stats box /
        // control popover) is built. Populated by widgets::client's
        // events.subscribe-driven refresh loop, started at the end of init.
        use bread_shared::widget::WidgetPlacement;
        let widget_right_of_workspaces = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        widget_right_of_workspaces.add_css_class("bread-widget-slot");
        workspace_row.append(&widget_right_of_workspaces);

        let widget_left_of_clock = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        widget_left_of_clock.add_css_class("bread-widget-slot");
        let widget_right_of_clock = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        widget_right_of_clock.add_css_class("bread-widget-slot");

        let widget_left_of_stats = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        widget_left_of_stats.add_css_class("bread-widget-slot");

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
        let vol_lbl = stat_label();
        let bat_lbl = stat_label();

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
        clock_box.set_margin_top(3);
        for digit in &clock_digits {
            clock_box.append(digit);
        }
        let date_lbl = gtk4::Label::new(Some(&bar::clock::date()));
        date_lbl.add_css_class("date-label");
        date_lbl.set_visible(false);

        // Center area: [media_widget · widgets · clock · widgets]
        let center_area = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
        center_area.add_css_class("center-area");
        center_area.set_valign(gtk4::Align::Center);
        center_area.set_vexpand(false);
        center_area.append(&media_widget);
        center_area.append(&widget_left_of_clock);
        center_area.append(&clock_box);
        center_area.append(&widget_right_of_clock);

        // ── Stats box (right side) ───────────────────────────────────────
        // Demo order: [vol 64] [wifi] [bat 83] [☰]
        let stats_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 2);
        stats_box.add_css_class("stats-box");
        stats_box.set_margin_end(2);
        stats_box.set_valign(gtk4::Align::Center);
        stats_box.set_vexpand(false);
        stats_box.append(&widget_left_of_stats);

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
        vol_lbl.add_css_class("stat-label");
        vol_box.append(&vol_img);
        vol_box.append(&vol_lbl);
        stats_box.append(&vol_box);

        let bat_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        bat_box.add_css_class("stat-pair");
        bar_chip(&bat_box);
        bat_img.add_css_class("stat-icon");
        bat_lbl.add_css_class("stat-label");
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
        connectivity_pair.add_css_class("wifi-pair");
        connectivity_pair.add_css_class("icon-only");
        bar_chip(&connectivity_pair);
        wifi_img.set_halign(gtk4::Align::Center);
        wifi_img.set_hexpand(false);
        connectivity_pair.append(&wifi_img);
        stats_box.append(&connectivity_pair);
        stats_box.append(&bat_box);

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
        panel_vol_slider.connect_value_changed(move |s| {
            vol_lbl_live.set_label(&format!("{:.0}", s.value() * 100.0));
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

        stats_box.append(&hamburger_btn);

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

        let widget_containers = std::collections::HashMap::from([
            (
                WidgetPlacement::RightOfWorkspaces,
                widget_right_of_workspaces,
            ),
            (WidgetPlacement::LeftOfClock, widget_left_of_clock),
            (WidgetPlacement::RightOfClock, widget_right_of_clock),
            (WidgetPlacement::LeftOfStats, widget_left_of_stats),
            (WidgetPlacement::Tray, widget_tray_box),
        ]);

        // ── Assemble ─────────────────────────────────────────────────────
        let widgets = view_output!();
        widgets.center_box.set_start_widget(Some(&workspace_row));
        widgets.center_box.set_center_widget(Some(&center_area));
        widgets.center_box.set_end_widget(Some(&stats_box));

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
            system_stats_box,
            system_sep,
            cpu_pair,
            mem_pair,
            pwr_pair,
            gpu_pair,
            cpu_lbl,
            mem_lbl,
            pwr_lbl,
            gpu_lbl,
            vol_lbl,
            bat_lbl,
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
                        self.workspace_trail.stretch(from.as_ref(), &btn);
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
                flip_clock_digits(&self.clock_digits, &bar::clock::time());
                self.date_lbl.set_label(&bar::clock::date());
            }
            AppInput::StatsUpdate(stats) => {
                let cpu = match stats.cpu_temp {
                    Some(t) => format!("{} · {:.0}°", stats.cpu, t),
                    None => stats.cpu,
                };
                self.cpu_lbl.set_label(&cpu);
                self.mem_lbl.set_label(&stats.mem);
                self.pwr_lbl.set_label(&stats.power);
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

                tick_label(&self.vol_lbl, &stats.volume_pct.to_string());
                self.vol_lbl.set_tooltip_text(Some(&format!("volume {}%", stats.volume_pct)));
                tick_label(&self.bat_lbl, &stats.bat);
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
        }
    }
}

impl App {
    fn reconcile_widgets(&mut self, specs: Vec<bread_shared::widget::WidgetSpec>) {
        for container in self.widget_containers.values() {
            while let Some(child) = container.first_child() {
                container.remove(&child);
            }
        }

        let mut by_placement: std::collections::HashMap<
            bread_shared::widget::WidgetPlacement,
            Vec<&bread_shared::widget::WidgetSpec>,
        > = std::collections::HashMap::new();
        for spec in &specs {
            by_placement.entry(spec.placement).or_default().push(spec);
        }

        for (placement, mut group) in by_placement {
            let Some(container) = self.widget_containers.get(&placement) else {
                continue;
            };
            group.sort_by_key(|s| s.order);
            for spec in group {
                if !spec.visible {
                    continue;
                }
                let node = widgets::build_node(&spec.root, &spec.id);
                if let Some(tooltip) = &spec.tooltip {
                    node.set_tooltip_text(Some(tooltip));
                }
                container.append(&node);
            }
        }

        // The Tray placement has its own section/separator (handled below,
        // same as the existing SNI tray items) — an empty inline slot has no
        // such wrapper, so it must hide itself to stop contributing to
        // center_area's `spacing` gap.
        for (placement, container) in &self.widget_containers {
            if *placement == bread_shared::widget::WidgetPlacement::Tray {
                continue;
            }
            container.set_visible(container.first_child().is_some());
        }

        let has_tray_widgets = specs
            .iter()
            .any(|s| s.visible && s.placement == bread_shared::widget::WidgetPlacement::Tray);
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
        while let Some(child) = self.workspace_box.first_child() {
            self.workspace_box.remove(&child);
        }
        self.button_map.clear();
        for ws in &self.workspaces {
            if ws.monitor != self.monitor {
                continue;
            }
            // Persistent empty Hyprland workspaces stay off the bar unless
            // this output is actually looking at them.
            if ws.windows == 0 && ws.id != self.active_ws {
                continue;
            }
            let btn = bar::workspaces::make_button(ws.id, &ws.name, self.active_ws, ws.windows > 0);
            if !prev.contains(&ws.id) {
                play_once(&btn, "ws-in", 360);
            }
            self.workspace_box.append(&btn);
            self.button_map.insert(ws.id, btn);
        }
        match self.button_map.get(&self.active_ws).cloned() {
            Some(btn) if animate => self.workspace_trail.stretch(None, &btn),
            Some(btn) => self.workspace_trail.place(&btn),
            None => self.workspace_trail.clear(),
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

fn tick_label(lbl: &gtk4::Label, text: &str) {
    if lbl.label().as_str() == text {
        return;
    }
    lbl.set_label(text);
    play_once(lbl, "tick", 360);
}

fn reveal_media(widget: &gtk4::Box, show: bool) {
    if show && !widget.is_visible() {
        play_once(widget, "media-in", 420);
    }
    widget.set_visible(show);
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

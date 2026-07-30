macro_rules! asset {
    ($n:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/", $n))
    };
}

mod bar;
mod notifications;
mod osd;
mod screenshot;
mod theme;
mod widgets;

/// Thresholds above which the bar's CPU/RAM/power-draw readouts appear at
/// all — see `AppInput::StatsUpdate`. Below these, the bar stays quiet.
const CPU_ATTENTION_THRESHOLD: f32 = 70.0;
const MEM_ATTENTION_THRESHOLD: f32 = 80.0;
const POWER_ATTENTION_THRESHOLD: f32 = 30.0;

use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use hyprland::data::Workspace;
use hyprland::shared::WorkspaceId;
use relm4::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

pub struct App {
    // ── Workspaces ────────────────────────────────────────────────────────
    workspaces: Vec<Workspace>,
    active_ws: WorkspaceId,
    workspace_box: gtk4::Box,
    button_map: std::collections::HashMap<WorkspaceId, gtk4::Button>,

    // ── Clock ─────────────────────────────────────────────────────────────
    time_str: String,
    clock_lbl: gtk4::Label,

    // ── Stats bar ─────────────────────────────────────────────────────────
    // The system-stats trio (CPU/RAM/power draw) only shows up when a value
    // crosses a "you probably want to know about this" threshold — otherwise
    // the bar stays quiet. See AppInput::StatsUpdate.
    system_stats_box: gtk4::Box,
    system_sep: gtk4::Separator,
    cpu_pair: gtk4::Box,
    mem_pair: gtk4::Box,
    pwr_pair: gtk4::Box,
    cpu_lbl: gtk4::Label,
    mem_lbl: gtk4::Label,
    pwr_lbl: gtk4::Label,
    bat_lbl: gtk4::Label,
    bat_img: gtk4::Image,
    bat_textures: std::collections::HashMap<usize, gtk4::gdk::Texture>,
    ac_img: gtk4::Image,
    bt_img: gtk4::Image,
    bt_textures: std::collections::HashMap<usize, gtk4::gdk::Texture>,
    wifi_lbl: gtk4::Label,
    wifi_img: gtk4::Image,
    wifi_textures: std::collections::HashMap<usize, gtk4::gdk::Texture>,

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
    control_popover: gtk4::Popover,
    panel_vol_slider: gtk4::Scale,
    panel_bright_slider: gtk4::Scale,
    panel_loading: Rc<Cell<bool>>,
    panel_sink_store: gtk4::StringList,
    panel_sink_dropdown: gtk4::DropDown,
    panel_sink_signal: Option<gtk4::glib::SignalHandlerId>,
    panel_sinks: Vec<bar::control::AudioSink>,
    panel_cpu_lbl: gtk4::Label,
    panel_mem_lbl: gtk4::Label,
    panel_pwr_lbl: gtk4::Label,
    panel_gpu_lbl: gtk4::Label,
    panel_net_lbl: gtk4::Label,

    // ── Tray ──────────────────────────────────────────────────────────────
    tray_section: gtk4::Box,
    tray_sep: gtk4::Separator,
    tray_box: gtk4::Box,
    tray_items: std::collections::HashMap<String, gtk4::Button>,

    // ── Lua-declared widgets ─────────────────────────────────────────────
    // One container per WidgetPlacement (see bread_shared::widget), fully
    // rebuilt on every AppInput::WidgetsUpdate — see widgets::client's
    // module doc for why that's simpler than incremental patching here.
    widget_containers:
        std::collections::HashMap<bread_shared::widget::WidgetPlacement, gtk4::Box>,
    widget_tray_section: gtk4::Box,
    widget_tray_sep: gtk4::Separator,
}

#[derive(Debug)]
pub enum AppInput {
    WorkspaceList(Vec<Workspace>),
    ActiveWorkspace(WorkspaceId),
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
}

#[relm4::component(pub)]
impl SimpleComponent for App {
    type Init = Option<screenshot::ScreenshotRequest>;
    type Input = AppInput;
    type Output = ();

    view! {
        gtk::ApplicationWindow {
            add_css_class: "breadbar",
            set_title: Some("breadbar"),
            set_default_height: 32,

            #[name = "center_box"]
            gtk::CenterBox {
            }
        }
    }

    fn init(
        screenshot_req: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        root.init_layer_shell();
        root.set_namespace(Some("breadbar"));
        root.set_layer(Layer::Top);
        root.set_anchor(Edge::Top, true);
        root.set_anchor(Edge::Left, true);
        root.set_anchor(Edge::Right, true);
        root.set_exclusive_zone(32);

        // ── Workspace row (left) ────────────────────────────────────────
        // Built imperatively (not via the view! macro) so a widget
        // container can sit as a plain sibling of workspace_box — see
        // WidgetPlacement::RightOfWorkspaces below.
        let workspace_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        let workspace_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        workspace_row.append(&workspace_box);

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

        // ── SVG icon sets ────────────────────────────────────────────────
        use bar::stats::{
            AC_POWER, BAT_HIGH, BAT_LOW, BAT_MID, BT_CONNECTED, BT_OFF, BT_ON, WIFI_MEDIUM,
            WIFI_OFF, WIFI_STRONG, WIFI_WEAK,
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
        let wifi_textures: std::collections::HashMap<usize, gtk4::gdk::Texture> =
            [WIFI_STRONG, WIFI_MEDIUM, WIFI_WEAK, WIFI_OFF]
                .into_iter()
                .map(|p| (p.as_ptr() as usize, svg_texture(p)))
                .collect();

        // ── Stat labels ──────────────────────────────────────────────────
        let cpu_lbl = stat_label();
        let mem_lbl = stat_label();
        let pwr_lbl = stat_label();
        let bat_lbl = stat_label();

        let bat_img = gtk4::Image::from_paintable(Some(
            bat_textures.get(&(BAT_MID.as_ptr() as usize)).unwrap(),
        ));
        let ac_img = gtk4::Image::from_paintable(Some(&svg_texture(AC_POWER)));
        ac_img.set_visible(false);
        let bt_img = gtk4::Image::from_paintable(Some(
            bt_textures.get(&(BT_OFF.as_ptr() as usize)).unwrap(),
        ));

        // ── WiFi pair + popover ──────────────────────────────────────────
        let wifi_lbl = gtk4::Label::new(None);
        wifi_lbl.add_css_class("stat-label");
        wifi_lbl.add_css_class("wifi-label");
        wifi_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        wifi_lbl.set_max_width_chars(28);
        wifi_lbl.set_xalign(0.0);
        let wifi_img =
            gtk4::Image::from_paintable(Some(&svg_texture(asset!("WiFi Connecting.svg"))));

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
        media_widget.set_visible(false);

        let media_indicator = gtk4::Label::new(Some("▶"));
        media_indicator.add_css_class("media-indicator");

        let media_track_lbl = gtk4::Label::new(None);
        media_track_lbl.add_css_class("media-track-lbl");
        media_track_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        media_track_lbl.set_max_width_chars(42);
        media_track_lbl.set_xalign(0.0);

        media_widget.append(&media_indicator);
        media_widget.append(&media_track_lbl);

        // Media controls popover
        let media_controls_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        media_controls_box.add_css_class("media-controls");
        media_controls_box.set_margin_top(4);
        media_controls_box.set_margin_bottom(4);
        media_controls_box.set_margin_start(4);
        media_controls_box.set_margin_end(4);

        let prev_btn = gtk4::Button::new();
        prev_btn.set_child(Some(&gtk4::Image::from_paintable(Some(&svg_texture(
            asset!("Previous.svg"),
        )))));
        prev_btn.add_css_class("flat");
        prev_btn.add_css_class("media-btn");
        prev_btn.connect_clicked(|_| bar::media::spawn_cmd("previous"));

        let media_play_icon = gtk4::Image::from_paintable(Some(&svg_texture(asset!("Pause.svg"))));
        let media_play_btn = gtk4::Button::new();
        media_play_btn.set_child(Some(&media_play_icon));
        media_play_btn.add_css_class("flat");
        media_play_btn.add_css_class("media-btn");
        media_play_btn.add_css_class("media-play-btn");
        media_play_btn.connect_clicked(|_| bar::media::spawn_cmd("play-pause"));

        let next_btn = gtk4::Button::new();
        next_btn.set_child(Some(&gtk4::Image::from_paintable(Some(&svg_texture(
            asset!("Next.svg"),
        )))));
        next_btn.add_css_class("flat");
        next_btn.add_css_class("media-btn");
        next_btn.connect_clicked(|_| bar::media::spawn_cmd("next"));

        media_controls_box.append(&prev_btn);
        media_controls_box.append(&media_play_btn);
        media_controls_box.append(&next_btn);

        let media_popover = gtk4::Popover::new();
        media_popover.add_css_class("media-popover");
        media_popover.set_child(Some(&media_controls_box));
        media_popover.set_parent(&media_widget);

        let mpop = media_popover.clone();
        let mgesture = gtk4::GestureClick::new();
        mgesture.connect_released(move |_, _, _, _| {
            if mpop.is_visible() { mpop.popdown(); } else { mpop.popup(); }
        });
        media_widget.add_controller(mgesture);

        // Clock label
        let clock_lbl = gtk4::Label::new(Some(&bar::clock::current()));
        clock_lbl.add_css_class("clock-label");

        // Center area: [media_widget · widgets · clock · widgets]
        let center_area = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
        center_area.add_css_class("center-area");
        center_area.append(&media_widget);
        center_area.append(&widget_left_of_clock);
        center_area.append(&clock_lbl);
        center_area.append(&widget_right_of_clock);

        // ── Stats box (right side) ───────────────────────────────────────
        let stats_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        stats_box.add_css_class("stats-box");
        stats_box.append(&widget_left_of_stats);

        // CPU/RAM/power draw: hidden by default (see StatsUpdate), so this
        // whole sub-group — plus its separator — collapses away when quiet.
        let cpu_pair = stat_pair(asset!("CPU.svg"), &cpu_lbl);
        let mem_pair = stat_pair(asset!("RAM Usage.svg"), &mem_lbl);
        let pwr_pair = stat_pair(asset!("Power Draw.svg"), &pwr_lbl);
        let system_stats_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        system_stats_box.append(&cpu_pair);
        system_stats_box.append(&mem_pair);
        system_stats_box.append(&pwr_pair);
        system_stats_box.set_visible(false);
        stats_box.append(&system_stats_box);
        let system_sep = gtk4::Separator::new(gtk4::Orientation::Vertical);
        system_sep.add_css_class("bar-sep");
        system_sep.set_visible(false);
        stats_box.append(&system_sep);

        let bat_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        bat_box.add_css_class("stat-pair");
        bat_img.add_css_class("stat-icon");
        bat_lbl.add_css_class("stat-label");
        ac_img.add_css_class("stat-icon");
        ac_img.set_margin_start(6);
        bat_box.append(&bat_img);
        bat_box.append(&bat_lbl);
        bat_box.append(&ac_img);
        stats_box.append(&bat_box);

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
        // hhomogeneous/vhomogeneous are ON (GTK's default), even though that
        // means the popover is always sized to the *larger* of the two panes
        // (visible empty space under the shorter one) — the alternative,
        // sizing to just the active pane, means the popup has to resize
        // itself in place when you switch tabs while it's open. Under
        // gtk4-layer-shell that in-place resize doesn't reliably reach the
        // compositor as a proper xdg_popup reposition; switching from the
        // shorter tab to the taller one after a close/reopen cycle made the
        // whole popover silently vanish instead of growing. A constant
        // footprint sidesteps the resize entirely.
        let content_stack = gtk4::Stack::new();
        content_stack.set_hhomogeneous(true);
        content_stack.set_vhomogeneous(true);
        // Reserve enough room up front for the tallest realistic content
        // (WiFi tab with a handful of nearby networks). Homogeneous sizing
        // alone still lets the *first* real data load (Scanning… → populated
        // list) trigger a live resize while the popup is mapped, which hits
        // the same reposition fragility as the tab-switch case — claiming
        // the space before anything is shown avoids that resize too.
        content_stack.set_size_request(220, 420);
        content_stack.add_named(&wifi_pane, Some("wifi"));
        content_stack.add_named(&bt_pane, Some("bluetooth"));
        content_stack.set_visible_child_name("wifi");

        let wifi_tab_btn = gtk4::ToggleButton::with_label("Wi-Fi");
        wifi_tab_btn.add_css_class("popover-tab");
        wifi_tab_btn.set_active(true);
        let bt_tab_btn = gtk4::ToggleButton::with_label("Bluetooth");
        bt_tab_btn.add_css_class("popover-tab");
        bt_tab_btn.set_group(Some(&wifi_tab_btn));

        let tab_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        tab_row.add_css_class("popover-tab-row");
        tab_row.append(&wifi_tab_btn);
        tab_row.append(&bt_tab_btn);

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
        connectivity_inner.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
        connectivity_inner.append(&content_stack);

        let connectivity_popover = gtk4::Popover::new();
        connectivity_popover.add_css_class("wifi-popover");
        connectivity_popover.set_child(Some(&connectivity_inner));

        let connectivity_pair = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        connectivity_pair.add_css_class("stat-pair");
        connectivity_pair.add_css_class("wifi-pair");
        connectivity_pair.append(&bt_img);
        connectivity_pair.append(&wifi_img);
        connectivity_pair.append(&wifi_lbl);
        // Anchored to wifi_lbl specifically, not the connectivity_pair row —
        // a Popover parented to a multi-child Box via set_parent() balloons
        // that Box's own allocation after a popup→popdown→popup cycle (its
        // width roughly quadrupled in testing, shoving every bar item to its
        // left further left). Anchoring to a single leaf widget instead
        // sidesteps whatever GTK4/gtk4-layer-shell interaction causes that;
        // the click target below still covers the whole row regardless.
        connectivity_popover.set_parent(&wifi_lbl);
        stats_box.append(&connectivity_pair);

        let cpop = connectivity_popover.clone();
        let gesture = gtk4::GestureClick::new();
        gesture.connect_released(move |_, _, _, _| {
            if cpop.is_visible() { cpop.popdown(); } else { cpop.popup(); }
        });
        connectivity_pair.add_controller(gesture);

        let sender_conn = sender.clone();
        connectivity_popover.connect_show(move |_| {
            bar::wifi::spawn_popover_load(sender_conn.clone());
            bar::bluetooth::spawn_popover_load(sender_conn.clone());
        });

        // ── Control panel popover ────────────────────────────────────────
        let panel_inner = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        panel_inner.add_css_class("control-panel-inner");

        // Volume row
        let vol_row = build_slider_row(bar::stats::ICON_VOLUME, 0.0, 1.5, 0.02);
        let panel_vol_slider = vol_row.1.clone();
        panel_inner.append(&vol_row.0);

        // Brightness row
        let bright_row = build_slider_row(bar::stats::ICON_BRIGHTNESS, 0.0, 1.0, 0.02);
        let panel_bright_slider = bright_row.1.clone();
        panel_inner.append(&bright_row.0);

        panel_inner.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));

        // Stats section
        let stats_section = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        stats_section.add_css_class("control-panel-stats");

        let panel_cpu_lbl = gtk4::Label::new(Some("CPU  —"));
        panel_cpu_lbl.add_css_class("control-panel-stat");
        panel_cpu_lbl.set_xalign(0.0);

        let panel_mem_lbl = gtk4::Label::new(Some("RAM  —"));
        panel_mem_lbl.add_css_class("control-panel-stat");
        panel_mem_lbl.set_xalign(0.0);

        let panel_pwr_lbl = gtk4::Label::new(Some("PWR  —"));
        panel_pwr_lbl.add_css_class("control-panel-stat");
        panel_pwr_lbl.set_xalign(0.0);

        let panel_gpu_lbl = gtk4::Label::new(Some("GPU  —"));
        panel_gpu_lbl.add_css_class("control-panel-stat");
        panel_gpu_lbl.set_xalign(0.0);

        let panel_net_lbl = gtk4::Label::new(Some("↓ —  ↑ —"));
        panel_net_lbl.add_css_class("control-panel-stat");
        panel_net_lbl.set_xalign(0.0);

        stats_section.append(&panel_cpu_lbl);
        stats_section.append(&panel_mem_lbl);
        stats_section.append(&panel_pwr_lbl);
        stats_section.append(&panel_gpu_lbl);
        stats_section.append(&panel_net_lbl);
        panel_inner.append(&stats_section);

        panel_inner.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));

        // Audio output section
        let sink_section = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
        sink_section.add_css_class("control-panel-section");
        let sink_header = gtk4::Label::new(Some("Audio Output"));
        sink_header.add_css_class("control-panel-section-header");
        sink_header.set_xalign(0.0);

        let panel_sink_store = gtk4::StringList::new(&[]);
        let panel_sink_dropdown = gtk4::DropDown::new(
            Some(panel_sink_store.clone().upcast::<gtk4::gio::ListModel>()),
            Option::<gtk4::Expression>::None,
        );
        panel_sink_dropdown.add_css_class("control-panel-sink-dropdown");
        panel_sink_dropdown.set_hexpand(true);

        sink_section.append(&sink_header);
        sink_section.append(&panel_sink_dropdown);
        panel_inner.append(&sink_section);

        panel_inner.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));

        // Tray section
        let tray_section = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
        tray_section.add_css_class("control-panel-section");
        let tray_header = gtk4::Label::new(Some("Apps"));
        tray_header.add_css_class("control-panel-section-header");
        tray_header.set_xalign(0.0);
        let tray_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        tray_box.add_css_class("tray-box");
        tray_section.append(&tray_header);
        tray_section.append(&tray_box);
        // Collapsed (along with its separator) until an SNI app actually
        // registers — an empty "Apps" section heading is dead weight.
        tray_section.set_visible(false);
        panel_inner.append(&tray_section);

        let tray_sep = gtk4::Separator::new(gtk4::Orientation::Horizontal);
        tray_sep.set_visible(false);
        panel_inner.append(&tray_sep);

        // Widgets section — Lua-declared widgets with placement = "tray".
        // Same collapse-when-empty idiom as the Apps section above.
        let widget_tray_section = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
        widget_tray_section.add_css_class("control-panel-section");
        let widget_tray_header = gtk4::Label::new(Some("Widgets"));
        widget_tray_header.add_css_class("control-panel-section-header");
        widget_tray_header.set_xalign(0.0);
        let widget_tray_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        widget_tray_box.add_css_class("tray-box");
        widget_tray_section.append(&widget_tray_header);
        widget_tray_section.append(&widget_tray_box);
        widget_tray_section.set_visible(false);
        panel_inner.append(&widget_tray_section);

        let widget_tray_sep = gtk4::Separator::new(gtk4::Orientation::Horizontal);
        widget_tray_sep.set_visible(false);
        panel_inner.append(&widget_tray_sep);

        // Power section
        let power_section = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
        power_section.add_css_class("control-panel-section");
        let power_header = gtk4::Label::new(Some("Power"));
        power_header.add_css_class("control-panel-section-header");
        power_header.set_xalign(0.0);
        power_section.append(&power_header);

        let power_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        power_row.add_css_class("power-row");
        for (icon_svg, cmd) in [
            // breadlock is the ecosystem's own screen locker — hyprlock is
            // the thing it was built to replace; the bar shouldn't still
            // be pointing at it.
            (bar::stats::ICON_LOCK, vec!["breadlock"]),
            (bar::stats::ICON_SLEEP, vec!["systemctl", "suspend"]),
            (bar::stats::ICON_RESTART, vec!["systemctl", "reboot"]),
            (bar::stats::ICON_SHUTDOWN, vec!["systemctl", "poweroff"]),
        ] {
            let btn = gtk4::Button::new();
            btn.set_child(Some(&gtk4::Image::from_paintable(Some(&svg_texture(icon_svg)))));
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
        power_section.append(&power_row);
        panel_inner.append(&power_section);

        let control_popover = gtk4::Popover::new();
        control_popover.add_css_class("control-panel");
        control_popover.set_child(Some(&panel_inner));

        // Hamburger button
        let hamburger_btn = gtk4::Button::with_label("☰");
        hamburger_btn.add_css_class("flat");
        hamburger_btn.add_css_class("control-panel-btn");

        control_popover.set_parent(&hamburger_btn);

        let cpop = control_popover.clone();
        hamburger_btn.connect_clicked(move |_| {
            if cpop.is_visible() { cpop.popdown(); } else { cpop.popup(); }
        });

        let sender_cp = sender.clone();
        control_popover.connect_show(move |_| {
            bar::control::spawn_load(sender_cp.clone());
        });

        // Slider signals — use Rc<Cell<bool>> to suppress feedback during data load
        let panel_loading = Rc::new(Cell::new(false));

        let loading_v = panel_loading.clone();
        panel_vol_slider.connect_value_changed(move |s| {
            if loading_v.get() { return; }
            bar::control::spawn_set_volume(s.value());
        });

        let loading_b = panel_loading.clone();
        panel_bright_slider.connect_value_changed(move |s| {
            if loading_b.get() { return; }
            bar::control::spawn_set_brightness(s.value());
        });

        stats_box.append(&hamburger_btn);

        let widget_containers = std::collections::HashMap::from([
            (WidgetPlacement::RightOfWorkspaces, widget_right_of_workspaces),
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
        let control_popover_for_screenshot = control_popover.clone();
        let connectivity_popover_for_screenshot = connectivity_popover.clone();
        let wifi_tab_btn_for_screenshot = wifi_tab_btn.clone();
        let bt_tab_btn_for_screenshot = bt_tab_btn.clone();
        let media_popover_for_screenshot = media_popover.clone();
        let media_widget_for_screenshot = media_widget.clone();
        let media_track_lbl_for_screenshot = media_track_lbl.clone();

        let model = App {
            workspaces: vec![],
            active_ws: 1,
            workspace_box,
            button_map: std::collections::HashMap::new(),
            time_str: bar::clock::current(),
            clock_lbl,
            system_stats_box,
            system_sep,
            cpu_pair,
            mem_pair,
            pwr_pair,
            cpu_lbl,
            mem_lbl,
            pwr_lbl,
            bat_lbl,
            bat_img,
            bat_textures,
            ac_img,
            bt_img,
            bt_textures,
            wifi_lbl,
            wifi_img,
            wifi_textures,
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
            control_popover,
            panel_vol_slider,
            panel_bright_slider,
            panel_loading,
            panel_sink_store,
            panel_sink_dropdown,
            panel_sink_signal: None,
            panel_sinks: vec![],
            panel_cpu_lbl,
            panel_mem_lbl,
            panel_pwr_lbl,
            panel_gpu_lbl,
            panel_net_lbl,
            tray_section,
            tray_sep,
            tray_box,
            tray_items: std::collections::HashMap::new(),
            widget_containers,
            widget_tray_section,
            widget_tray_sep,
        };

        theme::apply();
        bar::workspaces::spawn_watcher(sender.clone());
        bar::clock::spawn_ticker(sender.clone());
        bar::stats::spawn_poller(sender.clone());
        bar::tray::spawn_watcher(sender.clone());
        bar::wifi::spawn_status_poller(sender.clone());
        bar::media::spawn_poller(sender.clone());
        widgets::client::spawn(sender.clone());

        // Screenshot mode primes these with sample content instead of the
        // real D-Bus/pactl/backlight sources — see notifications::SampleKind
        // and osd::SampleKind's doc comments.
        let notif_sample = screenshot_req.as_ref().and_then(|r| match r.view.as_str() {
            "notification" => Some(notifications::SampleKind::Normal),
            "notification-critical" => Some(notifications::SampleKind::Critical),
            _ => None,
        });
        let notification_window = notifications::spawn(notif_sample);
        let osd_sample = screenshot_req.as_ref().and_then(|r| match r.view.as_str() {
            "osd-volume" => Some(osd::SampleKind::Volume),
            "osd-brightness" => Some(osd::SampleKind::Brightness),
            _ => None,
        });
        let osd_window = osd::spawn(osd_sample);

        if let Some(req) = screenshot_req {
            let notification_window = matches!(req.view.as_str(), "notification" | "notification-critical")
                .then_some(notification_window);
            let osd_window = matches!(req.view.as_str(), "osd-volume" | "osd-brightness")
                .then_some(osd_window);
            screenshot::dispatch(
                &root,
                req,
                screenshot::Handles {
                    control_popover: control_popover_for_screenshot,
                    connectivity_popover: connectivity_popover_for_screenshot,
                    wifi_tab_btn: wifi_tab_btn_for_screenshot,
                    bt_tab_btn: bt_tab_btn_for_screenshot,
                    media_popover: media_popover_for_screenshot,
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
            AppInput::WorkspaceList(list) => {
                let mut sorted = list;
                sorted.sort_by_key(|w| w.id);
                self.workspaces = sorted;
                self.rebuild_buttons();
            }
            AppInput::ActiveWorkspace(id) => {
                if let Some(old) = self.button_map.get(&self.active_ws) {
                    old.remove_css_class("active");
                }
                self.active_ws = id;
                if let Some(btn) = self.button_map.get(&self.active_ws) {
                    btn.add_css_class("active");
                }
            }
            AppInput::ClockTick => {
                self.time_str = bar::clock::current();
                self.clock_lbl.set_label(&self.time_str);
            }
            AppInput::StatsUpdate(stats) => {
                self.cpu_lbl.set_label(&stats.cpu);
                self.mem_lbl.set_label(&stats.mem);
                self.pwr_lbl.set_label(&stats.power);

                // Bar information diet: CPU/RAM/power draw only surface once
                // they're actually worth knowing about, individually, so a
                // hot CPU doesn't drag an idle RAM/power reading along with it.
                let cpu_hot = stats.cpu_pct > CPU_ATTENTION_THRESHOLD;
                let mem_hot = stats.mem_pct > MEM_ATTENTION_THRESHOLD;
                let pwr_hot = stats.power_watts > POWER_ATTENTION_THRESHOLD;
                self.cpu_pair.set_visible(cpu_hot);
                self.mem_pair.set_visible(mem_hot);
                self.pwr_pair.set_visible(pwr_hot);
                let any_hot = cpu_hot || mem_hot || pwr_hot;
                self.system_stats_box.set_visible(any_hot);
                self.system_sep.set_visible(any_hot);

                self.bat_lbl.set_label(&stats.bat);
                if let Some(tex) = self.bat_textures.get(&(stats.bat_icon.as_ptr() as usize)) {
                    self.bat_img.set_paintable(Some(tex));
                }
                self.ac_img.set_visible(stats.ac_connected);
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
                    bar::stats::WIFI_OFF
                } else {
                    stats.wifi_icon
                };
                if let Some(tex) = self.wifi_textures.get(&(icon.as_ptr() as usize)) {
                    self.wifi_img.set_paintable(Some(tex));
                }

                // Live-update control panel stats while open
                if self.control_popover.is_visible() {
                    let cpu_str = match (stats.cpu_temp, stats.cpu.as_str()) {
                        (Some(t), pct) => format!("CPU  {pct}  {t:.0}°C"),
                        (None, pct) => format!("CPU  {pct}"),
                    };
                    self.panel_cpu_lbl.set_label(&cpu_str);

                    self.panel_mem_lbl
                        .set_label(&format!("RAM  {:.0}%  {}", stats.mem_pct, stats.mem));
                    self.panel_pwr_lbl
                        .set_label(&format!("PWR  {}", stats.power));

                    let gpu_str = match (stats.gpu_usage, stats.gpu_temp) {
                        (Some(u), Some(t)) => format!("GPU  {u}%  {t:.0}°C"),
                        (Some(u), None) => format!("GPU  {u}%"),
                        (None, Some(t)) => format!("GPU  {t:.0}°C"),
                        (None, None) => "GPU  —".to_string(),
                    };
                    self.panel_gpu_lbl.set_label(&gpu_str);

                    self.panel_net_lbl.set_label(&format!(
                        "↓ {}  ↑ {}",
                        fmt_speed(stats.net_rx_kbs),
                        fmt_speed(stats.net_tx_kbs),
                    ));
                }
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
                    self.media_play_icon.set_paintable(Some(&svg_texture(icon_svg)));

                    if state.playing {
                        self.media_paused_at = None;
                    } else if self.media_paused_at.is_none() {
                        self.media_paused_at = Some(std::time::Instant::now());
                    }

                    let within_linger = self
                        .media_paused_at
                        .map_or(true, |t| t.elapsed().as_secs() < 30 * 60);
                    self.media_last = Some(state);
                    self.media_widget.set_visible(within_linger);
                } else {
                    // Player gone — honour linger from last pause
                    if let Some(paused_at) = self.media_paused_at {
                        if paused_at.elapsed().as_secs() < 30 * 60 {
                            self.media_widget.set_visible(true);
                        } else {
                            self.media_widget.set_visible(false);
                            self.media_last = None;
                            self.media_paused_at = None;
                        }
                    } else {
                        self.media_widget.set_visible(false);
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

                // Rebuild sink dropdown — disconnect, repopulate, reconnect
                if let Some(id) = self.panel_sink_signal.take() {
                    self.panel_sink_dropdown.disconnect(id);
                }
                // Clear store
                let n = self.panel_sink_store.n_items();
                for i in (0..n).rev() {
                    self.panel_sink_store.remove(i);
                }
                for sink in &data.sinks {
                    self.panel_sink_store.append(&sink.description);
                }
                if let Some(idx) = data.sinks.iter().position(|s| s.is_default) {
                    self.panel_sink_dropdown.set_selected(idx as u32);
                }
                self.panel_sinks = data.sinks;

                let sinks = self.panel_sinks.clone();
                let id = self.panel_sink_dropdown.connect_selected_notify(move |dd| {
                    let idx = dd.selected() as usize;
                    if let Some(sink) = sinks.get(idx) {
                        bar::control::spawn_set_sink(sink.name.clone());
                    }
                });
                self.panel_sink_signal = Some(id);
            }
            AppInput::WidgetsUpdate(specs) => {
                self.reconcile_widgets(specs);
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

        let has_tray_widgets = specs.iter().any(|s| {
            s.visible && s.placement == bread_shared::widget::WidgetPlacement::Tray
        });
        self.widget_tray_section.set_visible(has_tray_widgets);
        self.widget_tray_sep.set_visible(has_tray_widgets);
    }

    fn apply_wifi_label(&self) {
        let label = match &self.wifi_profile {
            Some(p) => format!("{p} · {}", self.current_ssid),
            None => self.current_ssid.clone(),
        };
        self.wifi_lbl.set_label(&label);
    }

    fn rebuild_buttons(&mut self) {
        while let Some(child) = self.workspace_box.first_child() {
            self.workspace_box.remove(&child);
        }
        self.button_map.clear();
        for ws in &self.workspaces {
            let btn = bar::workspaces::make_button(ws.id, &ws.name, self.active_ws);
            self.workspace_box.append(&btn);
            self.button_map.insert(ws.id, btn);
        }
    }

    fn rebuild_wifi_popover(&mut self, sender: &ComponentSender<Self>) {
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
                parts.push(if st.tailscale_ok { "tailscale ✓" } else { "tailscale ✗" });
            }
            let status_lbl = gtk4::Label::new(Some(&parts.join("   ")));
            status_lbl.add_css_class("wifi-popover-status");
            status_lbl.set_xalign(0.0);
            header.append(&status_lbl);

            self.wifi_pane.append(&header);
            self.wifi_pane
                .append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
        }

        let Some(data) = &self.wifi_popover_data else {
            let lbl = gtk4::Label::new(Some("Scanning…"));
            lbl.add_css_class("wifi-popover-loading");
            self.wifi_pane.append(&lbl);
            return;
        };

        let ph = gtk4::Label::new(Some("Profiles"));
        ph.add_css_class("wifi-popover-section");
        ph.set_xalign(0.0);
        ph.set_margin_top(6);
        ph.set_margin_bottom(2);
        self.wifi_pane.append(&ph);

        for (name, active) in &data.profiles {
            let row = gtk4::Button::new();
            row.add_css_class("flat");
            row.add_css_class("wifi-popover-row");
            if *active {
                row.add_css_class("wifi-popover-row-active");
            }
            let lbl = gtk4::Label::new(Some(&format!(
                "{}{}",
                if *active { "● " } else { "  " },
                name
            )));
            lbl.set_xalign(0.0);
            row.set_child(Some(&lbl));

            let name_clone = name.clone();
            let sender_clone = sender.clone();
            row.connect_clicked(move |btn| {
                sender_clone.input(AppInput::SetProfile(name_clone.clone()));
                bar::wifi::spawn_profile_set(name_clone.clone());
                close_parent_popover(btn);
            });
            self.wifi_pane.append(&row);
        }

        if !data.scan.is_empty() {
            self.wifi_pane
                .append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
            let nh = gtk4::Label::new(Some("Nearby"));
            nh.add_css_class("wifi-popover-section");
            nh.set_xalign(0.0);
            nh.set_margin_top(6);
            nh.set_margin_bottom(2);
            self.wifi_pane.append(&nh);

            for entry in &data.scan {
                let row = gtk4::Button::new();
                row.add_css_class("flat");
                row.add_css_class("wifi-popover-row");
                if !entry.saved {
                    row.add_css_class("wifi-popover-row-unsaved");
                }
                let is_current = entry.ssid == self.current_ssid;
                if is_current {
                    row.add_css_class("wifi-popover-row-active");
                }

                let row_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
                let icon_svg = wifi_icon_for_signal(entry.signal);
                if let Some(tex) = self.wifi_textures.get(&(icon_svg.as_ptr() as usize)) {
                    let img = gtk4::Image::from_paintable(Some(tex));
                    img.add_css_class("stat-icon");
                    row_box.append(&img);
                }
                let lbl = gtk4::Label::new(Some(&format!(
                    "{}{}",
                    if is_current { "● " } else { "  " },
                    entry.ssid,
                )));
                lbl.set_xalign(0.0);
                row_box.append(&lbl);
                row.set_child(Some(&row_box));

                let ssid_clone = entry.ssid.clone();
                let saved = entry.saved;
                row.connect_clicked(move |btn| {
                    if saved {
                        bar::wifi::spawn_join(ssid_clone.clone());
                    } else {
                        show_add_network_dialog(btn, ssid_clone.clone(), |_| {});
                    }
                    close_parent_popover(btn);
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
            let dh = gtk4::Label::new(Some("Paired"));
            dh.add_css_class("wifi-popover-section");
            dh.set_xalign(0.0);
            dh.set_margin_top(2);
            dh.set_margin_bottom(2);
            self.bt_pane.append(&dh);

            for dev in &data.devices {
                let row = gtk4::Button::new();
                row.add_css_class("flat");
                row.add_css_class("wifi-popover-row");
                if dev.connected {
                    row.add_css_class("wifi-popover-row-active");
                }
                let lbl = gtk4::Label::new(Some(&format!(
                    "{}{}",
                    if dev.connected { "● " } else { "  " },
                    dev.name,
                )));
                lbl.set_xalign(0.0);
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
        let settings_icon = gtk4::Image::from_paintable(Some(&svg_texture(bar::stats::ICON_BT_SETTINGS)));
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

fn build_slider_row(icon_svg: &str, min: f64, max: f64, step: f64) -> (gtk4::Box, gtk4::Scale) {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.add_css_class("control-panel-row");
    row.set_margin_top(2);
    row.set_margin_bottom(2);

    // Rendered larger than the standard 16px stat/section icons: these are
    // the two things in the panel you actually drag, so they should read as
    // primary controls rather than blend in with passive readouts.
    let icon = gtk4::Image::from_paintable(Some(&svg_texture_sized(icon_svg, 20)));
    icon.add_css_class("control-panel-row-icon");

    let slider = gtk4::Scale::with_range(gtk4::Orientation::Horizontal, min, max, step);
    slider.set_draw_value(false);
    slider.set_hexpand(true);
    slider.set_width_request(180);
    slider.add_css_class("control-panel-slider");

    row.append(&icon);
    row.append(&slider);
    (row, slider)
}

fn fmt_speed(kbs: f32) -> String {
    if kbs >= 1024.0 {
        format!("{:.1} MB/s", kbs / 1024.0)
    } else {
        format!("{:.0} KB/s", kbs)
    }
}

fn wifi_icon_for_signal(pct: u8) -> &'static str {
    use bar::stats::{WIFI_MEDIUM, WIFI_OFF, WIFI_STRONG, WIFI_WEAK};
    match pct {
        75..=100 => WIFI_STRONG,
        50..=74 => WIFI_MEDIUM,
        25..=49 => WIFI_WEAK,
        _ => WIFI_OFF,
    }
}

fn close_parent_popover(widget: &gtk4::Button) {
    if let Some(w) = widget.ancestor(gtk4::Popover::static_type()) {
        if let Ok(p) = w.downcast::<gtk4::Popover>() {
            p.popdown();
        }
    }
}

/// Small modal prompting for a password, then saves + joins the network via
/// `breadcrumbs add` + `breadcrumbs join`. `on_build` runs on the freshly
/// built dialog *before* it's presented — screenshot mode's only hook point,
/// since `connect_map` registered any later would miss a map that already
/// happened. The real call site passes a no-op.
fn show_add_network_dialog(anchor: &impl IsA<gtk4::Widget>, ssid: String, on_build: impl FnOnce(&gtk4::Window)) {
    let dialog = gtk4::Window::new();
    dialog.set_title(Some(&format!("Add “{ssid}”")));
    dialog.set_resizable(false);
    dialog.add_css_class("wifi-add-dialog");
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
    let img = gtk4::Image::from_paintable(Some(&svg_texture(icon_svg)));
    img.add_css_class("stat-icon");
    pair.append(&img);
    pair.append(label);
    pair
}

pub(crate) fn svg_texture(svg_src: &str) -> gtk4::gdk::Texture {
    svg_texture_sized(svg_src, 16)
}

/// Same as `svg_texture` but rendered at an explicit pixel size — used to give
/// primary/interactive icons (e.g. sliders you actually drag) more visual
/// weight than passive informational ones, which otherwise all read as the
/// same flat 16px stroke glyph once emoji stopped providing accidental variety.
pub(crate) fn svg_texture_sized(svg_src: &str, px: u32) -> gtk4::gdk::Texture {
    use resvg::{tiny_skia, usvg};
    let fg = theme::fg_color();
    let dim = format!(r#"width="{px}" height="{px}""#);
    let svg = svg_src
        .replace("currentColor", &fg)
        .replace(r#"width="24" height="24""#, &dim);
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).expect("parse svg");
    let size = tree.size().to_int_size();
    let (w, h) = (size.width(), size.height());
    let mut pixmap = tiny_skia::Pixmap::new(w, h).expect("alloc pixmap");
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut pixmap.as_mut());
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
    app.run::<App>(screenshot_req);
}

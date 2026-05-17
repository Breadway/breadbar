macro_rules! asset {
    ($n:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/assets/", $n)
    };
}

mod bar;
mod notifications;
mod theme;

use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use hyprland::data::Workspace;
use hyprland::shared::WorkspaceId;
use relm4::prelude::*;

pub struct App {
    workspaces: Vec<Workspace>,
    active_ws: WorkspaceId,
    time_str: String,
    workspace_box: gtk4::Box,
    cpu_lbl: gtk4::Label,
    mem_lbl: gtk4::Label,
    pwr_lbl: gtk4::Label,
    bat_lbl: gtk4::Label,
    wifi_lbl: gtk4::Label,
    wifi_img: gtk4::Image,
}

#[derive(Debug)]
pub enum AppInput {
    WorkspaceList(Vec<Workspace>),
    ActiveWorkspace(WorkspaceId),
    ClockTick,
    StatsUpdate(bar::stats::Stats),
}

#[relm4::component(pub)]
impl SimpleComponent for App {
    type Init = ();
    type Input = AppInput;
    type Output = ();

    view! {
        gtk::ApplicationWindow {
            add_css_class: "breadbar",
            set_title: Some("breadbar"),
            set_default_height: 32,

            #[name = "center_box"]
            gtk::CenterBox {
                #[wrap(Some)]
                set_start_widget = &gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 0,
                    set_margin_start: 8,

                    #[name = "workspace_box"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_spacing: 4,
                    }
                },

                #[wrap(Some)]
                set_center_widget = &gtk::Label {
                    #[watch]
                    set_label: &model.time_str,
                },
            }
        }
    }

    fn init(
        _: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        root.init_layer_shell();
        root.set_layer(Layer::Top);
        root.set_anchor(Edge::Top, true);
        root.set_anchor(Edge::Left, true);
        root.set_anchor(Edge::Right, true);
        root.set_exclusive_zone(32);

        let cpu_lbl = stat_label(4);
        let mem_lbl = stat_label(4);
        let pwr_lbl = stat_label(5);
        let bat_lbl = stat_label(4);
        let wifi_lbl = gtk4::Label::new(None);
        wifi_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        wifi_lbl.set_max_width_chars(12);
        let wifi_img = gtk4::Image::from_paintable(Some(&svg_texture(asset!("WiFi Connecting.svg"))));

        let mut model = App {
            workspaces: vec![],
            active_ws: 1,
            time_str: bar::clock::current(),
            workspace_box: gtk4::Box::new(gtk4::Orientation::Horizontal, 4),
            cpu_lbl: cpu_lbl.clone(),
            mem_lbl: mem_lbl.clone(),
            pwr_lbl: pwr_lbl.clone(),
            bat_lbl: bat_lbl.clone(),
            wifi_lbl: wifi_lbl.clone(),
            wifi_img: wifi_img.clone(),
        };
        let widgets = view_output!();
        model.workspace_box = widgets.workspace_box.clone();

        let stats_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        stats_box.set_margin_end(8);
        stats_box.append(&stat_pair(asset!("CPU.svg"), &cpu_lbl));
        stats_box.append(&stat_pair(asset!("RAM Usage.svg"), &mem_lbl));
        stats_box.append(&stat_pair(asset!("Power Draw.svg"), &pwr_lbl));
        stats_box.append(&stat_pair(asset!("Battery.svg"), &bat_lbl));
        let wifi_pair = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        wifi_pair.append(&wifi_img);
        wifi_pair.append(&wifi_lbl);
        stats_box.append(&wifi_pair);
        widgets.center_box.set_end_widget(Some(&stats_box));

        theme::apply();
        bar::workspaces::spawn_watcher(sender.clone());
        bar::clock::spawn_ticker(sender.clone());
        bar::stats::spawn_poller(sender);
        notifications::spawn();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _: ComponentSender<Self>) {
        match msg {
            AppInput::WorkspaceList(list) => {
                let mut sorted = list;
                sorted.sort_by_key(|w| w.id);
                self.workspaces = sorted;
                self.rebuild_buttons();
            }
            AppInput::ActiveWorkspace(id) => {
                self.active_ws = id;
                self.rebuild_buttons();
            }
            AppInput::ClockTick => {
                self.time_str = bar::clock::current();
            }
            AppInput::StatsUpdate(stats) => {
                self.cpu_lbl.set_label(&stats.cpu);
                self.mem_lbl.set_label(&stats.mem);
                self.pwr_lbl.set_label(&stats.power);
                self.bat_lbl.set_label(&stats.bat);
                self.wifi_lbl.set_label(&stats.wifi_ssid);
                self.wifi_img.set_paintable(Some(&svg_texture(stats.wifi_icon)));
            }
        }
    }
}

impl App {
    fn rebuild_buttons(&self) {
        while let Some(child) = self.workspace_box.first_child() {
            self.workspace_box.remove(&child);
        }
        for ws in &self.workspaces {
            self.workspace_box
                .append(&bar::workspaces::make_button(ws.id, &ws.name, self.active_ws));
        }
    }
}

fn stat_pair(icon_path: &str, label: &gtk4::Label) -> gtk4::Box {
    let pair = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    pair.append(&gtk4::Image::from_paintable(Some(&svg_texture(icon_path))));
    pair.append(label);
    pair
}

fn svg_texture(path: &str) -> gtk4::gdk::Texture {
    let svg = std::fs::read_to_string(path)
        .unwrap_or_default()
        .replace("currentColor", "white");
    let bytes = gtk4::glib::Bytes::from_owned(svg.into_bytes());
    gtk4::gdk::Texture::from_bytes(&bytes).expect("svg load")
}

fn stat_label(width_chars: i32) -> gtk4::Label {
    let lbl = gtk4::Label::new(None);
    lbl.set_width_chars(width_chars);
    lbl.set_xalign(1.0);
    lbl
}

fn main() {
    relm4::spawn(async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut stream = signal(SignalKind::hangup()).expect("SIGHUP handler");
        loop {
            stream.recv().await;
            gtk4::glib::MainContext::default().invoke(theme::apply);
        }
    });

    let app = RelmApp::new("sh.breadway.breadbar");
    app.run::<App>(());
}

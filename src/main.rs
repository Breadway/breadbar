// Embed asset SVGs into the binary at compile time. Previously these were
// referenced by their build-time filesystem path (CARGO_MANIFEST_DIR), which
// does not exist on an installed system — so the packaged binary loaded empty
// bytes and panicked. include_str! bakes the contents in instead.
macro_rules! asset {
    ($n:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/", $n))
    };
}

mod bar;
mod notifications;
mod osd;
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
    button_map: std::collections::HashMap<WorkspaceId, gtk4::Button>,
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
    // Pre-loaded textures indexed by constant pointer values.
    wifi_textures: std::collections::HashMap<usize, gtk4::gdk::Texture>,
    tray_box: gtk4::Box,
    tray_items: std::collections::HashMap<String, gtk4::Button>,
}

#[derive(Debug)]
pub enum AppInput {
    WorkspaceList(Vec<Workspace>),
    ActiveWorkspace(WorkspaceId),
    ClockTick,
    StatsUpdate(bar::stats::Stats),
    TrayUpdate(bar::tray::TrayUpdate),
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

        let cpu_lbl = stat_label();
        let mem_lbl = stat_label();
        let pwr_lbl = stat_label();
        let bat_lbl = stat_label();
        let wifi_lbl = gtk4::Label::new(None);
        wifi_lbl.add_css_class("stat-label");
        wifi_lbl.add_css_class("wifi-label");
        wifi_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        wifi_lbl.set_max_width_chars(22);
        wifi_lbl.set_xalign(0.0);
        let wifi_img =
            gtk4::Image::from_paintable(Some(&svg_texture(asset!("WiFi Connecting.svg"))));

        use bar::stats::{
            AC_POWER, BAT_HIGH, BAT_LOW, BAT_MID, BT_CONNECTED, BT_OFF, BT_ON, WIFI_MEDIUM,
            WIFI_OFF, WIFI_STRONG, WIFI_WEAK,
        };
        let bat_textures: std::collections::HashMap<usize, gtk4::gdk::Texture> =
            [BAT_HIGH, BAT_MID, BAT_LOW]
                .into_iter()
                .map(|p| (p.as_ptr() as usize, svg_texture(p)))
                .collect();
        // BAT_MID was just inserted into bat_textures above — key is always present.
        let bat_img = gtk4::Image::from_paintable(Some(
            bat_textures.get(&(BAT_MID.as_ptr() as usize)).unwrap(),
        ));
        let ac_img = gtk4::Image::from_paintable(Some(&svg_texture(AC_POWER)));
        ac_img.set_visible(false);

        let bt_textures: std::collections::HashMap<usize, gtk4::gdk::Texture> =
            [BT_OFF, BT_ON, BT_CONNECTED]
                .into_iter()
                .map(|p| (p.as_ptr() as usize, svg_texture(p)))
                .collect();
        // BT_OFF was just inserted into bt_textures above — key is always present.
        let bt_img = gtk4::Image::from_paintable(Some(
            bt_textures.get(&(BT_OFF.as_ptr() as usize)).unwrap(),
        ));

        let wifi_textures = [WIFI_STRONG, WIFI_MEDIUM, WIFI_WEAK, WIFI_OFF]
            .into_iter()
            .map(|p| (p.as_ptr() as usize, svg_texture(p)))
            .collect();

        let mut model = App {
            workspaces: vec![],
            active_ws: 1,
            time_str: bar::clock::current(),
            workspace_box: gtk4::Box::new(gtk4::Orientation::Horizontal, 4),
            button_map: std::collections::HashMap::new(),
            cpu_lbl: cpu_lbl.clone(),
            mem_lbl: mem_lbl.clone(),
            pwr_lbl: pwr_lbl.clone(),
            bat_lbl: bat_lbl.clone(),
            bat_img: bat_img.clone(),
            bat_textures,
            ac_img: ac_img.clone(),
            bt_img: bt_img.clone(),
            bt_textures,
            wifi_lbl: wifi_lbl.clone(),
            wifi_img: wifi_img.clone(),
            wifi_textures,
            tray_box: gtk4::Box::new(gtk4::Orientation::Horizontal, 4),
            tray_items: std::collections::HashMap::new(),
        };
        let widgets = view_output!();
        model.workspace_box = widgets.workspace_box.clone();

        let stats_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        stats_box.add_css_class("stats-box");
        stats_box.append(&stat_pair(asset!("CPU.svg"), &cpu_lbl));
        stats_box.append(&stat_pair(asset!("RAM Usage.svg"), &mem_lbl));
        stats_box.append(&stat_pair(asset!("Power Draw.svg"), &pwr_lbl));
        let bat_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        bat_box.add_css_class("stat-pair");
        bat_img.add_css_class("stat-icon");
        bat_lbl.add_css_class("stat-label");
        ac_img.add_css_class("stat-icon");
        bat_box.append(&bat_img);
        bat_box.append(&bat_lbl);
        bat_box.append(&ac_img);
        stats_box.append(&bat_box);
        bt_img.add_css_class("bt-icon");
        stats_box.append(&bt_img);
        let wifi_pair = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        wifi_pair.add_css_class("stat-pair");
        wifi_img.add_css_class("stat-icon");
        wifi_pair.append(&wifi_img);
        wifi_pair.append(&wifi_lbl);
        stats_box.append(&wifi_pair);
        model.tray_box.add_css_class("tray-box");
        stats_box.append(&model.tray_box);
        widgets.center_box.set_end_widget(Some(&stats_box));

        theme::apply();
        bar::workspaces::spawn_watcher(sender.clone());
        bar::clock::spawn_ticker(sender.clone());
        bar::stats::spawn_poller(sender.clone());
        bar::tray::spawn_watcher(sender.clone());
        notifications::spawn();
        osd::spawn();

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
            }
            AppInput::StatsUpdate(stats) => {
                self.cpu_lbl.set_label(&stats.cpu);
                self.mem_lbl.set_label(&stats.mem);
                self.pwr_lbl.set_label(&stats.power);
                self.bat_lbl.set_label(&stats.bat);
                if let Some(tex) = self.bat_textures.get(&(stats.bat_icon.as_ptr() as usize)) {
                    self.bat_img.set_paintable(Some(tex));
                }
                self.ac_img.set_visible(stats.ac_connected);
                if let Some(tex) = self.bt_textures.get(&(stats.bt_icon.as_ptr() as usize)) {
                    self.bt_img.set_paintable(Some(tex));
                }
                self.wifi_lbl.set_label(&stats.wifi_ssid);
                if let Some(tex) = self.wifi_textures.get(&(stats.wifi_icon.as_ptr() as usize)) {
                    self.wifi_img.set_paintable(Some(tex));
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
            }
            AppInput::TrayUpdate(bar::tray::TrayUpdate::Remove { id }) => {
                if let Some(btn) = self.tray_items.remove(&id) {
                    self.tray_box.remove(&btn);
                }
            }
        }
    }
}

impl App {
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

// Rasterise an (embedded) SVG to a texture. Done in pure Rust with resvg
// because librsvg dropped its gdk-pixbuf SVG loader, so gdk::Texture::from_bytes
// can no longer decode SVG on a stock system.
fn svg_texture(svg_src: &str) -> gtk4::gdk::Texture {
    use resvg::{tiny_skia, usvg};
    let fg = theme::fg_color();
    let svg = svg_src
        .replace("currentColor", &fg)
        .replace(r#"width="24" height="24""#, r#"width="16" height="16""#);
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

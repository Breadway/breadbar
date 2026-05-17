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
    stats_str: String,
    // GObject handle — manipulated directly in update() to avoid update_view conflicts.
    workspace_box: gtk4::Box,
}

#[derive(Debug)]
pub enum AppInput {
    WorkspaceList(Vec<Workspace>),
    ActiveWorkspace(WorkspaceId),
    ClockTick,
    StatsUpdate(String),
}

#[relm4::component(pub)]
impl SimpleComponent for App {
    type Init = ();
    type Input = AppInput;
    type Output = ();

    view! {
        gtk::ApplicationWindow {
            add_css_class: "aster-bar",
            set_title: Some("aster"),
            set_default_height: 32,

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

                #[wrap(Some)]
                set_end_widget = &gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 8,
                    set_margin_end: 8,

                    gtk::Label {
                        #[watch]
                        set_label: &model.stats_str,
                    }
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

        let mut model = App {
            workspaces: vec![],
            active_ws: 1,
            time_str: bar::clock::current(),
            stats_str: String::new(),
            workspace_box: gtk4::Box::new(gtk4::Orientation::Horizontal, 4),
        };
        let widgets = view_output!();

        model.workspace_box = widgets.workspace_box.clone();

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
            AppInput::StatsUpdate(s) => {
                self.stats_str = s;
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

fn main() {
    // Reload theme CSS on SIGHUP (e.g. after pywal runs).
    relm4::spawn(async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut stream = signal(SignalKind::hangup()).expect("SIGHUP handler");
        loop {
            stream.recv().await;
            gtk4::glib::MainContext::default().invoke(theme::apply);
        }
    });

    let app = RelmApp::new("sh.breadway.aster");
    app.run::<App>(());
}

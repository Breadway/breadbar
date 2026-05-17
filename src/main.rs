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
    // Stored handle so update() can manipulate the live widget directly.
    workspace_box: gtk4::Box,
}

#[derive(Debug)]
pub enum AppInput {
    WorkspaceList(Vec<Workspace>),
    ActiveWorkspace(WorkspaceId),
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
                    set_label: "00:00",
                },

                #[wrap(Some)]
                set_end_widget = &gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 8,
                    set_margin_end: 8,

                    gtk::Label {
                        set_label: "—  —  —  —",
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

        // Placeholder until view_output! gives us the real handle.
        let mut model = App {
            workspaces: vec![],
            active_ws: 1,
            workspace_box: gtk4::Box::new(gtk4::Orientation::Horizontal, 4),
        };
        let widgets = view_output!();

        // Swap in the actual widget so update() can reach it.
        model.workspace_box = widgets.workspace_box.clone();

        theme::apply();
        bar::workspaces::spawn_watcher(sender);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _: ComponentSender<Self>) {
        match msg {
            AppInput::WorkspaceList(list) => {
                let mut sorted = list;
                sorted.sort_by_key(|w| w.id);
                self.workspaces = sorted;
            }
            AppInput::ActiveWorkspace(id) => {
                self.active_ws = id;
            }
        }
        self.rebuild_buttons();
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
    let app = RelmApp::new("sh.breadway.aster");
    app.run::<App>(());
}

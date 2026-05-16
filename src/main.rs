mod bar;
mod notifications;
mod theme;

use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use relm4::prelude::*;

struct App;

#[relm4::component]
impl SimpleComponent for App {
    type Init = ();
    type Input = ();
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
                    set_spacing: 4,
                    set_margin_start: 8,

                    gtk::Label {
                        set_label: "1  2  3",
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
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        root.init_layer_shell();
        root.set_layer(Layer::Top);
        root.set_anchor(Edge::Top, true);
        root.set_anchor(Edge::Left, true);
        root.set_anchor(Edge::Right, true);
        root.set_exclusive_zone(32);

        let model = App;
        let widgets = view_output!();

        theme::apply();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, _: Self::Input, _: ComponentSender<Self>) {}
}

fn main() {
    let app = RelmApp::new("sh.breadway.aster");
    app.run::<App>(());
}

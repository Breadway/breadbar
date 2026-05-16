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
            set_title: Some("aster"),
            set_default_height: 32,
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
        ComponentParts { model, widgets }
    }

    fn update(&mut self, _: Self::Input, _: ComponentSender<Self>) {}
}

fn main() {
    let app = RelmApp::new("sh.breadway.aster");
    app.run::<App>(());
}

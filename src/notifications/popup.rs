use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Duration};

use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use tokio::sync::mpsc::Receiver;

use super::NotifEvent;

type Cards = Rc<RefCell<HashMap<u32, gtk4::Box>>>;

pub async fn run(mut rx: Receiver<NotifEvent>) {
    let window = create_window();
    let cards_box = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    cards_box.set_margin_top(8);
    cards_box.set_margin_bottom(8);
    cards_box.set_margin_start(8);
    cards_box.set_margin_end(8);
    window.set_child(Some(&cards_box));

    let cards: Cards = Rc::new(RefCell::new(HashMap::new()));

    while let Some(event) = rx.recv().await {
        match event {
            NotifEvent::Show { id, app_name, summary, body, timeout_ms } => {
                // Replace existing card with same id (replaces_id case)
                if let Some(old) = cards.borrow_mut().remove(&id) {
                    cards_box.remove(&old);
                }
                let card = make_card(&app_name, &summary, &body);
                cards_box.prepend(&card);
                cards.borrow_mut().insert(id, card.clone());
                window.set_visible(true);

                // Auto-dismiss via GLib-native timer (safe inside spawn_local)
                let cards_clone = cards.clone();
                let cards_box_clone = cards_box.clone();
                let win_clone = window.clone();
                relm4::spawn_local(async move {
                    gtk4::glib::timeout_future(Duration::from_millis(timeout_ms as u64)).await;
                    dismiss(&cards_box_clone, &win_clone, &cards_clone, id);
                });
            }
            NotifEvent::Close(id) => {
                dismiss(&cards_box, &window, &cards, id);
            }
        }
    }
}

fn dismiss(cards_box: &gtk4::Box, window: &gtk4::Window, cards: &Cards, id: u32) {
    if let Some(card) = cards.borrow_mut().remove(&id) {
        cards_box.remove(&card);
    }
    if cards.borrow().is_empty() {
        window.set_visible(false);
    }
}

fn create_window() -> gtk4::Window {
    let window = gtk4::Window::new();
    window.add_css_class("aster-notification");
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Right, true);
    window.set_margin(Edge::Top, 20);
    window.set_margin(Edge::Right, 20);
    window.set_default_width(320);
    window
}

fn make_card(app_name: &str, summary: &str, body: &str) -> gtk4::Box {
    let card = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    card.add_css_class("notification-card");

    if !app_name.is_empty() {
        let lbl = gtk4::Label::new(Some(app_name));
        lbl.add_css_class("notification-app");
        lbl.set_xalign(0.0);
        card.append(&lbl);
    }

    let summary_lbl = gtk4::Label::new(Some(summary));
    summary_lbl.add_css_class("notification-summary");
    summary_lbl.set_xalign(0.0);
    summary_lbl.set_wrap(true);
    card.append(&summary_lbl);

    if !body.is_empty() {
        let body_lbl = gtk4::Label::new(Some(body));
        body_lbl.add_css_class("notification-body");
        body_lbl.set_xalign(0.0);
        body_lbl.set_wrap(true);
        card.append(&body_lbl);
    }

    card
}

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use super::Urgency;

pub const LIMIT: usize = 50;
const BODY_MAX_CHARS: usize = 96;

pub type Store = Arc<Mutex<VecDeque<Entry>>>;

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: u32,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub urgency: Urgency,
    pub received: SystemTime,
}

pub struct Ui {
    pub window: gtk4::Window,
    pub list: gtk4::Box,
    pub store: Store,
}

pub fn new_store() -> Store {
    Arc::new(Mutex::new(VecDeque::new()))
}

/// Insert or replace by `id`, newest first. Drops anything past [`LIMIT`].
pub fn record(store: &Store, entry: Entry) {
    let mut hist = store.lock().unwrap();
    if let Some(pos) = hist.iter().position(|e| e.id == entry.id) {
        hist.remove(pos);
    }
    hist.push_front(entry);
    while hist.len() > LIMIT {
        hist.pop_back();
    }
}

pub fn build_window(store: Store) -> Ui {
    let window = gtk4::Window::new();
    window.add_css_class("breadbar-history");
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Right, true);
    window.set_margin(Edge::Top, 48);
    window.set_margin(Edge::Right, 20);
    window.set_default_width(360);
    window.set_keyboard_mode(KeyboardMode::OnDemand);

    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    outer.set_margin_top(10);
    outer.set_margin_bottom(10);
    outer.set_margin_start(10);
    outer.set_margin_end(10);

    let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let title = gtk4::Label::new(Some("Notifications"));
    title.add_css_class("history-title");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    header.append(&title);

    let close_btn = gtk4::Button::with_label("Close");
    close_btn.add_css_class("flat");
    close_btn.add_css_class("history-close");
    let win_close = window.clone();
    close_btn.connect_clicked(move |_| {
        win_close.set_visible(false);
    });
    header.append(&close_btn);
    outer.append(&header);

    let list = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroll.set_propagate_natural_height(true);
    scroll.set_max_content_height(480);
    scroll.set_min_content_width(320);
    scroll.set_child(Some(&list));
    outer.append(&scroll);

    window.set_child(Some(&outer));

    let win_esc = window.clone();
    let keys = gtk4::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk4::gdk::Key::Escape {
            win_esc.set_visible(false);
            gtk4::glib::Propagation::Stop
        } else {
            gtk4::glib::Propagation::Proceed
        }
    });
    window.add_controller(keys);

    window.connect_close_request(|w| {
        w.set_visible(false);
        gtk4::glib::Propagation::Stop
    });

    Ui {
        window,
        list,
        store,
    }
}

pub fn toggle(ui: &Ui) {
    if ui.window.is_visible() {
        ui.window.set_visible(false);
    } else {
        rebuild(&ui.list, &ui.store);
        ui.window.set_visible(true);
    }
}

pub fn refresh_if_visible(ui: &Ui) {
    if ui.window.is_visible() {
        rebuild(&ui.list, &ui.store);
    }
}

pub fn rebuild(list: &gtk4::Box, store: &Store) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    let entries: Vec<Entry> = store.lock().unwrap().iter().cloned().collect();
    if entries.is_empty() {
        let empty = gtk4::Label::new(Some("No notifications yet"));
        empty.add_css_class("history-empty");
        empty.set_xalign(0.0);
        list.append(&empty);
        return;
    }

    for entry in entries {
        list.append(&make_row(&entry));
    }
}

fn make_row(entry: &Entry) -> gtk4::Box {
    let card = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    card.add_css_class("notification-card");
    card.add_css_class("history-card");
    if let Some(class) = entry.urgency.css_class() {
        card.add_css_class(class);
    }

    let top = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let show_app = !entry.app_name.is_empty()
        && !entry.app_name.eq_ignore_ascii_case(&entry.summary);
    if show_app {
        let app = gtk4::Label::new(Some(&entry.app_name));
        app.add_css_class("notification-app");
        app.set_xalign(0.0);
        app.set_hexpand(true);
        app.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        top.append(&app);
    } else {
        let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        top.append(&spacer);
    }
    let time = gtk4::Label::new(Some(&format_time(entry.received)));
    time.add_css_class("history-time");
    time.set_xalign(1.0);
    top.append(&time);
    card.append(&top);

    if !entry.summary.is_empty() {
        let summary = gtk4::Label::new(Some(&entry.summary));
        summary.add_css_class("notification-summary");
        summary.set_xalign(0.0);
        summary.set_wrap(true);
        summary.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
        card.append(&summary);
    }

    let body = collapse_ws(&entry.body);
    if !body.is_empty() {
        let body_lbl = gtk4::Label::new(Some(&truncate(&body, BODY_MAX_CHARS)));
        body_lbl.add_css_class("notification-body");
        body_lbl.add_css_class("history-body");
        body_lbl.set_xalign(0.0);
        body_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        body_lbl.set_max_width_chars(48);
        card.append(&body_lbl);
    }

    card
}

fn format_time(received: SystemTime) -> String {
    let Ok(dur) = received.duration_since(SystemTime::UNIX_EPOCH) else {
        return "--:--".into();
    };
    let Ok(dt) = gtk4::glib::DateTime::from_unix_local(dur.as_secs() as i64) else {
        return "--:--".into();
    };
    dt.format("%H:%M")
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "--:--".into())
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let taken: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{taken}…")
    } else {
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u32, summary: &str) -> Entry {
        Entry {
            id,
            app_name: "app".into(),
            summary: summary.into(),
            body: String::new(),
            urgency: Urgency::Normal,
            received: SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn record_is_newest_first_and_bounded() {
        let store = new_store();
        for i in 0..(LIMIT as u32 + 5) {
            record(&store, entry(i, &format!("n{i}")));
        }
        let hist = store.lock().unwrap();
        assert_eq!(hist.len(), LIMIT);
        assert_eq!(hist.front().unwrap().id, LIMIT as u32 + 4);
        assert_eq!(hist.back().unwrap().id, 5);
    }

    #[test]
    fn record_replaces_same_id_and_moves_to_front() {
        let store = new_store();
        record(&store, entry(1, "old"));
        record(&store, entry(2, "other"));
        record(&store, entry(1, "new"));
        let hist = store.lock().unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].id, 1);
        assert_eq!(hist[0].summary, "new");
        assert_eq!(hist[1].id, 2);
    }

    #[test]
    fn truncate_adds_ellipsis_past_limit() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello…");
    }
}

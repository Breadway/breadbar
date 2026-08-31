use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use gtk4::prelude::*;
use gtk4_layer_shell::{KeyboardMode, LayerShell};
use serde::{Deserialize, Serialize};

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

/// Load the last [`LIMIT`] entries from `$XDG_STATE_HOME/breadbar/history.json`
/// (or `~/.local/state/breadbar/history.json`). Missing or corrupt files
/// yield an empty store — never fail startup.
pub fn load_store() -> Store {
    let store = new_store();
    if let Some(path) = history_path() {
        load_into(&store, &path);
    }
    store
}

/// Next D-Bus notification id so persisted rows are not replaced on restart.
pub fn next_id(store: &Store) -> u32 {
    store
        .lock()
        .unwrap()
        .iter()
        .map(|e| e.id)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
        .max(1)
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

/// Best-effort write of the in-memory store (already bounded) to the
/// XDG state file. Failures are silent — history stays in memory.
pub fn persist(store: &Store) {
    if let Some(path) = history_path() {
        let _ = persist_to(store, &path);
    }
}

fn history_path() -> Option<PathBuf> {
    Some(state_dir()?.join("history.json"))
}

fn state_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("breadbar"));
        }
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/state/breadbar"))
}

#[derive(Serialize, Deserialize)]
struct PersistedEntry {
    id: u32,
    app_name: String,
    summary: String,
    body: String,
    urgency: String,
    received_unix: u64,
}

fn urgency_name(u: Urgency) -> &'static str {
    match u {
        Urgency::Low => "low",
        Urgency::Normal => "normal",
        Urgency::Critical => "critical",
    }
}

fn urgency_from_name(s: &str) -> Urgency {
    match s {
        "low" => Urgency::Low,
        "critical" => Urgency::Critical,
        _ => Urgency::Normal,
    }
}

fn to_persisted(entry: &Entry) -> PersistedEntry {
    let received_unix = entry
        .received
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    PersistedEntry {
        id: entry.id,
        app_name: entry.app_name.clone(),
        summary: entry.summary.clone(),
        body: entry.body.clone(),
        urgency: urgency_name(entry.urgency).into(),
        received_unix,
    }
}

fn from_persisted(entry: PersistedEntry) -> Entry {
    Entry {
        id: entry.id,
        app_name: entry.app_name,
        summary: entry.summary,
        body: entry.body,
        urgency: urgency_from_name(&entry.urgency),
        received: SystemTime::UNIX_EPOCH + Duration::from_secs(entry.received_unix),
    }
}

fn persist_to(store: &Store, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let payload: Vec<PersistedEntry> = store.lock().unwrap().iter().map(to_persisted).collect();
    let bytes = serde_json::to_vec(&payload).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

fn load_into(store: &Store, path: &Path) {
    let Ok(bytes) = fs::read(path) else {
        return;
    };
    let Ok(parsed) = serde_json::from_slice::<Vec<PersistedEntry>>(&bytes) else {
        return;
    };
    let mut hist = store.lock().unwrap();
    hist.clear();
    for entry in parsed.into_iter().take(LIMIT) {
        hist.push_back(from_persisted(entry));
    }
}

pub fn build_window(store: Store) -> Ui {
    let window = gtk4::Window::new();
    window.add_css_class("breadbar-history");
    window.init_layer_shell();
    window.set_namespace(Some("breadbar-notif"));
    crate::surface::apply(&window, "breadbar-notif");
    // Overrides `[surfaces."breadbar-notif"].width` (320px, the live-toast
    // popup's width — see `surface::apply`'s doc comment): the history
    // window genuinely wants a different width on the same namespace, and
    // that isn't something the manifest schema models today. Both calls
    // must be overridden, not just `set_default_width` — `apply()` also
    // pins `set_size_request` to the toast's 320px, and a bare width alone
    // would lose to that pin the same way it lost to a wide child before.
    window.set_default_width(360);
    window.set_size_request(360, -1);
    window.set_keyboard_mode(KeyboardMode::OnDemand);
    crate::theme::bind_auto(&window);

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
    let show_app =
        !entry.app_name.is_empty() && !entry.app_name.eq_ignore_ascii_case(&entry.summary);
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

    #[test]
    fn persist_roundtrip_keeps_newest_first_and_bound() {
        let dir = std::env::temp_dir().join(format!(
            "breadbar-history-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");
        let store = new_store();
        for i in 0..(LIMIT as u32 + 3) {
            record(&store, entry(i, &format!("n{i}")));
        }
        persist_to(&store, &path).unwrap();

        let loaded = new_store();
        load_into(&loaded, &path);
        assert_eq!(next_id(&loaded), LIMIT as u32 + 3);
        let hist = loaded.lock().unwrap();
        assert_eq!(hist.len(), LIMIT);
        assert_eq!(hist.front().unwrap().id, LIMIT as u32 + 2);
        assert_eq!(
            hist.front().unwrap().summary,
            format!("n{}", LIMIT as u32 + 2)
        );
        drop(hist);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_into_ignores_corrupt_file() {
        let dir = std::env::temp_dir().join(format!(
            "breadbar-history-bad-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");
        fs::write(&path, "not-json").unwrap();
        let store = new_store();
        load_into(&store, &path);
        assert!(store.lock().unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}

use crate::{App, AppInput};
use futures_lite::StreamExt;
use gtk4::prelude::Cast;
use relm4::ComponentSender;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zbus::{interface, object_server::SignalEmitter};

#[derive(Debug)]
pub enum TrayIconData {
    Pixels { width: i32, height: i32, data: Vec<u8> },
    Name(String),
}

#[derive(Debug)]
pub enum TrayUpdate {
    Add { id: String, icon: Option<TrayIconData>, title: String },
    Remove { id: String },
}

struct WatcherState {
    items: Vec<String>,
}

struct Watcher {
    state: Arc<Mutex<WatcherState>>,
    tx: tokio::sync::mpsc::UnboundedSender<(String, String)>,
}

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(
        &self,
        service: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] ctx: SignalEmitter<'_>,
    ) {
        let sender_name = header.sender().map(|s| s.to_string()).unwrap_or_default();
        let (bus, path) = parse_service(&service, &sender_name);
        let full = format!("{}{}", bus, path);
        {
            let mut state = self.state.lock().unwrap();
            if !state.items.contains(&full) {
                state.items.push(full.clone());
            }
        }
        let _ = Self::status_notifier_item_registered(&ctx, &full).await;
        let _ = self.tx.send((bus, path));
    }

    async fn register_status_notifier_host(
        &self,
        _service: String,
        #[zbus(signal_emitter)] ctx: SignalEmitter<'_>,
    ) {
        let _ = Self::status_notifier_host_registered(&ctx).await;
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.state.lock().unwrap().items.clone()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        ctx: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        ctx: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(ctx: &SignalEmitter<'_>) -> zbus::Result<()>;
}

fn parse_service(service: &str, sender: &str) -> (String, String) {
    if service.starts_with('/') {
        return (sender.to_string(), service.to_string());
    }
    match service.find('/') {
        Some(slash) => (service[..slash].to_string(), service[slash..].to_string()),
        None => (service.to_string(), "/StatusNotifierItem".to_string()),
    }
}

async fn read_item(
    conn: &zbus::Connection,
    bus: &str,
    path: &str,
) -> (Option<TrayIconData>, String) {
    let Ok(proxy) = zbus::Proxy::new(conn, bus, path, "org.kde.StatusNotifierItem").await else {
        return (None, String::new());
    };
    let icon = read_icon(&proxy).await;
    let title = proxy.get_property::<String>("Title").await.unwrap_or_default();
    (icon, title)
}

async fn read_icon(proxy: &zbus::Proxy<'_>) -> Option<TrayIconData> {
    let pixmaps: Vec<(i32, i32, Vec<u8>)> =
        proxy.get_property("IconPixmap").await.unwrap_or_default();

    if !pixmaps.is_empty() {
        return pixmaps
            .into_iter()
            .filter(|(w, h, _)| *w > 0 && *h > 0)
            .min_by_key(|(w, h, _)| (w.max(h) - 22).abs())
            .map(|(width, height, data)| TrayIconData::Pixels { width, height, data });
    }

    let name: String = proxy.get_property("IconName").await.ok()?;
    if name.is_empty() {
        return None;
    }
    Some(TrayIconData::Name(name))
}

/// Call `Activate(0, 0)` on the SNI item identified by `id` (`{bus}{path}`).
pub fn spawn_activate(id: String) {
    relm4::spawn(async move {
        let (bus, path) = match id.find('/') {
            Some(slash) => (id[..slash].to_string(), id[slash..].to_string()),
            None => (id, "/StatusNotifierItem".to_string()),
        };
        let Ok(conn) = zbus::Connection::session().await else { return };
        let Ok(proxy) = zbus::Proxy::new(&conn, bus.as_str(), path.as_str(), "org.kde.StatusNotifierItem").await else { return };
        let _ = proxy.call_method("Activate", &(0i32, 0i32)).await;
    });
}

pub fn spawn_watcher(sender: ComponentSender<App>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    // Maps bus name → item ids, shared between registration and cleanup tasks.
    let bus_map: Arc<Mutex<HashMap<String, Vec<String>>>> = Arc::new(Mutex::new(HashMap::new()));
    let bus_map_cleanup = bus_map.clone();
    let sender_cleanup = sender.clone();

    // Registration task — owns the watcher service and processes new items.
    relm4::spawn(async move {
        let watcher = Watcher {
            state: Arc::new(Mutex::new(WatcherState { items: Vec::new() })),
            tx,
        };
        // Builder steps fail only on invalid static strings — safe to unwrap.
        let conn = zbus::connection::Builder::session()
            .unwrap()
            .name("org.kde.StatusNotifierWatcher")
            .unwrap()
            .serve_at("/StatusNotifierWatcher", watcher)
            .unwrap()
            .build()
            .await
            .expect("failed to register org.kde.StatusNotifierWatcher");

        while let Some((bus, path)) = rx.recv().await {
            let (icon, title) = read_item(&conn, &bus, &path).await;
            let id = format!("{}{}", bus, path);
            bus_map.lock().unwrap().entry(bus).or_default().push(id.clone());
            sender.input(AppInput::TrayUpdate(TrayUpdate::Add { id, icon, title }));
        }
    });

    // Cleanup task — watches NameOwnerChanged and removes items when their owner exits.
    relm4::spawn(async move {
        let Ok(conn) = zbus::Connection::session().await else { return };
        let Ok(proxy) = zbus::fdo::DBusProxy::new(&conn).await else { return };
        let Ok(mut stream) = proxy.receive_name_owner_changed().await else { return };
        while let Some(signal) = stream.next().await {
            let Ok(args) = signal.args() else { continue };
            if args.new_owner().is_none() {
                let gone = args.name().to_string();
                if let Some(ids) = bus_map_cleanup.lock().unwrap().remove(&gone) {
                    for id in ids {
                        sender_cleanup.input(AppInput::TrayUpdate(TrayUpdate::Remove { id }));
                    }
                }
            }
        }
    });
}

/// Convert SNI ARGB pixel data (network byte order) to a GTK4 `Image`.
/// Falls back to an icon-name lookup or a placeholder on failure.
pub fn make_tray_image(icon: Option<&TrayIconData>) -> gtk4::Image {
    let img = match icon {
        Some(TrayIconData::Pixels { width, height, data }) => pixels_to_image(*width, *height, data),
        Some(TrayIconData::Name(name)) => {
            let img = gtk4::Image::from_icon_name(name);
            img.set_pixel_size(16);
            Some(img)
        }
        None => None,
    };
    img.unwrap_or_else(|| {
        let img = gtk4::Image::from_icon_name("image-missing");
        img.set_pixel_size(16);
        img
    })
}

fn pixels_to_image(width: i32, height: i32, data: &[u8]) -> Option<gtk4::Image> {
    if data.len() != (width * height * 4) as usize {
        return None;
    }
    // SNI delivers ARGB big-endian: bytes are [A, R, G, B] per pixel.
    // GTK4 R8g8b8a8 expects [R, G, B, A] per pixel.
    let mut rgba = Vec::with_capacity(data.len());
    for chunk in data.chunks_exact(4) {
        rgba.push(chunk[1]);
        rgba.push(chunk[2]);
        rgba.push(chunk[3]);
        rgba.push(chunk[0]);
    }
    let bytes = gtk4::glib::Bytes::from_owned(rgba);
    let tex = gtk4::gdk::MemoryTexture::new(
        width,
        height,
        gtk4::gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        (width * 4) as usize,
    );
    let tex: gtk4::gdk::Texture = tex.upcast();
    let img = gtk4::Image::from_paintable(Some(&tex));
    img.set_pixel_size(16);
    Some(img)
}

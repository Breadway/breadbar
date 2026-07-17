use crate::{App, AppInput};
use relm4::ComponentSender;
use std::fs;

#[derive(Debug, Clone)]
pub struct BtDevice {
    pub address: String,
    pub name: String,
    pub connected: bool,
    pub paired: bool,
}

#[derive(Debug, Clone)]
pub struct BtPopoverData {
    pub powered: bool,
    pub devices: Vec<BtDevice>,
}

/// Same rfkill scan `bar::stats` uses for the bar icon — kept independent
/// (rather than shared) since it's a two-line read and pulling in a shared
/// helper isn't worth the coupling.
fn powered() -> bool {
    fs::read_dir("/sys/class/rfkill")
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .any(|e| {
            let p = e.path();
            fs::read_to_string(p.join("type"))
                .map(|t| t.trim() == "bluetooth")
                .unwrap_or(false)
                && fs::read_to_string(p.join("state"))
                    .map(|s| s.trim() == "1")
                    .unwrap_or(false)
        })
}

async fn fetch_devices() -> Vec<BtDevice> {
    try_fetch_devices().await.unwrap_or_default()
}

async fn try_fetch_devices() -> Option<Vec<BtDevice>> {
    let conn = zbus::Connection::system().await.ok()?;
    let mgr = zbus::fdo::ObjectManagerProxy::builder(&conn)
        .destination("org.bluez")
        .ok()?
        .path("/")
        .ok()?
        .build()
        .await
        .ok()?;
    let objects = mgr.get_managed_objects().await.ok()?;

    let mut devices: Vec<BtDevice> = objects
        .values()
        .filter_map(|ifaces| ifaces.get("org.bluez.Device1"))
        .filter_map(|props| {
            let paired = props
                .get("Paired")
                .and_then(|v| bool::try_from(v.clone()).ok())
                .unwrap_or(false);
            if !paired {
                return None;
            }
            let address = props
                .get("Address")
                .and_then(|v| String::try_from(v.clone()).ok())?;
            let name = props
                .get("Alias")
                .or_else(|| props.get("Name"))
                .and_then(|v| String::try_from(v.clone()).ok())
                .unwrap_or_else(|| address.clone());
            let connected = props
                .get("Connected")
                .and_then(|v| bool::try_from(v.clone()).ok())
                .unwrap_or(false);
            Some(BtDevice { address, name, connected, paired })
        })
        .collect();

    devices.sort_by(|a, b| b.connected.cmp(&a.connected).then(a.name.cmp(&b.name)));
    Some(devices)
}

pub fn spawn_popover_load(sender: ComponentSender<App>) {
    relm4::spawn(async move {
        let devices = fetch_devices().await;
        sender.input(AppInput::BtPopoverData(BtPopoverData {
            powered: powered(),
            devices,
        }));
    });
}

/// Fire-and-forget: toggle the adapter's rfkill soft-block.
pub fn spawn_set_powered(on: bool) {
    relm4::spawn(async move {
        let _ = tokio::process::Command::new("rfkill")
            .args([if on { "unblock" } else { "block" }, "bluetooth"])
            .output()
            .await;
    });
}

/// Fire-and-forget: connect a paired device by address via `bluetoothctl`.
pub fn spawn_connect(address: String) {
    relm4::spawn(async move {
        let _ = tokio::process::Command::new("bluetoothctl")
            .args(["connect", &address])
            .output()
            .await;
    });
}

/// Fire-and-forget: disconnect a device by address via `bluetoothctl`.
pub fn spawn_disconnect(address: String) {
    relm4::spawn(async move {
        let _ = tokio::process::Command::new("bluetoothctl")
            .args(["disconnect", &address])
            .output()
            .await;
    });
}

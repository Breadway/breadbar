use crate::{App, AppInput};
use relm4::ComponentSender;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct BtDevice {
    pub address: String,
    pub name: String,
    pub connected: bool,
    pub paired: bool,
}

/// Whether BlueZ itself is running. This is deliberately separate from the
/// adapter's rfkill state: an unblocked adapter with `bluetooth.service`
/// inactive used to read as "On" because the old code only looked at
/// `/sys/class/rfkill`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtServiceState {
    Active,
    Inactive,
}

impl BtServiceState {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// The one-line state the dashboard chip and drawer describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtSummary {
    /// `bluetooth.service` is not running, so nothing below it can be on.
    ServiceOff,
    /// The service is running but BlueZ has no adapter object.
    NoAdapter,
    /// Adapter present and BlueZ reports it powered off.
    Off,
    /// Adapter present and powered on.
    On,
}

#[derive(Debug, Clone)]
pub struct BtPopoverData {
    pub service: BtServiceState,
    pub adapter_present: bool,
    pub powered: bool,
    pub devices: Vec<BtDevice>,
}

impl BtPopoverData {
    pub fn summary(&self) -> BtSummary {
        if !self.service.is_active() {
            BtSummary::ServiceOff
        } else if !self.adapter_present {
            BtSummary::NoAdapter
        } else if !self.powered {
            BtSummary::Off
        } else {
            BtSummary::On
        }
    }

    /// "On" here means the service, adapter and adapter power are all real.
    /// Never derive this from a cached chip class.
    pub fn is_on(&self) -> bool {
        self.summary() == BtSummary::On
    }

    /// The chip's secondary line. Connected devices only count while the
    /// adapter is actually powered on.
    pub fn chip_label(&self) -> String {
        match self.summary() {
            BtSummary::ServiceOff => "Service off".to_string(),
            BtSummary::NoAdapter => "No adapter".to_string(),
            BtSummary::Off => "Off".to_string(),
            BtSummary::On => match connected_names(&self.devices).as_slice() {
                [] => "On".to_string(),
                [one] => (*one).to_string(),
                [first, rest @ ..] => format!("{first} +{}", rest.len()),
            },
        }
    }

    /// The drawer's explanatory line when there is nothing to list.
    pub fn drawer_note(&self) -> Option<&'static str> {
        match self.summary() {
            BtSummary::ServiceOff => Some("Bluetooth service is off"),
            BtSummary::NoAdapter => Some("No Bluetooth adapter"),
            BtSummary::Off => Some("Bluetooth is off"),
            BtSummary::On => None,
        }
    }
}

fn connected_names(devices: &[BtDevice]) -> Vec<&str> {
    devices
        .iter()
        .filter(|d| d.connected)
        .map(|d| d.name.as_str())
        .collect()
}

/// `systemctl is-active` is the authority for the service. Any failure
/// (unit not installed, systemctl missing, unusual state such as
/// `activating` or `failed`) is reported as inactive so the tile can never
/// claim "On" from a service that is not actually up.
async fn service_state() -> BtServiceState {
    match tokio::process::Command::new("systemctl")
        .args(["is-active", "bluetooth.service"])
        .output()
        .await
    {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            if stdout.trim() == "active" {
                BtServiceState::Active
            } else {
                BtServiceState::Inactive
            }
        }
        _ => BtServiceState::Inactive,
    }
}

#[derive(Debug, Default)]
struct BluezSnapshot {
    adapter_present: bool,
    powered: bool,
    devices: Vec<BtDevice>,
}

async fn fetch_snapshot() -> BluezSnapshot {
    let Ok(conn) = zbus::Connection::system().await else {
        return BluezSnapshot::default();
    };
    let Some(builder) = zbus::fdo::ObjectManagerProxy::builder(&conn)
        .destination("org.bluez")
        .ok()
        .and_then(|b| b.path("/").ok())
    else {
        return BluezSnapshot::default();
    };
    let Ok(mgr) = builder.build().await else {
        return BluezSnapshot::default();
    };
    let Ok(objects) = mgr.get_managed_objects().await else {
        return BluezSnapshot::default();
    };

    let mut snapshot = BluezSnapshot::default();
    let mut powered = false;
    let mut devices = Vec::new();

    for ifaces in objects.values() {
        if let Some(props) = ifaces.get("org.bluez.Adapter1") {
            snapshot.adapter_present = true;
            if props
                .get("Powered")
                .and_then(|v| bool::try_from(v.clone()).ok())
                .unwrap_or(false)
            {
                powered = true;
            }
        }
        let Some(props) = ifaces.get("org.bluez.Device1") else {
            continue;
        };
        let paired = props
            .get("Paired")
            .and_then(|v| bool::try_from(v.clone()).ok())
            .unwrap_or(false);
        if !paired {
            continue;
        }
        let Some(address) = props
            .get("Address")
            .and_then(|v| String::try_from(v.clone()).ok())
        else {
            continue;
        };
        let name = props
            .get("Alias")
            .or_else(|| props.get("Name"))
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| address.clone());
        let connected = props
            .get("Connected")
            .and_then(|v| bool::try_from(v.clone()).ok())
            .unwrap_or(false);
        devices.push(BtDevice {
            address,
            name,
            connected,
            paired,
        });
    }

    // Only BlueZ's Adapter1 `Powered` property counts. rfkill reports whether
    // the adapter is soft-blocked, which stayed unblocked while the service
    // was down; that was the stale "On" this card is about.
    snapshot.powered = snapshot.adapter_present && powered;
    devices.sort_by(|a, b| b.connected.cmp(&a.connected).then(a.name.cmp(&b.name)));
    snapshot.devices = devices;
    snapshot
}

async fn load(sender: ComponentSender<App>) {
    let (service, snapshot) = tokio::join!(service_state(), fetch_snapshot());
    sender.input(AppInput::BtPopoverData(BtPopoverData {
        service,
        adapter_present: snapshot.adapter_present,
        powered: snapshot.powered,
        devices: snapshot.devices,
    }));
}

pub fn spawn_popover_load(sender: ComponentSender<App>) {
    relm4::spawn(load(sender));
}

/// Fire-and-forget: toggle the adapter's rfkill soft-block, then reload the
/// real state. The chip must not paint "On" optimistically; the reload is
/// what moves it.
pub fn spawn_set_powered(sender: ComponentSender<App>, on: bool) {
    relm4::spawn(async move {
        let _ = tokio::process::Command::new("rfkill")
            .args([if on { "unblock" } else { "block" }, "bluetooth"])
            .output()
            .await;
        // rfkill is synchronous, but BlueZ's adapter property can lag it by a
        // poll turn; without this the reload can still read the old value.
        tokio::time::sleep(Duration::from_millis(150)).await;
        load(sender).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn data(
        service: BtServiceState,
        adapter_present: bool,
        powered: bool,
        devices: Vec<BtDevice>,
    ) -> BtPopoverData {
        BtPopoverData {
            service,
            adapter_present,
            powered,
            devices,
        }
    }

    fn device(name: &str, connected: bool) -> BtDevice {
        BtDevice {
            address: format!("AA:BB:CC:DD:EE:{name}"),
            name: name.to_string(),
            connected,
            paired: true,
        }
    }

    #[test]
    fn inactive_service_is_never_on_even_with_a_powered_adapter() {
        // The exact reported bug: rfkill said the adapter was unblocked
        // while bluetooth.service was down, so the tile said "On".
        let d = data(
            BtServiceState::Inactive,
            true,
            true,
            vec![device("Buds", true)],
        );
        assert_eq!(d.summary(), BtSummary::ServiceOff);
        assert!(!d.is_on());
        assert_eq!(d.chip_label(), "Service off");
        assert_eq!(d.drawer_note(), Some("Bluetooth service is off"));
    }

    #[test]
    fn active_service_without_an_adapter_is_distinct_from_off() {
        let d = data(BtServiceState::Active, false, false, vec![]);
        assert_eq!(d.summary(), BtSummary::NoAdapter);
        assert!(!d.is_on());
        assert_eq!(d.chip_label(), "No adapter");
        assert_eq!(d.drawer_note(), Some("No Bluetooth adapter"));
        assert_ne!(
            d.summary(),
            data(BtServiceState::Active, true, false, vec![]).summary()
        );
    }

    #[test]
    fn active_service_with_powered_off_adapter_is_off() {
        let d = data(BtServiceState::Active, true, false, vec![]);
        assert_eq!(d.summary(), BtSummary::Off);
        assert!(!d.is_on());
        assert_eq!(d.chip_label(), "Off");
    }

    #[test]
    fn active_service_with_powered_adapter_is_on() {
        let d = data(BtServiceState::Active, true, true, vec![]);
        assert_eq!(d.summary(), BtSummary::On);
        assert!(d.is_on());
        assert_eq!(d.chip_label(), "On");
        assert_eq!(d.drawer_note(), None);
    }

    #[test]
    fn connected_device_names_only_show_when_really_on() {
        let buds = device("Buds", true);
        let on = data(BtServiceState::Active, true, true, vec![buds.clone()]);
        assert_eq!(on.chip_label(), "Buds");
        let off = data(BtServiceState::Active, true, false, vec![buds]);
        assert_eq!(off.chip_label(), "Off");
    }

    #[test]
    fn multiple_connected_devices_are_summarised() {
        let d = data(
            BtServiceState::Active,
            true,
            true,
            vec![
                device("Buds", true),
                device("Mouse", true),
                device("Keys", false),
            ],
        );
        assert_eq!(d.chip_label(), "Buds +1");
    }
}

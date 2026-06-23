use crate::{App, AppInput};
use relm4::ComponentSender;
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicU8, Ordering},
        LazyLock, Mutex, OnceLock,
    },
};
use tokio::sync::OnceCell as AsyncOnce;

static WIFI_IFACE: OnceLock<Option<String>> = OnceLock::new();
static NET_PREV: LazyLock<Mutex<Option<(u64, u64, std::time::Instant)>>> =
    LazyLock::new(|| Mutex::new(None));
static BT_CONN: AsyncOnce<zbus::Connection> = AsyncOnce::const_new();
static BT_CACHE: LazyLock<Mutex<&'static str>> = LazyLock::new(|| Mutex::new(BT_OFF));
static BT_TICK: AtomicU8 = AtomicU8::new(0);

// Embedded SVG contents (not paths). These &str constants double as stable
// HashMap keys via their .as_ptr(); include_str! keeps each one a single
// 'static literal, so pointer identity still holds.
pub const WIFI_STRONG: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/WiFi Strong.svg"));
pub const WIFI_MEDIUM: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/WiFi Medium.svg"));
pub const WIFI_WEAK: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/WiFi Weak.svg"));
pub const WIFI_OFF: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/WiFi Disconnect.svg"));

pub const BAT_HIGH: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/Battery 3 Bars.svg"));
pub const BAT_MID: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/Battery 2 Bars.svg"));
pub const BAT_LOW: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/Battery 1 Bar.svg"));
pub const AC_POWER: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/AC Power.svg"));

pub const BT_OFF: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/Bluetooth Off.svg"));
pub const BT_ON: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/Bluetooth.svg"));
pub const BT_CONNECTED: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/Bluetooth Connected.svg"
));

#[derive(Debug)]
pub struct Stats {
    pub cpu: String,
    pub mem: String,
    pub power: String,
    pub bat: String,
    pub bat_icon: &'static str,
    pub ac_connected: bool,
    pub bt_icon: &'static str,
    pub wifi_ssid: String,
    pub wifi_icon: &'static str,
    pub wifi_profile: Option<String>,
    pub cpu_temp: Option<f32>,
    pub gpu_usage: Option<u8>,
    pub net_rx_kbs: f32,
    pub net_tx_kbs: f32,
}

struct CpuSnapshot {
    total: u64,
    idle: u64,
}

static PREV_CPU: OnceLock<Mutex<CpuSnapshot>> = OnceLock::new();
static BAT_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static AC_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static WIFI_CACHE: LazyLock<Mutex<(String, &'static str)>> =
    LazyLock::new(|| Mutex::new(("—".to_string(), WIFI_OFF)));
static WIFI_TICK: AtomicU8 = AtomicU8::new(0);

fn read_cpu() -> f32 {
    let text = fs::read_to_string("/proc/stat").unwrap_or_default();
    let line = text.lines().next().unwrap_or_default();
    let mut total = 0u64;
    let mut idle = 0u64;
    let mut count = 0usize;
    for (i, s) in line.split_whitespace().skip(1).enumerate() {
        if let Ok(v) = s.parse::<u64>() {
            total += v;
            if i == 3 || i == 4 {
                idle += v;
            }
            count += 1;
        }
    }
    if count < 5 {
        return 0.0;
    }

    let state = PREV_CPU.get_or_init(|| Mutex::new(CpuSnapshot { total, idle }));
    let mut prev = state.lock().unwrap();
    let dtotal = total.saturating_sub(prev.total);
    let didle = idle.saturating_sub(prev.idle);
    *prev = CpuSnapshot { total, idle };

    if dtotal == 0 {
        return 0.0;
    }
    (dtotal - didle) as f32 / dtotal as f32 * 100.0
}

fn read_ram() -> u64 {
    let text = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mut total = 0u64;
    let mut avail = 0u64;
    let mut found = 0u8;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("MemTotal:") => {
                total = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                found += 1;
            }
            Some("MemAvailable:") => {
                avail = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                found += 1;
            }
            _ => {}
        }
        if found == 2 {
            break;
        }
    }
    total.saturating_sub(avail)
}

fn bat_path() -> Option<&'static PathBuf> {
    BAT_PATH
        .get_or_init(|| {
            fs::read_dir("/sys/class/power_supply")
                .ok()?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .find(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("BAT"))
                })
        })
        .as_ref()
}

fn read_power() -> Option<f32> {
    let path = bat_path()?;
    if let Ok(v) = fs::read_to_string(path.join("power_now")) {
        if let Ok(uw) = v.trim().parse::<u64>() {
            return Some(uw as f32 / 1_000_000.0);
        }
    }
    let ua: u64 = fs::read_to_string(path.join("current_now"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let uv: u64 = fs::read_to_string(path.join("voltage_now"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some((ua as f64 * uv as f64 / 1e12) as f32)
}

fn read_battery() -> Option<u8> {
    fs::read_to_string(bat_path()?.join("capacity"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn bat_level_icon(pct: u8) -> &'static str {
    if pct >= 67 {
        BAT_HIGH
    } else if pct >= 34 {
        BAT_MID
    } else {
        BAT_LOW
    }
}

fn read_ac() -> bool {
    AC_PATH
        .get_or_init(|| {
            fs::read_dir("/sys/class/power_supply")
                .ok()?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .find(|p| {
                    fs::read_to_string(p.join("type"))
                        .map(|t| t.trim() == "Mains")
                        .unwrap_or(false)
                })
        })
        .as_ref()
        .and_then(|p| fs::read_to_string(p.join("online")).ok())
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

fn bt_rfkill_on() -> bool {
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

async fn read_bt() -> &'static str {
    if !bt_rfkill_on() {
        return BT_OFF;
    }
    bt_connected_icon().await.unwrap_or(BT_ON)
}

async fn bt_connected_icon() -> Option<&'static str> {
    let conn = BT_CONN
        .get_or_try_init(zbus::Connection::system)
        .await
        .ok()?;
    let mgr = zbus::fdo::ObjectManagerProxy::builder(conn)
        .destination("org.bluez")
        .ok()?
        .path("/")
        .ok()?
        .build()
        .await
        .ok()?;
    let objects = mgr.get_managed_objects().await.ok()?;
    let connected = objects
        .values()
        .filter_map(|ifaces| ifaces.get("org.bluez.Device1"))
        .any(|props| {
            props
                .get("Connected")
                .and_then(|v| bool::try_from(v.clone()).ok())
                .unwrap_or(false)
        });
    Some(if connected { BT_CONNECTED } else { BT_ON })
}

fn wifi_iface() -> Option<&'static str> {
    WIFI_IFACE
        .get_or_init(|| {
            fs::read_dir("/sys/class/net")
                .ok()?
                .filter_map(|e| e.ok())
                .find(|e| e.path().join("wireless").is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
        })
        .as_deref()
}

async fn read_wifi() -> (String, &'static str) {
    let Some(iface) = wifi_iface() else {
        return ("—".into(), WIFI_OFF);
    };

    let link_out = tokio::process::Command::new("iw")
        .args(["dev", iface, "link"])
        .output()
        .await
        .ok();
    let link_stdout = match link_out {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return ("—".into(), WIFI_OFF),
    };

    let mut ssid = None;
    let mut rssi: Option<i32> = None;
    for line in link_stdout.lines() {
        let t = line.trim();
        if let Some(s) = t.strip_prefix("SSID: ") {
            ssid = Some(s.to_string());
        } else if let Some(r) = t.strip_prefix("signal: ") {
            rssi = r.split_whitespace().next().and_then(|s| s.parse().ok());
        }
    }

    let Some(ssid) = ssid else {
        return ("—".into(), WIFI_OFF);
    };

    let icon = match rssi {
        Some(r) if r >= -55 => WIFI_STRONG,
        Some(r) if r >= -70 => WIFI_MEDIUM,
        _ => WIFI_WEAK,
    };

    (ssid, icon)
}

fn read_cpu_temp() -> Option<f32> {
    for entry in fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
        let path = entry.path();
        let name = fs::read_to_string(path.join("name")).ok()?;
        if name.trim() == "k10temp" {
            let raw = fs::read_to_string(path.join("temp1_input")).ok()?;
            return Some(raw.trim().parse::<f32>().ok()? / 1000.0);
        }
    }
    None
}

fn read_gpu_usage() -> Option<u8> {
    for entry in fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let path = entry.path().join("device/gpu_busy_percent");
        if path.exists() {
            return fs::read_to_string(&path).ok()?.trim().parse().ok();
        }
    }
    None
}

fn read_net_throughput() -> (f32, f32) {
    let text = match fs::read_to_string("/proc/net/dev") {
        Ok(t) => t,
        Err(_) => return (0.0, 0.0),
    };
    let mut total_rx = 0u64;
    let mut total_tx = 0u64;
    for line in text.lines().skip(2) {
        let colon = match line.find(':') {
            Some(i) => i,
            None => continue,
        };
        let iface = line[..colon].trim();
        if matches!(iface, "lo")
            || iface.starts_with("docker")
            || iface.starts_with("veth")
            || iface.starts_with("br-")
            || iface.starts_with("virbr")
        {
            continue;
        }
        let fields: Vec<&str> = line[colon + 1..].split_whitespace().collect();
        if fields.len() >= 9 {
            total_rx += fields[0].parse::<u64>().unwrap_or(0);
            total_tx += fields[8].parse::<u64>().unwrap_or(0);
        }
    }
    let now = std::time::Instant::now();
    let mut guard = NET_PREV.lock().unwrap();
    let result = if let Some((last_rx, last_tx, last_t)) = *guard {
        let dt = now.duration_since(last_t).as_secs_f32().max(0.001);
        let rx = total_rx.saturating_sub(last_rx) as f32 / 1024.0 / dt;
        let tx = total_tx.saturating_sub(last_tx) as f32 / 1024.0 / dt;
        (rx, tx)
    } else {
        (0.0, 0.0)
    };
    *guard = Some((total_rx, total_tx, now));
    result
}

fn read_crumbs_profile() -> Option<String> {
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".local/state"))
                .unwrap_or_else(|| PathBuf::from("/tmp"))
        });
    let text = fs::read_to_string(state_home.join("breadcrumbs/state.toml")).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("profile") {
            let val = rest
                .trim_start_matches(|c: char| c == ' ' || c == '=')
                .trim_matches('"');
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

pub async fn poll() -> Stats {
    let cpu = read_cpu();
    let mem = read_ram();
    let power = read_power().map_or_else(|| "—W".into(), |w| format!("{w:.1}W"));
    let pct = read_battery();
    let bat = pct.map_or_else(|| "—".into(), |p| format!("{p}%"));
    let bat_icon = pct.map_or(BAT_MID, bat_level_icon);
    let ac_connected = read_ac();
    // BT and WiFi both refresh every 8 cycles (~16 s); cache in between.
    let bt_icon = {
        let tick = BT_TICK.fetch_add(1, Ordering::Relaxed);
        if tick.is_multiple_of(8) {
            let fresh = read_bt().await;
            *BT_CACHE.lock().unwrap() = fresh;
            fresh
        } else {
            *BT_CACHE.lock().unwrap()
        }
    };
    let (wifi_ssid, wifi_icon) = {
        let tick = WIFI_TICK.fetch_add(1, Ordering::Relaxed);
        if tick.is_multiple_of(8) {
            let fresh = read_wifi().await;
            *WIFI_CACHE.lock().unwrap() = fresh.clone();
            fresh
        } else {
            WIFI_CACHE.lock().unwrap().clone()
        }
    };
    let wifi_profile = read_crumbs_profile();
    let cpu_temp = read_cpu_temp();
    let gpu_usage = read_gpu_usage();
    let (net_rx_kbs, net_tx_kbs) = read_net_throughput();
    Stats {
        cpu: format!("{cpu:.0}%"),
        mem: if mem >= 1024 * 1024 {
            format!("{:.1}G", mem as f32 / (1024.0 * 1024.0))
        } else {
            format!("{}M", mem / 1024)
        },
        power,
        bat,
        bat_icon,
        ac_connected,
        bt_icon,
        wifi_ssid,
        wifi_icon,
        wifi_profile,
        cpu_temp,
        gpu_usage,
        net_rx_kbs,
        net_tx_kbs,
    }
}

pub fn spawn_poller(sender: ComponentSender<App>) {
    relm4::spawn(async move {
        loop {
            sender.input(AppInput::StatsUpdate(poll().await));
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
}

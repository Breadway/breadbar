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

pub const WIFI_STRONG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/WiFi Strong.svg");
pub const WIFI_MEDIUM: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/WiFi Medium.svg");
pub const WIFI_WEAK: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/WiFi Weak.svg");
pub const WIFI_OFF: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/WiFi Connecting.svg");

#[derive(Debug)]
pub struct Stats {
    pub cpu: String,
    pub mem: String,
    pub power: String,
    pub bat: String,
    pub wifi_ssid: String,
    pub wifi_icon: &'static str,
}

struct CpuSnapshot {
    total: u64,
    idle: u64,
}

static PREV_CPU: OnceLock<Mutex<CpuSnapshot>> = OnceLock::new();
static BAT_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static WIFI_CACHE: LazyLock<Mutex<(String, &'static str)>> =
    LazyLock::new(|| Mutex::new(("—".to_string(), WIFI_OFF)));
static WIFI_TICK: AtomicU8 = AtomicU8::new(0);

fn read_cpu() -> f32 {
    let text = fs::read_to_string("/proc/stat").unwrap_or_default();
    let line = text.lines().next().unwrap_or_default();
    let vals: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|s| s.parse().ok())
        .collect();
    if vals.len() < 5 {
        return 0.0;
    }
    let idle = vals[3] + vals.get(4).copied().unwrap_or(0);
    let total: u64 = vals.iter().sum();

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
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("MemTotal:") => total = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0),
            Some("MemAvailable:") => avail = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0),
            _ => {}
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
                .find(|p| p.file_name().map_or(false, |n| n.to_string_lossy().starts_with("BAT")))
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

async fn read_wifi() -> (String, &'static str) {
    let dev_out = tokio::process::Command::new("iw")
        .arg("dev")
        .output()
        .await
        .ok();
    let dev_stdout = match dev_out {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return ("—".into(), WIFI_OFF),
    };

    let iface = dev_stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("Interface ").map(str::to_string));
    let Some(iface) = iface else {
        return ("—".into(), WIFI_OFF);
    };

    let link_out = tokio::process::Command::new("iw")
        .args(["dev", &iface, "link"])
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

pub async fn poll() -> Stats {
    let cpu = read_cpu();
    let mem = read_ram();
    let power = read_power().map_or_else(|| "  —W".into(), |w| format!("{w:4.1}W"));
    let bat = read_battery().map_or_else(|| " —".into(), |p| format!("{p:3}%"));
    // Refresh WiFi every 8 cycles (~16 s); cache the result in between.
    let (wifi_ssid, wifi_icon) = {
        let tick = WIFI_TICK.fetch_add(1, Ordering::Relaxed);
        if tick % 8 == 0 {
            let fresh = read_wifi().await;
            *WIFI_CACHE.lock().unwrap() = fresh.clone();
            fresh
        } else {
            WIFI_CACHE.lock().unwrap().clone()
        }
    };
    Stats {
        cpu: format!("{cpu:3.0}%"),
        mem: if mem >= 1024 * 1024 {
            format!("{:.1}G", mem as f32 / (1024.0 * 1024.0))
        } else {
            format!("{}M", mem / 1024)
        },
        power,
        bat,
        wifi_ssid,
        wifi_icon,
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

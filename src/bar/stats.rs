use crate::{App, AppInput};
use relm4::ComponentSender;
use std::{fs, path::PathBuf, sync::Mutex};

struct CpuSnapshot {
    total: u64,
    idle: u64,
}

static PREV_CPU: std::sync::OnceLock<Mutex<CpuSnapshot>> = std::sync::OnceLock::new();

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

fn read_ram() -> f32 {
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
    if total == 0 {
        return 0.0;
    }
    (total - avail) as f32 / total as f32 * 100.0
}

fn bat_path() -> Option<PathBuf> {
    fs::read_dir("/sys/class/power_supply")
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map_or(false, |n| n.to_string_lossy().starts_with("BAT"))
        })
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

async fn read_wifi() -> String {
    let out = tokio::process::Command::new("iw")
        .arg("dev")
        .output()
        .await
        .ok();
    let stdout = match out {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return "—".into(),
    };
    stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("ssid ").map(str::to_string))
        .unwrap_or_else(|| "—".into())
}

pub async fn poll() -> String {
    let cpu = read_cpu();
    let ram = read_ram();
    let power = read_power().map_or_else(|| "—W".into(), |w| format!("{w:.1}W"));
    let bat = read_battery().map_or_else(|| "—".into(), |p| format!("{p}%"));
    let wifi = read_wifi().await;
    format!("CPU {cpu:.0}%  MEM {ram:.0}%  {power}  BAT {bat}  {wifi}")
}

pub fn spawn_poller(sender: ComponentSender<App>) {
    relm4::spawn(async move {
        loop {
            sender.input(AppInput::StatsUpdate(poll().await));
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
}

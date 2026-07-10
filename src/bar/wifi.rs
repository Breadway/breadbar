use crate::{App, AppInput};
use relm4::ComponentSender;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct CrumbsStatus {
    pub profile: String,
    pub ssid: Option<String>,
    pub ip: Option<String>,
    pub internet: bool,
    pub captive_portal: bool,
    pub tailscale_ok: bool,
    pub tailscale_required: bool,
}

#[derive(Debug, Clone)]
pub struct ScanEntry {
    pub ssid: String,
    pub signal: u8,  // 0–100 percentage
    pub saved: bool,
}

#[derive(Debug, Clone)]
pub struct WifiPopoverData {
    pub profiles: Vec<(String, bool)>, // (name, is_active)
    pub scan: Vec<ScanEntry>,
}

async fn fetch_status() -> Option<CrumbsStatus> {
    let out = tokio::time::timeout(
        Duration::from_secs(8),
        tokio::process::Command::new("breadcrumbs")
            .args(["status", "--json"])
            .output(),
    )
    .await
    .ok()?
    .ok()?;

    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    Some(CrumbsStatus {
        profile: v["profile"].as_str().unwrap_or("").to_string(),
        ssid: v["ssid"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
        ip: v["ip"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
        internet: v["internet"].as_bool().unwrap_or(true),
        captive_portal: v["captive_portal"].is_string(),
        tailscale_ok: v["tailscale"]["ok"].as_bool().unwrap_or(true),
        tailscale_required: v["tailscale"]["required"].as_bool().unwrap_or(false),
    })
}

async fn fetch_profile_list() -> Vec<(String, bool)> {
    let Ok(Ok(out)) = tokio::time::timeout(
        Duration::from_secs(4),
        tokio::process::Command::new("breadcrumbs")
            .args(["profile", "list"])
            .output(),
    )
    .await
    else {
        return vec![];
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let active = line.starts_with('*');
            let name = line.trim_start_matches(['*', ' ']).trim().to_string();
            if name.is_empty() { None } else { Some((name, active)) }
        })
        .collect()
}

async fn fetch_scan() -> Vec<ScanEntry> {
    let Ok(Ok(out)) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("breadcrumbs")
            .args(["scan-list", "--json"])
            .output(),
    )
    .await
    else {
        return vec![];
    };
    let arr: Vec<serde_json::Value> =
        serde_json::from_slice(&out.stdout).unwrap_or_default();
    arr.into_iter()
        .filter_map(|v| {
            let ssid = v["ssid"].as_str()?.to_string();
            if ssid.is_empty() {
                return None;
            }
            let signal = v["signal"]
                .as_str()
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(0);
            let saved = v["saved"].as_bool().unwrap_or(false);
            Some(ScanEntry { ssid, signal, saved })
        })
        .collect()
}

/// Background poller — updates internet/TS status every 30 s.
pub fn spawn_status_poller(sender: ComponentSender<App>) {
    relm4::spawn(async move {
        loop {
            if let Some(status) = fetch_status().await {
                sender.input(AppInput::CrumbsStatus(status));
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
}

/// Called when the popover opens — loads profiles + scan in parallel.
pub fn spawn_popover_load(sender: ComponentSender<App>) {
    relm4::spawn(async move {
        let (profiles, scan) = tokio::join!(fetch_profile_list(), fetch_scan());
        sender.input(AppInput::WifiPopoverData(WifiPopoverData { profiles, scan }));
    });
}

/// Fire-and-forget: set the active breadcrumbs profile (applies it).
pub fn spawn_profile_set(name: String) {
    relm4::spawn(async move {
        let _ = tokio::process::Command::new("breadcrumbs")
            .args(["profile", "set", &name])
            .output()
            .await;
    });
}

/// Fire-and-forget: connect to a specific saved SSID via `breadcrumbs join`.
pub fn spawn_join(ssid: String) {
    relm4::spawn(async move {
        let _ = tokio::process::Command::new("breadcrumbs")
            .args(["join", &ssid])
            .output()
            .await;
    });
}

/// Fire-and-forget: save a new network with its password, then join it.
pub fn spawn_add_and_join(ssid: String, password: String) {
    relm4::spawn(async move {
        let added = tokio::process::Command::new("breadcrumbs")
            .args(["add", &ssid, &password])
            .output()
            .await;
        if matches!(added, Ok(o) if o.status.success()) {
            let _ = tokio::process::Command::new("breadcrumbs")
                .args(["join", &ssid])
                .output()
                .await;
        }
    });
}


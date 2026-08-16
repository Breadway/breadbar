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
    /// False while nmcli is still listing APs — profiles must still be usable.
    pub scan_ready: bool,
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

async fn saved_ssids() -> std::collections::HashSet<String> {
    let out = tokio::process::Command::new("nmcli")
        .args(["-t", "-f", "NAME,TYPE", "connection", "show"])
        .output()
        .await;
    let Ok(o) = out else {
        return std::collections::HashSet::new();
    };
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|line| {
            let (name, ty) = line.rsplit_once(':')?;
            if ty == "802-11-wireless" || ty == "wifi" {
                Some(name.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Cached AP list (no rescan). Fast enough to paint next to profiles.
async fn fetch_scan() -> Vec<ScanEntry> {
    let out = tokio::time::timeout(
        Duration::from_secs(4),
        tokio::process::Command::new("nmcli")
            .args(["-t", "-f", "SSID,SIGNAL,IN-USE", "device", "wifi", "list"])
            .output(),
    )
    .await;
    let Ok(Ok(o)) = out else {
        return vec![];
    };
    let saved = saved_ssids().await;
    let mut seen = std::collections::HashSet::new();
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.rsplitn(3, ':');
            let _in_use = parts.next()?;
            let signal = parts.next()?.parse::<u8>().ok().unwrap_or(0);
            let ssid = parts.next()?.replace("\\:", ":");
            if ssid.is_empty() || ssid == "--" || !seen.insert(ssid.clone()) {
                return None;
            }
            let saved = saved.contains(&ssid);
            Some(ScanEntry {
                ssid,
                signal,
                saved,
            })
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

/// Profiles first (so you can switch Home/Away immediately), then the
/// cached AP list. A background rescan refreshes the list if it finds more.
pub fn spawn_popover_load(sender: ComponentSender<App>) {
    relm4::spawn(async move {
        let profiles = fetch_profile_list().await;
        sender.input(AppInput::WifiPopoverData(WifiPopoverData {
            profiles: profiles.clone(),
            scan: vec![],
            scan_ready: false,
        }));
        let scan = fetch_scan().await;
        sender.input(AppInput::WifiPopoverData(WifiPopoverData {
            profiles: profiles.clone(),
            scan: scan.clone(),
            scan_ready: true,
        }));
        let _ = tokio::process::Command::new("nmcli")
            .args(["device", "wifi", "rescan"])
            .output()
            .await;
        let scan = fetch_scan().await;
        sender.input(AppInput::WifiPopoverData(WifiPopoverData {
            profiles,
            scan,
            scan_ready: true,
        }));
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

/// Fire-and-forget: connect to a known SSID via NetworkManager.
pub fn spawn_join(ssid: String) {
    relm4::spawn(async move {
        let _ = tokio::process::Command::new("nmcli")
            .args(["device", "wifi", "connect", &ssid])
            .output()
            .await;
    });
}

/// Save in breadcrumbs (if the CLI still accepts `add`) and connect with nmcli.
pub fn spawn_add_and_join(ssid: String, password: String) {
    relm4::spawn(async move {
        let _ = tokio::process::Command::new("breadcrumbs")
            .args(["add", &ssid, &password])
            .output()
            .await;
        let _ = tokio::process::Command::new("nmcli")
            .args(["device", "wifi", "connect", &ssid, "password", &password])
            .output()
            .await;
    });
}


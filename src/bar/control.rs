use crate::{App, AppInput};
use relm4::ComponentSender;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct AudioSink {
    pub name: String,
    pub description: String,
    pub is_default: bool,
}

#[derive(Debug, Clone)]
pub struct ControlPanelData {
    pub volume: f64,
    pub brightness: f64,
    pub sinks: Vec<AudioSink>,
}

async fn fetch_volume() -> f64 {
    let out = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new("wpctl")
            .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
            .output(),
    )
    .await;
    match out {
        Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .trim()
            .strip_prefix("Volume:")
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.5)
            .clamp(0.0, 1.5),
        _ => 0.5,
    }
}

async fn fetch_brightness() -> f64 {
    let cur = tokio::process::Command::new("brightnessctl")
        .arg("get")
        .output()
        .await
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok())
        .unwrap_or(0.0);
    let max = tokio::process::Command::new("brightnessctl")
        .arg("max")
        .output()
        .await
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok())
        .unwrap_or(255.0);
    if max == 0.0 {
        0.5
    } else {
        (cur / max).clamp(0.0, 1.0)
    }
}

async fn fetch_sinks() -> Vec<AudioSink> {
    let default = tokio::process::Command::new("pactl")
        .args(["info"])
        .output()
        .await
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .find(|l| l.starts_with("Default Sink:"))
                .map(|l| l.trim_start_matches("Default Sink:").trim().to_string())
        })
        .unwrap_or_default();

    let out = tokio::process::Command::new("pactl")
        .args(["-f", "json", "list", "sinks"])
        .output()
        .await;

    let Ok(o) = out else { return vec![] };
    let arr: Vec<serde_json::Value> = serde_json::from_slice(&o.stdout).unwrap_or_default();
    arr.into_iter()
        .filter_map(|v| {
            let name = v["name"].as_str()?.to_string();
            let description = v["description"].as_str().unwrap_or(&name).to_string();
            let is_default = name == default;
            Some(AudioSink { name, description, is_default })
        })
        .collect()
}

pub fn spawn_load(sender: ComponentSender<App>) {
    relm4::spawn(async move {
        let (volume, brightness, sinks) =
            tokio::join!(fetch_volume(), fetch_brightness(), fetch_sinks());
        sender.input(AppInput::ControlPanelData(ControlPanelData {
            volume,
            brightness,
            sinks,
        }));
    });
}

pub fn spawn_set_volume(v: f64) {
    relm4::spawn(async move {
        let pct = format!("{:.0}%", (v * 100.0).clamp(0.0, 150.0));
        let _ = tokio::process::Command::new("wpctl")
            .args(["set-volume", "@DEFAULT_AUDIO_SINK@", &pct])
            .output()
            .await;
    });
}

pub fn spawn_set_brightness(v: f64) {
    relm4::spawn(async move {
        let pct = format!("{:.0}%", (v * 100.0).clamp(1.0, 100.0));
        let _ = tokio::process::Command::new("brightnessctl")
            .args(["set", &pct])
            .output()
            .await;
    });
}

pub fn spawn_set_sink(name: String, sender: ComponentSender<App>) {
    relm4::spawn(async move {
        let _ = tokio::process::Command::new("pactl")
            .args(["set-default-sink", &name])
            .output()
            .await;
        // Default sink alone leaves already-playing streams on the old
        // device — move them too so the switch is audible immediately.
        if let Ok(o) = tokio::process::Command::new("pactl")
            .args(["list", "short", "sink-inputs"])
            .output()
            .await
        {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                let Some(id) = line.split_whitespace().next() else {
                    continue;
                };
                let _ = tokio::process::Command::new("pactl")
                    .args(["move-sink-input", id, &name])
                    .output()
                    .await;
            }
        }
        spawn_load(sender);
    });
}

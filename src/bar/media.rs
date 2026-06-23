use crate::{App, AppInput};
use relm4::ComponentSender;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct MediaState {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub has_player: bool,
}

async fn fetch() -> MediaState {
    let none = || MediaState {
        title: String::new(),
        artist: String::new(),
        playing: false,
        has_player: false,
    };

    let status_out = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new("playerctl")
            .args(["status"])
            .output(),
    )
    .await;

    let status = match status_out {
        Ok(Ok(out)) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        _ => return none(),
    };

    if status == "Stopped" {
        return none();
    }

    let playing = status == "Playing";

    let meta_out = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new("playerctl")
            .args(["metadata", "--format", "{{artist}}\t{{title}}"])
            .output(),
    )
    .await;

    let (artist, title) = match meta_out {
        Ok(Ok(out)) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let mut parts = s.splitn(2, '\t');
            let a = parts.next().unwrap_or("").to_string();
            let t = parts.next().unwrap_or("").to_string();
            (a, t)
        }
        _ => (String::new(), String::new()),
    };

    MediaState {
        title,
        artist,
        playing,
        has_player: true,
    }
}

pub fn spawn_poller(sender: ComponentSender<App>) {
    relm4::spawn(async move {
        loop {
            sender.input(AppInput::MediaUpdate(fetch().await));
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

pub fn spawn_cmd(cmd: &'static str) {
    relm4::spawn(async move {
        let _ = tokio::process::Command::new("playerctl")
            .arg(cmd)
            .output()
            .await;
    });
}

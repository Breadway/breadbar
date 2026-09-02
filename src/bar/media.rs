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

    let status = match super::proc::stdout_ok("playerctl", &["status"], Duration::from_secs(2)).await
    {
        Some(stdout) => String::from_utf8_lossy(&stdout).trim().to_string(),
        None => return none(),
    };

    if status == "Stopped" {
        return none();
    }

    let playing = status == "Playing";

    let (artist, title) = match super::proc::stdout_ok(
        "playerctl",
        &["metadata", "--format", "{{artist}}\t{{title}}"],
        Duration::from_secs(2),
    )
    .await
    {
        Some(stdout) => {
            let s = String::from_utf8_lossy(&stdout).trim().to_string();
            let mut parts = s.splitn(2, '\t');
            let a = parts.next().unwrap_or("").to_string();
            let t = parts.next().unwrap_or("").to_string();
            (a, t)
        }
        None => (String::new(), String::new()),
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

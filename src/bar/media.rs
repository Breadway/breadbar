use crate::{App, AppInput};
use relm4::ComponentSender;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct MediaState {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub has_player: bool,
    /// The MPRIS player's display name ("Spotify"), for the stage's
    /// "Artist · Player" line.
    pub player: String,
    /// Local cover art (`mpris:artUrl` when it is a `file://` URL). Remote
    /// art is not fetched; the stage falls back to its placeholder tile.
    pub art: Option<std::path::PathBuf>,
}

/// Title-cases a player bus name for display: `spotify` -> "Spotify",
/// `firefox.instance_1_23` -> "Firefox".
pub fn player_display_name(raw: &str) -> String {
    let base = raw.split('.').next().unwrap_or(raw);
    let mut chars = base.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The local path of a `file://` art URL, percent-decoding the common
/// escapes; `None` for anything remote.
pub fn local_art_path(url: &str) -> Option<std::path::PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let mut out = Vec::with_capacity(rest.len());
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(std::path::PathBuf::from(
        String::from_utf8_lossy(&out).into_owned(),
    ))
}

async fn fetch() -> MediaState {
    let none = || MediaState {
        title: String::new(),
        artist: String::new(),
        playing: false,
        has_player: false,
        player: String::new(),
        art: None,
    };

    let status =
        match super::proc::stdout_ok("playerctl", &["status"], Duration::from_secs(2)).await {
            Some(stdout) => String::from_utf8_lossy(&stdout).trim().to_string(),
            None => return none(),
        };

    if status == "Stopped" {
        return none();
    }

    let playing = status == "Playing";

    let (artist, title, player, art) = match super::proc::stdout_ok(
        "playerctl",
        &[
            "metadata",
            "--format",
            "{{artist}}\t{{title}}\t{{playerName}}\t{{mpris:artUrl}}",
        ],
        Duration::from_secs(2),
    )
    .await
    {
        Some(stdout) => {
            let s = String::from_utf8_lossy(&stdout).trim_end().to_string();
            let mut parts = s.splitn(4, '\t');
            let a = parts.next().unwrap_or("").to_string();
            let t = parts.next().unwrap_or("").to_string();
            let p = player_display_name(parts.next().unwrap_or(""));
            let art = parts.next().and_then(local_art_path);
            (a, t, p, art)
        }
        None => (String::new(), String::new(), String::new(), None),
    };

    MediaState {
        title,
        artist,
        playing,
        has_player: true,
        player,
        art,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_names_are_title_cased_without_instance_suffix() {
        assert_eq!(player_display_name("spotify"), "Spotify");
        assert_eq!(player_display_name("firefox.instance_1_23"), "Firefox");
        assert_eq!(player_display_name(""), "");
    }

    #[test]
    fn only_local_art_urls_resolve() {
        assert_eq!(
            local_art_path("file:///tmp/cover%20art.png"),
            Some(std::path::PathBuf::from("/tmp/cover art.png"))
        );
        assert_eq!(local_art_path("https://i.scdn.co/image/x"), None);
    }
}

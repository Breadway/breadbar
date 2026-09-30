//! The spotlight stage's settings menu: per-user overrides for pill
//! auto-hide, widgets, the workspace view, the now-playing title and the
//! stage's System card, stored in
//! `$XDG_CONFIG_HOME/breadbar/spotlight.json`. Each field left unset falls
//! back to the active theme. Every bar instance watches the file, so a toggle
//! made on one monitor's stage applies on all of them.

use bread_theme::shell::WorkspaceDisplay;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Tuck the pill above the screen edge until needed.
    pub autohide: Option<bool>,
    /// Show Lua widgets in the pill and as cards on the stage.
    pub widgets: Option<bool>,
    /// Which workspaces the pill shows.
    pub workspaces: Option<WorkspacesPref>,
    /// Show the playing track's title in the pill.
    pub nowplaying: Option<bool>,
    /// Show the System card (CPU, RAM, GPU, power draw, network) on the
    /// stage.
    pub system: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspacesPref {
    None,
    Current,
    All,
}

impl From<WorkspacesPref> for WorkspaceDisplay {
    fn from(p: WorkspacesPref) -> Self {
        match p {
            WorkspacesPref::None => WorkspaceDisplay::None,
            WorkspacesPref::Current => WorkspaceDisplay::Current,
            WorkspacesPref::All => WorkspaceDisplay::All,
        }
    }
}

impl From<WorkspaceDisplay> for WorkspacesPref {
    fn from(d: WorkspaceDisplay) -> Self {
        match d {
            WorkspaceDisplay::None => WorkspacesPref::None,
            WorkspaceDisplay::Current => WorkspacesPref::Current,
            WorkspaceDisplay::All => WorkspacesPref::All,
        }
    }
}

impl Prefs {
    pub fn autohide_on(&self) -> bool {
        self.autohide.unwrap_or(true)
    }

    pub fn widgets_on(&self) -> bool {
        self.widgets.unwrap_or(true)
    }

    pub fn nowplaying_on(&self) -> bool {
        self.nowplaying.unwrap_or(true)
    }

    pub fn system_on(&self) -> bool {
        self.system.unwrap_or(true)
    }

    pub fn workspaces_or(&self, theme_default: WorkspaceDisplay) -> WorkspaceDisplay {
        self.workspaces.map(Into::into).unwrap_or(theme_default)
    }
}

/// Parses the file's contents. Anything unreadable yields the defaults, so a
/// hand-edited typo can never take the bar down.
pub fn parse(text: &str) -> Prefs {
    serde_json::from_str(text).unwrap_or_default()
}

pub fn path() -> Option<PathBuf> {
    let base = match std::env::var("XDG_CONFIG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("breadbar").join("spotlight.json"))
}

pub fn load() -> Prefs {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| parse(&t))
        .unwrap_or_default()
}

/// Atomic write (tmp + rename), so a watcher never reads a half-written file.
pub fn save(prefs: &Prefs) -> std::io::Result<()> {
    let path = path().ok_or_else(|| std::io::Error::other("no config directory"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(prefs).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json + "\n")?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_broken_file_means_theme_defaults() {
        for text in ["", "{}", "not json", "{\"autohide\": \"yes\"}"] {
            let p = parse(text);
            assert!(p.autohide_on(), "{text}");
            assert!(p.widgets_on(), "{text}");
            assert!(p.nowplaying_on(), "{text}");
            assert!(p.system_on(), "{text}");
            assert_eq!(
                p.workspaces_or(WorkspaceDisplay::Current),
                WorkspaceDisplay::Current
            );
        }
    }

    #[test]
    fn set_fields_override_the_theme() {
        let p = parse(r#"{"autohide": false, "widgets": false, "workspaces": "none"}"#);
        assert!(!p.autohide_on());
        assert!(!p.widgets_on());
        assert_eq!(
            p.workspaces_or(WorkspaceDisplay::All),
            WorkspaceDisplay::None
        );
    }

    #[test]
    fn round_trips_through_json() {
        let p = Prefs {
            autohide: Some(false),
            widgets: None,
            workspaces: Some(WorkspacesPref::Current),
            nowplaying: Some(false),
            system: None,
        };
        assert_eq!(parse(&serde_json::to_string(&p).unwrap()), p);
    }
}

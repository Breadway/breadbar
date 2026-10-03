//! Notification policy shared with BOS Settings through the bar's D-Bus API.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppPolicy {
    #[default]
    All,
    Banners,
    History,
    Off,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub dnd: bool,
    pub schedule: bool,
    pub start: String,
    pub end: String,
    pub lock_screen: bool,
    pub sound: bool,
    pub apps: BTreeMap<String, AppPolicy>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            dnd: false,
            schedule: false,
            start: "22:00".into(),
            end: "07:00".into(),
            lock_screen: false,
            sound: true,
            apps: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }
}
fn path() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("breadbar/notifications.json")
}
pub fn load() -> Preferences {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
pub fn minute(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    let h = h.parse::<u32>().ok()?;
    let m = m.parse::<u32>().ok()?;
    (h < 24 && m < 60).then_some(h * 60 + m)
}
pub fn effective_dnd(p: &Preferences, now: u32) -> bool {
    if p.dnd {
        return true;
    }
    if !p.schedule {
        return false;
    }
    let (Some(start), Some(end)) = (minute(&p.start), minute(&p.end)) else {
        return false;
    };
    if start == end {
        return false;
    }
    if start < end {
        now >= start && now < end
    } else {
        now >= start || now < end
    }
}
pub fn clock_minute() -> u32 {
    gtk4::glib::DateTime::now_local()
        .ok()
        .map(|d| d.hour() as u32 * 60 + d.minute() as u32)
        .unwrap_or(0)
}
pub fn save(p: &Preferences) -> Result<(), String> {
    if minute(&p.start).is_none()
        || minute(&p.end).is_none()
        || p.apps.len() > 500
        || p.apps
            .keys()
            .any(|k| k.len() > 256 || k.contains(['\n', '\0']))
    {
        return Err("Invalid notification settings".into());
    }
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    bread_utils::atomic::write_atomic_backed_up(
        &path,
        &serde_json::to_string_pretty(p).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
/// Read the typed settings shared with BOS Settings. Unknown values cannot enter CSS.
pub fn text_css() -> String {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    let shared_path = dir.join("hypr/bos-settings-v2.json");
    // A later bar-specific edit keeps its established precedence over global text preferences.
    if let (Ok(shared), Ok(bar)) = (
        std::fs::metadata(&shared_path).and_then(|m| m.modified()),
        std::fs::metadata(dir.join("breadbar/style.css")).and_then(|m| m.modified()),
    ) {
        if bar > shared {
            return String::new();
        }
    }
    let Some(value) = std::fs::read_to_string(dir.join("hypr/bos-settings-v2.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    else {
        return String::new();
    };
    let size = value
        .get("text_size")
        .and_then(|v| v.as_u64())
        .filter(|s| [12, 14, 16, 18].contains(s))
        .unwrap_or(14);
    let font = value
        .get("font_family")
        .and_then(|v| v.as_str())
        .filter(|s| ["Varela Round", "Inter", "Noto Sans"].contains(s))
        .unwrap_or("Varela Round");
    format!("* {{font-family: '{font}'; font-size: {size}px;}}")
}

pub struct Presentation {
    pub retain: bool,
    pub banner: bool,
    pub sound: bool,
}
pub fn presentation(
    p: &Preferences,
    app: &str,
    critical: bool,
    now: u32,
    locked: bool,
) -> Presentation {
    let policy = p.apps.get(app).cloned().unwrap_or_default();
    let retain = policy != AppPolicy::Off;
    let banner = retain
        && policy != AppPolicy::History
        && (!effective_dnd(p, now) || critical)
        && (!locked || p.lock_screen);
    Presentation {
        retain,
        banner,
        sound: banner && p.sound && policy == AppPolicy::All,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn per_app_policy_dnd_and_locked_delivery() {
        let mut p = Preferences::default();
        assert!(presentation(&p, "App", false, 0, false).sound);
        p.apps.insert("App".into(), AppPolicy::Banners);
        let d = presentation(&p, "App", false, 0, false);
        assert!(d.banner);
        assert!(!d.sound);
        p.apps.insert("App".into(), AppPolicy::History);
        let d = presentation(&p, "App", true, 0, false);
        assert!(d.retain);
        assert!(!d.banner);
        p.apps.insert("App".into(), AppPolicy::Off);
        assert!(!presentation(&p, "App", true, 0, false).retain);
        p.apps.clear();
        p.dnd = true;
        assert!(!presentation(&p, "App", false, 0, false).banner);
        assert!(presentation(&p, "App", true, 0, false).banner);
        assert!(!presentation(&p, "App", true, 0, true).banner);
        p.lock_screen = true;
        assert!(presentation(&p, "App", true, 0, true).banner);
    }
    #[test]
    fn overnight_and_daytime_schedules() {
        let mut p = Preferences {
            schedule: true,
            ..Default::default()
        };
        assert!(effective_dnd(&p, 23 * 60));
        assert!(effective_dnd(&p, 6 * 60));
        assert!(!effective_dnd(&p, 7 * 60));
        p.start = "09:00".into();
        p.end = "17:00".into();
        assert!(effective_dnd(&p, 12 * 60));
        assert!(!effective_dnd(&p, 18 * 60));
    }
    #[test]
    fn invalid_time_and_manual_override() {
        assert_eq!(minute("24:00"), None);
        assert_eq!(minute("07:60"), None);
        assert_eq!(minute("7:00"), None);
        assert!(effective_dnd(
            &Preferences {
                dnd: true,
                ..Default::default()
            },
            0
        ));
    }
}

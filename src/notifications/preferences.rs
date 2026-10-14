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
            start: DEFAULT_START.into(),
            end: DEFAULT_END.into(),
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
/// Fallback quiet-hours interval used when the schedule is on but a stored
/// time no longer parses. `save` rejects invalid times, but a file edited by
/// hand (or written by an older build) can still hold one; silently
/// switching the schedule off would be the worse failure, so keep the
/// schedule on the compiled default and log the fallback.
pub const DEFAULT_START: &str = "22:00";
pub const DEFAULT_END: &str = "07:00";
fn interval(p: &Preferences) -> (u32, u32) {
    if let (Some(start), Some(end)) = (minute(&p.start), minute(&p.end)) {
        return (start, end);
    }
    eprintln!(
        "breadbar: invalid notification schedule {:?}-{:?}; falling back to {DEFAULT_START}-{DEFAULT_END}",
        p.start, p.end
    );
    // The defaults are compile-time constants in valid HH:MM shape, so this
    // always parses; the fallback keeps the schedule on instead of silently off.
    (
        minute(DEFAULT_START).unwrap_or(22 * 60),
        minute(DEFAULT_END).unwrap_or(7 * 60),
    )
}
pub fn effective_dnd(p: &Preferences, now: u32) -> bool {
    if p.dnd {
        return true;
    }
    if !p.schedule {
        return false;
    }
    let (start, end) = interval(p);
    // An empty interval (start == end) means off: there is no instant that
    // satisfies it either way round, so treat it as disabled rather than
    // all-day.
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
/// Where a notification goes. This is the single source of truth for both
/// suppression paths, and the live `notify()` path must agree with it (see
/// the agreement test below):
/// - DND (manual or scheduled) suppresses banners for ordinary
///   notifications only; critical alerts still banner.
/// - A locked session suppresses banners for ordinary notifications unless
///   lock-screen preview is on; critical alerts keep history plus banner
///   either way, since they are the one thing a locked screen must show.
/// - Per-app policy still wins over urgency: `History` keeps history without
///   a banner and `Off` drops the notification entirely, even for critical
///   alerts.
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
        && (!locked || p.lock_screen || critical);
    Presentation {
        retain,
        banner,
        sound: banner && p.sound && policy == AppPolicy::All,
    }
}
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run `test` with XDG config/state pointed at a fresh temp dir (and no
/// session id, so live lock detection would report unlocked), holding the
/// shared env lock and restoring every variable afterwards. All tests that
/// touch process environment must go through here; the lock serializes them
/// across modules.
#[cfg(test)]
pub(crate) fn with_isolated_home(name: &str, test: impl FnOnce(&std::path::Path)) {
    let _env = ENV_LOCK.lock().unwrap();
    let prev_config = std::env::var_os("XDG_CONFIG_HOME");
    let prev_state = std::env::var_os("XDG_STATE_HOME");
    let prev_session = std::env::var_os("XDG_SESSION_ID");
    let dir = std::env::temp_dir().join(format!(
        "breadbar-prefs-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
    std::env::set_var("XDG_STATE_HOME", dir.join("state"));
    std::env::remove_var("XDG_SESSION_ID");
    test(&dir);
    match prev_config {
        Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
        None => std::env::remove_var("XDG_CONFIG_HOME"),
    }
    match prev_state {
        Some(v) => std::env::set_var("XDG_STATE_HOME", v),
        None => std::env::remove_var("XDG_STATE_HOME"),
    }
    match prev_session {
        Some(v) => std::env::set_var("XDG_SESSION_ID", v),
        None => std::env::remove_var("XDG_SESSION_ID"),
    }
    let _ = std::fs::remove_dir_all(&dir);
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
        // A locked session without lock-screen preview suppresses ordinary
        // banners; critical alerts keep history plus banner.
        p.dnd = false;
        assert!(!presentation(&p, "App", false, 0, true).banner);
        assert!(presentation(&p, "App", true, 0, true).banner);
        p.lock_screen = true;
        assert!(presentation(&p, "App", false, 0, true).banner);
        assert!(presentation(&p, "App", true, 0, true).banner);
    }
    /// Pins the contract the live `notify()` path must honor: a critical
    /// alert under manual DND on a locked session still gets history plus a
    /// banner, with or without lock-screen preview. If this predicate and
    /// `notify()` ever disagree, one of this test or its `notify()` twin
    /// fails.
    #[test]
    fn critical_under_dnd_and_lock_keeps_history_plus_banner() {
        for lock_screen in [false, true] {
            let p = Preferences {
                dnd: true,
                lock_screen,
                ..Default::default()
            };
            let d = presentation(&p, "App", true, 12 * 60, true);
            assert!(
                d.retain,
                "critical must keep history (lock_screen={lock_screen})"
            );
            assert!(d.banner, "critical must banner (lock_screen={lock_screen})");
            assert!(
                !presentation(&p, "App", false, 12 * 60, true).banner,
                "DND still suppresses ordinary banners (lock_screen={lock_screen})"
            );
            // Without DND the lock path alone decides: preview on means banner.
            let q = Preferences {
                lock_screen,
                ..Default::default()
            };
            let d = presentation(&q, "App", false, 12 * 60, true);
            assert!(
                d.retain,
                "ordinary keeps history (lock_screen={lock_screen})"
            );
            assert_eq!(
                d.banner, lock_screen,
                "ordinary banners on a locked session only with preview (lock_screen={lock_screen})"
            );
        }
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
    #[test]
    fn invalid_stored_schedule_falls_back_to_default_instead_of_off() {
        let p = Preferences {
            schedule: true,
            start: "xx".into(),
            end: "yy".into(),
            ..Default::default()
        };
        assert!(effective_dnd(&p, 23 * 60));
        assert!(effective_dnd(&p, 6 * 60));
        assert!(!effective_dnd(&p, 12 * 60));
        // Manual DND still wins outright, and a disabled schedule stays off.
        assert!(effective_dnd(
            &Preferences {
                dnd: true,
                ..p.clone()
            },
            12 * 60
        ));
        assert!(!effective_dnd(
            &Preferences {
                schedule: false,
                ..p.clone()
            },
            23 * 60
        ));
    }
    #[test]
    fn equal_start_and_end_is_off() {
        let p = Preferences {
            schedule: true,
            start: "09:00".into(),
            end: "09:00".into(),
            ..Default::default()
        };
        assert!(!effective_dnd(&p, 9 * 60));
        assert!(!effective_dnd(&p, 12 * 60));
    }
}

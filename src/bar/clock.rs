use crate::{App, AppInput};
use relm4::ComponentSender;

pub fn now() -> gtk4::glib::DateTime {
    gtk4::glib::DateTime::now_local().expect("local time")
}

pub fn time() -> String {
    let dt = now();
    if clock_24h() == Some(false) {
        format!(
            "{:02}:{:02}",
            if dt.hour() % 12 == 0 {
                12
            } else {
                dt.hour() % 12
            },
            dt.minute()
        )
    } else {
        format!("{:02}:{:02}", dt.hour(), dt.minute())
    }
}

pub fn date() -> String {
    now().format("%a %d/%m").expect("date format").to_string()
}

pub fn current() -> String {
    format!("{}  {}", date(), time())
}

/// `modules.clock.format` rendered against GLib's own `DateTime::format`
/// (a strftime subset: `%H`, `%M`, `%a`, `%d`, `%m`, ... all work). Falls
/// back to [`time`]'s hardcoded "HH:MM" on a malformed format string rather
/// than propagating an error; a broken theme's clock format must degrade,
/// not crash the bar, same as every other "malformed theme" fallback in
/// this system.
///
/// The `clock_24h` setting from `breadbar/clock.json` layers over the theme
/// format instead of replacing it: only the hour fields change, so a theme
/// format like `"%a %d/%m %H:%M"` keeps its date half under either setting.
pub fn formatted(format: &str) -> String {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    formatted_in(format, &base.join("breadbar/clock.json"))
}

/// [`formatted`] against an explicit clock.json path, so tests can drive the
/// layering without touching process environment.
pub(crate) fn formatted_in(format: &str, clock_json: &std::path::Path) -> String {
    let layered = apply_hour_preference(format, clock_24h_in(clock_json));
    now()
        .format(&layered)
        .map(|s| s.to_string())
        .unwrap_or_else(|_| time())
}

/// Layer the 12/24-hour preference over a theme clock format: `None` (no
/// clock.json) leaves the theme format untouched; `Some(true)` swaps
/// 12-hour fields for 24-hour ones and drops the meridiem; `Some(false)`
/// swaps the other way and adds a meridiem when the theme has none, since a
/// bare 12-hour time is ambiguous.
pub(crate) fn apply_hour_preference(format: &str, clock_24h: Option<bool>) -> String {
    match clock_24h {
        None => format.to_string(),
        Some(true) => {
            let out = format.replace("%I", "%H").replace("%l", "%H");
            let out = out.replace(" %p", "").replace(" %P", "");
            let out = out.replace("%p", "").replace("%P", "");
            out.split_whitespace().collect::<Vec<_>>().join(" ")
        }
        Some(false) => {
            let mut out = format.replace("%H", "%I").replace("%k", "%l");
            if !out.contains("%p") && !out.contains("%P") {
                out.push_str(" %p");
            }
            out
        }
    }
}

pub fn spawn_ticker(sender: ComponentSender<App>) {
    relm4::spawn(async move {
        loop {
            sender.input(AppInput::ClockTick);
            // Sleep until the top of the next minute; display is HH:MM only.
            let secs = gtk4::glib::DateTime::now_local().map_or(0, |dt| dt.second());
            let wait = (60 - secs.rem_euclid(60)) as u64;
            tokio::time::sleep(std::time::Duration::from_secs(wait.max(1))).await;
        }
    });
}

fn clock_24h() -> Option<bool> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    clock_24h_in(&base.join("breadbar/clock.json"))
}

fn clock_24h_in(path: &std::path::Path) -> Option<bool> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()?
        .get("clock_24h")?
        .as_bool()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hour_preference_layers_over_theme_format() {
        // No preference: the theme format passes through untouched.
        assert_eq!(
            apply_hour_preference("%a %d/%m %H:%M", None),
            "%a %d/%m %H:%M"
        );
        // 24-hour: 12-hour fields become 24-hour, meridiem is dropped.
        assert_eq!(apply_hour_preference("%I:%M %p", Some(true)), "%H:%M");
        assert_eq!(
            apply_hour_preference("%a %d/%m %I:%M %p", Some(true)),
            "%a %d/%m %H:%M"
        );
        // 12-hour: 24-hour fields become 12-hour, meridiem is added.
        assert_eq!(apply_hour_preference("%H:%M", Some(false)), "%I:%M %p");
        assert_eq!(
            apply_hour_preference("%a %d/%m %H:%M", Some(false)),
            "%a %d/%m %I:%M %p"
        );
        // A theme that already carries a meridiem keeps it (no doubling).
        assert_eq!(apply_hour_preference("%I:%M %p", Some(false)), "%I:%M %p");
    }

    #[test]
    fn formatted_keeps_theme_format_under_clock_json() {
        let dir = std::env::temp_dir().join(format!(
            "breadbar-clock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let clock_json = dir.join("clock.json");
        // 12-hour preference: the date half of the theme format survives,
        // and the time renders 12-hour with a meridiem.
        std::fs::write(&clock_json, r#"{"clock_24h": false}"#).unwrap();
        let rendered = formatted_in("%a %d/%m %H:%M", &clock_json);
        let expected_date = now().format("%a %d/%m").expect("date format").to_string();
        assert!(
            rendered.starts_with(&expected_date),
            "theme date half lost: {rendered:?}"
        );
        assert!(
            rendered.ends_with("AM") || rendered.ends_with("PM"),
            "12-hour rendering needs a meridiem: {rendered:?}"
        );
        // 24-hour preference: same theme, no meridiem.
        std::fs::write(&clock_json, r#"{"clock_24h": true}"#).unwrap();
        let rendered = formatted_in("%a %d/%m %H:%M", &clock_json);
        assert!(
            rendered.starts_with(&expected_date),
            "theme date half lost: {rendered:?}"
        );
        assert!(
            !rendered.ends_with("AM") && !rendered.ends_with("PM"),
            "24-hour rendering must not carry a meridiem: {rendered:?}"
        );
        // Missing clock.json: theme format untouched.
        std::fs::remove_file(&clock_json).unwrap();
        assert_eq!(
            formatted_in("%H:%M", &clock_json),
            now().format("%H:%M").expect("clock format").to_string()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

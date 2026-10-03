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
pub fn formatted(format: &str) -> String {
    let preset = clock_24h().map(|v| if v { "%H:%M" } else { "%I:%M %p" });
    now()
        .format(preset.unwrap_or(format))
        .map(|s| s.to_string())
        .unwrap_or_else(|_| time())
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
    let text = std::fs::read_to_string(base.join("breadbar/clock.json")).ok()?;
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()?
        .get("clock_24h")?
        .as_bool()
}

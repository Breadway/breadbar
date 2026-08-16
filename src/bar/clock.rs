use crate::{App, AppInput};
use relm4::ComponentSender;

pub fn now() -> gtk4::glib::DateTime {
    gtk4::glib::DateTime::now_local().expect("local time")
}

pub fn time() -> String {
    let dt = now();
    format!("{:02}:{:02}", dt.hour(), dt.minute())
}

pub fn date() -> String {
    now().format("%a %d/%m").expect("date format").to_string()
}

pub fn current() -> String {
    format!("{}  {}", date(), time())
}

pub fn spawn_ticker(sender: ComponentSender<App>) {
    relm4::spawn(async move {
        loop {
            sender.input(AppInput::ClockTick);
            // Sleep until the top of the next minute — display is HH:MM only.
            let secs = gtk4::glib::DateTime::now_local().map_or(0, |dt| dt.second());
            let wait = (60 - secs.rem_euclid(60)) as u64;
            tokio::time::sleep(std::time::Duration::from_secs(wait.max(1))).await;
        }
    });
}

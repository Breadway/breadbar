use crate::{App, AppInput};
use relm4::ComponentSender;

pub fn current() -> String {
    let dt = gtk4::glib::DateTime::now_local().expect("local time");
    format!("{:02}:{:02}", dt.hour(), dt.minute())
}

pub fn spawn_ticker(sender: ComponentSender<App>) {
    relm4::spawn(async move {
        loop {
            sender.input(AppInput::ClockTick);
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
}

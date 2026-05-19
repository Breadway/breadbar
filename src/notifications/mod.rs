pub mod popup;

use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::mpsc;
use zbus::zvariant::OwnedValue;

pub enum NotifEvent {
    Show {
        id: u32,
        app_name: String,
        summary: String,
        body: String,
        timeout_ms: u32,
    },
    Close(u32),
}

struct NotifServer {
    tx: mpsc::Sender<NotifEvent>,
    next_id: AtomicU32,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl NotifServer {
    // The org.freedesktop.Notifications spec mandates exactly these 8 parameters.
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        _app_icon: &str,
        summary: &str,
        body: &str,
        _actions: Vec<String>,
        _hints: std::collections::HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            self.next_id.fetch_add(1, Ordering::Relaxed)
        };
        let timeout_ms = if expire_timeout <= 0 {
            5000
        } else {
            expire_timeout as u32
        };
        let _ = self
            .tx
            .send(NotifEvent::Show {
                id,
                app_name: app_name.to_string(),
                summary: summary.to_string(),
                body: body.to_string(),
                timeout_ms,
            })
            .await;
        id
    }

    async fn close_notification(&self, id: u32) {
        let _ = self.tx.send(NotifEvent::Close(id)).await;
    }

    fn get_capabilities(&self) -> Vec<String> {
        vec!["body".to_string()]
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "breadbar".into(),
            "breadway".into(),
            "0.1.0".into(),
            "1.2".into(),
        )
    }
}

pub fn spawn() {
    let (tx, rx) = mpsc::channel(32);

    relm4::spawn(async move {
        let server = NotifServer {
            tx,
            next_id: AtomicU32::new(1),
        };
        // Builder failures here would only occur with invalid static strings — safe to unwrap.
        let _conn = zbus::connection::Builder::session()
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .serve_at("/org/freedesktop/Notifications", server)
            .unwrap()
            .build()
            .await
            .expect("failed to claim org.freedesktop.Notifications on D-Bus session bus");
        std::future::pending::<()>().await
    });

    relm4::spawn_local(popup::run(rx));
}

use futures_lite::StreamExt;
use gtk4::prelude::*;
use hyprland::{
    data::{Workspace, Workspaces},
    event_listener::{Event, EventStream},
    prelude::*,
    shared::WorkspaceId,
};
use relm4::ComponentSender;

use crate::AppInput;

/// Fetches the current workspace list + active workspace and pushes both to
/// the app — used both for the initial state and to re-sync after the event
/// stream reconnects (state may have changed while we were disconnected).
async fn sync_state(sender: &ComponentSender<crate::App>) {
    if let Ok(ws) = Workspaces::get_async().await {
        sender.input(AppInput::WorkspaceList(ws.to_vec()));
    }
    if let Ok(active) = Workspace::get_active_async().await {
        sender.input(AppInput::ActiveWorkspace(active.id));
    }
}

pub fn spawn_watcher(sender: ComponentSender<crate::App>) {
    relm4::spawn(async move {
        sync_state(&sender).await;

        // Hyprland's IPC event socket can drop out from under us — a
        // Hyprland restart/reload, or just a transient hiccup — at which
        // point `stream.next()` yields `None` (or an `Err`, also excluded
        // by this `while let Some(Ok(..))` pattern). That used to just fall
        // through and end this whole task permanently, freezing every
        // workspace button for the rest of the bar's life. Reconnect with a
        // capped exponential backoff instead of giving up.
        let mut backoff = std::time::Duration::from_millis(500);
        const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);

        loop {
            let mut stream = EventStream::new();
            while let Some(Ok(event)) = stream.next().await {
                backoff = std::time::Duration::from_millis(500);
                match event {
                    Event::WorkspaceChanged(data) => {
                        sender.input(AppInput::ActiveWorkspace(data.id));
                    }
                    Event::WorkspaceAdded(_) | Event::WorkspaceDeleted(_) => {
                        if let Ok(ws) = Workspaces::get_async().await {
                            sender.input(AppInput::WorkspaceList(ws.to_vec()));
                        }
                    }
                    _ => {}
                }
            }

            eprintln!(
                "breadbar: Hyprland event stream ended (restart/reload/IPC hiccup); \
                 reconnecting in {:?}",
                backoff
            );
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
            sync_state(&sender).await;
        }
    });
}

pub fn make_button(id: WorkspaceId, name: &str, active: WorkspaceId) -> gtk4::Button {
    let btn = gtk4::Button::with_label(name);
    btn.add_css_class("workspace-btn");
    if id == active {
        btn.add_css_class("active");
    }
    btn.connect_clicked(move |_| {
        use hyprland::dispatch::{Dispatch, DispatchType, WorkspaceIdentifierWithSpecial};
        let _ = Dispatch::call(DispatchType::Workspace(WorkspaceIdentifierWithSpecial::Id(
            id,
        )));
    });
    btn
}

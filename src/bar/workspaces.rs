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

pub fn spawn_watcher(sender: ComponentSender<crate::App>) {
    relm4::spawn(async move {
        if let Ok(ws) = Workspaces::get_async().await {
            sender.input(AppInput::WorkspaceList(ws.to_vec()));
        }
        if let Ok(active) = Workspace::get_active_async().await {
            sender.input(AppInput::ActiveWorkspace(active.id));
        }

        let mut stream = EventStream::new();
        while let Some(Ok(event)) = stream.next().await {
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
        let _ = Dispatch::call(DispatchType::Workspace(WorkspaceIdentifierWithSpecial::Id(id)));
    });
    btn
}

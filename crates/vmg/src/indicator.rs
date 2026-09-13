//! The panel indicator, a `StatusNotifierItem` listing the machines.

use async_channel::Sender;
use ksni::blocking::TrayMethods as _;
use ksni::menu::StandardItem;

use crate::application::{APP_ID, APP_NAME};
use crate::model::Snapshot;
use crate::worker::Update;
use vm_core::reports::State;

/// What the menu lists, decided without D-Bus.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Model {
    pub machines: Vec<(String, String)>,
    pub running: usize,
}

impl Model {
    pub fn of(snapshot: &Snapshot) -> Self {
        Self {
            machines: snapshot
                .machines
                .iter()
                .map(|row| (row.name.clone(), row.state.slug().to_owned()))
                .collect(),
            running: snapshot
                .machines
                .iter()
                .filter(|row| matches!(row.state, State::Live(_)))
                .count(),
        }
    }

    pub fn tooltip(&self) -> String {
        match self.machines.len() {
            0 => "No machines".to_owned(),
            total => format!("{} of {total} machines running", self.running),
        }
    }
}

pub struct Tray {
    model: Model,
    updates: Sender<Update>,
}

/// Menu labels treat a doubled underscore as a literal one.
fn escape(label: &str) -> String {
    label.replace('_', "__")
}

impl Tray {
    fn request(&self, update: Update) {
        if self.updates.send_blocking(update).is_err() {
            crate::warn("the window is no longer listening to the indicator");
        }
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        APP_ID.to_owned()
    }

    fn title(&self) -> String {
        APP_NAME.to_owned()
    }

    fn icon_name(&self) -> String {
        "computer-symbolic".to_owned()
    }

    fn attention_icon_name(&self) -> String {
        "computer-symbolic".to_owned()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            icon_name: "computer-symbolic".to_owned(),
            icon_pixmap: Vec::new(),
            title: APP_NAME.to_owned(),
            description: self.model.tooltip(),
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.request(Update::OpenRequested);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let mut items: Vec<ksni::MenuItem<Self>> = Vec::new();
        if self.model.machines.is_empty() {
            items.push(
                StandardItem {
                    label: "No machines".to_owned(),
                    enabled: false,
                    ..StandardItem::default()
                }
                .into(),
            );
        }
        for (name, state) in &self.model.machines {
            let shown = name.clone();
            items.push(
                StandardItem {
                    label: escape(&format!("{name} ({state})")),
                    activate: Box::new(move |tray: &mut Self| {
                        tray.request(Update::ShowMachine(shown.clone()));
                    }),
                    ..StandardItem::default()
                }
                .into(),
            );
        }
        items.push(ksni::MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Open".to_owned(),
                activate: Box::new(|tray: &mut Self| tray.request(Update::OpenRequested)),
                ..StandardItem::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Quit".to_owned(),
                activate: Box::new(|tray: &mut Self| tray.request(Update::QuitRequested)),
                ..StandardItem::default()
            }
            .into(),
        );
        items
    }
}

pub struct Handle(ksni::blocking::Handle<Tray>);

/// Publishes the indicator when the desktop has a host for it, off the main thread.
pub fn start(updates: Sender<Update>) -> Option<Handle> {
    if !host_available() {
        return None;
    }
    let tray = Tray {
        model: Model::default(),
        updates,
    };
    match tray.spawn() {
        Ok(handle) => Some(Handle(handle)),
        Err(error) => {
            crate::warn(&format!("could not publish the panel indicator: {error}"));
            None
        }
    }
}

pub fn refresh(handle: &Handle, model: Model) {
    handle.0.update(move |tray| tray.model = model);
}

/// GNOME has no host of its own, so this is often false there.
fn host_available() -> bool {
    let Ok(connection) = zbus::blocking::Connection::session() else {
        return false;
    };
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        "org.kde.StatusNotifierWatcher",
        "/StatusNotifierWatcher",
        "org.kde.StatusNotifierWatcher",
    );
    proxy.is_ok_and(|proxy| {
        proxy
            .get_property::<bool>("IsStatusNotifierHostRegistered")
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underscores_in_labels_survive_the_menu() {
        assert_eq!(escape("pub_sub"), "pub__sub");
    }

    #[test]
    fn the_tooltip_counts_running_machines() {
        assert_eq!(Model::default().tooltip(), "No machines");
        let model = Model {
            machines: vec![
                ("a".to_owned(), "running".to_owned()),
                ("b".to_owned(), "stopped".to_owned()),
            ],
            running: 1,
        };
        assert_eq!(model.tooltip(), "1 of 2 machines running");
    }
}

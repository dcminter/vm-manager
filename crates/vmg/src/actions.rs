//! What the window does when an action is chosen, and what it says when one ends.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use vm_core::configuration;
use vm_core::reports::{PullStatus, RunStatus, State, StopOutcome};

use crate::forms;
use crate::model::{self, Action, NodeId};
use crate::terminals;
use crate::window::VmgWindow;
use crate::worker::{Command, Outcome, Terminal};

impl Outcome {
    /// One line for a toast.
    pub fn describe(&self) -> String {
        match self {
            Self::Inspected(inspect) => format!("Inspected {}:{}", inspect.name, inspect.tag),
            Self::Pulled(pulled) => match pulled.status {
                PullStatus::Fetched => format!("Pulled {}:{}", pulled.name, pulled.tag),
                PullStatus::AlreadyPresent => {
                    format!("{}:{} was already held", pulled.name, pulled.tag)
                }
            },
            Self::Run(run) => match run.status {
                RunStatus::Created => format!("Started {} from {}", run.name, run.image),
                RunStatus::Restarted => format!("Started {}", run.name),
                RunStatus::AlreadyRunning => format!("{} is already running", run.name),
            },
            Self::Stopped(stopped) => match stopped.outcome {
                StopOutcome::PoweredDown => format!("{} shut down", stopped.name),
                StopOutcome::Killed => format!("Killed {}", stopped.name),
                StopOutcome::Unresponsive => {
                    format!("{} did not shut down, so it was killed", stopped.name)
                }
                StopOutcome::Unreachable => {
                    format!("{} was ended without asking the guest", stopped.name)
                }
                StopOutcome::AlreadyStopped => format!("{} was already stopped", stopped.name),
            },
            Self::Switched(switched) => format!("{} is {}", switched.name, switched.state),
            Self::Removed(removed) => format!("Removed {}", removed.name),
            Self::Cloned(cloned) => format!(
                "Cloned {} as {}:{} ({})",
                cloned.source,
                cloned.name,
                cloned.tag,
                model::human(cloned.size)
            ),
            Self::Untagged(untagged) => {
                if untagged.size > 0 {
                    format!(
                        "Removed {}:{}, freeing {}",
                        untagged.name,
                        untagged.tag,
                        model::human(untagged.size)
                    )
                } else {
                    format!("Removed the name {}:{}", untagged.name, untagged.tag)
                }
            }
            Self::Pruned(pruned) => {
                if pruned.dry_run {
                    format!("{} items would be removed", pruned.items.len())
                } else {
                    format!(
                        "Removed {} items, freeing {}",
                        pruned.items.len(),
                        model::human(pruned.total())
                    )
                }
            }
            Self::Imported(imported) => format!(
                "Imported {} as {}:{}",
                imported.source, imported.outcome.name, imported.outcome.tag
            ),
            Self::Exported(exported) => format!(
                "Exported {}:{} to {}",
                exported.name, exported.tag, exported.path
            ),
            Self::Updated(update) => {
                let failed: Vec<&str> = update
                    .catalogues
                    .iter()
                    .filter(|held| held.outcome.is_err())
                    .map(|held| held.name.as_str())
                    .collect();
                if failed.is_empty() {
                    format!("Updated {} catalogues", update.catalogues.len())
                } else {
                    format!("Could not update {}", failed.join(", "))
                }
            }
            Self::Screenshot(shot) => {
                format!("Saved {} ({}x{})", shot.path, shot.width, shot.height)
            }
            Self::Screen(Some(screen)) => {
                format!("The screen of {} is at {}", screen.name, screen.socket)
            }
            Self::Screen(None) => "Closed the screen viewer".to_owned(),
            Self::Configured(configuration::Outcome::Changed(changed)) => changed.summary.clone(),
            Self::Configured(_) => "Configuration read".to_owned(),
            Self::Terminal {
                kind: Terminal::Shell,
                name,
                ..
            } => format!("Opened a shell on {name}"),
            Self::Terminal {
                kind: Terminal::Copy,
                name,
                ..
            } => format!("Copying with {name}"),
        }
    }
}

impl VmgWindow {
    fn sink(&self) -> Rc<dyn Fn(Command)> {
        let window = self.clone();
        Rc::new(move |command| window.send(command))
    }

    /// Acts on one node.
    pub fn invoke(&self, node: &NodeId, action: Action) {
        let snapshot = self.snapshot();
        match node {
            NodeId::Host | NodeId::Machines | NodeId::Images => match action {
                Action::Run => forms::run_dialog(self, "", &snapshot.host, self.sink()),
                Action::Import => forms::import_dialog(self, &snapshot.host, self.sink()),
                Action::Update => self.send(Command::Update { named: None }),
                Action::Prune => forms::prune_dialog(self, self.sink()),
                Action::Configure => forms::configuration_dialog(self, &snapshot, self.sink()),
                _ => {}
            },
            NodeId::Machine(name) => self.invoke_machine(name, action),
            NodeId::Image { name, tag } => {
                let reference = format!("{name}:{tag}");
                match action {
                    Action::Run => forms::run_dialog(self, &reference, &snapshot.host, self.sink()),
                    Action::Pull => self.send(Command::Pull { reference }),
                    Action::Export => {
                        let architectures = model::image_summaries(&snapshot)
                            .into_iter()
                            .find(|image| &image.name == name && &image.tag == tag)
                            .map_or_else(
                                || vec![snapshot.host.arch.clone()],
                                |image| image.architectures,
                            );
                        forms::export_dialog(
                            self,
                            &reference,
                            &architectures,
                            &snapshot.host.arch,
                            self.sink(),
                        );
                    }
                    Action::RemoveImage => self.confirm_remove_images(&[reference]),
                    _ => {}
                }
            }
        }
    }

    fn invoke_machine(&self, name: &str, action: Action) {
        let snapshot = self.snapshot();
        let state = snapshot
            .machines
            .iter()
            .find(|row| row.name == name)
            .map_or(State::Damaged, |row| row.state.clone());
        let running = matches!(state, State::Live(_));
        let record = snapshot.records.get(name).cloned();
        let name = name.to_owned();
        match action {
            Action::Start => self.send(Command::Start {
                name,
                changes: Box::default(),
            }),
            Action::Settings => {
                if let Some(held) = record {
                    forms::settings_dialog(self, &held, true, self.sink());
                }
            }
            Action::Eject => {
                let window = self.clone();
                forms::confirm(
                    self,
                    &format!("Eject the CD-ROM from {name}?"),
                    "The machine starts without it and can no longer boot the installer.",
                    "Eject and Start",
                    &[],
                    move || {
                        window.send(Command::Start {
                            name: name.clone(),
                            changes: Box::new(vm_core::machines::Changes {
                                eject: true,
                                ..Default::default()
                            }),
                        });
                    },
                );
            }
            Action::Stop => forms::stop_dialog(self, &name, self.sink()),
            Action::Kill => {
                let window = self.clone();
                forms::confirm(
                    self,
                    &format!("Kill {name}?"),
                    "The guest is not told, so unflushed writes are lost.",
                    "Kill",
                    &[],
                    move || {
                        window.send(Command::Stop {
                            name: name.clone(),
                            timeout: 10,
                            force: true,
                        });
                    },
                );
            }
            Action::Pause => self.send(Command::Pause { name }),
            Action::Resume => self.send(Command::Resume { name }),
            Action::Console => match terminals::console(&name) {
                Ok(tab) => {
                    self.open_terminal(&format!("console:{name}"), &format!("{name} console"), tab);
                }
                Err(error) => self.toast(&error.to_string(), true),
            },
            Action::Shell => self.send(Command::Shell {
                name,
                command: Vec::new(),
            }),
            Action::Logs => {
                let tab = terminals::logs(&name);
                self.open_terminal(&format!("logs:{name}"), &format!("{name} log"), tab);
            }
            Action::Screen => self.send(Command::Screen { name }),
            Action::Screenshot => self.choose_screenshot(&name),
            Action::Clone => forms::clone_dialog(self, &name, running, self.sink()),
            Action::CopyFiles => forms::copy_dialog(self, &name, self.sink()),
            Action::Remove => self.confirm_remove_machines(&[(name, running)]),
            _ => {}
        }
    }

    /// Acts on every checked row.
    pub fn invoke_many(&self, nodes: &[NodeId], action: Action) {
        let snapshot = self.snapshot();
        let machines: Vec<(String, bool)> = nodes
            .iter()
            .filter_map(|node| match node {
                NodeId::Machine(name) => Some(name.clone()),
                _ => None,
            })
            .map(|name| {
                let running = snapshot
                    .machines
                    .iter()
                    .any(|row| row.name == name && matches!(row.state, State::Live(_)));
                (name, running)
            })
            .collect();
        let images: Vec<String> = nodes.iter().filter_map(NodeId::reference).collect();
        match action {
            Action::Start => {
                for (name, _) in machines {
                    self.send(Command::Start {
                        name,
                        changes: Box::default(),
                    });
                }
            }
            Action::Stop | Action::Kill => {
                for (name, _) in machines {
                    self.send(Command::Stop {
                        name,
                        timeout: if action == Action::Kill { 10 } else { 30 },
                        force: action == Action::Kill,
                    });
                }
            }
            Action::Remove => self.confirm_remove_machines(&machines),
            Action::Pull => {
                for reference in images {
                    self.send(Command::Pull { reference });
                }
            }
            Action::RemoveImage => self.confirm_remove_images(&images),
            _ => {}
        }
    }

    fn confirm_remove_machines(&self, machines: &[(String, bool)]) {
        if machines.is_empty() {
            return;
        }
        let names: Vec<String> = machines.iter().map(|(name, _)| name.clone()).collect();
        let running = machines.iter().any(|(_, running)| *running);
        let window = self.clone();
        let held = machines.to_vec();
        let heading = if names.len() == 1 {
            format!("Remove {}?", names[0])
        } else {
            "Remove these machines?".to_owned()
        };
        forms::confirm(
            self,
            &heading,
            if running {
                "A running machine is killed first. Each disk is deleted."
            } else {
                "Each disk is deleted."
            },
            "Remove",
            if names.len() == 1 { &[] } else { &names },
            move || {
                for (name, running) in &held {
                    window.send(Command::Remove {
                        name: name.clone(),
                        force: *running,
                    });
                }
            },
        );
    }

    fn confirm_remove_images(&self, references: &[String]) {
        if references.is_empty() {
            return;
        }
        let window = self.clone();
        let held = references.to_vec();
        let heading = if references.len() == 1 {
            format!("Remove {}?", references[0])
        } else {
            "Remove these images?".to_owned()
        };
        forms::confirm(
            self,
            &heading,
            "The image leaves the store; machines that need it can no longer start.",
            "Remove",
            if references.len() == 1 {
                &[]
            } else {
                references
            },
            move || {
                for reference in &held {
                    window.send(Command::RemoveImage {
                        reference: reference.clone(),
                        force: true,
                    });
                }
            },
        );
    }

    fn choose_screenshot(&self, name: &str) {
        let dialog = gtk::FileDialog::builder()
            .modal(true)
            .initial_name(format!("{name}.png"))
            .build();
        let name = name.to_owned();
        dialog.save(
            Some(self),
            gtk::gio::Cancellable::NONE,
            glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |result| {
                    if let Ok(file) = result
                        && let Some(path) = file.path()
                    {
                        window.send(Command::Screenshot { name, file: path });
                    }
                }
            ),
        );
    }

    /// Reports an outcome, and opens what it calls for.
    pub fn apply_finished(&self, command: &Command, outcome: &Outcome) {
        match outcome {
            Outcome::Inspected(inspect) => {
                self.remember_inspect(inspect.clone());
                self.render_detail(&NodeId::image(&inspect.name, &inspect.tag));
                return;
            }
            Outcome::Pruned(pruned) if pruned.dry_run => {
                let Command::Prune { target, all, .. } = command else {
                    return;
                };
                if pruned.items.is_empty() {
                    self.toast("Nothing to prune", false);
                    return;
                }
                let items: Vec<String> = pruned
                    .items
                    .iter()
                    .map(|item| format!("{} ({})", item.name, model::human(item.size)))
                    .collect();
                let (window, target, all) = (self.clone(), *target, *all);
                forms::confirm(
                    self,
                    "Remove these?",
                    &format!(
                        "{} items, freeing {}",
                        items.len(),
                        model::human(pruned.total())
                    ),
                    "Prune",
                    &items,
                    move || {
                        window.send(Command::Prune {
                            target,
                            all,
                            dry_run: false,
                        });
                    },
                );
                return;
            }
            Outcome::Terminal {
                kind,
                name,
                program,
                arguments,
            } => {
                let tab = terminals::spawn(program, arguments);
                let key = format!("{program}:{name}:{}", vm_core::instance::now());
                let title = match kind {
                    Terminal::Shell => format!("{name} shell"),
                    Terminal::Copy => format!("{name} copy"),
                };
                self.open_terminal(&key, &title, tab);
                return;
            }
            _ => {}
        }
        self.toast(&outcome.describe(), false);
        if command.alters() {
            self.forget_inspections();
            self.refresh();
        }
    }
}

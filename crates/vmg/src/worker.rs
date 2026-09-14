//! Runs operations on their own threads and reports back over a channel.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use async_channel::Sender;
use vm_core::catalogue::Catalogue;
use vm_core::config::Config;
use vm_core::error::{Error, Result};
use vm_core::instance::Instances;
use vm_core::machines::{self, Changes, Event, PruneTarget, Request};
use vm_core::reports;
use vm_core::store::Store;
use vm_core::{configuration, images, screen};

use crate::model::{Host, Snapshot};

/// An image to import, owned so it can cross to a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub source: String,
    pub reference: String,
    pub arch: String,
    pub description: Option<String>,
    pub login: vm_core::catalogue::Login,
    pub hardware: vm_core::import::Hardware,
    pub digest: Option<String>,
    pub fetchable: bool,
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub reference: String,
    pub file: PathBuf,
    pub arch: String,
    pub compression: vm_core::compression::Compression,
    pub force: bool,
}

/// Which terminal a command's arguments are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    Shell,
    Copy,
}

#[derive(Debug, Clone)]
pub enum Command {
    Refresh,
    Inspect {
        reference: String,
        arch: String,
    },
    Pull {
        reference: String,
    },
    Run(Box<Request>),
    Start {
        name: String,
        changes: Box<Changes>,
    },
    Stop {
        name: String,
        timeout: u64,
        force: bool,
    },
    Pause {
        name: String,
    },
    Unpause {
        name: String,
    },
    Remove {
        name: String,
        force: bool,
    },
    Clone {
        name: String,
        target: String,
        description: Option<String>,
        force: bool,
    },
    RemoveImage {
        reference: String,
        force: bool,
    },
    Prune {
        target: Option<PruneTarget>,
        all: bool,
        dry_run: bool,
    },
    Import(Box<Import>),
    Export(Box<Export>),
    Update {
        named: Option<String>,
    },
    Screenshot {
        name: String,
        file: PathBuf,
    },
    Screen {
        name: String,
    },
    Configure(Box<configuration::Action>),
    Shell {
        name: String,
        command: Vec<String>,
    },
    Copy {
        from: String,
        to: String,
    },
}

impl Command {
    /// A short description for the busy indicator and for failures.
    pub fn subject(&self) -> String {
        match self {
            Self::Refresh => "Refreshing".to_owned(),
            Self::Inspect { reference, .. } => format!("Inspecting {reference}"),
            Self::Pull { reference } => format!("Pulling {reference}"),
            Self::Run(request) => format!("Running {}", request.reference),
            Self::Start { name, .. } => format!("Starting {name}"),
            Self::Stop {
                name, force: false, ..
            } => format!("Stopping {name}"),
            Self::Stop {
                name, force: true, ..
            } => format!("Killing {name}"),
            Self::Pause { name } => format!("Pausing {name}"),
            Self::Unpause { name } => format!("Unpausing {name}"),
            Self::Remove { name, .. } => format!("Removing {name}"),
            Self::Clone { name, target, .. } => format!("Cloning {name} to {target}"),
            Self::RemoveImage { reference, .. } => format!("Removing {reference}"),
            Self::Prune { dry_run: true, .. } => "Listing what pruning would remove".to_owned(),
            Self::Prune { .. } => "Pruning".to_owned(),
            Self::Import(import) => format!("Importing {}", import.reference),
            Self::Export(export) => format!("Exporting {}", export.reference),
            Self::Update { named: Some(name) } => format!("Updating {name}"),
            Self::Update { named: None } => "Updating catalogues".to_owned(),
            Self::Screenshot { name, .. } => format!("Photographing {name}"),
            Self::Screen { name } => format!("Showing {name}"),
            Self::Configure(_) => "Changing the configuration".to_owned(),
            Self::Shell { name, .. } => format!("Opening a shell on {name}"),
            Self::Copy { from, to } => format!("Copying {from} to {to}"),
        }
    }

    /// Whether the listing changes when this completes.
    pub const fn alters(&self) -> bool {
        !matches!(
            self,
            Self::Refresh
                | Self::Inspect { .. }
                | Self::Screen { .. }
                | Self::Shell { .. }
                | Self::Copy { .. }
                | Self::Prune { dry_run: true, .. }
        )
    }
}

/// What a finished command produced.
#[derive(Debug, Clone)]
pub enum Outcome {
    Inspected(reports::Inspect),
    Pulled(reports::Pull),
    Run(reports::Run),
    Stopped(reports::Stopped),
    Switched(reports::Switched),
    Removed(reports::Removed),
    Cloned(reports::Cloned),
    Untagged(reports::Untagged),
    Pruned(reports::Pruned),
    Imported(reports::Imported),
    Exported(reports::Exported),
    Updated(reports::Update),
    Screenshot(reports::Screenshot),
    Screen(Option<reports::Screen>),
    Configured(configuration::Outcome),
    Terminal {
        kind: Terminal,
        name: String,
        program: String,
        arguments: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub enum Update {
    Snapshot(Box<Snapshot>),
    Progress {
        job: u64,
        label: String,
        fraction: Option<f64>,
    },
    Finished {
        job: u64,
        command: Box<Command>,
        outcome: Box<Outcome>,
    },
    Failed {
        job: u64,
        command: Box<Command>,
        error: String,
        kind: &'static str,
    },
    IndicatorAvailable(bool),
    ShowMachine(String),
    OpenRequested,
    QuitRequested,
}

/// Runs a command on its own thread, reporting under the job number given.
pub fn submit(job: u64, command: Command, updates: Sender<Update>) {
    let sent = updates.clone();
    let progress = move |label: String, fraction: Option<f64>| {
        let _ = sent.send_blocking(Update::Progress {
            job,
            label,
            fraction,
        });
    };
    std::thread::spawn(move || {
        let update = match perform(&command, &progress) {
            Ok(Performed::Snapshot(snapshot)) => Update::Snapshot(Box::new(snapshot)),
            Ok(Performed::Outcome(outcome)) => Update::Finished {
                job,
                command: Box::new(command),
                outcome: Box::new(outcome),
            },
            Err(error) => Update::Failed {
                job,
                kind: error.kind(),
                error: error.to_string(),
                command: Box::new(command),
            },
        };
        let _ = updates.send_blocking(update);
    });
}

enum Performed {
    Snapshot(Snapshot),
    Outcome(Outcome),
}

fn perform(command: &Command, progress: &dyn Fn(String, Option<f64>)) -> Result<Performed> {
    let mut observe = observer(progress);
    let outcome = match command {
        Command::Refresh => return snapshot().map(Performed::Snapshot),
        Command::Inspect { reference, arch } => {
            let (catalogue, store) = catalogue()?;
            Outcome::Inspected(images::inspect(&catalogue, &store, reference, arch)?)
        }
        Command::Pull { reference } => {
            let (catalogue, store) = catalogue()?;
            Outcome::Pulled(images::pull(&catalogue, &store, reference, &mut observe)?)
        }
        Command::Run(request) => {
            let (catalogue, store) = catalogue()?;
            Outcome::Run(machines::run(&catalogue, &store, request, &mut observe)?)
        }
        Command::Start { name, changes } => Outcome::Run(machines::start(name, changes)?),
        Command::Stop {
            name,
            timeout,
            force,
        } => Outcome::Stopped(machines::stop(
            name,
            Duration::from_secs(*timeout),
            *force,
            &mut observe,
        )?),
        Command::Pause { name } => Outcome::Switched(machines::pause(name)?),
        Command::Unpause { name } => Outcome::Switched(machines::unpause(name)?),
        Command::Remove { name, force } => Outcome::Removed(machines::remove(name, *force)?),
        Command::Clone {
            name,
            target,
            description,
            force,
        } => Outcome::Cloned(machines::clone(
            name,
            target,
            description.as_deref(),
            *force,
            &mut observe,
        )?),
        Command::RemoveImage { reference, force } => {
            let (catalogue, store) = catalogue()?;
            Outcome::Untagged(machines::remove_image(
                &catalogue, &store, reference, *force,
            )?)
        }
        Command::Prune {
            target,
            all,
            dry_run,
        } => {
            let (catalogue, store) = catalogue()?;
            Outcome::Pruned(machines::prune(
                &catalogue, &store, *target, *all, *dry_run,
            )?)
        }
        Command::Import(import) => {
            let (catalogue, store) = catalogue()?;
            Outcome::Imported(import_image(&catalogue, &store, import, &mut observe)?)
        }
        Command::Export(export) => {
            let (catalogue, store) = catalogue()?;
            let request = images::ExportRequest {
                reference: &export.reference,
                file: &export.file,
                arch: &export.arch,
                compression: export.compression,
                force: export.force,
            };
            Outcome::Exported(images::export(&catalogue, &store, &request, &mut observe)?)
        }
        Command::Update { named } => {
            Outcome::Updated(images::update(&Config::load()?, named.as_deref())?)
        }
        Command::Screenshot { name, file } => {
            Outcome::Screenshot(screen::capture(name, Some(file))?)
        }
        Command::Screen { name } => Outcome::Screen(screen::show(name, true)?),
        Command::Configure(action) => Outcome::Configured(configuration::run(Some(action))?),
        Command::Shell { name, command } => Outcome::Terminal {
            kind: Terminal::Shell,
            name: name.clone(),
            program: "ssh".to_owned(),
            arguments: machines::ssh_arguments(name, command)?,
        },
        Command::Copy { from, to } => copy_terminal(from, to)?,
    };
    Ok(Performed::Outcome(outcome))
}

/// The scp run for a copy, named after whichever side is the guest.
fn copy_terminal(from: &str, to: &str) -> Result<Outcome> {
    let (source, target) = (
        vm_core::access::Location::parse(from),
        vm_core::access::Location::parse(to),
    );
    let name = source
        .instance()
        .or_else(|| target.instance())
        .cloned()
        .unwrap_or_default();
    Ok(Outcome::Terminal {
        kind: Terminal::Copy,
        name,
        program: "scp".to_owned(),
        arguments: machines::scp_arguments(from, to)?,
    })
}

/// Turns core events into progress reports.
fn observer(progress: &dyn Fn(String, Option<f64>)) -> impl FnMut(Event) {
    move |event| {
        let (label, fraction) = match event {
            Event::Fetching(url) => (format!("Fetching {url}"), None),
            Event::Received(received) => ("Receiving".to_owned(), fraction_of(received)),
            Event::Converting(percent) => {
                ("Converting".to_owned(), Some(f64::from(percent) / 100.0))
            }
            Event::Cloning(stage, percent) => {
                (stage.label().to_owned(), Some(f64::from(percent) / 100.0))
            }
            Event::Shutdown { name, seconds } => (
                format!("Waiting up to {seconds}s for {name} to shut down"),
                None,
            ),
            Event::Exporting(received) => ("Exporting".to_owned(), fraction_of(received)),
            Event::Importing {
                from_url,
                event: vm_core::import::Event::Receiving(received),
            } => (
                if from_url { "Fetching" } else { "Reading" }.to_owned(),
                fraction_of(received),
            ),
            Event::Importing {
                event: vm_core::import::Event::Converting(percent),
                ..
            } => ("Converting".to_owned(), Some(f64::from(percent) / 100.0)),
            Event::Importing {
                event: vm_core::import::Event::Hashing(percent),
                ..
            } => ("Verifying".to_owned(), Some(f64::from(percent) / 100.0)),
        };
        progress(label, fraction);
    }
}

fn fraction_of(progress: vm_core::store::Progress) -> Option<f64> {
    let total = progress.total.filter(|total| *total > 0)?;
    Some(progress.received as f64 / total as f64)
}

fn import_image(
    catalogue: &Catalogue,
    store: &Store,
    import: &Import,
    observe: &mut dyn FnMut(Event),
) -> Result<reports::Imported> {
    let source = vm_core::settings::parse_source(&import.source).map_err(|_| Error::Reference {
        input: import.source.clone(),
        reason: "a source needs a path or URL",
    })?;
    let reference: vm_core::Reference = import.reference.parse()?;
    let digest = import
        .digest
        .as_deref()
        .map(str::parse::<vm_core::reference::Digest>)
        .transpose()?;
    let request = vm_core::import::Request {
        source: &source,
        reference: &reference,
        arch: &import.arch,
        description: import.description.as_deref(),
        login: import.login,
        hardware: import.hardware.clone(),
        digest: digest.as_ref(),
        fetchable: import.fetchable,
        force: import.force,
    };
    images::import(catalogue, store, &request, observe)
}

/// The layered catalogue and the store, as every image command sees them.
fn catalogue() -> Result<(Catalogue, Store)> {
    let config = Config::load()?;
    let sources = vm_core::paths::catalogue_sources(&config);
    let catalogue = Catalogue::load_layered(&sources)?;
    let store = Store::discover()?;
    Ok((catalogue, store))
}

fn snapshot() -> Result<Snapshot> {
    let config = Config::load()?;
    let catalogues = vm_core::paths::catalogue_sources(&config);
    let catalogue = Catalogue::load_layered(&catalogues)?;
    let store = Store::discover()?;
    let machines = machines::list(true)?.rows;
    let instances = Instances::discover()?;
    let mut records = BTreeMap::new();
    for name in instances.names()? {
        if let Ok(record) = instances.open(&name).and_then(|directory| directory.read()) {
            records.insert(name, record);
        }
    }
    let images = images::list(&catalogue, &store, true, reports::Origin::All).rows;
    Ok(Snapshot {
        host: host(&config, &store, &instances),
        machines,
        records,
        images,
        catalogues,
    })
}

fn host(config: &Config, store: &Store, instances: &Instances) -> Host {
    let config_path = Config::path();
    Host {
        name: std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map_or_else(|_| "this host".to_owned(), |name| name.trim().to_owned()),
        arch: vm_core::host_architecture().to_owned(),
        hypervisor: hypervisor_version(),
        accelerated: std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kvm")
            .is_ok(),
        store: store.root().display().to_string(),
        instances: instances.root().display().to_string(),
        config: config_path
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string()),
        config_exists: config_path.is_some_and(|path| path.exists()),
        auto_pull: config.auto_pull,
        add_ssh_config: config.add_ssh_config,
        default_user: config.user().unwrap_or_default(),
    }
}

/// The first line the hypervisor prints for its version, once per refresh.
fn hypervisor_version() -> Option<String> {
    let output =
        std::process::Command::new(vm_core::hypervisor::binary_for(vm_core::host_architecture()))
            .arg("--version")
            .output()
            .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next()?.trim();
    line.strip_prefix("QEMU emulator version ")
        .map(|rest| format!("QEMU {}", rest.split(' ').next().unwrap_or(rest)))
        .or_else(|| Some(line.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_commands_subject_names_what_it_acts_on() {
        assert_eq!(
            Command::Pull {
                reference: "debian:trixie".to_owned()
            }
            .subject(),
            "Pulling debian:trixie"
        );
        assert_eq!(
            Command::Stop {
                name: "demo".to_owned(),
                timeout: 30,
                force: true
            }
            .subject(),
            "Killing demo"
        );
        assert_eq!(
            Command::Update { named: None }.subject(),
            "Updating catalogues"
        );
    }

    #[test]
    fn only_commands_that_change_something_ask_for_a_refresh() {
        assert!(!Command::Refresh.alters());
        assert!(
            !Command::Prune {
                target: None,
                all: false,
                dry_run: true
            }
            .alters()
        );
        assert!(
            Command::Prune {
                target: None,
                all: false,
                dry_run: false
            }
            .alters()
        );
        assert!(
            Command::Pause {
                name: "demo".to_owned()
            }
            .alters()
        );
    }

    #[test]
    fn progress_is_a_fraction_only_when_the_total_is_known() {
        let known = vm_core::store::Progress {
            received: 50,
            total: Some(200),
        };
        assert_eq!(fraction_of(known), Some(0.25));
        let unknown = vm_core::store::Progress {
            received: 50,
            total: None,
        };
        assert_eq!(fraction_of(unknown), None);
    }
}

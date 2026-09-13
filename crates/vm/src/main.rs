mod completion;
mod console;
mod machines;
mod output;
mod progress;
mod reports;
mod screen;
mod style;
mod table;
mod terminal;
mod units;

use clap::{Parser, Subcommand};
use clap_complete::engine::{ArgValueCandidates, ArgValueCompleter};
use output::{Format, Report};
use reports::Origin;
use std::process::ExitCode;
use style::Style;
use vm_core::catalogue::Catalogue;
use vm_core::store::{Pulled, Store};
use vm_core::{Reference, host_architecture, paths};

#[derive(Debug, Parser)]
#[command(
    name = "vm",
    version,
    about = "Create and manage QEMU virtual machines"
)]
struct Cli {
    /// Output format
    #[arg(
        long,
        global = true,
        value_enum,
        env = "VM_FORMAT",
        default_value = "text"
    )]
    format: Format,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the images known to the local catalogue
    Images {
        /// Show every architecture rather than this host's
        #[arg(long)]
        all_architectures: bool,
        /// Show only images made on this host, such as clones
        #[arg(long, conflicts_with = "remote")]
        local: bool,
        /// Show only images from the fetched catalogue
        #[arg(long)]
        remote: bool,
    },
    /// Show what an image reference resolves to
    Inspect {
        /// Image reference, such as debian:trixie
        #[arg(add = ArgValueCandidates::new(completion::catalogue_image))]
        reference: String,
    },
    /// Fetch an image into the local store
    Pull {
        /// Image reference, such as debian:trixie
        #[arg(add = ArgValueCandidates::new(completion::catalogue_image))]
        reference: String,
    },
    /// Refresh the local catalogue from its remote source
    Update,
    /// Create and start a virtual machine
    Run {
        /// Image reference, such as debian:trixie
        #[arg(add = ArgValueCandidates::new(completion::catalogue_image))]
        reference: String,
        /// Name for the instance; one is invented if this is omitted
        #[arg(long)]
        name: Option<String>,
        /// Memory, in mebibytes unless suffixed with M or G
        #[arg(long, short, default_value = "2G", value_parser = machines::parse_memory)]
        memory: u64,
        /// Processors
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u32).range(1..=255))]
        cpus: u32,
        /// Forward a host port to a guest port, as host:guest
        #[arg(long, short, value_parser = machines::parse_port)]
        publish: Vec<vm_core::instance::Port>,
        /// Account to create in the guest; defaults to the config file's setting, or vm
        #[arg(long, value_parser = machines::parse_user)]
        user: Option<String>,
        /// Share a host directory with the guest, as host:guest
        #[arg(long, short = 'v', value_parser = machines::parse_share)]
        volume: Vec<vm_core::instance::Share>,
        /// Grow the disk to this size, such as 40G
        #[arg(long)]
        disk_size: Option<String>,
        /// When to fetch the image
        #[arg(long, value_enum)]
        pull: Option<machines::Pull>,
        /// Firmware, bios or uefi, instead of what the image asks for
        #[arg(long, value_parser = machines::parse_firmware, add = ArgValueCandidates::new(completion::firmware))]
        firmware: Option<vm_core::machine::Firmware>,
        /// QEMU CPU model instead of what the image asks for, such as Penryn,+avx
        #[arg(long, value_parser = machines::parse_cpu)]
        cpu: Option<String>,
        /// Machine type, q35 or pc, instead of what the image asks for
        #[arg(long, value_parser = machines::parse_machine, add = ArgValueCandidates::new(completion::chipset))]
        machine: Option<vm_core::machine::Chipset>,
        /// Disk controller, virtio, ide (pc only) or sata (q35 only), instead of what the image asks for
        #[arg(long, value_parser = machines::parse_disk, add = ArgValueCandidates::new(completion::disk))]
        disk: Option<vm_core::machine::Disk>,
        /// Ask for a password the account can log in with at the console
        #[arg(long)]
        password: bool,
        /// Let plain ssh reach the machine by name, through ~/.ssh/config
        #[arg(long, conflicts_with = "no_ssh_config")]
        add_ssh_config: bool,
        /// Add no SSH configuration entry, whatever the configuration file says
        #[arg(long)]
        no_ssh_config: bool,
    },
    /// Start an instance that is not running, changing how it is set up
    Start {
        /// Instance name
        #[arg(add = ArgValueCandidates::new(completion::stopped_instance))]
        name: String,
        /// Memory, in mebibytes unless suffixed with M or G
        #[arg(long, short, value_parser = machines::parse_memory)]
        memory: Option<u64>,
        /// Processors
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=255))]
        cpus: Option<u32>,
        /// Forward a host port to a guest port, as host:guest, replacing existing forwards
        #[arg(long, short, value_parser = machines::parse_port, conflicts_with = "no_publish")]
        publish: Vec<vm_core::instance::Port>,
        /// Forward nothing
        #[arg(long)]
        no_publish: bool,
        /// Share a host directory with the guest, as host:guest, replacing existing shares
        #[arg(long, short = 'v', value_parser = machines::parse_share, conflicts_with = "no_volume")]
        volume: Vec<vm_core::instance::Share>,
        /// Share nothing
        #[arg(long)]
        no_volume: bool,
        /// Account to use in the guest; the one it has is left in place
        #[arg(long, value_parser = machines::parse_user)]
        user: Option<String>,
        /// Grow the disk to this size, such as 40G
        #[arg(long)]
        disk_size: Option<String>,
        /// Firmware, bios or uefi; a disk prepared for only the other will not boot
        #[arg(long, value_parser = machines::parse_firmware, add = ArgValueCandidates::new(completion::firmware))]
        firmware: Option<vm_core::machine::Firmware>,
        /// QEMU CPU model, such as Penryn,+avx
        #[arg(long, value_parser = machines::parse_cpu)]
        cpu: Option<String>,
        /// Machine type, q35 or pc
        #[arg(long, value_parser = machines::parse_machine, add = ArgValueCandidates::new(completion::chipset))]
        machine: Option<vm_core::machine::Chipset>,
        /// Disk controller, virtio, ide (pc only) or sata (q35 only); a guest without its driver will not boot
        #[arg(long, value_parser = machines::parse_disk, add = ArgValueCandidates::new(completion::disk))]
        disk: Option<vm_core::machine::Disk>,
        /// Ask for a new console password for the account
        #[arg(long, conflicts_with = "no_password")]
        password: bool,
        /// Take the account's console password away
        #[arg(long)]
        no_password: bool,
        /// Let plain ssh reach the machine by name, through ~/.ssh/config
        #[arg(long, conflicts_with = "no_ssh_config")]
        add_ssh_config: bool,
        /// Remove the machine's SSH configuration entry
        #[arg(long)]
        no_ssh_config: bool,
    },
    /// Open a shell on an instance, or run a command in it
    Ssh {
        /// Instance name
        #[arg(add = ArgValueCandidates::new(completion::running_instance))]
        name: String,
        /// Command to run instead of a shell
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Copy files to or from an instance, naming one side as name:path
    Cp {
        /// Source, as a path or name:path
        #[arg(add = ArgValueCompleter::new(completion::copy_side))]
        from: String,
        /// Destination, as a path or name:path
        #[arg(add = ArgValueCompleter::new(completion::copy_side))]
        to: String,
    },
    /// List instances
    Ps {
        /// Include instances that are not running
        #[arg(long, short)]
        all: bool,
        /// List them again every second until interrupted
        #[arg(long, short)]
        follow: bool,
    },
    /// Shut an instance down
    Stop {
        /// Instance name
        #[arg(add = ArgValueCandidates::new(completion::running_instance))]
        name: String,
        /// Seconds to wait for the guest before insisting
        #[arg(long, short, default_value_t = 30)]
        timeout: u64,
    },
    /// Stop an instance's processors without telling the guest
    Pause {
        /// Instance name
        #[arg(add = ArgValueCandidates::new(completion::running_instance))]
        name: String,
    },
    /// Let a paused instance carry on
    Resume {
        /// Instance name
        #[arg(add = ArgValueCandidates::new(completion::running_instance))]
        name: String,
    },
    /// Stop an instance without telling the guest
    Kill {
        /// Instance name
        #[arg(add = ArgValueCandidates::new(completion::running_instance))]
        name: String,
    },
    /// Save an instance's disk as a new image
    Clone {
        /// Instance name
        #[arg(add = ArgValueCandidates::new(completion::any_instance))]
        name: String,
        /// Name for the new image, as repository:tag
        image: String,
        /// Clone without pausing a running guest
        #[arg(long, short)]
        force: bool,
        /// Description for the new image, instead of naming the source instance
        #[arg(long, value_parser = machines::parse_description)]
        description: Option<String>,
    },
    /// Show an instance's console
    Logs {
        /// Instance name
        #[arg(add = ArgValueCandidates::new(completion::any_instance))]
        name: String,
        /// Write new output as it arrives, until the instance stops
        #[arg(long, short)]
        follow: bool,
        /// Show only the last few lines
        #[arg(long, short = 'n')]
        lines: Option<usize>,
    },
    /// Attach this terminal to an instance's serial console; Ctrl-] detaches
    Console {
        /// Instance name
        #[arg(add = ArgValueCandidates::new(completion::running_instance))]
        name: String,
    },
    /// Open a VNC viewer on an instance's screen
    Screen {
        /// Instance name
        #[arg(add = ArgValueCandidates::new(completion::running_instance))]
        name: String,
    },
    /// Save a picture of an instance's screen as PNG
    Screenshot {
        /// Instance name
        #[arg(add = ArgValueCandidates::new(completion::running_instance))]
        name: String,
        /// Where to save it; NAME.png in this directory if omitted
        #[arg(value_hint = clap::ValueHint::AnyPath)]
        file: Option<std::path::PathBuf>,
    },
    /// Delete an image from the local store
    Rmi {
        /// Image reference, such as debian:trixie
        #[arg(add = ArgValueCandidates::new(completion::held_image))]
        reference: String,
        /// Delete it even though instances are built on it
        #[arg(long, short)]
        force: bool,
    },
    /// Delete an instance and its disk
    Rm {
        /// Instance name
        #[arg(add = ArgValueCandidates::new(completion::any_instance))]
        name: String,
        /// Remove it even if it is running
        #[arg(long, short)]
        force: bool,
    },
}

fn main() -> ExitCode {
    clap_complete::CompleteEnv::with_factory(<Cli as clap::CommandFactory>::command).complete();
    let cli = Cli::parse();
    let style = if cli.format.is_text() {
        Style::for_stdout()
    } else {
        Style::plain()
    };
    match run(&cli, style) {
        Ok(Outcome::Written) => ExitCode::SUCCESS,
        Ok(Outcome::Reported(report)) => match output::emit(report.as_ref(), cli.format, style) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) if output::is_closed_pipe(&error) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("vm: cannot write output: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            output::emit_error(&error, cli.format);
            ExitCode::FAILURE
        }
    }
}

/// A list given on the command line replaces the old one; `None` means none was given.
fn replacement<T: Clone>(given: &[T], none: bool) -> Option<Vec<T>> {
    if none {
        Some(Vec::new())
    } else if given.is_empty() {
        None
    } else {
        Some(given.to_vec())
    }
}

/// The hash of a password read from the user, when one was asked for.
fn asked_password(asked: bool) -> vm_core::Result<Option<String>> {
    if asked {
        vm_core::crypt::hash(&terminal::password()?).map(Some)
    } else {
        Ok(None)
    }
}

/// A report to write, or nothing if the command wrote as it went.
enum Outcome {
    Reported(Box<dyn Report>),
    Written,
}

fn run(cli: &Cli, style: Style) -> vm_core::Result<Outcome> {
    if let Some(outcome) = machine_command(cli, style)? {
        return Ok(outcome);
    }
    catalogue_command(cli, style).map(Outcome::Reported)
}

/// The changes a `vm start` asks for.
fn start_changes(command: &Command) -> vm_core::Result<machines::Changes> {
    let Command::Start {
        memory,
        cpus,
        publish,
        no_publish,
        volume,
        no_volume,
        user,
        disk_size,
        firmware,
        cpu,
        machine,
        disk,
        password,
        no_password,
        add_ssh_config,
        no_ssh_config,
        ..
    } = command
    else {
        return Ok(machines::Changes::default());
    };
    Ok(machines::Changes {
        machine: *machine,
        disk: *disk,
        password: if *no_password {
            Some("*".to_owned())
        } else {
            asked_password(*password)?
        },
        memory: *memory,
        cpus: *cpus,
        ports: replacement(publish, *no_publish),
        shares: replacement(volume, *no_volume),
        user: user.clone(),
        disk_size: disk_size.clone(),
        firmware: *firmware,
        cpu: cpu.clone(),
        ssh_config: toggle(*add_ssh_config, *no_ssh_config),
    })
}

/// Runs a command that needs no catalogue or store; `None` if this is not one.
fn machine_command(cli: &Cli, style: Style) -> vm_core::Result<Option<Outcome>> {
    let report: Box<dyn Report> = match &cli.command {
        Command::Ps { all, follow } => {
            if *follow {
                machines::watch(*all, cli.format, style)?;
                return Ok(Some(Outcome::Written));
            }
            Box::new(machines::list(*all)?)
        }
        Command::Start { name, .. } => {
            Box::new(machines::start(name, &start_changes(&cli.command)?)?)
        }
        Command::Logs {
            name,
            follow,
            lines,
        } => {
            if *follow {
                if !cli.format.is_text() {
                    return Err(vm_core::Error::FollowNeedsText);
                }
                machines::follow(name, *lines)?;
                return Ok(Some(Outcome::Written));
            }
            Box::new(machines::logs(name, *lines)?)
        }
        Command::Console { name } => {
            if !cli.format.is_text() {
                return Err(vm_core::Error::ConsoleNeedsText);
            }
            console::attach(name)?;
            return Ok(Some(Outcome::Written));
        }
        Command::Screen { name } => match screen::show(name, cli.format.is_text())? {
            Some(report) => Box::new(report),
            None => return Ok(Some(Outcome::Written)),
        },
        Command::Screenshot { name, file } => Box::new(screen::capture(name, file.as_deref())?),
        Command::Ssh { name, command } => {
            // Either this replaces the process or it reports why it could not.
            return machines::connect(name, command).map(|held| match held {});
        }
        Command::Cp { from, to } => {
            return machines::copy(from, to).map(|held| match held {});
        }
        Command::Stop { name, timeout } => Box::new(machines::stop(
            name,
            std::time::Duration::from_secs(*timeout),
            false,
            cli.format.is_text(),
        )?),
        Command::Pause { name } => Box::new(machines::pause(name)?),
        Command::Resume { name } => Box::new(machines::resume(name)?),
        Command::Kill { name } => Box::new(machines::stop(
            name,
            std::time::Duration::from_secs(10),
            true,
            cli.format.is_text(),
        )?),
        Command::Rm { name, force } => Box::new(machines::remove(name, *force)?),
        Command::Clone {
            name,
            image,
            force,
            description,
        } => Box::new(machines::clone(
            name,
            image,
            description.as_deref(),
            *force,
            cli.format.is_text(),
        )?),
        _ => return Ok(None),
    };
    Ok(Some(Outcome::Reported(report)))
}

/// The rest, which need both.
fn catalogue_command(cli: &Cli, style: Style) -> vm_core::Result<Box<dyn Report>> {
    let origin = match &cli.command {
        Command::Images { local, remote, .. } => Origin::of(*local, *remote),
        _ => Origin::All,
    };
    let catalogue = Catalogue::load_layered(&origin_layers(
        origin,
        paths::catalogue_directory(),
        paths::local_catalogue_directory(),
    ))?;
    let store = Store::discover()?;
    match &cli.command {
        Command::Images {
            all_architectures, ..
        } => Ok(Box::new(images(
            &catalogue,
            &store,
            *all_architectures,
            origin,
        ))),
        Command::Inspect { reference } => Ok(Box::new(inspect(&catalogue, &store, reference)?)),
        Command::Pull { reference } => Ok(Box::new(pull(
            &catalogue, &store, style, cli.format, reference,
        )?)),
        Command::Update => Ok(Box::new(update()?)),
        Command::Rmi { reference, force } => Ok(Box::new(machines::remove_image(
            &catalogue, &store, reference, *force,
        )?)),
        Command::Run {
            reference,
            name,
            memory,
            cpus,
            publish,
            user,
            volume,
            disk_size,
            pull,
            firmware,
            cpu,
            machine,
            disk,
            password,
            add_ssh_config,
            no_ssh_config,
        } => Ok(Box::new(machines::run(
            &catalogue,
            &store,
            &machines::Request {
                reference: reference.clone(),
                name: name.clone(),
                memory: *memory,
                cpus: *cpus,
                ports: publish.clone(),
                user: user.clone(),
                shares: volume.clone(),
                disk_size: disk_size.clone(),
                pull: *pull,
                firmware: *firmware,
                cpu: cpu.clone(),
                machine: *machine,
                disk: *disk,
                password: asked_password(*password)?,
                ssh_config: toggle(*add_ssh_config, *no_ssh_config),
            },
            style,
            cli.format.is_text(),
        )?)),
        Command::Ps { .. }
        | Command::Start { .. }
        | Command::Ssh { .. }
        | Command::Cp { .. }
        | Command::Stop { .. }
        | Command::Kill { .. }
        | Command::Pause { .. }
        | Command::Resume { .. }
        | Command::Clone { .. }
        | Command::Logs { .. }
        | Command::Console { .. }
        | Command::Screen { .. }
        | Command::Screenshot { .. }
        | Command::Rm { .. } => unreachable!("handled above"),
    }
}

/// A pair of opposing flags, where neither means as it was.
const fn toggle(on: bool, off: bool) -> Option<bool> {
    match (on, off) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    }
}

/// The fetched catalogue, with local clones layered over it.
fn catalogue_layers() -> Vec<std::path::PathBuf> {
    origin_layers(
        Origin::All,
        paths::catalogue_directory(),
        paths::local_catalogue_directory(),
    )
}

/// The catalogue directories holding images of an origin, in layering order.
fn origin_layers(
    origin: Origin,
    fetched: std::path::PathBuf,
    local: Option<std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    match origin {
        Origin::All => std::iter::once(fetched).chain(local).collect(),
        Origin::Local => local.into_iter().collect(),
        Origin::Remote => vec![fetched],
    }
}

fn update() -> vm_core::Result<reports::Update> {
    let config = vm_core::config::Config::load()?;
    let destination = paths::data_directory()
        .ok_or(vm_core::Error::NoImageStore)?
        .join("catalogue");
    let updated = vm_core::update::run(&config, &destination, &vm_core::store::http_agent())?;
    Ok(reports::Update {
        url: updated.url,
        path: updated.path.display().to_string(),
        files: updated.files,
        entries: updated.entries,
    })
}

fn images(
    catalogue: &Catalogue,
    store: &Store,
    all_architectures: bool,
    origin: Origin,
) -> reports::Images {
    let host = host_architecture();
    let rows = catalogue
        .entries()
        .into_iter()
        .flat_map(|entry| {
            entry
                .artifacts
                .iter()
                .filter(|artifact| all_architectures || artifact.arch == host)
                .map(|artifact| reports::ImageRow {
                    name: entry.name.clone(),
                    tag: entry.tag.clone(),
                    arch: artifact.arch.clone(),
                    description: entry.description.clone(),
                    held: store.contains(&artifact.digest),
                    size: artifact.size.or_else(|| held_size(store, artifact)),
                })
                .collect::<Vec<_>>()
        })
        .collect();
    reports::Images { rows, origin }
}

/// The size of a held image, for an entry that does not state one.
fn held_size(store: &Store, artifact: &vm_core::catalogue::Artifact) -> Option<u64> {
    std::fs::metadata(store.path_for(&artifact.digest))
        .ok()
        .map(|data| data.len())
}

fn inspect(
    catalogue: &Catalogue,
    store: &Store,
    reference: &str,
) -> vm_core::Result<reports::Inspect> {
    let reference: Reference = reference.parse()?;
    let (entry, artifact) = catalogue.resolve(&reference, host_architecture())?;
    Ok(reports::Inspect::new(
        entry,
        artifact,
        store.contains(&artifact.digest),
    ))
}

fn pull(
    catalogue: &Catalogue,
    store: &Store,
    style: Style,
    format: Format,
    reference: &str,
) -> vm_core::Result<reports::Pull> {
    let reference: Reference = reference.parse()?;
    let (entry, artifact) = catalogue.resolve(&reference, host_architecture())?;
    let mut bar = progress::Bar::new("  ", format.is_text());
    if format.is_text() && !store.contains(&artifact.digest) {
        if let Some(url) = &artifact.url {
            eprintln!("Fetching {}", style.name(url));
        }
    }
    let outcome = store.pull(artifact, &vm_core::store::http_agent(), &mut |update| {
        bar.update(update);
    });
    bar.clear();
    let outcome = outcome?;
    store.record(entry, artifact)?;
    let path = store.path_for(&artifact.digest);
    Ok(reports::Pull {
        name: entry.name.clone(),
        tag: entry.tag.clone(),
        arch: artifact.arch.clone(),
        digest: artifact.digest.to_string(),
        path: path.display().to_string(),
        size: std::fs::metadata(&path).map_or(0, |data| data.len()),
        status: match outcome {
            Pulled::Fetched => reports::PullStatus::Fetched,
            Pulled::AlreadyPresent => reports::PullStatus::AlreadyPresent,
        },
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn nothing_given_is_different_from_nothing_wanted() {
        let given = [1, 2];
        assert_eq!(replacement(&given, false), Some(vec![1, 2]));
        assert_eq!(replacement::<i32>(&[], false), None);
        assert_eq!(replacement::<i32>(&[], true), Some(Vec::new()));
    }

    #[test]
    fn a_list_and_its_refusal_cannot_be_asked_for_together() {
        use clap::Parser as _;
        let outcome = Cli::try_parse_from(["vm", "start", "one", "-p", "80:80", "--no-publish"]);
        assert!(outcome.is_err());
        let outcome = Cli::try_parse_from(["vm", "start", "one", "-v", "/tmp:/mnt", "--no-volume"]);
        assert!(outcome.is_err());
    }

    #[test]
    fn a_listing_can_be_followed_and_can_include_what_is_stopped() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from(["vm", "ps", "-a", "-f"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Ps {
                all: true,
                follow: true
            }
        ));
        let cli = Cli::try_parse_from(["vm", "ps"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Ps {
                all: false,
                follow: false
            }
        ));
    }

    #[test]
    fn a_followed_listing_is_not_refused_a_document_format() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from(["vm", "ps", "--follow", "--format", "json"]).unwrap();
        assert_eq!(cli.format, Format::Json);
    }

    #[test]
    fn run_takes_a_machine_in_place_of_the_images_own() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from([
            "vm",
            "run",
            "debian:trixie",
            "--firmware",
            "uefi",
            "--cpu",
            "Penryn,+avx",
        ])
        .unwrap();
        let Command::Run { firmware, cpu, .. } = cli.command else {
            panic!("not a run");
        };
        assert_eq!(firmware, Some(vm_core::machine::Firmware::Uefi));
        assert_eq!(cpu.as_deref(), Some("Penryn,+avx"));
    }

    #[test]
    fn run_leaves_the_machine_to_the_image_when_nothing_is_said() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from(["vm", "run", "debian:trixie"]).unwrap();
        let Command::Run { firmware, cpu, .. } = cli.command else {
            panic!("not a run");
        };
        assert_eq!(firmware, None);
        assert_eq!(cpu, None);
    }

    #[test]
    fn start_takes_machine_changes() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from(["vm", "start", "pd", "--firmware", "bios", "--cpu", "max"])
            .unwrap();
        let Command::Start { firmware, cpu, .. } = cli.command else {
            panic!("not a start");
        };
        assert_eq!(firmware, Some(vm_core::machine::Firmware::Bios));
        assert_eq!(cpu.as_deref(), Some("max"));
    }

    #[test]
    fn run_and_start_take_a_chipset_and_disk_controller() {
        use clap::Parser as _;
        let cli =
            Cli::try_parse_from(["vm", "run", "x", "--machine", "pc", "--disk", "ide"]).unwrap();
        let Command::Run { machine, disk, .. } = cli.command else {
            panic!("not a run");
        };
        assert_eq!(machine, Some(vm_core::machine::Chipset::Pc));
        assert_eq!(disk, Some(vm_core::machine::Disk::Ide));
        let cli = Cli::try_parse_from(["vm", "start", "x", "--disk", "sata"]).unwrap();
        let Command::Start { machine, disk, .. } = cli.command else {
            panic!("not a start");
        };
        assert_eq!(machine, None);
        assert_eq!(disk, Some(vm_core::machine::Disk::Sata));
    }

    #[test]
    fn unusable_machine_settings_are_refused_by_the_parser() {
        use clap::Parser as _;
        assert!(Cli::try_parse_from(["vm", "run", "x", "--firmware", "coreboot"]).is_err());
        assert!(Cli::try_parse_from(["vm", "run", "x", "--cpu", "max -S"]).is_err());
        assert!(Cli::try_parse_from(["vm", "start", "x", "--cpu", ""]).is_err());
        assert!(Cli::try_parse_from(["vm", "run", "x", "--machine", "isapc"]).is_err());
        assert!(Cli::try_parse_from(["vm", "start", "x", "--disk", "scsi"]).is_err());
    }

    #[test]
    fn ssh_configuration_is_asked_for_turned_off_or_left_alone() {
        use clap::Parser as _;
        let asked = |arguments: &[&str]| match Cli::try_parse_from(arguments).unwrap().command {
            Command::Run {
                add_ssh_config,
                no_ssh_config,
                ..
            }
            | Command::Start {
                add_ssh_config,
                no_ssh_config,
                ..
            } => toggle(add_ssh_config, no_ssh_config),
            _ => panic!("neither a run nor a start"),
        };
        assert_eq!(asked(&["vm", "run", "x", "--add-ssh-config"]), Some(true));
        assert_eq!(asked(&["vm", "run", "x", "--no-ssh-config"]), Some(false));
        assert_eq!(asked(&["vm", "run", "x"]), None);
        assert_eq!(asked(&["vm", "start", "x", "--add-ssh-config"]), Some(true));
        assert_eq!(asked(&["vm", "start", "x", "--no-ssh-config"]), Some(false));
        assert_eq!(asked(&["vm", "start", "x"]), None);
        assert!(
            Cli::try_parse_from(["vm", "run", "x", "--add-ssh-config", "--no-ssh-config"]).is_err()
        );
    }

    #[test]
    fn a_run_without_user_leaves_the_account_to_the_config_file() {
        use clap::Parser as _;
        let user = |arguments: &[&str]| match Cli::try_parse_from(arguments).unwrap().command {
            Command::Run { user, .. } => user,
            _ => panic!("not a run"),
        };
        assert_eq!(user(&["vm", "run", "x"]), None);
        assert_eq!(
            user(&["vm", "run", "x", "--user", "dave"]),
            Some("dave".to_owned())
        );
        assert!(Cli::try_parse_from(["vm", "run", "x", "--user", "Bad Name"]).is_err());
    }

    #[test]
    fn images_can_be_limited_to_one_origin() {
        use clap::Parser as _;
        let origin = |arguments: &[&str]| match Cli::try_parse_from(arguments).unwrap().command {
            Command::Images { local, remote, .. } => Origin::of(local, remote),
            _ => panic!("not images"),
        };
        assert_eq!(origin(&["vm", "images"]), Origin::All);
        assert_eq!(origin(&["vm", "images", "--local"]), Origin::Local);
        assert_eq!(origin(&["vm", "images", "--remote"]), Origin::Remote);
        assert!(Cli::try_parse_from(["vm", "images", "--local", "--remote"]).is_err());
    }

    #[test]
    fn each_origin_reads_only_its_own_catalogue() {
        let fetched = std::path::PathBuf::from("/data/vm/catalogue");
        let local = std::path::PathBuf::from("/data/vm/local");
        let layers = |origin| origin_layers(origin, fetched.clone(), Some(local.clone()));
        assert_eq!(layers(Origin::All), vec![fetched.clone(), local.clone()]);
        assert_eq!(layers(Origin::Local), vec![local.clone()]);
        assert_eq!(layers(Origin::Remote), vec![fetched.clone()]);
        assert!(origin_layers(Origin::Local, fetched.clone(), None).is_empty());
        assert_eq!(
            origin_layers(Origin::All, fetched.clone(), None),
            vec![fetched]
        );
    }

    #[test]
    fn a_clone_description_is_optional() {
        use clap::Parser as _;
        let description = |arguments: &[&str]| match Cli::try_parse_from(arguments).unwrap().command
        {
            Command::Clone { description, .. } => description,
            _ => panic!("not a clone"),
        };
        assert_eq!(description(&["vm", "clone", "a", "b:c"]), None);
        assert_eq!(
            description(&["vm", "clone", "a", "b:c", "--description", "With tools"]),
            Some("With tools".to_owned())
        );
        assert!(Cli::try_parse_from(["vm", "clone", "a", "b:c", "--description", ""]).is_err());
    }

    #[test]
    fn the_command_line_is_consistent() {
        use clap::CommandFactory as _;
        Cli::command().debug_assert();
    }
}

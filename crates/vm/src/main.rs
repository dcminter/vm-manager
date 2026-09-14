mod completion;
mod configuration;
mod console;
mod machines;
mod output;
mod progress;
mod reports;
mod style;
mod table;
mod terminal;
mod units;

use clap::{Parser, Subcommand};
use clap_complete::engine::{ArgValueCandidates, ArgValueCompleter};
use output::{Batch, Format, Report};
use reports::Origin;
use std::process::ExitCode;
use style::Style;
use vm_core::catalogue::{Catalogue, Source};
use vm_core::machines::{Changes, Request};
use vm_core::settings;
use vm_core::store::Store;
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
        /// Show only images from local catalogues
        #[arg(long, conflicts_with = "remote")]
        local: bool,
        /// Show only images from remote catalogues
        #[arg(long)]
        remote: bool,
        /// Show only images from the named catalogue
        #[arg(long, add = ArgValueCandidates::new(completion::catalogue_name))]
        catalogue: Option<String>,
    },
    /// Show an instance, or what an image reference resolves to
    Inspect {
        /// Instance name, or image reference such as debian:trixie
        #[arg(add = ArgValueCandidates::new(completion::inspectable))]
        reference: String,
        /// Look only for an instance or only for an image; an instance is otherwise preferred
        #[arg(long = "type", value_enum, value_name = "TYPE")]
        kind: Option<InspectType>,
        /// Architecture of the image build to show, instead of this host's
        #[arg(long, add = ArgValueCandidates::new(completion::architecture))]
        arch: Option<String>,
    },
    /// Fetch an image into the local store
    Pull {
        /// Image reference, such as debian:trixie
        #[arg(add = ArgValueCandidates::new(completion::catalogue_image))]
        reference: String,
    },
    /// Refresh the remote catalogues
    Update {
        /// Refresh only this remote catalogue
        #[arg(add = ArgValueCandidates::new(completion::remote_catalogue_name))]
        catalogue: Option<String>,
    },
    /// Create and start a virtual machine
    Run {
        /// Image reference, such as debian:trixie
        #[arg(add = ArgValueCandidates::new(completion::catalogue_image))]
        reference: String,
        /// Name for the instance; one is invented if this is omitted
        #[arg(long)]
        name: Option<String>,
        /// Memory, in mebibytes unless suffixed with M or G
        #[arg(long, short, default_value = "2G", value_parser = settings::parse_memory)]
        memory: u64,
        /// Processors
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u32).range(1..=255))]
        cpus: u32,
        /// Forward a host port to a guest port, as host:guest or address:host:guest; repeatable
        #[arg(long, short, alias = "publish", value_name = "[ADDRESS:]HOST:GUEST", value_parser = settings::parse_port)]
        port: Vec<vm_core::instance::Port>,
        /// Account to create in the guest; defaults to the config file's setting, or vm
        #[arg(long, short, value_parser = settings::parse_user)]
        user: Option<String>,
        /// Share a host directory with the guest, as host:guest, read-only as host:guest:ro; repeatable
        #[arg(long, short = 'v', value_name = "HOST:GUEST[:ro]", value_parser = settings::parse_share)]
        volume: Vec<vm_core::instance::Share>,
        /// Grow the disk to this size, such as 40G
        #[arg(long)]
        disk_size: Option<String>,
        /// When to fetch the image
        #[arg(long, value_enum)]
        pull: Option<Pull>,
        /// Firmware, bios or uefi, instead of what the image asks for
        #[arg(long, value_parser = settings::parse_firmware, add = ArgValueCandidates::new(completion::firmware))]
        firmware: Option<vm_core::machine::Firmware>,
        /// QEMU CPU model instead of what the image asks for, such as Penryn,+avx
        #[arg(long, value_parser = settings::parse_cpu_model)]
        cpu_model: Option<String>,
        /// Remove the instance once it stops
        #[arg(long = "rm")]
        auto_remove: bool,
        /// Machine type, q35 or pc, instead of what the image asks for
        #[arg(long, value_parser = settings::parse_machine, add = ArgValueCandidates::new(completion::chipset))]
        machine: Option<vm_core::machine::Chipset>,
        /// Disk controller, virtio, ide (pc only) or sata (q35 only), instead of what the image asks for
        #[arg(long, value_parser = settings::parse_disk, add = ArgValueCandidates::new(completion::disk))]
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
        #[arg(long, short, value_parser = settings::parse_memory)]
        memory: Option<u64>,
        /// Processors
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=255))]
        cpus: Option<u32>,
        /// Forward a host port to a guest port, as host:guest or address:host:guest, replacing existing forwards; repeatable
        #[arg(long, short, alias = "publish", value_name = "[ADDRESS:]HOST:GUEST", value_parser = settings::parse_port, conflicts_with = "no_port")]
        port: Vec<vm_core::instance::Port>,
        /// Forward nothing
        #[arg(long, alias = "no-publish")]
        no_port: bool,
        /// Share a host directory with the guest, as host:guest or host:guest:ro, replacing existing shares; repeatable
        #[arg(long, short = 'v', value_name = "HOST:GUEST[:ro]", value_parser = settings::parse_share, conflicts_with = "no_volume")]
        volume: Vec<vm_core::instance::Share>,
        /// Share nothing
        #[arg(long)]
        no_volume: bool,
        /// Account to use in the guest; the one it has is left in place
        #[arg(long, short, value_parser = settings::parse_user)]
        user: Option<String>,
        /// Grow the disk to this size, such as 40G
        #[arg(long)]
        disk_size: Option<String>,
        /// Firmware, bios or uefi; a disk prepared for only the other will not boot
        #[arg(long, value_parser = settings::parse_firmware, add = ArgValueCandidates::new(completion::firmware))]
        firmware: Option<vm_core::machine::Firmware>,
        /// QEMU CPU model, such as Penryn,+avx
        #[arg(long, value_parser = settings::parse_cpu_model)]
        cpu_model: Option<String>,
        /// Machine type, q35 or pc
        #[arg(long, value_parser = settings::parse_machine, add = ArgValueCandidates::new(completion::chipset))]
        machine: Option<vm_core::machine::Chipset>,
        /// Disk controller, virtio, ide (pc only) or sata (q35 only); a guest without its driver will not boot
        #[arg(long, value_parser = settings::parse_disk, add = ArgValueCandidates::new(completion::disk))]
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
        /// Take the CD-ROM out of the drive, so the machine no longer needs its image
        #[arg(long)]
        eject: bool,
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
        #[arg(long, conflicts_with = "quiet")]
        follow: bool,
        /// List only their names
        #[arg(long, short)]
        quiet: bool,
    },
    /// Shut instances down
    Stop {
        /// Instance names
        #[arg(required = true, add = ArgValueCandidates::new(completion::running_instance))]
        names: Vec<String>,
        /// Seconds to wait for each guest before insisting
        #[arg(long, short, default_value_t = 30)]
        timeout: u64,
    },
    /// Stop instances' processors without telling the guests
    Pause {
        /// Instance names
        #[arg(required = true, add = ArgValueCandidates::new(completion::running_instance))]
        names: Vec<String>,
    },
    /// Let paused instances carry on
    #[command(alias = "resume")]
    Unpause {
        /// Instance names
        #[arg(required = true, add = ArgValueCandidates::new(completion::running_instance))]
        names: Vec<String>,
    },
    /// Stop instances without telling the guests
    Kill {
        /// Instance names
        #[arg(required = true, add = ArgValueCandidates::new(completion::running_instance))]
        names: Vec<String>,
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
        #[arg(long, short = 'm', value_parser = settings::parse_description)]
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
        /// Show only this many of the last lines
        #[arg(long = "tail", short = 'n', value_name = "LINES")]
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
    /// Bring an image file or download into the store
    Import {
        /// Image file, or http or https URL, compressed or not
        #[arg(value_parser = settings::parse_source, value_hint = clap::ValueHint::AnyPath)]
        source: vm_core::import::Source,
        /// Name for the image, as repository:tag
        image: String,
        /// Description for the image, instead of naming the source
        #[arg(long, short = 'm', value_parser = settings::parse_description)]
        description: Option<String>,
        /// How the guest is reached: cloud-init, or none for the console only
        #[arg(long, default_value = "none", value_parser = settings::parse_login, add = ArgValueCandidates::new(completion::login))]
        login: vm_core::catalogue::Login,
        /// Architecture of the image, instead of this host's
        #[arg(long, value_parser = settings::parse_arch, add = ArgValueCandidates::new(completion::known_architecture))]
        arch: Option<String>,
        /// Firmware the image needs, bios or uefi
        #[arg(long, value_parser = settings::parse_firmware, add = ArgValueCandidates::new(completion::firmware))]
        firmware: Option<vm_core::machine::Firmware>,
        /// QEMU CPU model the image needs, such as Penryn,+avx
        #[arg(long, value_parser = settings::parse_cpu_model)]
        cpu_model: Option<String>,
        /// Machine type the image needs, q35 or pc
        #[arg(long, value_parser = settings::parse_machine, add = ArgValueCandidates::new(completion::chipset))]
        machine: Option<vm_core::machine::Chipset>,
        /// Disk controller the image needs, virtio, ide (pc only) or sata (q35 only)
        #[arg(long, value_parser = settings::parse_disk, add = ArgValueCandidates::new(completion::disk))]
        disk: Option<vm_core::machine::Disk>,
        /// Digest the source must match as published, such as sha256:...
        #[arg(long, value_parser = settings::parse_digest)]
        digest: Option<vm_core::reference::Digest>,
        /// Keep no URL, so the image cannot be fetched again
        #[arg(long)]
        forget_url: bool,
        /// Replace an image of the same name made by vm clone or vm import
        #[arg(long, short)]
        force: bool,
    },
    /// Copy an image from the local store to a file
    Export {
        /// Image reference, such as debian:trixie
        #[arg(add = ArgValueCandidates::new(completion::held_image))]
        reference: String,
        /// File to write, or a directory to write it in
        #[arg(value_hint = clap::ValueHint::AnyPath)]
        file: std::path::PathBuf,
        /// Architecture of the build to export, instead of this host's
        #[arg(long, add = ArgValueCandidates::new(completion::architecture))]
        arch: Option<String>,
        /// Compress the file with xz, or with gzip or zstd if named
        #[arg(long, value_name = "SCHEME", num_args = 0..=1, default_missing_value = "xz", value_parser = settings::parse_compression, add = ArgValueCandidates::new(completion::compression))]
        compress: Option<vm_core::compression::Compression>,
        /// Replace the file if it exists
        #[arg(long, short)]
        force: bool,
    },
    /// Delete images from the local store
    Rmi {
        /// Image references, such as debian:trixie
        #[arg(required = true, add = ArgValueCandidates::new(completion::held_image))]
        references: Vec<String>,
        /// Delete them even though machines need them
        #[arg(long, short)]
        force: bool,
    },
    /// Show the config file, or change a setting or catalogue in it
    Config {
        #[command(subcommand)]
        action: Option<configuration::Action>,
    },
    /// Remove machines and images nothing needs
    Prune {
        /// Prune only machines or only images
        #[arg(value_enum)]
        target: Option<PruneTarget>,
        /// Also remove every stopped machine and every pulled image no machine uses
        #[arg(long, short)]
        all: bool,
        /// Show what would be removed, and remove nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Open the graphical front end, vmg
    Gui,
    /// Delete instances and their disks
    Rm {
        /// Instance names
        #[arg(required = true, add = ArgValueCandidates::new(completion::any_instance))]
        names: Vec<String>,
        /// Remove them even if they are running
        #[arg(long, short)]
        force: bool,
    },
}

/// Which kind of thing `vm inspect` looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum InspectType {
    Instance,
    Image,
}

fn main() -> ExitCode {
    clap_complete::CompleteEnv::with_factory(<Cli as clap::CommandFactory>::command)
        .shells(completion::SHELLS)
        .complete();
    let cli = Cli::parse();
    let style = if cli.format.is_text() {
        Style::for_stdout()
    } else {
        Style::plain()
    };
    match run(&cli, style) {
        Ok(Outcome::Written) => ExitCode::SUCCESS,
        Ok(Outcome::Reported(report)) => match output::emit(report.as_ref(), cli.format, style) {
            Ok(()) if report.succeeded() => ExitCode::SUCCESS,
            Ok(()) => ExitCode::FAILURE,
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

/// When `vm run` fetches the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Pull {
    /// Fetch the image even if it is already held.
    Always,
    /// Fetch it only if it is not held (default).
    Missing,
    /// Never fetch; a missing image is an error.
    Never,
}

impl Pull {
    const fn plain(self) -> vm_core::machines::Pull {
        match self {
            Self::Always => vm_core::machines::Pull::Always,
            Self::Missing => vm_core::machines::Pull::Missing,
            Self::Never => vm_core::machines::Pull::Never,
        }
    }
}

/// What `vm prune` removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum PruneTarget {
    Machines,
    Images,
}

impl PruneTarget {
    const fn plain(self) -> vm_core::machines::PruneTarget {
        match self {
            Self::Machines => vm_core::machines::PruneTarget::Machines,
            Self::Images => vm_core::machines::PruneTarget::Images,
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
    if let Command::Config { action } = &cli.command {
        return configuration::run(action.as_ref()).map(Outcome::Reported);
    }
    if let Some(outcome) = machine_command(cli, style)? {
        return Ok(outcome);
    }
    catalogue_command(cli, style).map(Outcome::Reported)
}

/// What a `vm run` asks for.
fn run_request(command: &Command) -> vm_core::Result<Request> {
    let Command::Run {
        reference,
        name,
        memory,
        cpus,
        port,
        user,
        volume,
        disk_size,
        pull,
        firmware,
        cpu_model,
        auto_remove,
        machine,
        disk,
        password,
        add_ssh_config,
        no_ssh_config,
    } = command
    else {
        unreachable!("not a run");
    };
    Ok(Request {
        reference: reference.clone(),
        name: name.clone(),
        memory: *memory,
        cpus: *cpus,
        ports: port.clone(),
        user: user.clone(),
        shares: volume.clone(),
        disk_size: disk_size.clone(),
        pull: pull.map(Pull::plain),
        firmware: *firmware,
        cpu_model: cpu_model.clone(),
        machine: *machine,
        disk: *disk,
        password: asked_password(*password)?,
        ssh_config: toggle(*add_ssh_config, *no_ssh_config),
        auto_remove: *auto_remove,
    })
}

/// The changes a `vm start` asks for.
fn start_changes(command: &Command) -> vm_core::Result<Changes> {
    let Command::Start {
        memory,
        cpus,
        port,
        no_port,
        volume,
        no_volume,
        user,
        disk_size,
        firmware,
        cpu_model,
        machine,
        disk,
        password,
        no_password,
        add_ssh_config,
        no_ssh_config,
        eject,
        ..
    } = command
    else {
        return Ok(Changes::default());
    };
    Ok(Changes {
        machine: *machine,
        disk: *disk,
        password: if *no_password {
            Some("*".to_owned())
        } else {
            asked_password(*password)?
        },
        memory: *memory,
        cpus: *cpus,
        ports: replacement(port, *no_port),
        shares: replacement(volume, *no_volume),
        user: user.clone(),
        disk_size: disk_size.clone(),
        firmware: *firmware,
        cpu_model: cpu_model.clone(),
        ssh_config: toggle(*add_ssh_config, *no_ssh_config),
        eject: *eject,
    })
}

/// Runs a command that needs no catalogue or store; `None` if this is not one.
fn machine_command(cli: &Cli, style: Style) -> vm_core::Result<Option<Outcome>> {
    let report: Box<dyn Report> = match &cli.command {
        Command::Ps { all, follow, quiet } => {
            if *follow {
                machines::watch(*all, cli.format, style)?;
                return Ok(Some(Outcome::Written));
            }
            listing(*all, *quiet)?
        }
        Command::Inspect {
            reference,
            kind,
            arch,
        } if inspects_instance(reference, *kind, arch.is_some()) => {
            Box::new(vm_core::machines::inspect(reference)?)
        }
        Command::Start { name, .. } => Box::new(vm_core::machines::start(
            name,
            &start_changes(&cli.command)?,
        )?),
        Command::Logs {
            name,
            follow,
            lines,
        } => {
            if *follow {
                if !cli.format.is_text() {
                    return Err(vm_core::Error::FollowNeedsText);
                }
                machines::follow(name, *lines, style)?;
                return Ok(Some(Outcome::Written));
            }
            Box::new(vm_core::machines::logs(name, *lines)?)
        }
        Command::Gui => {
            // Either this replaces the process or it reports why it could not.
            return Err(machines::gui());
        }
        Command::Console { name } => {
            if !cli.format.is_text() {
                return Err(vm_core::Error::ConsoleNeedsText);
            }
            console::attach(name)?;
            return Ok(Some(Outcome::Written));
        }
        Command::Screen { name } => match vm_core::screen::show(name, cli.format.is_text())? {
            Some(report) => Box::new(report),
            None => return Ok(Some(Outcome::Written)),
        },
        Command::Screenshot { name, file } => {
            Box::new(vm_core::screen::capture(name, file.as_deref())?)
        }
        Command::Ssh { name, command } => {
            // Either this replaces the process or it reports why it could not.
            return machines::connect(name, command).map(|held| match held {});
        }
        Command::Cp { from, to } => {
            return machines::copy(from, to).map(|held| match held {});
        }
        Command::Stop { names, timeout } => Batch::each(names, |name| {
            Ok(Box::new(vm_core::machines::stop(
                name,
                std::time::Duration::from_secs(*timeout),
                false,
                &mut progress::observer(cli.format.is_text(), style),
            )?))
        })?,
        Command::Pause { names } => {
            Batch::each(names, |name| Ok(Box::new(vm_core::machines::pause(name)?)))?
        }
        Command::Unpause { names } => Batch::each(names, |name| {
            Ok(Box::new(vm_core::machines::unpause(name)?))
        })?,
        Command::Kill { names } => Batch::each(names, |name| {
            Ok(Box::new(vm_core::machines::stop(
                name,
                std::time::Duration::from_secs(10),
                true,
                &mut progress::observer(cli.format.is_text(), style),
            )?))
        })?,
        Command::Rm { names, force } => Batch::each(names, |name| {
            Ok(Box::new(vm_core::machines::remove(name, *force)?))
        })?,
        Command::Clone {
            name,
            image,
            force,
            description,
        } => Box::new(vm_core::machines::clone(
            name,
            image,
            description.as_deref(),
            *force,
            &mut progress::observer(cli.format.is_text(), style),
        )?),
        _ => return Ok(None),
    };
    Ok(Some(Outcome::Reported(report)))
}

/// The rest, which need both.
fn catalogue_command(cli: &Cli, style: Style) -> vm_core::Result<Box<dyn Report>> {
    let config = vm_core::config::Config::load()?;
    let sources = paths::catalogue_sources(&config);
    let (origin, named) = match &cli.command {
        Command::Images {
            local,
            remote,
            catalogue,
            ..
        } => (Origin::of(*local, *remote), catalogue.as_deref()),
        _ => (Origin::All, None),
    };
    let catalogue =
        Catalogue::load_layered(&vm_core::images::select_sources(sources, origin, named)?)?;
    let store = Store::discover()?;
    match &cli.command {
        Command::Images {
            all_architectures, ..
        } => Ok(Box::new(vm_core::images::list(
            &catalogue,
            &store,
            *all_architectures,
            origin,
        ))),
        Command::Inspect {
            reference, arch, ..
        } => Ok(Box::new(vm_core::images::inspect(
            &catalogue,
            &store,
            reference,
            arch.as_deref().unwrap_or(host_architecture()),
        )?)),
        Command::Pull { reference } => Ok(Box::new(vm_core::images::pull(
            &catalogue,
            &store,
            reference,
            &mut progress::observer(cli.format.is_text(), style),
        )?)),
        Command::Update { catalogue } => Ok(Box::new(vm_core::images::update(
            &config,
            catalogue.as_deref(),
        )?)),
        Command::Import { .. } => Ok(Box::new(import(cli, &catalogue, &store, style)?)),
        Command::Export {
            reference,
            file,
            arch,
            compress,
            force,
        } => Ok(Box::new(vm_core::images::export(
            &catalogue,
            &store,
            &vm_core::images::ExportRequest {
                reference,
                file,
                arch: arch.as_deref().unwrap_or(host_architecture()),
                compression: compress.unwrap_or_default(),
                force: *force,
            },
            &mut progress::observer(cli.format.is_text(), style),
        )?)),
        Command::Rmi { references, force } => Batch::each(references, |reference| {
            Ok(Box::new(vm_core::machines::remove_image(
                &catalogue, &store, reference, *force,
            )?))
        }),
        Command::Prune {
            target,
            all,
            dry_run,
        } => Ok(Box::new(vm_core::machines::prune(
            &catalogue,
            &store,
            target.map(PruneTarget::plain),
            *all,
            *dry_run,
        )?)),
        Command::Run { .. } => Ok(Box::new(vm_core::machines::run(
            &catalogue,
            &store,
            &run_request(&cli.command)?,
            &mut progress::observer(cli.format.is_text(), style),
        )?)),
        Command::Ps { .. }
        | Command::Start { .. }
        | Command::Ssh { .. }
        | Command::Cp { .. }
        | Command::Stop { .. }
        | Command::Kill { .. }
        | Command::Pause { .. }
        | Command::Unpause { .. }
        | Command::Clone { .. }
        | Command::Logs { .. }
        | Command::Console { .. }
        | Command::Gui
        | Command::Screen { .. }
        | Command::Screenshot { .. }
        | Command::Config { .. }
        | Command::Rm { .. } => unreachable!("handled above"),
    }
}

fn import(
    cli: &Cli,
    catalogue: &Catalogue,
    store: &Store,
    style: Style,
) -> vm_core::Result<reports::Imported> {
    let Command::Import {
        source,
        image,
        description,
        login,
        arch,
        firmware,
        cpu_model,
        machine,
        disk,
        digest,
        forget_url,
        force,
    } = &cli.command
    else {
        unreachable!("not an import");
    };
    let hardware = vm_core::import::Hardware {
        firmware: firmware.unwrap_or_default(),
        cpu_model: cpu_model.clone(),
        machine: machine.unwrap_or_default(),
        disk: disk.unwrap_or_default(),
    };
    vm_core::machine::check_disk(hardware.machine, hardware.disk)?;
    let reference: Reference = image.parse()?;
    vm_core::images::import(
        catalogue,
        store,
        &vm_core::import::Request {
            source,
            reference: &reference,
            arch: arch.as_deref().unwrap_or(host_architecture()),
            description: description.as_deref(),
            login: *login,
            hardware,
            digest: digest.as_ref(),
            fetchable: !forget_url,
            force: *force,
        },
        &mut progress::observer(cli.format.is_text(), style),
    )
}

/// The instances `vm ps` lists, in full or by name alone.
fn listing(all: bool, quiet: bool) -> vm_core::Result<Box<dyn Report>> {
    let listed = vm_core::machines::list(all)?;
    Ok(if quiet {
        Box::new(reports::Names(
            listed.rows.into_iter().map(|row| row.name).collect(),
        ))
    } else {
        Box::new(listed)
    })
}

/// Whether `vm inspect` shows an instance rather than an image.
fn inspects_instance(reference: &str, kind: Option<InspectType>, arch: bool) -> bool {
    match kind {
        Some(InspectType::Instance) => true,
        Some(InspectType::Image) => false,
        None => !arch && vm_core::machines::exists(reference).unwrap_or(false),
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

/// Every catalogue the config names, lowest precedence first.
fn catalogue_sources() -> vm_core::Result<Vec<Source>> {
    Ok(paths::catalogue_sources(&vm_core::config::Config::load()?))
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
        let outcome = Cli::try_parse_from(["vm", "start", "one", "-p", "80:80", "--no-port"]);
        assert!(outcome.is_err());
        let outcome = Cli::try_parse_from(["vm", "start", "one", "-v", "/tmp:/mnt", "--no-volume"]);
        assert!(outcome.is_err());
    }

    #[test]
    fn a_listing_can_be_followed_and_can_include_what_is_stopped() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from(["vm", "ps", "-a", "--follow"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Ps {
                all: true,
                follow: true,
                quiet: false
            }
        ));
        let cli = Cli::try_parse_from(["vm", "ps"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Ps {
                all: false,
                follow: false,
                quiet: false
            }
        ));
    }

    #[test]
    fn a_listing_can_name_only_the_instances_but_not_while_following() {
        use clap::Parser as _;
        for quiet in ["-q", "--quiet"] {
            let cli = Cli::try_parse_from(["vm", "ps", "-a", quiet]).unwrap();
            assert!(
                matches!(
                    cli.command,
                    Command::Ps {
                        all: true,
                        follow: false,
                        quiet: true
                    }
                ),
                "{quiet}"
            );
        }
        assert!(Cli::try_parse_from(["vm", "ps", "-q", "--follow"]).is_err());
        assert!(
            Cli::try_parse_from(["vm", "ps", "-f"]).is_err(),
            "-f is not short for --follow"
        );
    }

    #[test]
    fn ports_are_given_with_port_and_still_accepted_with_publish() {
        use clap::Parser as _;
        use vm_core::instance::Port;
        let ports = |arguments: &[&str]| match Cli::try_parse_from(arguments).unwrap().command {
            Command::Run { port, .. } | Command::Start { port, .. } => port,
            _ => panic!("neither a run nor a start"),
        };
        let expected = vec![
            Port::new(8080, 80),
            Port {
                address: Some(std::net::Ipv4Addr::UNSPECIFIED),
                host: 8443,
                guest: 443,
            },
        ];
        for arguments in [
            ["vm", "run", "x", "-p", "8080:80", "-p", "0.0.0.0:8443:443"],
            [
                "vm",
                "run",
                "x",
                "--port",
                "8080:80",
                "--port",
                "0.0.0.0:8443:443",
            ],
            [
                "vm",
                "run",
                "x",
                "--publish",
                "8080:80",
                "-p",
                "0.0.0.0:8443:443",
            ],
            [
                "vm",
                "start",
                "x",
                "--port",
                "8080:80",
                "--publish",
                "0.0.0.0:8443:443",
            ],
        ] {
            assert_eq!(ports(&arguments), expected, "{arguments:?}");
        }
        assert!(Cli::try_parse_from(["vm", "run", "x", "-p", "8080"]).is_err());
        let cleared = |arguments: &[&str]| match Cli::try_parse_from(arguments).unwrap().command {
            command @ Command::Start { .. } => start_changes(&command).unwrap().ports,
            _ => panic!("not a start"),
        };
        assert_eq!(
            cleared(&["vm", "start", "x", "--no-port"]),
            Some(Vec::new())
        );
        assert_eq!(
            cleared(&["vm", "start", "x", "--no-publish"]),
            Some(Vec::new())
        );
        assert_eq!(cleared(&["vm", "start", "x"]), None);
    }

    #[test]
    fn a_volume_can_be_read_only() {
        use clap::Parser as _;
        let directory = std::env::temp_dir().display().to_string();
        let first = format!("{directory}:/mnt/a:ro");
        let second = format!("{directory}:/mnt/b");
        let cli = Cli::try_parse_from(["vm", "run", "x", "-v", &first, "-v", &second]).unwrap();
        let Command::Run { volume, .. } = cli.command else {
            panic!("not a run");
        };
        assert_eq!(
            volume
                .iter()
                .map(|share| (share.target.as_str(), share.readonly))
                .collect::<Vec<_>>(),
            [("/mnt/a", true), ("/mnt/b", false)]
        );
    }

    #[test]
    fn a_run_can_ask_for_removal_once_stopped() {
        use clap::Parser as _;
        let removed = |arguments: &[&str]| match Cli::try_parse_from(arguments).unwrap().command {
            command @ Command::Run { .. } => run_request(&command).unwrap().auto_remove,
            _ => panic!("not a run"),
        };
        assert!(removed(&["vm", "run", "x", "--rm"]));
        assert!(!removed(&["vm", "run", "x"]));
        assert!(Cli::try_parse_from(["vm", "start", "x", "--rm"]).is_err());
    }

    #[test]
    fn the_user_has_a_short_form() {
        use clap::Parser as _;
        match Cli::try_parse_from(["vm", "run", "x", "-u", "dave"])
            .unwrap()
            .command
        {
            Command::Run { user, .. } => assert_eq!(user.as_deref(), Some("dave")),
            _ => panic!("not a run"),
        }
        match Cli::try_parse_from(["vm", "start", "x", "-u", "dave"])
            .unwrap()
            .command
        {
            Command::Start { user, .. } => assert_eq!(user.as_deref(), Some("dave")),
            _ => panic!("not a start"),
        }
    }

    #[test]
    fn the_old_cpu_flag_is_gone() {
        use clap::Parser as _;
        for command in ["run", "start", "import"] {
            let mut arguments = vec!["vm", command, "x"];
            if command == "import" {
                arguments.push("y:1");
            }
            arguments.extend(["--cpu", "max"]);
            assert!(Cli::try_parse_from(&arguments).is_err(), "{command}");
        }
    }

    #[test]
    fn commands_on_instances_take_several_names() {
        use clap::Parser as _;
        let names = |arguments: &[&str]| match Cli::try_parse_from(arguments).unwrap().command {
            Command::Stop { names, .. }
            | Command::Kill { names }
            | Command::Pause { names }
            | Command::Unpause { names }
            | Command::Rm { names, .. } => names,
            Command::Rmi { references, .. } => references,
            _ => panic!("not a command taking names"),
        };
        for command in ["stop", "kill", "pause", "unpause", "resume", "rm", "rmi"] {
            assert_eq!(names(&["vm", command, "one"]), ["one"], "{command}");
            assert_eq!(
                names(&["vm", command, "one", "two", "three"]),
                ["one", "two", "three"],
                "{command}"
            );
            assert!(Cli::try_parse_from(["vm", command]).is_err(), "{command}");
        }
        let cli = Cli::try_parse_from(["vm", "stop", "one", "two", "-t", "5"]).unwrap();
        assert!(matches!(cli.command, Command::Stop { timeout: 5, .. }));
        let cli = Cli::try_parse_from(["vm", "rm", "-f", "one", "two"]).unwrap();
        assert!(matches!(cli.command, Command::Rm { force: true, .. }));
    }

    #[test]
    fn a_description_has_a_short_form() {
        use clap::Parser as _;
        match Cli::try_parse_from(["vm", "clone", "one", "mine:1", "-m", "Mine"])
            .unwrap()
            .command
        {
            Command::Clone { description, .. } => assert_eq!(description.as_deref(), Some("Mine")),
            _ => panic!("not a clone"),
        }
        match Cli::try_parse_from(["vm", "import", "disk.vmdk", "mine:1", "-m", "Mine"])
            .unwrap()
            .command
        {
            Command::Import { description, .. } => {
                assert_eq!(description.as_deref(), Some("Mine"));
            }
            _ => panic!("not an import"),
        }
    }

    #[test]
    fn logs_take_a_tail_length() {
        use clap::Parser as _;
        let lines = |arguments: &[&str]| match Cli::try_parse_from(arguments).unwrap().command {
            Command::Logs { lines, .. } => lines,
            _ => panic!("not logs"),
        };
        assert_eq!(lines(&["vm", "logs", "one", "--tail", "5"]), Some(5));
        assert_eq!(lines(&["vm", "logs", "one", "-n", "7"]), Some(7));
        assert_eq!(lines(&["vm", "logs", "one"]), None);
        assert!(Cli::try_parse_from(["vm", "logs", "one", "--lines", "5"]).is_err());
    }

    #[test]
    fn inspect_can_be_told_what_to_look_for() {
        use clap::Parser as _;
        let kind = |arguments: &[&str]| match Cli::try_parse_from(arguments).unwrap().command {
            Command::Inspect { kind, .. } => kind,
            _ => panic!("not an inspect"),
        };
        assert_eq!(kind(&["vm", "inspect", "one"]), None);
        assert_eq!(
            kind(&["vm", "inspect", "one", "--type", "instance"]),
            Some(InspectType::Instance)
        );
        assert_eq!(
            kind(&["vm", "inspect", "debian", "--type", "image"]),
            Some(InspectType::Image)
        );
        assert!(Cli::try_parse_from(["vm", "inspect", "one", "--type", "container"]).is_err());
        assert!(inspects_instance(
            "anything",
            Some(InspectType::Instance),
            false
        ));
        assert!(!inspects_instance(
            "anything",
            Some(InspectType::Image),
            false
        ));
        assert!(
            !inspects_instance("anything", None, true),
            "an architecture is asked of an image"
        );
        assert!(!inspects_instance("debian:trixie", None, false));
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
            "--cpu-model",
            "Penryn,+avx",
        ])
        .unwrap();
        let Command::Run {
            firmware,
            cpu_model,
            ..
        } = cli.command
        else {
            panic!("not a run");
        };
        assert_eq!(firmware, Some(vm_core::machine::Firmware::Uefi));
        assert_eq!(cpu_model.as_deref(), Some("Penryn,+avx"));
    }

    #[test]
    fn run_leaves_the_machine_to_the_image_when_nothing_is_said() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from(["vm", "run", "debian:trixie"]).unwrap();
        let Command::Run {
            firmware,
            cpu_model,
            ..
        } = cli.command
        else {
            panic!("not a run");
        };
        assert_eq!(firmware, None);
        assert_eq!(cpu_model, None);
    }

    #[test]
    fn start_takes_machine_changes() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from([
            "vm",
            "start",
            "pd",
            "--firmware",
            "bios",
            "--cpu-model",
            "max",
        ])
        .unwrap();
        let Command::Start {
            firmware,
            cpu_model,
            ..
        } = cli.command
        else {
            panic!("not a start");
        };
        assert_eq!(firmware, Some(vm_core::machine::Firmware::Bios));
        assert_eq!(cpu_model.as_deref(), Some("max"));
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
        assert!(Cli::try_parse_from(["vm", "run", "x", "--cpu-model", "max -S"]).is_err());
        assert!(Cli::try_parse_from(["vm", "start", "x", "--cpu-model", ""]).is_err());
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
    fn images_and_update_take_a_catalogue_name() {
        use clap::Parser as _;
        match Cli::try_parse_from(["vm", "images", "--catalogue", "team"])
            .unwrap()
            .command
        {
            Command::Images { catalogue, .. } => assert_eq!(catalogue.as_deref(), Some("team")),
            _ => panic!("not images"),
        }
        match Cli::try_parse_from(["vm", "update"]).unwrap().command {
            Command::Update { catalogue } => assert_eq!(catalogue, None),
            _ => panic!("not an update"),
        }
        match Cli::try_parse_from(["vm", "update", "internal"])
            .unwrap()
            .command
        {
            Command::Update { catalogue } => assert_eq!(catalogue.as_deref(), Some("internal")),
            _ => panic!("not an update"),
        }
    }

    #[test]
    fn inspect_takes_an_architecture() {
        use clap::Parser as _;
        let arch = |arguments: &[&str]| match Cli::try_parse_from(arguments).unwrap().command {
            Command::Inspect { arch, .. } => arch,
            _ => panic!("not an inspect"),
        };
        assert_eq!(arch(&["vm", "inspect", "debian"]), None);
        assert_eq!(
            arch(&["vm", "inspect", "debian", "--arch", "arm64"]),
            Some("arm64".to_owned())
        );
    }

    #[test]
    fn prune_takes_an_optional_target() {
        use clap::Parser as _;
        let parsed = |arguments: &[&str]| match Cli::try_parse_from(arguments).unwrap().command {
            Command::Prune {
                target,
                all,
                dry_run,
            } => (target, all, dry_run),
            _ => panic!("not a prune"),
        };
        assert_eq!(parsed(&["vm", "prune"]), (None, false, false));
        assert_eq!(
            parsed(&["vm", "prune", "machines", "--all"]),
            (Some(PruneTarget::Machines), true, false)
        );
        assert_eq!(
            parsed(&["vm", "prune", "images", "--dry-run"]),
            (Some(PruneTarget::Images), false, true)
        );
        assert!(Cli::try_parse_from(["vm", "prune", "everything"]).is_err());
    }

    #[test]
    fn an_import_defaults_to_a_console_only_image_that_stays_fetchable() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from(["vm", "import", "https://example.invalid/x.vmdk", "mine:1"])
            .unwrap();
        let Command::Import {
            source,
            image,
            login,
            arch,
            digest,
            forget_url,
            force,
            firmware,
            ..
        } = cli.command
        else {
            panic!("not an import");
        };
        assert!(source.is_url());
        assert_eq!(image, "mine:1");
        assert_eq!(login, vm_core::catalogue::Login::None);
        assert_eq!(arch, None);
        assert_eq!(digest, None);
        assert!(!forget_url && !force);
        assert_eq!(firmware, None);
    }

    #[test]
    fn an_import_takes_what_the_image_needs() {
        use clap::Parser as _;
        let digest = format!("sha256:{}", "a".repeat(64));
        let cli = Cli::try_parse_from([
            "vm",
            "import",
            "disk.vmdk",
            "mine:1",
            "--login",
            "cloud-init",
            "--arch",
            "arm64",
            "--firmware",
            "uefi",
            "--machine",
            "pc",
            "--disk",
            "ide",
            "--cpu-model",
            "Penryn",
            "--digest",
            &digest,
            "--description",
            "Mine",
            "--forget-url",
            "--force",
        ])
        .unwrap();
        let Command::Import {
            source,
            login,
            arch,
            digest: given,
            forget_url,
            force,
            machine,
            disk,
            ..
        } = cli.command
        else {
            panic!("not an import");
        };
        assert_eq!(
            source,
            vm_core::import::Source::File(std::path::PathBuf::from("disk.vmdk"))
        );
        assert!(login.is_seedable());
        assert_eq!(arch.as_deref(), Some("arm64"));
        assert_eq!(given.unwrap().to_string(), digest);
        assert!(forget_url && force);
        assert_eq!(machine, Some(vm_core::machine::Chipset::Pc));
        assert_eq!(disk, Some(vm_core::machine::Disk::Ide));
        for bad in [
            ["--arch", "x86_64"],
            ["--login", "ssh"],
            ["--digest", "sha256:0"],
            ["--disk", "scsi"],
        ] {
            let mut arguments = vec!["vm", "import", "disk.vmdk", "mine:1"];
            arguments.extend(bad);
            assert!(Cli::try_parse_from(arguments).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn an_export_names_an_image_and_a_file() {
        use clap::Parser as _;
        let parsed = |arguments: &[&str]| match Cli::try_parse_from(arguments).unwrap().command {
            Command::Export {
                reference,
                file,
                arch,
                force,
                ..
            } => (reference, file, arch, force),
            _ => panic!("not an export"),
        };
        assert_eq!(
            parsed(&["vm", "export", "debian:trixie", "disk"]),
            (
                "debian:trixie".to_owned(),
                std::path::PathBuf::from("disk"),
                None,
                false
            )
        );
        assert_eq!(
            parsed(&["vm", "export", "debian", "d.qcow2", "--arch", "arm64", "-f"]).2,
            Some("arm64".to_owned())
        );
        assert!(Cli::try_parse_from(["vm", "export", "debian"]).is_err());
    }

    #[test]
    fn an_export_compresses_with_xz_unless_another_scheme_is_named() {
        use clap::Parser as _;
        use vm_core::compression::Compression;
        let compress = |arguments: &[&str]| match Cli::try_parse_from(arguments).unwrap().command {
            Command::Export { compress, file, .. } => (compress, file),
            _ => panic!("not an export"),
        };
        let disk = std::path::PathBuf::from("disk");
        assert_eq!(
            compress(&["vm", "export", "d", "disk"]),
            (None, disk.clone())
        );
        assert_eq!(
            compress(&["vm", "export", "d", "disk", "--compress"]),
            (Some(Compression::Xz), disk.clone())
        );
        assert_eq!(
            compress(&["vm", "export", "--compress", "gzip", "d", "disk"]),
            (Some(Compression::Gzip), disk.clone())
        );
        assert_eq!(
            compress(&["vm", "export", "d", "disk", "--compress=zstd"]),
            (Some(Compression::Zstd), disk)
        );
        assert!(Cli::try_parse_from(["vm", "export", "d", "disk", "--compress", "bzip2"]).is_err());
    }

    #[test]
    fn start_can_eject_the_cdrom() {
        use clap::Parser as _;
        let eject = |arguments: &[&str]| match Cli::try_parse_from(arguments).unwrap().command {
            command @ Command::Start { .. } => start_changes(&command).unwrap().eject,
            _ => panic!("not a start"),
        };
        assert!(eject(&["vm", "start", "one", "--eject"]));
        assert!(!eject(&["vm", "start", "one"]));
    }

    #[test]
    fn the_command_line_is_consistent() {
        use clap::CommandFactory as _;
        Cli::command().debug_assert();
    }
}

mod machines;
mod output;
mod progress;
mod reports;
mod style;
mod table;

use clap::{Parser, Subcommand};
use output::{Format, Report};
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
        short,
        global = true,
        value_enum,
        env = "VM_OUTPUT",
        default_value = "text"
    )]
    output: Format,

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
    },
    /// Show what an image reference resolves to
    Inspect {
        /// Image reference, such as debian:trixie
        reference: String,
    },
    /// Fetch an image into the local store
    Pull {
        /// Image reference, such as debian:trixie
        reference: String,
    },
    /// Refresh the local catalogue from its remote source
    Update,
    /// Create and start a virtual machine
    Run {
        /// Image reference, such as debian:trixie
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
        /// Account to create in the guest
        #[arg(long, default_value = "vm", value_parser = machines::parse_user)]
        user: String,
        /// Grow the disk to this size, such as 40G
        #[arg(long)]
        disk_size: Option<String>,
        /// When to fetch the image
        #[arg(long, value_enum)]
        pull: Option<machines::Pull>,
    },
    /// List instances
    Ps {
        /// Include instances that are not running
        #[arg(long, short)]
        all: bool,
    },
    /// Shut an instance down
    Stop {
        /// Instance name
        name: String,
        /// Seconds to wait for the guest before insisting
        #[arg(long, short, default_value_t = 30)]
        timeout: u64,
    },
    /// Stop an instance without telling the guest
    Kill {
        /// Instance name
        name: String,
    },
    /// Delete an instance and its disk
    Rm {
        /// Instance name
        name: String,
        /// Remove it even if it is running
        #[arg(long, short)]
        force: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let style = if cli.output.is_text() {
        Style::for_stdout()
    } else {
        Style::plain()
    };
    match run(&cli, style) {
        Ok(report) => match output::emit(report.as_ref(), cli.output, style) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) if output::is_closed_pipe(&error) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("vm: cannot write output: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            output::emit_error(&error, cli.output);
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli, style: Style) -> vm_core::Result<Box<dyn Report>> {
    // These commands read no catalogue and no store, so they work even when
    // neither is in place.
    match &cli.command {
        Command::Ps { all } => return Ok(Box::new(machines::list(*all)?)),
        Command::Stop { name, timeout } => {
            return Ok(Box::new(machines::stop(
                name,
                std::time::Duration::from_secs(*timeout),
                false,
            )?));
        }
        Command::Kill { name } => {
            return Ok(Box::new(machines::stop(
                name,
                std::time::Duration::from_secs(10),
                true,
            )?));
        }
        Command::Rm { name, force } => return Ok(Box::new(machines::remove(name, *force)?)),
        _ => {}
    }
    let catalogue = Catalogue::load(&paths::catalogue_directory())?;
    let store = Store::discover()?;
    match &cli.command {
        Command::Images { all_architectures } => {
            Ok(Box::new(images(&catalogue, &store, *all_architectures)))
        }
        Command::Inspect { reference } => Ok(Box::new(inspect(&catalogue, &store, reference)?)),
        Command::Pull { reference } => Ok(Box::new(pull(
            &catalogue, &store, style, cli.output, reference,
        )?)),
        Command::Update => Ok(Box::new(update()?)),
        Command::Run {
            reference,
            name,
            memory,
            cpus,
            publish,
            user,
            disk_size,
            pull,
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
                disk_size: disk_size.clone(),
                pull: *pull,
            },
            style,
            cli.output.is_text(),
        )?)),
        Command::Ps { .. } | Command::Stop { .. } | Command::Kill { .. } | Command::Rm { .. } => {
            unreachable!("handled above")
        }
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

fn images(catalogue: &Catalogue, store: &Store, all_architectures: bool) -> reports::Images {
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
                })
                .collect::<Vec<_>>()
        })
        .collect();
    reports::Images { rows }
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
        eprintln!("Fetching {}", style.name(&artifact.url));
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

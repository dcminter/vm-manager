mod machines;
mod output;
mod progress;
mod reports;
mod style;
mod table;
mod units;

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
        /// Share a host directory with the guest, as host:guest
        #[arg(long, short = 'v', value_parser = machines::parse_share)]
        volume: Vec<vm_core::instance::Share>,
        /// Grow the disk to this size, such as 40G
        #[arg(long)]
        disk_size: Option<String>,
        /// When to fetch the image
        #[arg(long, value_enum)]
        pull: Option<machines::Pull>,
    },
    /// Start an instance that is not running, changing how it is set up
    Start {
        /// Instance name
        name: String,
        /// Memory, in mebibytes unless suffixed with M or G
        #[arg(long, short, value_parser = machines::parse_memory)]
        memory: Option<u64>,
        /// Processors
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=255))]
        cpus: Option<u32>,
        /// Forward a host port to a guest port, as host:guest, replacing the
        /// forwards it had
        #[arg(long, short, value_parser = machines::parse_port, conflicts_with = "no_publish")]
        publish: Vec<vm_core::instance::Port>,
        /// Forward nothing
        #[arg(long)]
        no_publish: bool,
        /// Share a host directory with the guest, as host:guest, replacing the
        /// shares it had
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
    },
    /// Open a shell on an instance, or run a command in it
    Ssh {
        /// Instance name
        name: String,
        /// Command to run instead of a shell
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Copy files to or from an instance, naming one side as name:path
    Cp {
        /// Source, as a path or name:path
        from: String,
        /// Destination, as a path or name:path
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
        name: String,
        /// Seconds to wait for the guest before insisting
        #[arg(long, short, default_value_t = 30)]
        timeout: u64,
    },
    /// Stop an instance's processors without telling the guest
    Pause {
        /// Instance name
        name: String,
    },
    /// Let a paused instance carry on
    Resume {
        /// Instance name
        name: String,
    },
    /// Stop an instance without telling the guest
    Kill {
        /// Instance name
        name: String,
    },
    /// Save an instance's disk as a new image
    Clone {
        /// Instance name
        name: String,
        /// Name for the new image, as repository:tag
        image: String,
        /// Clone without pausing a running guest
        #[arg(long, short)]
        force: bool,
    },
    /// Show an instance's console
    Logs {
        /// Instance name
        name: String,
        /// Write new output as it arrives, until the instance stops
        #[arg(long, short)]
        follow: bool,
        /// Show only the last few lines
        #[arg(long, short = 'n')]
        lines: Option<usize>,
    },
    /// Delete an image from the local store
    Rmi {
        /// Image reference, such as debian:trixie
        reference: String,
        /// Delete it even though instances are built on it
        #[arg(long, short)]
        force: bool,
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

/// A list given on the command line replaces what an instance had; asking for
/// nothing is different from saying nothing.
fn replacement<T: Clone>(given: &[T], none: bool) -> Option<Vec<T>> {
    if none {
        Some(Vec::new())
    } else if given.is_empty() {
        None
    } else {
        Some(given.to_vec())
    }
}

/// What a command left behind: something to write out, or nothing, because it
/// wrote as it went and there is no last word to add.
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

/// The commands that read no catalogue and no store, so they work even when
/// neither is in place. `None` means this was not one of them.
fn machine_command(cli: &Cli, style: Style) -> vm_core::Result<Option<Outcome>> {
    let report: Box<dyn Report> = match &cli.command {
        Command::Ps { all, follow } => {
            if *follow {
                machines::watch(*all, cli.format, style)?;
                return Ok(Some(Outcome::Written));
            }
            Box::new(machines::list(*all)?)
        }
        Command::Start {
            name,
            memory,
            cpus,
            publish,
            no_publish,
            volume,
            no_volume,
            user,
            disk_size,
        } => {
            let changes = machines::Changes {
                memory: *memory,
                cpus: *cpus,
                ports: replacement(publish, *no_publish),
                shares: replacement(volume, *no_volume),
                user: user.clone(),
                disk_size: disk_size.clone(),
            };
            Box::new(machines::start(name, &changes)?)
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
        Command::Clone { name, image, force } => {
            Box::new(machines::clone(name, image, *force, cli.format.is_text())?)
        }
        _ => return Ok(None),
    };
    Ok(Some(Outcome::Reported(report)))
}

/// The rest, which need both.
fn catalogue_command(cli: &Cli, style: Style) -> vm_core::Result<Box<dyn Report>> {
    let catalogue = Catalogue::load_layered(&catalogue_layers())?;
    let store = Store::discover()?;
    match &cli.command {
        Command::Images { all_architectures } => {
            Ok(Box::new(images(&catalogue, &store, *all_architectures)))
        }
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
        | Command::Rm { .. } => unreachable!("handled above"),
    }
}

/// The catalogue as read: what was fetched, with anything cloned here
/// layered over it.
fn catalogue_layers() -> Vec<std::path::PathBuf> {
    let mut layers = vec![paths::catalogue_directory()];
    layers.extend(paths::local_catalogue_directory());
    layers
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
                    size: artifact.size.or_else(|| held_size(store, artifact)),
                })
                .collect::<Vec<_>>()
        })
        .collect();
    reports::Images { rows }
}

/// What a held build takes, for an entry that does not say.
///
/// The catalogue is the authority on what a pull will cost, because it can
/// answer for an image nobody has fetched. Once one is here, the file itself
/// answers, and it is the same number.
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

    /// Saying nothing leaves an instance as it was. Saying "none" is a change
    /// like any other, and the two must not look the same.
    #[test]
    fn nothing_given_is_different_from_nothing_wanted() {
        let given = [1, 2];
        assert_eq!(replacement(&given, false), Some(vec![1, 2]));
        assert_eq!(replacement::<i32>(&[], false), None);
        assert_eq!(replacement::<i32>(&[], true), Some(Vec::new()));
    }

    /// The flags conflict, so this is the parser's job rather than a rule the
    /// reader has to know: --no-publish wins only because it cannot be given
    /// alongside a forward.
    #[test]
    fn a_list_and_its_refusal_cannot_be_asked_for_together() {
        use clap::Parser as _;
        let outcome = Cli::try_parse_from(["vm", "start", "one", "-p", "80:80", "--no-publish"]);
        assert!(outcome.is_err());
        let outcome = Cli::try_parse_from(["vm", "start", "one", "-v", "/tmp:/mnt", "--no-volume"]);
        assert!(outcome.is_err());
    }

    /// Following a listing is not the same as listing everything, and the two
    /// have to be combinable: a display of what is there includes what is not
    /// running.
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

    /// Unlike `vm logs --follow`, this one streams a document as readily as a
    /// table, so no format is refused.
    #[test]
    fn a_followed_listing_is_not_refused_a_document_format() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from(["vm", "ps", "--follow", "--format", "json"]).unwrap();
        assert_eq!(cli.format, Format::Json);
    }

    /// Every command is reachable, and none of them collides with another over
    /// a short flag.
    #[test]
    fn the_command_line_is_consistent() {
        use clap::CommandFactory as _;
        Cli::command().debug_assert();
    }
}

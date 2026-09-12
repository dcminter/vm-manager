mod output;
mod progress;
mod reports;
mod style;
mod table;
mod value;

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
    }
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

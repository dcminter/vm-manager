mod progress;
mod style;
mod table;

use clap::{Parser, Subcommand};
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
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("vm: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> vm_core::Result<()> {
    let style = Style::for_stdout();
    let catalogue = Catalogue::load(&paths::catalogue_directory())?;
    match &cli.command {
        Command::Images { all_architectures } => {
            images(&catalogue, style, *all_architectures);
            Ok(())
        }
        Command::Inspect { reference } => inspect(&catalogue, style, reference),
        Command::Pull { reference } => pull(&catalogue, style, reference),
    }
}

fn pull(catalogue: &Catalogue, style: Style, reference: &str) -> vm_core::Result<()> {
    let reference: Reference = reference.parse()?;
    let (entry, artifact) = catalogue.resolve(&reference, host_architecture())?;
    let store = Store::discover()?;
    if store.contains(&artifact.digest) {
        store.record(entry, artifact)?;
        println!("{}:{} is already present", entry.name, entry.tag);
        return Ok(());
    }
    println!("Fetching {}", style.name(&artifact.url));
    let mut bar = progress::Bar::new("  ");
    let outcome = store.pull(artifact, &vm_core::store::http_agent(), &mut |update| {
        bar.update(update);
    });
    match outcome {
        Ok(Pulled::Fetched) => {
            bar.finish(&format!(
                "Pulled {}:{} ({})",
                entry.name,
                entry.tag,
                progress::human(file_size(&store, artifact))
            ));
            store.record(entry, artifact)?;
            Ok(())
        }
        Ok(Pulled::AlreadyPresent) => {
            bar.finish("Already present");
            Ok(())
        }
        Err(error) => {
            bar.finish("Failed");
            Err(error)
        }
    }
}

fn file_size(store: &Store, artifact: &vm_core::catalogue::Artifact) -> u64 {
    std::fs::metadata(store.path_for(&artifact.digest)).map_or(0, |data| data.len())
}

fn images(catalogue: &Catalogue, style: Style, all_architectures: bool) {
    let host = host_architecture();
    let mut rows = Vec::new();
    for entry in catalogue.entries() {
        for artifact in &entry.artifacts {
            if !all_architectures && artifact.arch != host {
                continue;
            }
            rows.push(vec![
                style.name(&entry.name),
                entry.tag.clone(),
                artifact.arch.clone(),
                entry.description.clone(),
            ]);
        }
    }
    if rows.is_empty() {
        println!(
            "{}",
            style.dim("No images in the catalogue. Try 'vm update'.")
        );
        return;
    }
    let headings = ["REPOSITORY", "TAG", "ARCH", "DESCRIPTION"];
    for (index, line) in table::render(&headings, &rows).into_iter().enumerate() {
        if index == 0 {
            println!("{}", style.heading(&line));
        } else {
            println!("{line}");
        }
    }
}

fn inspect(catalogue: &Catalogue, style: Style, reference: &str) -> vm_core::Result<()> {
    let reference: Reference = reference.parse()?;
    let (entry, artifact) = catalogue.resolve(&reference, host_architecture())?;
    let access = if entry.login.is_seedable() {
        "cloud-init; volumes and generated keys are available"
    } else {
        "console only; volumes and key injection are unavailable"
    };
    let mut fields = vec![
        ("Image", format!("{}:{}", entry.name, entry.tag)),
        ("Description", entry.description.clone()),
    ];
    if !entry.aliases.is_empty() {
        fields.push(("Aliases", entry.aliases.join(", ")));
    }
    fields.extend([
        ("Architecture", artifact.arch.clone()),
        ("Format", artifact.format.clone()),
        ("URL", artifact.url.clone()),
        ("Digest", artifact.digest.to_string()),
        ("Guest access", access.to_owned()),
    ]);
    let width = fields
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    for (label, value) in fields {
        let padding = " ".repeat(width - label.chars().count());
        println!("{}{padding}  {value}", style.heading(label));
    }
    Ok(())
}

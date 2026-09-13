//! Image operations: the same for every front end.

use crate::catalogue::{Artifact, Catalogue, Source};
use crate::compression::Compression;
use crate::config::{Config, STORE_CATALOGUE};
use crate::error::{Error, Result};
use crate::export;
use crate::import;
use crate::machines::{self, Event, Observer};
use crate::reports::{self, Origin};
use crate::store::{Pulled, Store};
use crate::{Reference, host_architecture, paths};
use std::path::Path;

/// Every build the catalogue names, for this architecture unless all are wanted.
pub fn list(
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
                    catalogue: entry.catalogue.clone(),
                })
                .collect::<Vec<_>>()
        })
        .collect();
    reports::Images { rows, origin }
}

/// The size of a held image, for an entry that does not state one.
fn held_size(store: &Store, artifact: &Artifact) -> Option<u64> {
    std::fs::metadata(store.path_for(&artifact.digest))
        .ok()
        .map(|data| data.len())
}

pub fn inspect(
    catalogue: &Catalogue,
    store: &Store,
    reference: &str,
    arch: &str,
) -> Result<reports::Inspect> {
    let reference: Reference = reference.parse()?;
    let (entry, artifact) = catalogue.resolve(&reference, arch)?;
    let mut report = reports::Inspect::new(entry, artifact, store.contains(&artifact.digest));
    if report.held {
        report.path = Some(store.path_for(&artifact.digest).display().to_string());
    }
    report.used_by = machines::holders(&artifact.digest.to_string()).unwrap_or_default();
    Ok(report)
}

/// Fetches an image into the store, reporting its progress.
pub fn pull(
    catalogue: &Catalogue,
    store: &Store,
    reference: &str,
    observe: Observer,
) -> Result<reports::Pull> {
    let reference: Reference = reference.parse()?;
    let (entry, artifact) = catalogue.resolve(&reference, host_architecture())?;
    if !store.contains(&artifact.digest) {
        if let Some(url) = &artifact.url {
            observe(Event::Fetching(url.clone()));
        }
    }
    let outcome = machines::fetch(store, artifact, observe)?;
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

/// Refreshes every remote catalogue, or the one named.
pub fn update(config: &Config, named: Option<&str>) -> Result<reports::Update> {
    let remotes = config.remotes();
    if let Some(name) = named
        && !remotes.iter().any(|remote| remote.name == name)
    {
        return Err(Error::UnknownCatalogue {
            name: name.to_owned(),
            available: remotes.into_iter().map(|remote| remote.name).collect(),
        });
    }
    let agent = crate::store::http_agent();
    let catalogues = remotes
        .iter()
        .filter(|remote| named.is_none_or(|name| remote.name == name))
        .map(|remote| {
            let destination =
                paths::remote_catalogue_directory(&remote.name).ok_or(Error::NoImageStore)?;
            let outcome = crate::update::run(remote, &destination, &agent)
                .map(|updated| (updated.files, updated.entries))
                .map_err(|error| error.to_string());
            Ok(reports::Updated {
                name: remote.name.clone(),
                url: remote.url.clone(),
                path: destination.display().to_string(),
                outcome,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(reports::Update { catalogues })
}

/// The catalogues of one kind, or the one named.
pub fn select_sources(
    sources: Vec<Source>,
    origin: Origin,
    named: Option<&str>,
) -> Result<Vec<Source>> {
    if let Some(name) = named
        && !sources.iter().any(|source| source.name == name)
    {
        return Err(Error::UnknownCatalogue {
            name: name.to_owned(),
            available: sources.into_iter().map(|source| source.name).collect(),
        });
    }
    Ok(sources
        .into_iter()
        .filter(|source| origin.admits(source.kind))
        .filter(|source| named.is_none_or(|name| source.name == name))
        .collect())
}

/// Imports an image, reporting each pass's progress.
pub fn import(
    catalogue: &Catalogue,
    store: &Store,
    request: &import::Request<'_>,
    observe: Observer,
) -> Result<reports::Imported> {
    let local = paths::local_catalogue_directory().ok_or(Error::NoImageStore)?;
    let imported = import::run(
        store,
        &local,
        request,
        &crate::store::http_agent(),
        &mut |event| {
            observe(Event::Importing {
                from_url: request.source.is_url(),
                event,
            });
        },
    )?;
    Ok(reports::Imported {
        source: request.source.describe(),
        hides: hidden(catalogue, &imported.name, &imported.tag),
        outcome: imported,
    })
}

/// The catalogues whose entry of the same name the import now hides.
fn hidden(catalogue: &Catalogue, name: &str, tag: &str) -> Vec<String> {
    catalogue.find(name, tag).map_or_else(Vec::new, |entry| {
        std::iter::once(&entry.catalogue)
            .chain(&entry.shadows)
            .filter(|held| *held != STORE_CATALOGUE)
            .cloned()
            .collect()
    })
}

/// What an export is asked for.
pub struct ExportRequest<'a> {
    pub reference: &'a str,
    pub file: &'a Path,
    pub arch: &'a str,
    pub compression: Compression,
    pub force: bool,
}

/// Copies a held image out of the store, reporting its progress.
pub fn export(
    catalogue: &Catalogue,
    store: &Store,
    request: &ExportRequest<'_>,
    observe: Observer,
) -> Result<reports::Exported> {
    let reference: Reference = request.reference.parse()?;
    let (entry, artifact) = catalogue.resolve(&reference, request.arch)?;
    if !store.contains(&artifact.digest) {
        return Err(Error::NotExportable {
            reference: request.reference.to_owned(),
        });
    }
    let (path, suffixed) = export::destination(
        request.file,
        artifact,
        request.compression,
        &entry.name,
        &entry.tag,
    )?;
    let exported = export::copy(
        &store.path_for(&artifact.digest),
        artifact,
        &path,
        request.compression,
        request.force,
        &mut |progress| observe(Event::Exporting(progress)),
    )?;
    Ok(reports::Exported {
        name: entry.name.clone(),
        tag: entry.tag.clone(),
        arch: artifact.arch.clone(),
        format: export::stored_format(artifact).to_owned(),
        compression: request.compression,
        cdrom: artifact.media.is_cdrom(),
        digest: artifact.digest.to_string(),
        path: exported.path.display().to_string(),
        size: exported.size,
        suffixed,
        verified: exported.verified,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::catalogue::{Kind, Source as Layer};

    fn catalogue(layers: &[(&str, Kind)]) -> (Catalogue, std::path::PathBuf) {
        let mut root = std::env::temp_dir();
        root.push(format!(
            "vm-imports-{}-{}",
            layers.len(),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let sources: Vec<Layer> = layers
            .iter()
            .map(|(name, kind)| {
                let directory = root.join(name);
                std::fs::create_dir_all(directory.join("mine")).unwrap();
                std::fs::write(
                    directory.join("mine/1.toml"),
                    format!(
                        "name = \"mine\"\ntag = \"1\"\ndescription = \"d\"\nlogin = \"none\"\n\n[[image]]\narch = \"amd64\"\nformat = \"qcow2\"\ndigest = \"sha256:{}\"\n",
                        "a".repeat(64)
                    ),
                )
                .unwrap();
                Layer {
                    name: (*name).to_owned(),
                    kind: *kind,
                    directory,
                }
            })
            .collect();
        (Catalogue::load_layered(&sources).unwrap(), root)
    }

    #[test]
    fn an_import_hides_the_other_catalogues_naming_it() {
        let (layered, root) = catalogue(&[
            ("project", Kind::Remote),
            ("team", Kind::Local),
            (STORE_CATALOGUE, Kind::Local),
        ]);
        assert_eq!(hidden(&layered, "mine", "1"), ["team", "project"]);
        assert!(hidden(&layered, "mine", "2").is_empty());
        let _ = std::fs::remove_dir_all(root);
        let (layered, root) = catalogue(&[("project", Kind::Remote)]);
        assert_eq!(hidden(&layered, "mine", "1"), ["project"]);
        let _ = std::fs::remove_dir_all(root);
    }

    fn sources() -> Vec<Source> {
        [
            ("project", Kind::Remote),
            ("internal", Kind::Remote),
            ("team", Kind::Local),
            (STORE_CATALOGUE, Kind::Local),
        ]
        .into_iter()
        .map(|(name, kind)| Source {
            name: name.to_owned(),
            kind,
            directory: std::path::PathBuf::from("/nowhere"),
        })
        .collect()
    }

    fn selected(origin: Origin, named: Option<&str>) -> Vec<String> {
        select_sources(sources(), origin, named)
            .unwrap()
            .into_iter()
            .map(|source| source.name)
            .collect()
    }

    #[test]
    fn a_listing_reads_one_kind_of_catalogue_or_one_by_name() {
        assert_eq!(
            selected(Origin::All, None),
            ["project", "internal", "team", "store"]
        );
        assert_eq!(selected(Origin::Remote, None), ["project", "internal"]);
        assert_eq!(selected(Origin::Local, None), ["team", "store"]);
        assert_eq!(selected(Origin::All, Some("team")), ["team"]);
        assert!(selected(Origin::Remote, Some("team")).is_empty());
        let error = select_sources(sources(), Origin::All, Some("nope")).unwrap_err();
        assert_eq!(error.kind(), "unknown-catalogue");
        assert!(error.to_string().contains("internal"), "{error}");
    }

    #[test]
    fn updating_an_unknown_catalogue_names_the_remotes() {
        let Err(error) = update(&Config::default(), Some("nope")) else {
            panic!("an unknown catalogue was updated");
        };
        assert_eq!(error.kind(), "unknown-catalogue");
        assert!(error.to_string().contains("project"), "{error}");
    }
}

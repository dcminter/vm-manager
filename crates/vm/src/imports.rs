//! `vm import`.

use crate::progress::Bar;
use crate::reports;
use std::cell::RefCell;
use vm_core::catalogue::Catalogue;
use vm_core::config::STORE_CATALOGUE;
use vm_core::error::{Error, Result};
use vm_core::import::{self, Event, Request, Source};
use vm_core::store::Store;

/// Imports an image, drawing each pass's progress.
pub fn run(
    catalogue: &Catalogue,
    store: &Store,
    request: &Request<'_>,
    text: bool,
) -> Result<reports::Imported> {
    let local = vm_core::paths::local_catalogue_directory().ok_or(Error::NoImageStore)?;
    let receiving = if request.source.is_url() {
        "fetching"
    } else {
        "reading"
    };
    let bar = RefCell::new(Bar::new("", text));
    let outcome = import::run(
        store,
        &local,
        request,
        &vm_core::store::http_agent(),
        &mut |event| {
            let mut bar = bar.borrow_mut();
            match event {
                Event::Receiving(progress) => {
                    bar.naming(&label(receiving));
                    bar.update(progress);
                }
                Event::Converting(percent) => {
                    bar.naming(&label("converting"));
                    bar.portion(percent);
                }
                Event::Hashing(percent) => {
                    bar.naming(&label("verifying"));
                    bar.portion(percent);
                }
            }
        },
    );
    bar.borrow().clear();
    let imported = outcome?;
    Ok(reports::Imported {
        source: request.source.describe(),
        hides: hidden(catalogue, &imported.name, &imported.tag),
        outcome: imported,
    })
}

fn label(pass: &str) -> String {
    format!("  {pass:<10}")
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

/// A source as given on the command line.
pub fn parse_source(text: &str) -> std::result::Result<Source, String> {
    if text.is_empty() {
        Err("a source needs a path or URL".to_owned())
    } else {
        Ok(Source::parse(text))
    }
}

pub fn parse_login(text: &str) -> std::result::Result<vm_core::catalogue::Login, String> {
    text.parse().map_err(|error: Error| error.to_string())
}

pub fn parse_digest(text: &str) -> std::result::Result<vm_core::reference::Digest, String> {
    text.parse().map_err(|error: Error| error.to_string())
}

pub fn parse_arch(text: &str) -> std::result::Result<String, String> {
    if vm_core::ARCHITECTURES.contains(&text) {
        Ok(text.to_owned())
    } else {
        Err(format!(
            "'{text}' is not an architecture; use {}",
            vm_core::ARCHITECTURES.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use vm_core::catalogue::{Kind, Source as Layer};

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

    #[test]
    fn an_architecture_must_be_one_a_machine_can_have() {
        assert_eq!(parse_arch("arm64").unwrap(), "arm64");
        assert!(parse_arch("x86_64").unwrap_err().contains("amd64"));
    }

    #[test]
    fn a_login_and_digest_are_checked_as_they_are_parsed() {
        assert!(parse_login("cloud-init").unwrap().is_seedable());
        assert!(parse_login("ssh").is_err());
        assert!(parse_digest(&format!("sha256:{}", "a".repeat(64))).is_ok());
        assert!(parse_digest("sha256:short").is_err());
        assert!(parse_source("").is_err());
        assert!(parse_source("https://example.invalid/x").unwrap().is_url());
    }
}

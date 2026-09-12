use crate::catalogue::Catalogue;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::tar;
use std::fs;
use std::path::{Path, PathBuf};

/// No single catalogue entry has any business being larger than this.
const MAX_ENTRY_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone)]
pub struct Updated {
    pub url: String,
    pub path: PathBuf,
    pub files: usize,
    pub entries: usize,
}

/// Replaces the fetched catalogue with a freshly downloaded one. The new
/// catalogue is staged and parsed before it replaces the old, so a broken
/// download never leaves the tool without a working catalogue.
pub fn run(config: &Config, destination: &Path, agent: &ureq::Agent) -> Result<Updated> {
    let files = fetch(&config.catalogue_url, agent)?;
    install(&files, config, destination)
}

/// Stages the entries, checks they parse, and only then replaces the live
/// catalogue. A download that is malformed leaves the old one untouched.
pub fn install(files: &[tar::File], config: &Config, destination: &Path) -> Result<Updated> {
    let wanted = select(files, &config.catalogue_path);
    if wanted.is_empty() {
        return Err(Error::EmptyCatalogue {
            url: config.catalogue_url.clone(),
            path: config.catalogue_path.clone(),
        });
    }
    let staging = staging_path(destination);
    remove(&staging)?;
    for (relative, contents) in &wanted {
        write_entry(&staging, relative, contents)?;
    }
    let entries = match Catalogue::load(&staging) {
        Ok(catalogue) => catalogue.entries().len(),
        Err(error) => {
            remove(&staging)?;
            return Err(error);
        }
    };
    swap(&staging, destination)?;
    Ok(Updated {
        url: config.catalogue_url.clone(),
        path: destination.to_owned(),
        files: wanted.len(),
        entries,
    })
}

fn fetch(url: &str, agent: &ureq::Agent) -> Result<Vec<tar::File>> {
    let response = agent.get(url).call().map_err(|source| Error::Download {
        url: url.to_owned(),
        source: Box::new(source),
    })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Error::HttpStatus {
            url: url.to_owned(),
            status,
        });
    }
    let mut body = flate2::read::GzDecoder::new(response.into_body().into_reader());
    let read = tar::read(&mut body, MAX_ENTRY_BYTES).map_err(|source| Error::Store {
        path: PathBuf::from(url),
        action: "read",
        source,
    })?;
    read.map_err(|problem| Error::MalformedArchive {
        url: url.to_owned(),
        reason: problem.to_string(),
    })
}

/// Keeps the `.toml` files below the configured directory, dropping the
/// single wrapping directory that archive services add.
fn select(files: &[tar::File], catalogue_path: &str) -> Vec<(String, Vec<u8>)> {
    let wanted = format!("{}/", catalogue_path.trim_matches('/'));
    files
        .iter()
        .filter_map(|file| {
            let inner = file
                .path
                .split_once('/')
                .map_or(file.path.as_str(), |(_, rest)| rest);
            let relative = inner.strip_prefix(&wanted)?;
            let is_entry = Path::new(relative)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"));
            if !is_entry || !tar::is_safe_path(relative) {
                return None;
            }
            Some((relative.to_owned(), file.contents.clone()))
        })
        .collect()
}

fn staging_path(destination: &Path) -> PathBuf {
    let mut staging = destination.as_os_str().to_owned();
    staging.push(".new");
    PathBuf::from(staging)
}

fn retired_path(destination: &Path) -> PathBuf {
    let mut retired = destination.as_os_str().to_owned();
    retired.push(".old");
    PathBuf::from(retired)
}

fn write_entry(staging: &Path, relative: &str, contents: &[u8]) -> Result<()> {
    let path = staging.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Store {
            path: parent.to_owned(),
            action: "create",
            source,
        })?;
    }
    fs::write(&path, contents).map_err(|source| Error::Store {
        path,
        action: "write",
        source,
    })
}

/// Puts the staged catalogue in place, keeping the old one until the new one
/// has landed so an interruption cannot leave nothing behind.
fn swap(staging: &Path, destination: &Path) -> Result<()> {
    let retired = retired_path(destination);
    remove(&retired)?;
    if destination.exists() {
        fs::rename(destination, &retired).map_err(|source| Error::Store {
            path: destination.to_owned(),
            action: "move aside",
            source,
        })?;
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Store {
            path: parent.to_owned(),
            action: "create",
            source,
        })?;
    }
    match fs::rename(staging, destination) {
        Ok(()) => {
            remove(&retired)?;
            Ok(())
        }
        Err(source) => {
            let _ = fs::rename(&retired, destination);
            Err(Error::Store {
                path: destination.to_owned(),
                action: "install",
                source,
            })
        }
    }
}

fn remove(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Store {
            path: path.to_owned(),
            action: "remove",
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("vm-update-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn catalogue(&self) -> PathBuf {
            self.0.join("catalogue")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn entry_toml(name: &str, tag: &str) -> String {
        format!(
            r#"
name = "{name}"
tag = "{tag}"
description = "test entry"
login = "none"

[[image]]
arch = "amd64"
format = "qcow2"
url = "https://example.invalid/{name}.qcow2"
digest = "sha512:{}"
"#,
            "a".repeat(128)
        )
    }

    fn archive(entries: &[(&str, String)]) -> Vec<tar::File> {
        entries
            .iter()
            .map(|(path, body)| tar::File {
                path: format!("repo-main/catalogue/{path}"),
                contents: body.clone().into_bytes(),
            })
            .collect()
    }

    #[test]
    fn an_install_lands_the_entries_and_counts_them() {
        let scratch = Scratch::new("lands");
        let files = archive(&[
            ("debian/trixie.toml", entry_toml("debian", "trixie")),
            ("debian/forky.toml", entry_toml("debian", "forky")),
        ]);
        let updated = install(&files, &Config::default(), &scratch.catalogue()).unwrap();
        assert_eq!(updated.files, 2);
        assert_eq!(updated.entries, 2);
        assert!(scratch.catalogue().join("debian/trixie.toml").is_file());
    }

    #[test]
    fn an_install_replaces_what_was_there_before() {
        let scratch = Scratch::new("replaces");
        let first = archive(&[("debian/trixie.toml", entry_toml("debian", "trixie"))]);
        install(&first, &Config::default(), &scratch.catalogue()).unwrap();
        let second = archive(&[("ubuntu/noble.toml", entry_toml("ubuntu", "noble"))]);
        install(&second, &Config::default(), &scratch.catalogue()).unwrap();
        assert!(scratch.catalogue().join("ubuntu/noble.toml").is_file());
        assert!(!scratch.catalogue().join("debian/trixie.toml").exists());
    }

    #[test]
    fn a_catalogue_that_does_not_parse_leaves_the_old_one_in_place() {
        let scratch = Scratch::new("rollback");
        let good = archive(&[("debian/trixie.toml", entry_toml("debian", "trixie"))]);
        install(&good, &Config::default(), &scratch.catalogue()).unwrap();
        let bad = archive(&[("debian/broken.toml", "this is not toml".to_owned())]);
        let error = install(&bad, &Config::default(), &scratch.catalogue()).unwrap_err();
        assert!(matches!(error, Error::CatalogueParse { .. }), "{error}");
        assert!(scratch.catalogue().join("debian/trixie.toml").is_file());
        let loaded = Catalogue::load(&scratch.catalogue()).unwrap();
        assert_eq!(loaded.entries().len(), 1);
    }

    #[test]
    fn a_failed_install_leaves_no_staging_directory_behind() {
        let scratch = Scratch::new("staging");
        let bad = archive(&[("debian/broken.toml", "not toml".to_owned())]);
        assert!(install(&bad, &Config::default(), &scratch.catalogue()).is_err());
        assert!(!staging_path(&scratch.catalogue()).exists());
    }

    #[test]
    fn an_archive_with_no_entries_is_refused_before_anything_is_written() {
        let scratch = Scratch::new("empty");
        let files = vec![tar::File {
            path: "repo-main/README.md".to_owned(),
            contents: b"nothing here".to_vec(),
        }];
        let error = install(&files, &Config::default(), &scratch.catalogue()).unwrap_err();
        assert!(matches!(error, Error::EmptyCatalogue { .. }), "{error}");
        assert!(!scratch.catalogue().exists());
    }

    #[test]
    fn a_successful_install_leaves_no_retired_copy_behind() {
        let scratch = Scratch::new("retired");
        let files = archive(&[("debian/trixie.toml", entry_toml("debian", "trixie"))]);
        install(&files, &Config::default(), &scratch.catalogue()).unwrap();
        install(&files, &Config::default(), &scratch.catalogue()).unwrap();
        assert!(!retired_path(&scratch.catalogue()).exists());
    }

    #[test]
    fn nested_directories_in_the_archive_are_recreated() {
        let scratch = Scratch::new("nested");
        let files = archive(&[("a/b/c/deep.toml", entry_toml("deep", "one"))]);
        install(&files, &Config::default(), &scratch.catalogue()).unwrap();
        assert!(scratch.catalogue().join("a/b/c/deep.toml").is_file());
    }

    fn file(path: &str, contents: &str) -> tar::File {
        tar::File {
            path: path.to_owned(),
            contents: contents.as_bytes().to_vec(),
        }
    }

    #[test]
    fn the_wrapping_directory_is_dropped() {
        let files = vec![file("vm-manager-main/catalogue/debian/trixie.toml", "x")];
        let selected = select(&files, "catalogue");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0, "debian/trixie.toml");
    }

    #[test]
    fn only_the_configured_directory_is_taken() {
        let files = vec![
            file("repo-main/catalogue/debian/trixie.toml", "wanted"),
            file("repo-main/src/main.rs", "no"),
            file("repo-main/README.md", "no"),
            file("repo-main/catalogues-elsewhere/other.toml", "no"),
        ];
        let selected = select(&files, "catalogue");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].1, b"wanted");
    }

    #[test]
    fn a_configured_path_with_slashes_is_accepted() {
        let files = vec![file("repo-main/data/images/debian.toml", "x")];
        assert_eq!(select(&files, "/data/images/").len(), 1);
    }

    #[test]
    fn non_toml_files_are_left_behind() {
        let files = vec![
            file("repo-main/catalogue/README.md", "no"),
            file("repo-main/catalogue/debian/trixie.toml", "yes"),
        ];
        assert_eq!(select(&files, "catalogue").len(), 1);
    }

    #[test]
    fn an_escaping_path_is_refused_even_inside_the_catalogue() {
        let files = vec![
            file("repo-main/catalogue/../../escape.toml", "no"),
            file("repo-main/catalogue/ok.toml", "yes"),
        ];
        let selected = select(&files, "catalogue");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0, "ok.toml");
    }

    #[test]
    fn an_archive_without_the_directory_selects_nothing() {
        let files = vec![file("repo-main/src/main.rs", "x")];
        assert!(select(&files, "catalogue").is_empty());
    }

    #[test]
    fn staging_and_retired_paths_sit_beside_the_destination() {
        let destination = Path::new("/home/x/.local/share/vm/catalogue");
        assert_eq!(
            staging_path(destination),
            PathBuf::from("/home/x/.local/share/vm/catalogue.new")
        );
        assert_eq!(
            retired_path(destination),
            PathBuf::from("/home/x/.local/share/vm/catalogue.old")
        );
    }
}

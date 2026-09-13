use crate::catalogue::Catalogue;
use crate::config::{Remote, is_fetchable};
use crate::error::{Error, Result};
use crate::tar;
use serde::Deserialize;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// No single catalogue entry has any business being larger than this.
const MAX_ENTRY_BYTES: u64 = 256 * 1024;

const MAX_POINTER_BYTES: u64 = 64 * 1024;

/// The file a remote's URL names, saying which archive is current.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pointer {
    version: String,
    /// An http or https URL, or a file name beside the pointer.
    archive: String,
}

#[derive(Debug, Clone)]
pub struct Updated {
    pub version: String,
    pub archive: String,
    pub path: PathBuf,
    pub files: usize,
    pub entries: usize,
}

/// Reads the pointer, then downloads the archive it names and installs it.
pub fn run(remote: &Remote, destination: &Path, agent: &ureq::Agent) -> Result<Updated> {
    let pointer = fetch_pointer(&remote.url, agent)?;
    let archive =
        archive_url(&remote.url, &pointer.archive).ok_or_else(|| Error::MalformedPointer {
            url: remote.url.clone(),
            reason: format!(
                "archive '{}' is neither an http or https URL nor a file name",
                pointer.archive
            ),
        })?;
    let files = fetch(&archive, agent)?;
    install(
        &files,
        &pointer.version,
        &archive,
        &remote.path,
        destination,
    )
}

fn fetch_pointer(url: &str, agent: &ureq::Agent) -> Result<Pointer> {
    let response = crate::store::get(agent, url)?;
    let mut text = String::new();
    response
        .into_body()
        .into_reader()
        .take(MAX_POINTER_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|source| Error::MalformedPointer {
            url: url.to_owned(),
            reason: source.to_string(),
        })?;
    parse_pointer(&text).map_err(|reason| Error::MalformedPointer {
        url: url.to_owned(),
        reason,
    })
}

fn parse_pointer(text: &str) -> std::result::Result<Pointer, String> {
    if text.len() as u64 > MAX_POINTER_BYTES {
        return Err(format!("it is larger than {MAX_POINTER_BYTES} bytes"));
    }
    let pointer: Pointer = basic_toml::from_str(text).map_err(|source| source.to_string())?;
    let printable = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+');
    if pointer.version.is_empty() || !pointer.version.chars().all(printable) {
        return Err(format!(
            "version '{}' is not letters, digits, '.', '-', '_' and '+'",
            pointer.version.escape_default()
        ));
    }
    Ok(pointer)
}

/// The archive's URL: as given when absolute, otherwise beside the pointer.
fn archive_url(pointer_url: &str, archive: &str) -> Option<String> {
    if archive.contains("://") {
        return is_fetchable(archive).then(|| archive.to_owned());
    }
    let plain_name = !archive.is_empty()
        && archive != "."
        && archive != ".."
        && !archive.contains(['/', '\\', ':', '?', '#'])
        && !archive.chars().any(char::is_whitespace);
    if !plain_name {
        return None;
    }
    let without_query = pointer_url
        .find(['?', '#'])
        .map_or(pointer_url, |end| &pointer_url[..end]);
    let authority = without_query.find("://")? + 3;
    let base = without_query[authority..].rfind('/').map_or_else(
        || format!("{without_query}/"),
        |slash| without_query[..=authority + slash].to_owned(),
    );
    Some(format!("{base}{archive}"))
}

/// Stages and parses the entries before replacing the live catalogue.
pub fn install(
    files: &[tar::File],
    version: &str,
    archive: &str,
    catalogue_path: &str,
    destination: &Path,
) -> Result<Updated> {
    let wanted = select(files, catalogue_path);
    if wanted.is_empty() {
        return Err(Error::EmptyCatalogue {
            url: archive.to_owned(),
            path: catalogue_path.to_owned(),
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
        version: version.to_owned(),
        archive: archive.to_owned(),
        path: destination.to_owned(),
        files: wanted.len(),
        entries,
    })
}

fn fetch(url: &str, agent: &ureq::Agent) -> Result<Vec<tar::File>> {
    let response = crate::store::get(agent, url)?;
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

/// The `.toml` files below the configured directory, without the archive's wrapping directory.
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

/// Moves the staged catalogue into place, keeping the old one until it lands.
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

    fn install_here(files: &[tar::File], destination: &Path) -> Result<Updated> {
        install(
            files,
            "1.0.0",
            "https://example.test/c.tar.gz",
            "catalogue",
            destination,
        )
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
        let updated = install_here(&files, &scratch.catalogue()).unwrap();
        assert_eq!(updated.files, 2);
        assert_eq!(updated.entries, 2);
        assert!(scratch.catalogue().join("debian/trixie.toml").is_file());
    }

    #[test]
    fn an_install_replaces_what_was_there_before() {
        let scratch = Scratch::new("replaces");
        let first = archive(&[("debian/trixie.toml", entry_toml("debian", "trixie"))]);
        install_here(&first, &scratch.catalogue()).unwrap();
        let second = archive(&[("ubuntu/noble.toml", entry_toml("ubuntu", "noble"))]);
        install_here(&second, &scratch.catalogue()).unwrap();
        assert!(scratch.catalogue().join("ubuntu/noble.toml").is_file());
        assert!(!scratch.catalogue().join("debian/trixie.toml").exists());
    }

    #[test]
    fn a_catalogue_that_does_not_parse_leaves_the_old_one_in_place() {
        let scratch = Scratch::new("rollback");
        let good = archive(&[("debian/trixie.toml", entry_toml("debian", "trixie"))]);
        install_here(&good, &scratch.catalogue()).unwrap();
        let bad = archive(&[("debian/broken.toml", "this is not toml".to_owned())]);
        let error = install_here(&bad, &scratch.catalogue()).unwrap_err();
        assert!(matches!(error, Error::CatalogueParse { .. }), "{error}");
        assert!(scratch.catalogue().join("debian/trixie.toml").is_file());
        let loaded = Catalogue::load(&scratch.catalogue()).unwrap();
        assert_eq!(loaded.entries().len(), 1);
    }

    #[test]
    fn a_failed_install_leaves_no_staging_directory_behind() {
        let scratch = Scratch::new("staging");
        let bad = archive(&[("debian/broken.toml", "not toml".to_owned())]);
        assert!(install_here(&bad, &scratch.catalogue()).is_err());
        assert!(!staging_path(&scratch.catalogue()).exists());
    }

    #[test]
    fn an_archive_with_no_entries_is_refused_before_anything_is_written() {
        let scratch = Scratch::new("empty");
        let files = vec![tar::File {
            path: "repo-main/README.md".to_owned(),
            contents: b"nothing here".to_vec(),
        }];
        let error = install_here(&files, &scratch.catalogue()).unwrap_err();
        assert!(matches!(error, Error::EmptyCatalogue { .. }), "{error}");
        assert!(!scratch.catalogue().exists());
    }

    #[test]
    fn a_successful_install_leaves_no_retired_copy_behind() {
        let scratch = Scratch::new("retired");
        let files = archive(&[("debian/trixie.toml", entry_toml("debian", "trixie"))]);
        install_here(&files, &scratch.catalogue()).unwrap();
        install_here(&files, &scratch.catalogue()).unwrap();
        assert!(!retired_path(&scratch.catalogue()).exists());
    }

    #[test]
    fn nested_directories_in_the_archive_are_recreated() {
        let scratch = Scratch::new("nested");
        let files = archive(&[("a/b/c/deep.toml", entry_toml("deep", "one"))]);
        install_here(&files, &scratch.catalogue()).unwrap();
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
    fn a_pointer_names_a_version_and_an_archive() {
        let pointer =
            parse_pointer("version = \"0.0.1\"\narchive = \"catalogue-0.0.1.tar.gz\"\n").unwrap();
        assert_eq!(pointer.version, "0.0.1");
        assert_eq!(pointer.archive, "catalogue-0.0.1.tar.gz");
    }

    #[test]
    fn a_pointer_missing_a_field_is_refused() {
        assert!(parse_pointer("version = \"0.0.1\"\n").is_err());
        assert!(parse_pointer("archive = \"c.tar.gz\"\n").is_err());
        assert!(parse_pointer("").is_err());
    }

    #[test]
    fn a_pointer_with_an_unknown_field_is_refused() {
        let text = "version = \"1\"\narchive = \"c.tar.gz\"\nsha = \"x\"\n";
        assert!(parse_pointer(text).is_err());
    }

    #[test]
    fn a_pointer_that_is_not_toml_is_refused() {
        assert!(parse_pointer("<html>moved</html>").is_err());
    }

    #[test]
    fn a_version_with_odd_characters_is_refused() {
        for version in ["", "1 0", "1.0\n", "1;rm", "\u{1b}[31m"] {
            let text = format!("version = {version:?}\narchive = \"c.tar.gz\"\n");
            assert!(parse_pointer(&text).is_err(), "{version:?}");
        }
        for version in ["1.2.3", "2026-09-13", "1.0+local_2"] {
            let text = format!("version = {version:?}\narchive = \"c.tar.gz\"\n");
            assert!(parse_pointer(&text).is_ok(), "{version:?}");
        }
    }

    #[test]
    fn an_oversized_pointer_is_refused() {
        let text = format!(
            "version = \"1\"\narchive = \"c.tar.gz\"\n#{}\n",
            "x".repeat(usize::try_from(MAX_POINTER_BYTES).unwrap())
        );
        assert!(parse_pointer(&text).unwrap_err().contains("larger"));
    }

    #[test]
    fn a_file_name_is_found_beside_the_pointer() {
        let cases = [
            (
                "https://vm-manager.com/catalogue.toml",
                "https://vm-manager.com/catalogue-0.0.1.tar.gz",
            ),
            (
                "https://mirror.test/vm/current.toml",
                "https://mirror.test/vm/catalogue-0.0.1.tar.gz",
            ),
            (
                "https://mirror.test/vm/",
                "https://mirror.test/vm/catalogue-0.0.1.tar.gz",
            ),
            (
                "https://mirror.test",
                "https://mirror.test/catalogue-0.0.1.tar.gz",
            ),
            (
                "https://mirror.test/vm/current.toml?token=a/b#part",
                "https://mirror.test/vm/catalogue-0.0.1.tar.gz",
            ),
        ];
        for (pointer, expected) in cases {
            assert_eq!(
                archive_url(pointer, "catalogue-0.0.1.tar.gz").as_deref(),
                Some(expected),
                "{pointer}"
            );
        }
    }

    #[test]
    fn an_absolute_archive_url_is_used_as_given() {
        assert_eq!(
            archive_url(
                "https://vm-manager.com/catalogue.toml",
                "http://elsewhere.test/a/c.tar.gz"
            )
            .as_deref(),
            Some("http://elsewhere.test/a/c.tar.gz")
        );
    }

    #[test]
    fn an_archive_that_is_neither_a_url_nor_a_file_name_is_refused() {
        for archive in [
            "",
            ".",
            "..",
            "../c.tar.gz",
            "sub/c.tar.gz",
            "/c.tar.gz",
            "c.tar.gz?x",
            "c .tar.gz",
            "ftp://mirror.test/c.tar.gz",
            "file:///etc/passwd",
            "https://",
        ] {
            assert_eq!(
                archive_url("https://vm-manager.com/catalogue.toml", archive),
                None,
                "{archive}"
            );
        }
    }

    fn gzipped_catalogue(scratch: &Scratch) -> Vec<u8> {
        let root = scratch.0.join("source");
        let entry = root.join("catalogue-0.0.1/catalogue/debian");
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join("trixie.toml"), entry_toml("debian", "trixie")).unwrap();
        let packed = std::process::Command::new("tar")
            .arg("-C")
            .arg(&root)
            .args(["-czf", "-", "catalogue-0.0.1"])
            .output()
            .unwrap();
        assert!(packed.status.success());
        packed.stdout
    }

    fn remote_at(url: String) -> Remote {
        Remote {
            name: "project".to_owned(),
            url,
            path: "catalogue".to_owned(),
        }
    }

    #[test]
    fn a_run_follows_the_pointer_to_the_archive() {
        let scratch = Scratch::new("run");
        let base = crate::testing::serve_paths(vec![
            (
                "/catalogue.toml",
                b"version = \"0.0.1\"\narchive = \"catalogue-0.0.1.tar.gz\"\n".to_vec(),
            ),
            ("/catalogue-0.0.1.tar.gz", gzipped_catalogue(&scratch)),
        ]);
        let remote = remote_at(format!("{base}/catalogue.toml"));
        let updated = run(&remote, &scratch.catalogue(), &crate::store::http_agent()).unwrap();
        assert_eq!(updated.version, "0.0.1");
        assert_eq!(updated.archive, format!("{base}/catalogue-0.0.1.tar.gz"));
        assert_eq!((updated.files, updated.entries), (1, 1));
        assert!(scratch.catalogue().join("debian/trixie.toml").is_file());
    }

    #[test]
    fn a_pointer_to_a_missing_archive_leaves_the_old_catalogue_in_place() {
        let scratch = Scratch::new("missing");
        let good = archive(&[("debian/trixie.toml", entry_toml("debian", "trixie"))]);
        install_here(&good, &scratch.catalogue()).unwrap();
        let base = crate::testing::serve_paths(vec![(
            "/catalogue.toml",
            b"version = \"0.0.2\"\narchive = \"catalogue-0.0.2.tar.gz\"\n".to_vec(),
        )]);
        let remote = remote_at(format!("{base}/catalogue.toml"));
        let error = run(&remote, &scratch.catalogue(), &crate::store::http_agent()).unwrap_err();
        assert!(
            matches!(&error, Error::Download { url, .. } | Error::HttpStatus { url, .. } if url.ends_with("0.0.2.tar.gz")),
            "{error}"
        );
        assert!(scratch.catalogue().join("debian/trixie.toml").is_file());
    }

    #[test]
    fn a_url_that_serves_an_archive_rather_than_a_pointer_is_refused() {
        let scratch = Scratch::new("not-pointer");
        let base =
            crate::testing::serve_paths(vec![("/catalogue.tar.gz", gzipped_catalogue(&scratch))]);
        let remote = remote_at(format!("{base}/catalogue.tar.gz"));
        let error = run(&remote, &scratch.catalogue(), &crate::store::http_agent()).unwrap_err();
        assert!(matches!(error, Error::MalformedPointer { .. }), "{error}");
        assert!(!scratch.catalogue().exists());
    }

    #[test]
    fn a_pointer_naming_an_unusable_archive_is_refused() {
        let scratch = Scratch::new("unusable");
        let base = crate::testing::serve_paths(vec![(
            "/catalogue.toml",
            b"version = \"1\"\narchive = \"../escape.tar.gz\"\n".to_vec(),
        )]);
        let remote = remote_at(format!("{base}/catalogue.toml"));
        let error = run(&remote, &scratch.catalogue(), &crate::store::http_agent()).unwrap_err();
        assert!(
            matches!(&error, Error::MalformedPointer { reason, .. } if reason.contains("escape")),
            "{error}"
        );
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

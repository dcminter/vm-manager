//! Bringing an image file or a download into the store and its catalogue.

use crate::catalogue::{Artifact, Catalogue, Login, Media, NewEntry};
use crate::clone::{self, ALGORITHM};
use crate::compression::Compression;
use crate::conversion::{self, Conversion};
use crate::digest;
use crate::error::{Error, Result};
use crate::machine::{Chipset, Disk, Firmware};
use crate::reference::{Algorithm, Digest, Reference};
use crate::store::{self, Progress, Store};
use crate::tar;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const OPERATION: &str = "importing an image";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    File(PathBuf),
    Url(String),
}

impl Source {
    /// An http or https URL, or otherwise a path.
    pub fn parse(text: &str) -> Self {
        if text.starts_with("https://") || text.starts_with("http://") {
            Self::Url(text.to_owned())
        } else {
            Self::File(PathBuf::from(text))
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::File(path) => path.display().to_string(),
            Self::Url(url) => url.clone(),
        }
    }

    pub const fn is_url(&self) -> bool {
        matches!(self, Self::Url(_))
    }
}

/// The virtual hardware an image needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hardware {
    pub firmware: Firmware,
    pub cpu_model: Option<String>,
    pub machine: Chipset,
    pub disk: Disk,
}

/// What `vm import` was asked for.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub source: &'a Source,
    pub reference: &'a Reference,
    pub arch: &'a str,
    pub description: Option<&'a str>,
    pub login: Login,
    pub hardware: Hardware,
    /// Checked against the file as published.
    pub digest: Option<&'a Digest>,
    /// Whether a downloaded image keeps its URL, so it can be fetched again.
    pub fetchable: bool,
    /// Whether an entry of the same name is replaced.
    pub force: bool,
}

/// How far an import has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Receiving(Progress),
    Converting(u8),
    Hashing(u8),
}

pub type Reporter<'a> = &'a mut dyn FnMut(Event);

/// What an import produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub name: String,
    pub tag: String,
    pub artifact: Artifact,
    /// The image's format before it was stored.
    pub format: String,
    /// Where the image is in the store.
    pub path: PathBuf,
    pub entry: PathBuf,
    pub replaced: bool,
}

/// Imports an image into the store, naming it in the catalogue at `local`.
pub fn run(
    store: &Store,
    local: &Path,
    request: &Request<'_>,
    agent: &ureq::Agent,
    report: Reporter<'_>,
) -> Result<Imported> {
    if request.reference.digest().is_some() {
        return Err(Error::Reference {
            input: request.reference.to_string(),
            reason: "a new image is named without a digest",
        });
    }
    let (name, tag) = (request.reference.repository(), request.reference.tag());
    let replaced = check_name(local, name, tag, request.force)?;
    let label = format!("import-{name}-{tag}");
    let staged = Staged {
        received: store.staging(&format!("{label}.received"))?,
        converted: store.staging(&format!("{label}.qcow2"))?,
    };
    let outcome = import(store, local, request, agent, report, &staged);
    staged.clear();
    outcome.map(|mut imported| {
        imported.replaced = replaced;
        imported
    })
}

/// Whether the store catalogue already has the name, refusing it unless it is to be replaced.
fn check_name(local: &Path, name: &str, tag: &str, force: bool) -> Result<bool> {
    let catalogue = Catalogue::load(local)?;
    let Some(entry) = catalogue.find(name, tag) else {
        return Ok(false);
    };
    let reference = format!("{name}:{tag}");
    if entry.tag != tag || entry.path != local.join(name).join(format!("{tag}.toml")) {
        return Err(Error::ImageExists {
            reference,
            alias_in: Some(entry.path.clone()),
        });
    }
    if !force {
        return Err(Error::ImageExists {
            reference,
            alias_in: None,
        });
    }
    Ok(true)
}

/// Files an import writes before it has a digest to store them under.
struct Staged {
    received: PathBuf,
    converted: PathBuf,
}

impl Staged {
    fn clear(&self) {
        let _ = fs::remove_file(&self.received);
        let _ = fs::remove_file(&self.converted);
    }
}

/// What arrived, before anything is made of it.
struct Received {
    /// The hex hash of the bytes as published.
    hash: String,
    compression: Compression,
    /// The file taken from the source, when it was a tar archive.
    member: Option<String>,
    /// The expanded image: the file given, or a staged copy.
    path: PathBuf,
    /// Where it came from, as it is described.
    origin: String,
}

fn import(
    store: &Store,
    local: &Path,
    request: &Request<'_>,
    agent: &ureq::Agent,
    report: Reporter<'_>,
    staged: &Staged,
) -> Result<Imported> {
    let algorithm = request.digest.map_or(ALGORITHM, Digest::algorithm);
    let received = receive(request.source, &staged.received, algorithm, agent, report)?;
    let published = Digest::new(algorithm, &received.hash);
    if let Some(expected) = request.digest
        && *expected != published
    {
        return Err(Error::ImportMismatch {
            source: received.origin,
            expected: expected.to_string(),
            actual: published.to_string(),
        });
    }
    let in_place = received.path != staged.received;
    let prepared = prepare(&received.path, in_place, staged, report)?;
    let fetchable = request.fetchable && request.source.is_url();
    let unchanged =
        !prepared.converted && received.compression.is_none() && received.member.is_none();
    let digest = if fetchable || (unchanged && algorithm == ALGORITHM) {
        store.place(&prepared.path, &published)?;
        published
    } else {
        store.adopt(&prepared.path, ALGORITHM, &mut |progress| {
            report(Event::Hashing(clone::proportion(&progress)));
        })?
    };
    let path = store.path_for(&digest);
    let artifact = Artifact {
        arch: request.arch.to_owned(),
        format: if prepared.media.is_cdrom() {
            "raw".to_owned()
        } else {
            "qcow2".to_owned()
        },
        url: match request.source {
            Source::Url(url) if fetchable => Some(url.clone()),
            _ => None,
        },
        digest,
        size: fs::metadata(&path).ok().map(|data| data.len()),
        compression: if fetchable {
            received.compression
        } else {
            Compression::None
        },
        source_format: (fetchable && prepared.converted).then(|| prepared.format.clone()),
        archive_member: received.member.clone().filter(|_| fetchable),
        media: prepared.media,
        firmware: request.hardware.firmware,
        cpu_model: request.hardware.cpu_model.clone(),
        machine: request.hardware.machine,
        disk: request.hardware.disk,
    };
    let (name, tag) = (request.reference.repository(), request.reference.tag());
    if fetchable {
        store.record_as(name, tag, &artifact)?;
    }
    let description = request.description.map_or_else(
        || format!("Imported from {}", received.origin),
        str::to_owned,
    );
    let entry = NewEntry {
        name,
        tag,
        description: &description,
        login: request.login,
        artifact: &artifact,
    }
    .write(local)?;
    Ok(Imported {
        name: name.to_owned(),
        tag: tag.to_owned(),
        artifact,
        format: prepared.format,
        path,
        entry,
        replaced: false,
    })
}

/// Reads the source through, expanding it if it is compressed and unpacking it if it is an archive.
fn receive(
    source: &Source,
    staged: &Path,
    algorithm: Algorithm,
    agent: &ureq::Agent,
    report: Reporter<'_>,
) -> Result<Received> {
    let (mut stream, total, file, origin): (Box<dyn Read>, _, _, _) = match source {
        Source::File(given) => {
            let unreadable = |error| Error::UnreadableSource {
                name: given.display().to_string(),
                error,
            };
            let path = fs::canonicalize(given).map_err(unreadable)?;
            let file = fs::File::open(&path).map_err(unreadable)?;
            let total = file.metadata().ok().map(|data| data.len());
            let origin = path.display().to_string();
            (Box::new(file), total, Some(path), origin)
        }
        Source::Url(url) => {
            let response = store::get(agent, url)?;
            let total = store::content_length(&response);
            (
                Box::new(response.into_body().into_reader()),
                total,
                None,
                url.clone(),
            )
        }
    };
    let failed = |error| Error::UnreadableSource {
        name: origin.clone(),
        error,
    };
    let mut head = Vec::new();
    (&mut stream)
        .take(512)
        .read_to_end(&mut head)
        .map_err(failed)?;
    let compression = conversion::sniff(&head);
    let archive = tar::is_archive(&head);
    let mut stream = std::io::Cursor::new(head).chain(stream);
    let mut observe = |received| report(Event::Receiving(Progress { received, total }));
    match file {
        // Read where it is, since a descriptor's extents sit beside it.
        Some(path) if compression.is_none() && !archive => {
            let (_, hash) =
                digest::copy_hashing(&mut stream, std::io::sink(), algorithm, &mut observe)
                    .map_err(failed)?;
            Ok(Received {
                hash,
                compression,
                member: None,
                path,
                origin,
            })
        }
        _ => {
            let received = store::receive(
                &mut stream,
                compression,
                store::Unpack::Detect,
                algorithm,
                staged,
                &mut observe,
            )?;
            Ok(Received {
                hash: received.hash,
                member: received.member,
                compression,
                path: staged.to_owned(),
                origin,
            })
        }
    }
}

/// An image ready to store.
struct Prepared {
    path: PathBuf,
    format: String,
    media: Media,
    converted: bool,
}

/// Converts a disk image to qcow2, or copies a CD-ROM image as it is.
fn prepare(
    expanded: &Path,
    in_place: bool,
    staged: &Staged,
    report: Reporter<'_>,
) -> Result<Prepared> {
    if conversion::is_iso(expanded) {
        if in_place {
            fs::copy(expanded, &staged.received).map_err(|source| Error::Store {
                path: staged.received.clone(),
                action: "write",
                source,
            })?;
        }
        return Ok(Prepared {
            path: staged.received.clone(),
            format: "iso".to_owned(),
            media: Media::Cdrom,
            converted: false,
        });
    }
    let probe = conversion::probe(expanded, OPERATION)?;
    if !in_place && probe.format == "qcow2" && probe.external.is_empty() {
        return Ok(Prepared {
            path: expanded.to_owned(),
            format: probe.format,
            media: Media::Disk,
            converted: false,
        });
    }
    let mut converting = |percent| report(Event::Converting(percent));
    if in_place {
        conversion::convert(
            &Conversion {
                source: expanded,
                format: &probe.format,
                destination: &staged.converted,
                in_use: false,
                operation: OPERATION,
            },
            Some(&mut converting),
        )?;
    } else {
        store::to_qcow2(
            expanded,
            &probe.format,
            &staged.converted,
            Some(&mut converting),
        )?;
    }
    Ok(Prepared {
        path: staged.converted.clone(),
        format: probe.format,
        media: Media::Disk,
        converted: true,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::testing::{create_image, pack, serve};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Option<Self> {
            let mut path = std::env::temp_dir();
            path.push(format!("vm-import-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            let scratch = Self(path);
            // Every test here needs qemu-img.
            create_image(&scratch.0.join("probe.qcow2"), "qcow2", "1M").then_some(scratch)
        }

        fn store(&self) -> Store {
            Store::new(self.0.join("images"))
        }

        fn local(&self) -> PathBuf {
            self.0.join("local")
        }

        fn image(&self, name: &str, format: &str) -> PathBuf {
            let path = self.0.join(name);
            assert!(create_image(&path, format, "4M"));
            path
        }

        fn import(
            &self,
            source: &Source,
            target: &str,
            adjust: impl FnOnce(&mut Options),
        ) -> Result<Imported> {
            let reference: Reference = target.parse().unwrap();
            let mut options = Options {
                arch: "amd64".to_owned(),
                description: None,
                login: Login::None,
                hardware: Hardware::default(),
                digest: None,
                fetchable: true,
                force: false,
            };
            adjust(&mut options);
            let request = Request {
                source,
                reference: &reference,
                arch: &options.arch,
                description: options.description.as_deref(),
                login: options.login,
                hardware: options.hardware,
                digest: options.digest.as_ref(),
                fetchable: options.fetchable,
                force: options.force,
            };
            run(
                &self.store(),
                &self.local(),
                &request,
                &store::http_agent(),
                &mut |_| {},
            )
        }

        fn catalogue(&self) -> Catalogue {
            Catalogue::load(&self.local()).unwrap()
        }

        /// Files left in the store's staging area.
        fn staged(&self) -> Vec<String> {
            fs::read_dir(self.0.join("images/blobs"))
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|entry| entry.file_name().to_string_lossy().into_owned())
                        .filter(|name| name.starts_with(".building-"))
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    struct Options {
        arch: String,
        description: Option<String>,
        login: Login,
        hardware: Hardware,
        digest: Option<Digest>,
        fetchable: bool,
        force: bool,
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sha256(bytes: &[u8]) -> Digest {
        let mut ignored = |_| {};
        let (_, hash) = digest::copy_hashing(
            &mut &bytes[..],
            std::io::sink(),
            Algorithm::Sha256,
            &mut ignored,
        )
        .unwrap();
        Digest::new(Algorithm::Sha256, &hash)
    }

    fn format_of(path: &Path) -> String {
        conversion::probe(path, "testing").unwrap().format
    }

    /// A file with the mark an ISO 9660 filesystem carries.
    fn iso_bytes() -> Vec<u8> {
        let mut bytes = vec![0_u8; 64 * 1024];
        bytes[32769..32774].copy_from_slice(b"CD001");
        bytes
    }

    #[test]
    fn a_disk_file_is_converted_to_qcow2_and_named_in_the_store_catalogue() {
        let Some(scratch) = Scratch::new("file") else {
            return;
        };
        let source = Source::File(scratch.image("disk.vmdk", "vmdk"));
        let imported = scratch.import(&source, "mine:1", |_| {}).unwrap();
        assert_eq!(imported.format, "vmdk");
        assert_eq!(format_of(&imported.path), "qcow2");
        assert_eq!(
            imported.artifact.digest,
            sha256(&fs::read(&imported.path).unwrap())
        );
        assert_eq!(imported.artifact.url, None);
        assert_eq!(imported.artifact.source_format, None);
        assert!(!imported.replaced);
        let catalogue = scratch.catalogue();
        let (entry, artifact) = catalogue
            .resolve(&"mine:1".parse().unwrap(), "amd64")
            .unwrap();
        assert_eq!(*artifact, imported.artifact);
        assert_eq!(entry.path, imported.entry);
        assert!(
            entry.description.contains("disk.vmdk"),
            "{}",
            entry.description
        );
        assert!(scratch.staged().is_empty(), "{:?}", scratch.staged());
    }

    #[test]
    fn a_compressed_file_is_expanded_before_it_is_converted() {
        let Some(scratch) = Scratch::new("packedfile") else {
            return;
        };
        let raw = fs::read(scratch.image("disk.img", "raw")).unwrap();
        let packed = scratch.0.join("disk.img.zst");
        fs::write(&packed, pack("zstd", &raw)).unwrap();
        let imported = scratch
            .import(&Source::File(packed), "mine:1", |_| {})
            .unwrap();
        assert_eq!(imported.format, "raw");
        assert_eq!(format_of(&imported.path), "qcow2");
        assert_eq!(imported.artifact.compression, Compression::None);
        assert!(scratch.staged().is_empty(), "{:?}", scratch.staged());
    }

    #[test]
    fn the_image_in_an_archive_file_is_taken_out_and_converted() {
        let Some(scratch) = Scratch::new("archivefile") else {
            return;
        };
        let raw = fs::read(scratch.image("disk.raw", "raw")).unwrap();
        let Some(archive) = crate::testing::tarred(&[("disk.raw", &raw)]) else {
            return;
        };
        let packed = scratch.0.join("image.tar.xz");
        fs::write(&packed, pack("xz", &archive)).unwrap();
        let imported = scratch
            .import(&Source::File(packed), "mine:1", |_| {})
            .unwrap();
        assert_eq!(imported.format, "raw");
        assert_eq!(format_of(&imported.path), "qcow2");
        assert_eq!(imported.artifact.archive_member, None);
        assert_eq!(
            imported.artifact.digest,
            sha256(&fs::read(&imported.path).unwrap())
        );
        assert!(scratch.staged().is_empty(), "{:?}", scratch.staged());
    }

    #[test]
    fn an_uncompressed_archive_file_is_stored_as_its_image_not_as_the_archive() {
        let Some(scratch) = Scratch::new("plainarchive") else {
            return;
        };
        let qcow2 = fs::read(scratch.image("disk.qcow2", "qcow2")).unwrap();
        let Some(archive) = crate::testing::tarred(&[("disk.qcow2", &qcow2)]) else {
            return;
        };
        let tarball = scratch.0.join("image.tar");
        fs::write(&tarball, &archive).unwrap();
        let imported = scratch
            .import(&Source::File(tarball), "mine:1", |_| {})
            .unwrap();
        assert_eq!(fs::read(&imported.path).unwrap(), qcow2);
        assert_eq!(imported.artifact.digest, sha256(&qcow2));
        assert_ne!(imported.artifact.digest, sha256(&archive));
    }

    #[test]
    fn an_archive_download_stays_fetchable_by_its_member() {
        let Some(scratch) = Scratch::new("archiveurl") else {
            return;
        };
        let raw = fs::read(scratch.image("disk.raw", "raw")).unwrap();
        let Some(archive) = crate::testing::tarred(&[("disk.raw", &raw)]) else {
            return;
        };
        let published = pack("gzip", &archive);
        let source = Source::Url(serve(published.clone()));
        let imported = scratch.import(&source, "mine:1", |_| {}).unwrap();
        let artifact = &imported.artifact;
        assert_eq!(artifact.digest, sha256(&published));
        assert_eq!(artifact.compression, Compression::Gzip);
        assert_eq!(artifact.archive_member.as_deref(), Some("disk.raw"));
        assert_eq!(artifact.source_format.as_deref(), Some("raw"));

        fs::remove_file(&imported.path).unwrap();
        let catalogue = scratch.catalogue();
        let (_, written) = catalogue
            .resolve(&"mine:1".parse().unwrap(), "amd64")
            .unwrap();
        assert_eq!(written.archive_member.as_deref(), Some("disk.raw"));
        let refetched = Artifact {
            url: Some(serve(published)),
            ..written.clone()
        };
        scratch
            .store()
            .pull(&refetched, &store::http_agent(), &mut |_| {}, None)
            .unwrap();
        assert_eq!(format_of(&imported.path), "qcow2");
    }

    #[test]
    fn an_archive_of_several_files_is_refused_and_nothing_is_left_behind() {
        let Some(scratch) = Scratch::new("severalfiles") else {
            return;
        };
        let Some(archive) = crate::testing::tarred(&[("a.vmdk", b"a"), ("b.ovf", b"b")]) else {
            return;
        };
        let tarball = scratch.0.join("appliance.ova");
        fs::write(&tarball, archive).unwrap();
        let error = scratch
            .import(&Source::File(tarball), "mine:1", |_| {})
            .unwrap_err();
        assert_eq!(error.kind(), "archive-unreadable", "{error}");
        assert!(error.to_string().contains("a.vmdk, b.ovf"), "{error}");
        assert!(scratch.staged().is_empty(), "{:?}", scratch.staged());
        assert!(scratch.catalogue().entries().is_empty());
    }

    #[test]
    fn a_cdrom_image_is_kept_as_it_is() {
        let Some(scratch) = Scratch::new("iso") else {
            return;
        };
        let iso = scratch.0.join("disc.iso");
        fs::write(&iso, iso_bytes()).unwrap();
        let imported = scratch
            .import(&Source::File(iso), "disc:1", |_| {})
            .unwrap();
        assert!(imported.artifact.media.is_cdrom());
        assert_eq!(imported.artifact.format, "raw");
        assert_eq!(imported.format, "iso");
        assert_eq!(fs::read(&imported.path).unwrap(), iso_bytes());
        assert_eq!(imported.artifact.digest, sha256(&iso_bytes()));
        let catalogue = scratch.catalogue();
        let (_, artifact) = catalogue
            .resolve(&"disc:1".parse().unwrap(), "amd64")
            .unwrap();
        assert!(artifact.media.is_cdrom());
    }

    #[test]
    fn a_download_stays_fetchable_and_is_stored_under_its_published_digest() {
        let Some(scratch) = Scratch::new("url") else {
            return;
        };
        let published = pack("xz", &fs::read(scratch.image("disk.vdi", "vdi")).unwrap());
        let source = Source::Url(serve(published.clone()));
        let imported = scratch.import(&source, "mine:1", |_| {}).unwrap();
        let artifact = &imported.artifact;
        assert_eq!(artifact.digest, sha256(&published));
        assert_eq!(artifact.url.as_deref(), Some(source.describe().as_str()));
        assert_eq!(artifact.compression, Compression::Xz);
        assert_eq!(artifact.source_format.as_deref(), Some("vdi"));
        assert_eq!(format_of(&imported.path), "qcow2");
        assert_eq!(imported.path, scratch.store().path_for(&artifact.digest));
        let record = scratch.0.join("images/refs/mine/1-amd64.toml");
        assert!(
            fs::read_to_string(record)
                .unwrap()
                .contains(&artifact.digest.to_string())
        );

        // What a later pull would do with the entry gives the same kind of file.
        fs::remove_file(&imported.path).unwrap();
        let catalogue = scratch.catalogue();
        let (_, written) = catalogue
            .resolve(&"mine:1".parse().unwrap(), "amd64")
            .unwrap();
        let again = serve(published);
        let refetched = Artifact {
            url: Some(again),
            ..written.clone()
        };
        scratch
            .store()
            .pull(&refetched, &store::http_agent(), &mut |_| {}, None)
            .unwrap();
        assert_eq!(format_of(&imported.path), "qcow2");
    }

    #[test]
    fn a_download_already_in_qcow2_is_stored_as_published() {
        let Some(scratch) = Scratch::new("urlqcow2") else {
            return;
        };
        let published = fs::read(scratch.image("disk.qcow2", "qcow2")).unwrap();
        let imported = scratch
            .import(&Source::Url(serve(published.clone())), "mine:1", |_| {})
            .unwrap();
        assert_eq!(imported.artifact.source_format, None);
        assert_eq!(fs::read(&imported.path).unwrap(), published);
    }

    #[test]
    fn a_download_can_forget_where_it_came_from() {
        let Some(scratch) = Scratch::new("forget") else {
            return;
        };
        let published = pack(
            "gzip",
            &fs::read(scratch.image("disk.vmdk", "vmdk")).unwrap(),
        );
        let source = Source::Url(serve(published.clone()));
        let imported = scratch
            .import(&source, "mine:1", |options| options.fetchable = false)
            .unwrap();
        let artifact = &imported.artifact;
        assert_eq!(artifact.url, None);
        assert_eq!(artifact.compression, Compression::None);
        assert_eq!(artifact.source_format, None);
        assert_eq!(artifact.digest, sha256(&fs::read(&imported.path).unwrap()));
        assert_ne!(artifact.digest, sha256(&published));
        assert!(!scratch.0.join("images/refs").exists());
    }

    #[test]
    fn a_digest_given_is_checked_against_the_file_as_published() {
        let Some(scratch) = Scratch::new("digest") else {
            return;
        };
        let path = scratch.image("disk.qcow2", "qcow2");
        let bytes = fs::read(&path).unwrap();
        let source = Source::File(path);
        let wrong = sha256(b"something else");
        let error = scratch
            .import(&source, "mine:1", |options| {
                options.digest = Some(wrong.clone());
            })
            .unwrap_err();
        assert_eq!(error.kind(), "digest-mismatch");
        assert!(error.to_string().contains(&wrong.to_string()), "{error}");
        assert!(scratch.catalogue().is_empty());
        assert!(scratch.staged().is_empty(), "{:?}", scratch.staged());

        let mut ignored = |_| {};
        let (_, hash) = digest::copy_hashing(
            &mut &bytes[..],
            std::io::sink(),
            Algorithm::Sha512,
            &mut ignored,
        )
        .unwrap();
        let right = Digest::new(Algorithm::Sha512, &hash);
        scratch
            .import(&source, "mine:1", |options| {
                options.digest = Some(right.clone());
            })
            .unwrap();
    }

    #[test]
    fn a_name_already_in_the_store_catalogue_is_replaced_only_when_asked() {
        let Some(scratch) = Scratch::new("exists") else {
            return;
        };
        let source = Source::File(scratch.image("disk.qcow2", "qcow2"));
        scratch.import(&source, "mine:1", |_| {}).unwrap();
        let error = scratch.import(&source, "mine:1", |_| {}).unwrap_err();
        assert_eq!(error.kind(), "image-exists");
        assert!(error.to_string().contains("--force"), "{error}");
        let imported = scratch
            .import(&source, "mine:1", |options| {
                options.force = true;
                options.description = Some("second".to_owned());
            })
            .unwrap();
        assert!(imported.replaced);
        let catalogue = scratch.catalogue();
        assert_eq!(catalogue.find("mine", "1").unwrap().description, "second");
    }

    #[test]
    fn an_alias_in_the_store_catalogue_is_not_replaced() {
        let Some(scratch) = Scratch::new("alias") else {
            return;
        };
        let local = scratch.local().join("mine");
        fs::create_dir_all(&local).unwrap();
        fs::write(
            local.join("1.toml"),
            format!(
                "name = \"mine\"\ntag = \"1\"\naliases = [\"latest\"]\ndescription = \"d\"\nlogin = \"none\"\n\n[[image]]\narch = \"amd64\"\nformat = \"qcow2\"\ndigest = \"sha256:{}\"\n",
                "a".repeat(64)
            ),
        )
        .unwrap();
        let source = Source::File(scratch.image("disk.qcow2", "qcow2"));
        let error = scratch
            .import(&source, "mine:latest", |options| options.force = true)
            .unwrap_err();
        assert_eq!(error.kind(), "image-exists");
        assert!(error.to_string().contains("1.toml"), "{error}");
    }

    #[test]
    fn a_new_image_cannot_be_named_with_a_digest() {
        let Some(scratch) = Scratch::new("pinned") else {
            return;
        };
        let source = Source::File(scratch.image("disk.qcow2", "qcow2"));
        let target = format!("mine:1@sha256:{}", "a".repeat(64));
        let error = scratch.import(&source, &target, |_| {}).unwrap_err();
        assert_eq!(error.kind(), "invalid-reference");
    }

    #[test]
    fn a_file_that_is_not_there_is_refused() {
        let Some(scratch) = Scratch::new("absent") else {
            return;
        };
        let error = scratch
            .import(
                &Source::File(scratch.0.join("absent.qcow2")),
                "mine:1",
                |_| {},
            )
            .unwrap_err();
        assert_eq!(error.kind(), "unreadable-source");
        assert!(error.to_string().contains("absent.qcow2"), "{error}");
    }

    #[test]
    fn a_download_that_reads_other_files_is_refused() {
        let Some(scratch) = Scratch::new("external") else {
            return;
        };
        let base = scratch.image("base.qcow2", "qcow2");
        let top = scratch.0.join("top.qcow2");
        let layered = std::process::Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2", "-F", "qcow2", "-b"])
            .arg(&base)
            .arg(&top)
            .status()
            .unwrap();
        assert!(layered.success());
        let error = scratch
            .import(
                &Source::Url(serve(fs::read(&top).unwrap())),
                "mine:1",
                |_| {},
            )
            .unwrap_err();
        assert_eq!(error.kind(), "conversion-failed", "{error}");
        assert!(scratch.catalogue().is_empty());
        assert!(scratch.staged().is_empty(), "{:?}", scratch.staged());
    }

    #[test]
    fn a_local_layered_image_is_flattened() {
        let Some(scratch) = Scratch::new("flatten") else {
            return;
        };
        let base = scratch.image("base.qcow2", "qcow2");
        let top = scratch.0.join("top.qcow2");
        let layered = std::process::Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2", "-F", "qcow2", "-b"])
            .arg(&base)
            .arg(&top)
            .status()
            .unwrap();
        assert!(layered.success());
        let imported = scratch
            .import(&Source::File(top), "mine:1", |_| {})
            .unwrap();
        assert!(
            conversion::probe(&imported.path, "testing")
                .unwrap()
                .external
                .is_empty()
        );
    }

    #[test]
    fn the_entry_states_the_hardware_and_access_asked_for() {
        let Some(scratch) = Scratch::new("hardware") else {
            return;
        };
        let source = Source::File(scratch.image("disk.qcow2", "qcow2"));
        scratch
            .import(&source, "mine:1", |options| {
                options.arch = "arm64".to_owned();
                options.login = Login::CloudInit;
                options.hardware = Hardware {
                    firmware: Firmware::Uefi,
                    cpu_model: Some("Penryn".to_owned()),
                    machine: Chipset::Pc,
                    disk: Disk::Ide,
                };
            })
            .unwrap();
        let catalogue = scratch.catalogue();
        let (entry, artifact) = catalogue
            .resolve(&"mine:1".parse().unwrap(), "arm64")
            .unwrap();
        assert_eq!(entry.login, Login::CloudInit);
        assert_eq!(artifact.firmware, Firmware::Uefi);
        assert_eq!(artifact.cpu_model.as_deref(), Some("Penryn"));
        assert_eq!(artifact.machine, Chipset::Pc);
        assert_eq!(artifact.disk, Disk::Ide);
    }

    #[test]
    fn progress_is_reported_for_each_pass() {
        let Some(scratch) = Scratch::new("events") else {
            return;
        };
        let source = Source::File(scratch.image("disk.vmdk", "vmdk"));
        let reference: Reference = "mine:1".parse().unwrap();
        let request = Request {
            source: &source,
            reference: &reference,
            arch: "amd64",
            description: None,
            login: Login::None,
            hardware: Hardware::default(),
            digest: None,
            fetchable: true,
            force: false,
        };
        let mut events = Vec::new();
        run(
            &scratch.store(),
            &scratch.local(),
            &request,
            &store::http_agent(),
            &mut |event| events.push(event),
        )
        .unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Receiving(_)))
        );
        assert!(events.contains(&Event::Converting(100)), "{events:?}");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Hashing(_)))
        );
    }

    #[test]
    fn a_source_is_a_url_only_when_it_says_so() {
        assert!(Source::parse("https://example.invalid/x.qcow2").is_url());
        assert!(Source::parse("http://example.invalid/x.qcow2").is_url());
        assert_eq!(
            Source::parse("./ftp://x"),
            Source::File(PathBuf::from("./ftp://x"))
        );
        assert!(!Source::parse("disk.vmdk").is_url());
    }
}

//! Finding and removing what machines and images no longer need.

use crate::catalogue::Catalogue;
use crate::error::{Error, Result};
use crate::instance::{Directory, Instances, socket_id};
use crate::reference::Digest;
use crate::store::Store;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long a partial file is left alone, in case something is still writing it.
pub const PARTIAL_AGE: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Machine,
    /// Sockets and pid files in the runtime directory.
    Runtime,
    Image,
    /// A record of a pulled reference.
    Record,
    /// A file from a pull, clone or import that did not finish.
    Partial,
}

impl Kind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Machine => "machine",
            Self::Runtime => "runtime",
            Self::Image => "image",
            Self::Record => "record",
            Self::Partial => "partial",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The machine's image is not held and cannot be fetched.
    ImageGone,
    Stopped,
    /// The machine's record cannot be read.
    Unreadable,
    /// Left by a machine that is not running.
    Leftover,
    /// No catalogue entry names the image.
    Orphaned,
    /// No machine uses the image.
    Unused,
    /// The image file a record names is gone.
    FileGone,
    /// Left by a pull, clone or import that did not finish.
    Interrupted,
}

impl Reason {
    pub const fn name(self) -> &'static str {
        match self {
            Self::ImageGone => "image-gone",
            Self::Stopped => "stopped",
            Self::Unreadable => "unreadable",
            Self::Leftover => "leftover",
            Self::Orphaned => "orphaned",
            Self::Unused => "unused",
            Self::FileGone => "file-gone",
            Self::Interrupted => "interrupted",
        }
    }

    pub const fn describe(self) -> &'static str {
        match self {
            Self::ImageGone => "its image is gone and cannot be fetched",
            Self::Stopped => "stopped",
            Self::Unreadable => "its record cannot be read",
            Self::Leftover => "left by a machine that is not running",
            Self::Orphaned => "no catalogue entry names it",
            Self::Unused => "no machine uses it",
            Self::FileGone => "its image file is gone",
            Self::Interrupted => "left by an unfinished pull, clone or import",
        }
    }
}

/// Something to remove, and the files that make it up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub kind: Kind,
    pub name: String,
    pub reason: Reason,
    /// Bytes allocated on disk.
    pub size: u64,
    pub paths: Vec<PathBuf>,
}

/// Machines to remove: those that cannot start again, or with `all` every stopped one.
pub fn machines(
    instances: &Instances,
    store: &Store,
    catalogue: &Catalogue,
    all: bool,
) -> Result<Vec<Item>> {
    let names = instances.names()?;
    let mut items = Vec::new();
    for name in &names {
        let Ok(directory) = instances.open(name) else {
            continue;
        };
        let reason = match directory.read() {
            Ok(held) if held.is_running() => continue,
            Ok(held) if held.needs_image() && !obtainable(store, catalogue, &held.digest) => {
                Some(Reason::ImageGone)
            }
            Ok(_) => all.then_some(Reason::Stopped),
            Err(_) if answers(&directory) => continue,
            Err(_) => all.then_some(Reason::Unreadable),
        };
        match reason {
            Some(reason) => items.push(machine(&directory, reason)),
            None => items.extend(leftover(name, directory.runtime_files())),
        }
    }
    items.extend(strays(instances.runtime(), &names));
    Ok(items)
}

/// Whether an image is held, or could be fetched again.
fn obtainable(store: &Store, catalogue: &Catalogue, digest: &str) -> bool {
    let Ok(digest) = digest.parse::<Digest>() else {
        return true;
    };
    store.contains(&digest)
        || catalogue.entries().into_iter().any(|entry| {
            entry
                .artifacts
                .iter()
                .any(|artifact| artifact.digest == digest && artifact.url.is_some())
        })
}

/// Whether something is listening on the machine's monitor.
fn answers(directory: &Directory) -> bool {
    listening(directory.monitor())
}

fn listening(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

fn machine(directory: &Directory, reason: Reason) -> Item {
    let mut paths = vec![directory.path().to_owned()];
    paths.extend(directory.runtime_files());
    Item {
        kind: Kind::Machine,
        name: directory.name().to_owned(),
        reason,
        size: occupied(directory.path()),
        paths,
    }
}

fn leftover(name: &str, files: Vec<PathBuf>) -> Option<Item> {
    (!files.is_empty()).then(|| Item {
        kind: Kind::Runtime,
        name: name.to_owned(),
        reason: Reason::Leftover,
        size: files.iter().map(|file| occupied(file)).sum(),
        paths: files,
    })
}

/// Runtime files belonging to no machine, grouped by socket id.
fn strays(runtime: &Path, names: &[String]) -> Vec<Item> {
    let known: Vec<String> = names.iter().map(|name| socket_id(name)).collect();
    let Ok(entries) = fs::read_dir(runtime) else {
        return Vec::new();
    };
    let mut groups: std::collections::BTreeMap<String, Vec<PathBuf>> =
        std::collections::BTreeMap::new();
    for entry in entries.flatten() {
        let Ok(file) = entry.file_name().into_string() else {
            continue;
        };
        let Some((id, _)) = file.split_once('.') else {
            continue;
        };
        if !known.iter().any(|held| held == id) {
            groups.entry(id.to_owned()).or_default().push(entry.path());
        }
    }
    groups
        .into_iter()
        .filter(|(_, files)| !files.iter().any(|file| listening(file)))
        .filter_map(|(id, mut files)| {
            files.sort();
            leftover(&id, files)
        })
        .collect()
}

/// The image files the machines not being removed are built on.
pub fn in_use(instances: &Instances, store: &Store, removing: &[Item]) -> Result<Vec<PathBuf>> {
    let mut used = Vec::new();
    for name in instances.names()? {
        let Ok(directory) = instances.open(&name) else {
            continue;
        };
        if removing
            .iter()
            .any(|item| item.kind == Kind::Machine && item.name == name)
        {
            continue;
        }
        if let Some(backing) = crate::disk::backing_file(&directory.overlay()) {
            used.push(canonical(&backing));
        }
        if let Ok(held) = directory.read()
            && held.needs_image()
            && let Ok(digest) = held.digest.parse::<Digest>()
        {
            used.push(canonical(&store.path_for(&digest)));
        }
    }
    Ok(used)
}

/// Images to remove: those nothing names or needs, and with `all` those no machine uses.
pub fn images(
    store: &Store,
    catalogue: &Catalogue,
    in_use: &[PathBuf],
    all: bool,
    now: SystemTime,
) -> Vec<Item> {
    let records = records(store);
    let blobs = store.root().join("blobs");
    let mut items = Vec::new();
    for (path, digest) in held(&blobs) {
        if in_use.iter().any(|used| *used == canonical(&path)) {
            continue;
        }
        let naming: Vec<_> = catalogue
            .entries()
            .into_iter()
            .filter_map(|entry| {
                entry
                    .artifacts
                    .iter()
                    .find(|artifact| artifact.digest == digest)
                    .map(|artifact| (entry, artifact))
            })
            .collect();
        let reason = if naming.is_empty() {
            Reason::Orphaned
        } else if all && naming.iter().all(|(_, artifact)| artifact.url.is_some()) {
            Reason::Unused
        } else {
            continue;
        };
        let recorded: Vec<&Record> = records
            .iter()
            .filter(|record| record.digest == digest.to_string())
            .collect();
        let mut names: Vec<String> = naming
            .iter()
            .map(|(entry, _)| format!("{}:{}", entry.name, entry.tag))
            .chain(recorded.iter().map(|record| record.reference()))
            .collect();
        names = names.into_iter().fold(Vec::new(), |mut unique, name| {
            if !unique.contains(&name) {
                unique.push(name);
            }
            unique
        });
        let name = names.first().cloned().unwrap_or_else(|| short(&digest));
        let mut paths = vec![path.clone()];
        paths.extend(recorded.iter().map(|record| record.path.clone()));
        items.push(Item {
            kind: Kind::Image,
            name,
            reason,
            size: occupied(&path),
            paths,
        });
    }
    for record in &records {
        let gone = record
            .digest
            .parse::<Digest>()
            .is_ok_and(|digest| !store.contains(&digest));
        if gone {
            items.push(Item {
                kind: Kind::Record,
                name: record.reference(),
                reason: Reason::FileGone,
                size: occupied(&record.path),
                paths: vec![record.path.clone()],
            });
        }
    }
    items.extend(partials(&blobs, now));
    items
}

/// Every image file in the store, with its digest.
fn held(blobs: &Path) -> Vec<(PathBuf, Digest)> {
    let mut found = Vec::new();
    for algorithm in listing(blobs).into_iter().filter(|path| path.is_dir()) {
        let Some(name) = algorithm.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        for file in listing(&algorithm) {
            let digest = file
                .file_name()
                .and_then(|hex| hex.to_str())
                .and_then(|hex| format!("{name}:{hex}").parse::<Digest>().ok());
            if let Some(digest) = digest.filter(|_| file.is_file()) {
                found.push((file, digest));
            }
        }
    }
    found
}

/// Files a pull, clone or import was writing, left for [`PARTIAL_AGE`] before counting as abandoned.
fn partials(blobs: &Path, now: SystemTime) -> Vec<Item> {
    let building = listing(blobs)
        .into_iter()
        .filter(|path| file_name(path).starts_with(".building-"));
    let partial = listing(blobs)
        .into_iter()
        .filter(|path| path.is_dir())
        .flat_map(|algorithm| listing(&algorithm))
        .filter(|path| file_name(path).ends_with(".partial"));
    building
        .chain(partial)
        .filter(|path| {
            fs::metadata(path)
                .and_then(|data| data.modified())
                .is_ok_and(|modified| {
                    now.duration_since(modified)
                        .is_ok_and(|age| age >= PARTIAL_AGE)
                })
        })
        .map(|path| Item {
            kind: Kind::Partial,
            name: file_name(&path),
            reason: Reason::Interrupted,
            size: occupied(&path),
            paths: vec![path],
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct RawRecord {
    name: String,
    tag: String,
    arch: String,
    digest: String,
}

#[derive(Debug)]
struct Record {
    name: String,
    tag: String,
    arch: String,
    digest: String,
    path: PathBuf,
}

impl Record {
    fn reference(&self) -> String {
        format!("{}:{} ({})", self.name, self.tag, self.arch)
    }
}

/// Every readable record of a pulled reference.
fn records(store: &Store) -> Vec<Record> {
    let mut found = Vec::new();
    for directory in listing(&store.root().join("refs")) {
        for path in listing(&directory) {
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(raw) = basic_toml::from_str::<RawRecord>(&text) {
                found.push(Record {
                    name: raw.name,
                    tag: raw.tag,
                    arch: raw.arch,
                    digest: raw.digest,
                    path,
                });
            }
        }
    }
    found
}

/// Removes every item's files, and any record directory left empty.
pub fn remove(items: &[Item]) -> Result<()> {
    for path in items.iter().flat_map(|item| &item.paths) {
        let outcome = match fs::symlink_metadata(path) {
            Ok(data) if data.is_dir() => fs::remove_dir_all(path),
            Ok(_) => fs::remove_file(path),
            Err(error) => Err(error),
        };
        match outcome {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::Store {
                    path: path.clone(),
                    action: "remove",
                    source,
                });
            }
        }
        if let Some(parent) = path.parent()
            && parent
                .parent()
                .is_some_and(|grandparent| grandparent.file_name() == Some("refs".as_ref()))
        {
            let _ = fs::remove_dir(parent);
        }
    }
    Ok(())
}

/// Bytes allocated to a file or directory tree.
fn occupied(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(data) = fs::symlink_metadata(path) else {
        return 0;
    };
    let own = data.blocks().saturating_mul(512);
    if data.is_dir() {
        listing(path)
            .iter()
            .map(|child| occupied(child))
            .fold(own, u64::saturating_add)
    } else {
        own
    }
}

fn listing(directory: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(directory)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    paths.sort();
    paths
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

/// A digest shortened for display.
fn short(digest: &Digest) -> String {
    let hex = digest.hex();
    format!("{}:{}", digest.algorithm_name(), &hex[..hex.len().min(12)])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::instance::Instance;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("vmp-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            for directory in ["store", "instances", "run", "catalogue"] {
                fs::create_dir_all(path.join(directory)).unwrap();
            }
            Self(path)
        }

        fn store(&self) -> Store {
            Store::new(self.0.join("store"))
        }

        fn instances(&self) -> Instances {
            Instances::at(self.0.join("instances"), self.0.join("run"))
        }

        fn catalogue(&self) -> Catalogue {
            Catalogue::load(&self.0.join("catalogue")).unwrap()
        }

        /// A catalogue entry naming `digest`, fetchable unless `url` is false.
        fn entry(&self, name: &str, digest: &Digest, url: bool) {
            let directory = self.0.join("catalogue").join(name);
            fs::create_dir_all(&directory).unwrap();
            let source = if url {
                format!("url = \"https://example.invalid/{name}.qcow2\"\n")
            } else {
                String::new()
            };
            fs::write(
                directory.join("1.toml"),
                format!(
                    "name = \"{name}\"\ntag = \"1\"\ndescription = \"test\"\nlogin = \"cloud-init\"\n\n\
                     [[image]]\narch = \"amd64\"\nformat = \"qcow2\"\ndigest = \"{digest}\"\n{source}"
                ),
            )
            .unwrap();
        }

        fn blob(&self, digest: &Digest) -> PathBuf {
            let path = self.store().path_for(digest);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, vec![1_u8; 8192]).unwrap();
            path
        }

        fn record(&self, name: &str, digest: &Digest) -> PathBuf {
            let directory = self.0.join("store/refs").join(name);
            fs::create_dir_all(&directory).unwrap();
            let path = directory.join("1-amd64.toml");
            fs::write(
                &path,
                format!(
                    "name = \"{name}\"\ntag = \"1\"\narch = \"amd64\"\ndigest = \"{digest}\"\nurl = \"\"\n"
                ),
            )
            .unwrap();
            path
        }

        fn machine(&self, name: &str, digest: &Digest, running: bool) -> Directory {
            let instances = self.instances();
            let directory = instances.create(name).unwrap();
            let handle = running
                .then(|| crate::process::Handle::of(std::process::id()))
                .flatten();
            directory
                .write(&Instance {
                    name: name.to_owned(),
                    image: "test:1".to_owned(),
                    digest: digest.to_string(),
                    arch: "amd64".to_owned(),
                    created: 1_700_000_000,
                    memory: 2048,
                    cpus: 2,
                    firmware: crate::machine::Firmware::Bios,
                    cpu_model: "max".to_owned(),
                    machine: crate::machine::Chipset::Q35,
                    disk: crate::machine::Disk::Virtio,
                    user: "vm".to_owned(),
                    seeded: true,
                    monitor: directory.monitor().to_owned(),
                    ssh_port: None,
                    pid: handle.map(|held| held.pid),
                    started: handle.map(|held| held.started),
                    generation: 0,
                    ssh_config: false,
                    auto_remove: false,
                    media: crate::catalogue::Media::Disk,
                    cdrom: None,
                    password: None,
                    ports: Vec::new(),
                    shares: Vec::new(),
                })
                .unwrap();
            directory
        }

        fn machines(&self, all: bool) -> Vec<Item> {
            machines(&self.instances(), &self.store(), &self.catalogue(), all).unwrap()
        }

        fn images(&self, all: bool) -> Vec<Item> {
            let used = in_use(&self.instances(), &self.store(), &[]).unwrap();
            images(
                &self.store(),
                &self.catalogue(),
                &used,
                all,
                SystemTime::now(),
            )
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn digest(fill: char) -> Digest {
        format!("sha256:{}", fill.to_string().repeat(64))
            .parse()
            .unwrap()
    }

    fn summary(items: &[Item]) -> Vec<(Kind, String, Reason)> {
        items
            .iter()
            .map(|item| (item.kind, item.name.clone(), item.reason))
            .collect()
    }

    /// A machine made from a CD-ROM image, with the CD-ROM still in or ejected.
    fn cdrom_machine(scratch: &Scratch, name: &str, digest: &Digest, inserted: bool) {
        let directory = scratch.machine(name, digest, false);
        let mut held = directory.read().unwrap();
        held.media = crate::catalogue::Media::Cdrom;
        held.cdrom = inserted.then(|| scratch.store().path_for(digest));
        directory.write(&held).unwrap();
    }

    #[test]
    fn a_machine_with_its_cdrom_ejected_does_not_need_the_image() {
        let scratch = Scratch::new("ejected");
        scratch.blob(&digest('a'));
        cdrom_machine(&scratch, "installed", &digest('a'), false);
        assert!(scratch.machines(false).is_empty());
        assert_eq!(
            summary(&scratch.images(false)),
            [(Kind::Image, short(&digest('a')), Reason::Orphaned)]
        );
        fs::remove_file(scratch.store().path_for(&digest('a'))).unwrap();
        assert!(
            scratch.machines(false).is_empty(),
            "the installed machine was taken"
        );
    }

    #[test]
    fn a_machine_with_its_cdrom_in_needs_the_image() {
        let scratch = Scratch::new("inserted");
        cdrom_machine(&scratch, "installing", &digest('a'), true);
        assert_eq!(
            summary(&scratch.machines(false)),
            [(Kind::Machine, "installing".to_owned(), Reason::ImageGone)]
        );
        scratch.blob(&digest('a'));
        assert!(scratch.machines(false).is_empty());
        assert!(scratch.images(false).is_empty());
    }

    #[test]
    fn a_stopped_machine_is_removed_only_with_all() {
        let scratch = Scratch::new("stopped");
        scratch.blob(&digest('a'));
        scratch.machine("one", &digest('a'), false);
        assert!(scratch.machines(false).is_empty());
        assert_eq!(
            summary(&scratch.machines(true)),
            [(Kind::Machine, "one".to_owned(), Reason::Stopped)]
        );
    }

    #[test]
    fn a_running_machine_is_never_removed() {
        let scratch = Scratch::new("running");
        scratch.machine("one", &digest('a'), true);
        assert!(scratch.machines(true).is_empty());
    }

    #[test]
    fn a_machine_whose_image_cannot_be_had_is_removed_by_default() {
        let scratch = Scratch::new("imagegone");
        scratch.machine("lost", &digest('a'), false);
        scratch.machine("fetchable", &digest('b'), false);
        scratch.entry("fetchable", &digest('b'), true);
        assert_eq!(
            summary(&scratch.machines(false)),
            [(Kind::Machine, "lost".to_owned(), Reason::ImageGone)]
        );
    }

    #[test]
    fn a_machine_with_an_unreadable_record_is_removed_only_with_all() {
        let scratch = Scratch::new("unreadable");
        let directory = scratch.instances().create("broken").unwrap();
        fs::write(directory.record(), "not toml").unwrap();
        assert!(scratch.machines(false).is_empty());
        assert_eq!(
            summary(&scratch.machines(true)),
            [(Kind::Machine, "broken".to_owned(), Reason::Unreadable)]
        );
    }

    #[test]
    fn a_machine_with_an_unreadable_record_that_answers_is_kept() {
        let scratch = Scratch::new("answers");
        let directory = scratch.instances().create("broken").unwrap();
        fs::write(directory.record(), "not toml").unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(directory.monitor()).unwrap();
        assert!(scratch.machines(true).is_empty());
    }

    #[test]
    fn a_removed_machine_takes_its_directory_and_runtime_files() {
        let scratch = Scratch::new("machinepaths");
        let directory = scratch.machine("lost", &digest('a'), false);
        let socket = directory.monitor().to_owned();
        fs::write(&socket, "").unwrap();
        let items = scratch.machines(false);
        assert_eq!(
            items[0].paths,
            [directory.path().to_owned(), socket.clone()]
        );
        assert!(items[0].size > 0);
        remove(&items).unwrap();
        assert!(!directory.path().exists());
        assert!(!socket.exists());
    }

    #[test]
    fn runtime_files_of_a_kept_stopped_machine_or_no_machine_are_leftovers() {
        let scratch = Scratch::new("runtime");
        scratch.blob(&digest('a'));
        let stopped = scratch.machine("stopped", &digest('a'), false);
        let running = scratch.machine("running", &digest('a'), true);
        fs::write(stopped.monitor(), "").unwrap();
        fs::write(running.monitor(), "").unwrap();
        let stray = scratch.0.join("run/0123456789abcdef.console");
        fs::write(&stray, "").unwrap();
        fs::write(scratch.0.join("run/no-dot-here"), "").unwrap();
        assert_eq!(
            summary(&scratch.machines(false)),
            [
                (Kind::Runtime, "stopped".to_owned(), Reason::Leftover),
                (
                    Kind::Runtime,
                    "0123456789abcdef".to_owned(),
                    Reason::Leftover
                ),
            ]
        );
    }

    #[test]
    fn runtime_files_something_still_listens_on_are_kept() {
        let scratch = Scratch::new("listening");
        let socket = scratch.0.join("run/fedcba9876543210.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        fs::write(scratch.0.join("run/fedcba9876543210.console"), "").unwrap();
        assert!(scratch.machines(true).is_empty());
    }

    #[test]
    fn an_image_no_entry_names_is_orphaned() {
        let scratch = Scratch::new("orphan");
        let path = scratch.blob(&digest('a'));
        let record = scratch.record("old", &digest('a'));
        let items = scratch.images(false);
        assert_eq!(
            summary(&items),
            [(Kind::Image, "old:1 (amd64)".to_owned(), Reason::Orphaned)]
        );
        assert_eq!(items[0].paths, [path, record]);
    }

    #[test]
    fn an_orphan_without_a_record_is_named_by_its_digest() {
        let scratch = Scratch::new("orphandigest");
        scratch.blob(&digest('a'));
        assert_eq!(scratch.images(false)[0].name, "sha256:aaaaaaaaaaaa");
    }

    #[test]
    fn a_pulled_image_no_machine_uses_is_removed_only_with_all() {
        let scratch = Scratch::new("unused");
        scratch.blob(&digest('a'));
        scratch.entry("debian", &digest('a'), true);
        assert!(scratch.images(false).is_empty());
        assert_eq!(
            summary(&scratch.images(true)),
            [(Kind::Image, "debian:1".to_owned(), Reason::Unused)]
        );
    }

    #[test]
    fn a_clone_is_never_removed() {
        let scratch = Scratch::new("clone");
        scratch.blob(&digest('a'));
        scratch.entry("mine", &digest('a'), false);
        scratch.entry("alsofetchable", &digest('a'), true);
        assert!(scratch.images(true).is_empty());
    }

    #[test]
    fn an_image_a_machine_uses_is_kept() {
        let scratch = Scratch::new("inuse");
        scratch.blob(&digest('a'));
        scratch.blob(&digest('b'));
        scratch.machine("one", &digest('a'), false);
        assert_eq!(
            summary(&scratch.images(true)),
            [(
                Kind::Image,
                "sha256:bbbbbbbbbbbb".to_owned(),
                Reason::Orphaned
            )]
        );
    }

    #[test]
    fn a_machine_with_an_unreadable_record_keeps_the_image_its_disk_names() {
        let scratch = Scratch::new("backing");
        let base = scratch.blob(&digest('a'));
        let directory = scratch.instances().create("broken").unwrap();
        fs::write(directory.record(), "not toml").unwrap();
        let made = std::process::Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2", "-F", "raw", "-b"])
            .arg(&base)
            .arg(directory.overlay())
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return;
        }
        assert!(scratch.images(true).is_empty());
    }

    #[test]
    fn the_images_of_machines_being_removed_are_not_in_use() {
        let scratch = Scratch::new("removing");
        scratch.blob(&digest('a'));
        scratch.entry("debian", &digest('a'), true);
        scratch.machine("one", &digest('a'), false);
        let instances = scratch.instances();
        let store = scratch.store();
        let catalogue = scratch.catalogue();
        let removing = machines(&instances, &store, &catalogue, true).unwrap();
        let used = in_use(&instances, &store, &removing).unwrap();
        assert!(used.is_empty());
        let kept = in_use(&instances, &store, &[]).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(
            summary(&images(&store, &catalogue, &used, true, SystemTime::now())),
            [(Kind::Image, "debian:1".to_owned(), Reason::Unused)]
        );
    }

    #[test]
    fn a_record_whose_file_is_gone_is_removed() {
        let scratch = Scratch::new("record");
        let path = scratch.record("marked", &digest('a'));
        let items = scratch.images(false);
        assert_eq!(
            summary(&items),
            [(
                Kind::Record,
                "marked:1 (amd64)".to_owned(),
                Reason::FileGone
            )]
        );
        remove(&items).unwrap();
        assert!(!path.exists());
        assert!(!scratch.0.join("store/refs/marked").exists());
        assert!(scratch.0.join("store/refs").exists());
    }

    #[test]
    fn a_partial_file_is_removed_once_it_is_old_enough() {
        let scratch = Scratch::new("partial");
        let blobs = scratch.0.join("store/blobs");
        fs::create_dir_all(blobs.join("sha512")).unwrap();
        fs::write(blobs.join(".building-mine-latest"), "x").unwrap();
        fs::write(blobs.join("sha512/abc.partial"), "x").unwrap();
        let store = scratch.store();
        let catalogue = scratch.catalogue();
        assert!(images(&store, &catalogue, &[], true, SystemTime::now()).is_empty());
        let later = SystemTime::now() + PARTIAL_AGE;
        assert_eq!(
            summary(&images(&store, &catalogue, &[], false, later)),
            [
                (
                    Kind::Partial,
                    ".building-mine-latest".to_owned(),
                    Reason::Interrupted
                ),
                (Kind::Partial, "abc.partial".to_owned(), Reason::Interrupted),
            ]
        );
    }

    #[test]
    fn removing_what_is_already_gone_succeeds() {
        let scratch = Scratch::new("gone");
        let item = Item {
            kind: Kind::Partial,
            name: "x".to_owned(),
            reason: Reason::Interrupted,
            size: 0,
            paths: vec![scratch.0.join("absent")],
        };
        remove(&[item]).unwrap();
    }

    #[test]
    fn every_kind_and_reason_has_a_stable_name() {
        let kinds = [
            Kind::Machine,
            Kind::Runtime,
            Kind::Image,
            Kind::Record,
            Kind::Partial,
        ];
        assert_eq!(
            kinds.map(Kind::name),
            ["machine", "runtime", "image", "record", "partial"]
        );
        let reasons = [
            Reason::ImageGone,
            Reason::Stopped,
            Reason::Unreadable,
            Reason::Leftover,
            Reason::Orphaned,
            Reason::Unused,
            Reason::FileGone,
            Reason::Interrupted,
        ];
        for reason in reasons {
            assert!(!reason.describe().is_empty());
        }
        assert_eq!(
            reasons.map(Reason::name),
            [
                "image-gone",
                "stopped",
                "unreadable",
                "leftover",
                "orphaned",
                "unused",
                "file-gone",
                "interrupted"
            ]
        );
    }
}

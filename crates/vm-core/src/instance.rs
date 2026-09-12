//! Instances: what was asked for, where it lives, and whether it is running.
//!
//! There is no daemon, so the state directory is the only record that an
//! instance exists. Creating its directory is the lock that stops two `vm run`
//! commands claiming one name, because a directory can only be created once.

use crate::error::{Error, Result};
use crate::process::Handle;
use crate::{paths, seed};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A host port forwarded to a guest port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    pub host: u16,
    pub guest: u16,
}

/// A host directory shared into the guest over virtiofs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Share {
    pub tag: String,
    pub source: PathBuf,
    pub target: String,
}

/// What an instance is, as written to `instance.toml`.
///
/// Scalars come before the lists because that is the order TOML itself
/// requires: a table cannot be followed by a bare key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    pub name: String,
    /// The reference as the user wrote it, kept for display.
    pub image: String,
    /// What it resolved to. The reference may move; this does not.
    pub digest: String,
    pub arch: String,
    /// Seconds since the epoch.
    pub created: u64,
    /// Mebibytes.
    pub memory: u64,
    pub cpus: u32,
    /// The account cloud-init was told to create, and the one `vm ssh` uses.
    pub user: String,
    /// Whether the image can be seeded at all, from its catalogue entry.
    pub seeded: bool,
    pub monitor: PathBuf,
    /// The host port forwarded to the guest's SSH port. Allocated at run time
    /// so that a machine can be reached without having been asked to publish
    /// anything, and absent when the image takes no key.
    pub ssh_port: Option<u16>,
    /// Absent until the hypervisor is launched, and again once it has gone.
    pub pid: Option<u32>,
    /// Paired with the pid so a reused pid is not mistaken for this one.
    pub started: Option<u64>,
    /// An empty list is left out rather than written as `[]`: TOML has no way
    /// to write a bare key after a table, so a written-out empty list after a
    /// populated one makes the file unwritable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<Port>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shares: Vec<Share>,
}

impl Instance {
    /// The process, if this instance claims to have one. Whether it is still
    /// running is a separate question, which [`Handle::is_running`] answers.
    pub const fn handle(&self) -> Option<Handle> {
        match (self.pid, self.started) {
            (Some(pid), Some(started)) => Some(Handle { pid, started }),
            _ => None,
        }
    }

    /// Whether the hypervisor recorded for this instance is still there.
    pub fn is_running(&self) -> bool {
        self.handle().is_some_and(|handle| handle.is_running())
    }

    /// Forgets a process that is no longer running. Stale state is reaped on
    /// read rather than by anything sweeping in the background.
    pub const fn forget_process(&mut self) {
        self.pid = None;
        self.started = None;
    }
}

/// The state directory holding every instance.
#[derive(Debug, Clone)]
pub struct Instances {
    root: PathBuf,
    runtime: PathBuf,
}

impl Instances {
    /// Locates the state directory from the environment.
    pub fn discover() -> Result<Self> {
        Ok(Self {
            root: paths::instances_directory().ok_or(Error::NoStateDirectory)?,
            runtime: paths::runtime_directory(),
        })
    }

    pub fn at(root: impl Into<PathBuf>, runtime: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            runtime: runtime.into(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Claims a name. The directory creation is the lock: a second attempt on
    /// the same name fails rather than joining the first.
    pub fn create(&self, name: &str) -> Result<Directory> {
        check_name(name)?;
        let path = self.root.join(name);
        fs::create_dir_all(&self.root).map_err(|source| Error::State {
            path: self.root.clone(),
            action: "create the state directory",
            source,
        })?;
        match fs::create_dir(&path) {
            Ok(()) => Ok(self.directory(name)),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(Error::InstanceExists {
                    name: name.to_owned(),
                })
            }
            Err(source) => Err(Error::State {
                path,
                action: "create the instance directory",
                source,
            }),
        }
    }

    pub fn open(&self, name: &str) -> Result<Directory> {
        check_name(name)?;
        if self.root.join(name).is_dir() {
            Ok(self.directory(name))
        } else {
            Err(Error::UnknownInstance {
                name: name.to_owned(),
            })
        }
    }

    /// Every instance that has a directory, in name order. A directory without
    /// a readable record is skipped rather than failing the listing, so one
    /// damaged instance does not hide the rest.
    pub fn names(&self) -> Result<Vec<String>> {
        let listing = match fs::read_dir(&self.root) {
            Ok(listing) => listing,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(Error::State {
                    path: self.root.clone(),
                    action: "read the state directory",
                    source,
                });
            }
        };
        let mut names: Vec<String> = listing
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        names.sort();
        Ok(names)
    }

    fn directory(&self, name: &str) -> Directory {
        Directory {
            path: self.root.join(name),
            monitor: self.runtime.join(format!("{}.sock", socket_id(name))),
            name: name.to_owned(),
        }
    }
}

/// One instance's own directory.
#[derive(Debug, Clone)]
pub struct Directory {
    path: PathBuf,
    monitor: PathBuf,
    name: String,
}

impl Directory {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The monitor socket, which lives in the runtime directory rather than
    /// here: a Unix socket path is bounded at 107 bytes and this one is not.
    pub fn monitor(&self) -> &Path {
        &self.monitor
    }

    pub fn record(&self) -> PathBuf {
        self.path.join("instance.toml")
    }

    /// The writable disk, backed by the image in the store.
    pub fn overlay(&self) -> PathBuf {
        self.path.join("overlay.qcow2")
    }

    pub fn seed(&self) -> PathBuf {
        self.path.join("seed.img")
    }

    /// Where the guest's serial console is written.
    pub fn console(&self) -> PathBuf {
        self.path.join("console.log")
    }

    /// Where the hypervisor's own complaints go.
    pub fn log(&self) -> PathBuf {
        self.path.join("hypervisor.log")
    }

    pub fn key(&self) -> PathBuf {
        self.path.join("id_ed25519")
    }

    pub fn public_key(&self) -> PathBuf {
        self.path.join("id_ed25519.pub")
    }

    /// A host-key file of this instance's own, so that rebuilding a machine
    /// never provokes a warning about the user's own `known_hosts`.
    pub fn known_hosts(&self) -> PathBuf {
        self.path.join("known_hosts")
    }

    pub fn read(&self) -> Result<Instance> {
        let path = self.record();
        let text = fs::read_to_string(&path).map_err(|source| Error::State {
            path: path.clone(),
            action: "read the instance record",
            source,
        })?;
        basic_toml::from_str(&text).map_err(|source| Error::InstanceRecord {
            path,
            reason: source.to_string(),
        })
    }

    /// Writes the record through a temporary file, so that an interrupted
    /// write leaves the previous record rather than half of a new one.
    pub fn write(&self, instance: &Instance) -> Result<()> {
        let text = basic_toml::to_string(instance).map_err(|source| Error::InstanceRecord {
            path: self.record(),
            reason: source.to_string(),
        })?;
        let staging = self.path.join("instance.toml.new");
        fs::write(&staging, text).map_err(|source| Error::State {
            path: staging.clone(),
            action: "write the instance record",
            source,
        })?;
        fs::rename(&staging, self.record()).map_err(|source| Error::State {
            path: self.record(),
            action: "replace the instance record",
            source,
        })
    }

    /// Removes the instance, monitor socket included. The caller is
    /// responsible for having stopped it first.
    pub fn remove(&self) -> Result<()> {
        let _ = fs::remove_file(&self.monitor);
        fs::remove_dir_all(&self.path).map_err(|source| Error::State {
            path: self.path.clone(),
            action: "remove the instance directory",
            source,
        })
    }
}

/// Seconds since the epoch, for the creation stamp. A clock before 1970 is not
/// worth an error path.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// A short, stable name for a socket. Derived from the instance name rather
/// than stored, so it is the same on every run, and short so that the path
/// stays inside the kernel's limit however long the instance name is.
fn socket_id(name: &str) -> String {
    use sha2::Digest as _;
    hexadecimal(&sha2::Sha256::digest(name.as_bytes()), 8)
}

/// Instance names double as guest hostnames, so they are held to what a
/// hostname allows rather than to what a directory allows.
fn check_name(name: &str) -> Result<()> {
    let refuse = |reason: &'static str| {
        Err(Error::InstanceName {
            name: name.to_owned(),
            reason,
        })
    };
    if name.is_empty() {
        return refuse("an instance needs a name");
    }
    if name.len() > 63 {
        return refuse("a name is at most 63 characters");
    }
    if name.starts_with('-') || name.ends_with('-') {
        return refuse("a name cannot begin or end with a dash");
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return refuse("a name holds only letters, digits and dashes");
    }
    Ok(())
}

/// Invents a name from the image it is built on, in the way `docker run`
/// does: usable without being asked for, and readable afterwards.
pub fn suggest_name(repository: &str, taken: &dyn Fn(&str) -> bool) -> String {
    let stem: String = repository
        .rsplit('/')
        .next()
        .unwrap_or(repository)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(20)
        .collect();
    let stem = if stem.is_empty() {
        "vm".to_owned()
    } else {
        stem
    };
    for attempt in 0..1000u32 {
        let candidate = format!("{stem}-{}", suffix(&stem, attempt));
        if !taken(&candidate) {
            return candidate;
        }
    }
    format!("{stem}-{}", now())
}

fn suffix(stem: &str, attempt: u32) -> String {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(stem.as_bytes());
    hasher.update(now().to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(attempt.to_le_bytes());
    hexadecimal(&hasher.finalize(), 3)
}

fn hexadecimal(bytes: &[u8], count: usize) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .take(count)
        .fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// Builds the cloud-init seed for an instance from its record.
pub fn seed_for(instance: &Instance, authorized_key: &str) -> seed::Seed {
    seed::Seed {
        instance_id: format!("{}-{}", instance.name, instance.created),
        hostname: instance.name.clone(),
        user: instance.user.clone(),
        authorized_key: authorized_key.to_owned(),
        mounts: instance
            .shares
            .iter()
            .map(|share| seed::Mount {
                tag: share.tag.clone(),
                target: share.target.clone(),
            })
            .collect(),
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
            path.push(format!("vm-instance-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn instances(&self) -> Instances {
            Instances::at(self.0.join("instances"), self.0.join("run"))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn instance(name: &str) -> Instance {
        Instance {
            name: name.to_owned(),
            image: "debian:trixie".to_owned(),
            digest: "sha512:abc".to_owned(),
            arch: "amd64".to_owned(),
            created: 1_700_000_000,
            memory: 2048,
            cpus: 2,
            user: "vm".to_owned(),
            seeded: true,
            monitor: PathBuf::from("/run/user/1000/vm/0011223344556677.sock"),
            ssh_port: Some(2222),
            pid: None,
            started: None,
            ports: Vec::new(),
            shares: Vec::new(),
        }
    }

    #[test]
    fn creating_an_instance_claims_its_name() {
        let scratch = Scratch::new("claim");
        let instances = scratch.instances();
        let directory = instances.create("one").unwrap();
        assert!(directory.path().is_dir());
        assert_eq!(directory.name(), "one");
    }

    /// The lock. Two commands racing for one name must not both proceed.
    #[test]
    fn a_name_cannot_be_claimed_twice() {
        let scratch = Scratch::new("twice");
        let instances = scratch.instances();
        instances.create("one").unwrap();
        let error = instances.create("one").unwrap_err();
        assert_eq!(error.kind(), "instance-exists");
    }

    #[test]
    fn opening_an_instance_that_was_never_created_is_refused() {
        let scratch = Scratch::new("absent");
        let error = scratch.instances().open("nothing").unwrap_err();
        assert_eq!(error.kind(), "unknown-instance");
    }

    #[test]
    fn a_record_survives_the_round_trip() {
        let scratch = Scratch::new("record");
        let instances = scratch.instances();
        let directory = instances.create("one").unwrap();
        let mut held = instance("one");
        held.ports.push(Port {
            host: 2222,
            guest: 22,
        });
        held.shares.push(Share {
            tag: "work".to_owned(),
            source: PathBuf::from("/home/x/work"),
            target: "/mnt/work".to_owned(),
        });
        directory.write(&held).unwrap();
        assert_eq!(directory.read().unwrap(), held);
    }

    /// TOML cannot express a key after a table, so every combination of
    /// populated and empty lists has to be written and read back.
    #[test]
    fn a_record_survives_the_round_trip_whichever_lists_are_empty() {
        let scratch = Scratch::new("combinations");
        let instances = scratch.instances();
        let port = Port {
            host: 2222,
            guest: 22,
        };
        let share = Share {
            tag: "work".to_owned(),
            source: PathBuf::from("/home/x/work"),
            target: "/mnt/work".to_owned(),
        };
        for (index, (ports, shares)) in [
            (vec![], vec![]),
            (vec![port.clone()], vec![]),
            (vec![], vec![share.clone()]),
            (vec![port], vec![share]),
        ]
        .into_iter()
        .enumerate()
        {
            let name = format!("one{index}");
            let directory = instances.create(&name).unwrap();
            let mut held = instance(&name);
            held.pid = Some(9);
            held.started = Some(7);
            held.ports = ports;
            held.shares = shares;
            directory.write(&held).unwrap();
            assert_eq!(directory.read().unwrap(), held, "combination {index}");
        }
    }

    #[test]
    fn a_rewritten_record_replaces_the_old_one_whole() {
        let scratch = Scratch::new("rewrite");
        let directory = scratch.instances().create("one").unwrap();
        directory.write(&instance("one")).unwrap();
        let mut second = instance("one");
        second.memory = 4096;
        directory.write(&second).unwrap();
        assert_eq!(directory.read().unwrap().memory, 4096);
        assert!(
            !directory.path().join("instance.toml.new").exists(),
            "the staging file was left behind"
        );
    }

    #[test]
    fn a_record_that_is_not_readable_is_reported_rather_than_guessed_at() {
        let scratch = Scratch::new("damaged");
        let directory = scratch.instances().create("one").unwrap();
        fs::write(directory.record(), "this is not toml").unwrap();
        assert_eq!(directory.read().unwrap_err().kind(), "instance-damaged");
    }

    #[test]
    fn an_absent_record_is_reported_as_unreadable() {
        let scratch = Scratch::new("norecord");
        let directory = scratch.instances().create("one").unwrap();
        assert_eq!(directory.read().unwrap_err().kind(), "state-unusable");
    }

    #[test]
    fn instances_are_listed_in_name_order() {
        let scratch = Scratch::new("listing");
        let instances = scratch.instances();
        for name in ["beta", "alpha", "gamma"] {
            instances.create(name).unwrap();
        }
        assert_eq!(instances.names().unwrap(), ["alpha", "beta", "gamma"]);
    }

    #[test]
    fn listing_a_state_directory_that_does_not_exist_yet_is_empty() {
        let scratch = Scratch::new("empty");
        assert!(scratch.instances().names().unwrap().is_empty());
    }

    #[test]
    fn removing_an_instance_takes_its_directory_and_its_socket() {
        let scratch = Scratch::new("remove");
        let instances = scratch.instances();
        let directory = instances.create("one").unwrap();
        fs::create_dir_all(directory.monitor().parent().unwrap()).unwrap();
        fs::write(directory.monitor(), b"").unwrap();
        directory.remove().unwrap();
        assert!(!directory.path().exists());
        assert!(!directory.monitor().exists());
    }

    /// The reason the socket does not live in the instance directory: this
    /// path would be well past the kernel's limit if it did.
    #[test]
    fn the_monitor_socket_stays_inside_the_kernels_limit() {
        let instances = Instances::at(
            "/home/somebody-with-a-long-name/.local/state/vm/instances",
            "/run/user/1000/vm",
        );
        let directory = instances.directory(&"a".repeat(63));
        assert!(
            directory.monitor().as_os_str().len() <= crate::qmp::MAX_SOCKET_PATH,
            "{} is too long",
            directory.monitor().display()
        );
        assert!(
            directory.path().as_os_str().len() > crate::qmp::MAX_SOCKET_PATH,
            "this test proves nothing unless the instance path is over the limit"
        );
    }

    #[test]
    fn the_socket_name_is_the_same_every_time_and_different_per_instance() {
        assert_eq!(socket_id("one"), socket_id("one"));
        assert_ne!(socket_id("one"), socket_id("two"));
        assert_eq!(socket_id("one").len(), 16);
    }

    #[test]
    fn names_that_are_not_hostnames_are_refused() {
        let scratch = Scratch::new("names");
        let instances = scratch.instances();
        for name in [
            "",
            "-leading",
            "trailing-",
            "has space",
            "has.dot",
            "a/b",
            &"x".repeat(64),
        ] {
            let error = instances.create(name).unwrap_err();
            assert_eq!(error.kind(), "invalid-instance-name", "{name}");
        }
    }

    #[test]
    fn an_instance_with_no_process_is_not_running() {
        assert!(!instance("one").is_running());
        assert!(instance("one").handle().is_none());
    }

    #[test]
    fn an_instance_naming_this_process_is_running() {
        let mut held = instance("one");
        let handle = Handle::of(std::process::id()).unwrap();
        held.pid = Some(handle.pid);
        held.started = Some(handle.started);
        assert!(held.is_running());
    }

    #[test]
    fn a_half_written_process_is_no_process() {
        let mut held = instance("one");
        held.pid = Some(std::process::id());
        assert!(
            held.handle().is_none(),
            "a pid without a start time is not enough"
        );
    }

    #[test]
    fn forgetting_a_process_leaves_nothing_behind() {
        let mut held = instance("one");
        held.pid = Some(1);
        held.started = Some(2);
        held.forget_process();
        assert!(held.handle().is_none());
    }

    #[test]
    fn a_suggested_name_is_built_from_the_image() {
        let name = suggest_name("debian", &|_| false);
        assert!(name.starts_with("debian-"), "{name}");
        assert!(check_name(&name).is_ok(), "{name}");
    }

    #[test]
    fn a_suggested_name_avoids_the_ones_already_taken() {
        let first = suggest_name("debian", &|_| false);
        let second = suggest_name("debian", &|name| name == first);
        assert_ne!(first, second);
    }

    #[test]
    fn a_repository_with_a_path_is_reduced_to_its_last_part() {
        let name = suggest_name("example.com/images/ubuntu", &|_| false);
        assert!(name.starts_with("ubuntu-"), "{name}");
    }

    #[test]
    fn a_repository_with_nothing_usable_in_it_still_yields_a_name() {
        let name = suggest_name("///", &|_| false);
        assert!(check_name(&name).is_ok(), "{name}");
    }

    #[test]
    fn the_seed_takes_its_hostname_and_shares_from_the_record() {
        let mut held = instance("demo");
        held.shares.push(Share {
            tag: "work".to_owned(),
            source: PathBuf::from("/home/x"),
            target: "/mnt/work".to_owned(),
        });
        let seed = seed_for(&held, "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKq7YQ== vm");
        assert_eq!(seed.hostname, "demo");
        assert_eq!(seed.user, "vm");
        assert_eq!(seed.mounts.len(), 1);
        assert_eq!(seed.mounts[0].target, "/mnt/work");
        assert!(seed.image().is_ok(), "the seed should be buildable");
    }

    /// The instance id must not change between boots of one instance, or
    /// cloud-init treats the machine as new and configures it again.
    #[test]
    fn the_seed_identifier_is_stable_across_boots() {
        let held = instance("demo");
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKq7YQ== vm";
        assert_eq!(
            seed_for(&held, key).instance_id,
            seed_for(&held, key).instance_id
        );
    }
}

//! Instances, recorded only in the state directory.

use crate::catalogue::Media;
use crate::error::{Error, Result};
use crate::machine::{self, Chipset, Disk, Firmware};
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

/// A host directory shared into the guest over virtiofs, with the `virtiofsd` serving it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Share {
    pub tag: String,
    pub source: PathBuf,
    pub target: String,
    pub pid: Option<u32>,
    pub started: Option<u64>,
}

impl Share {
    pub const fn handle(&self) -> Option<Handle> {
        match (self.pid, self.started) {
            (Some(pid), Some(started)) => Some(Handle { pid, started }),
            _ => None,
        }
    }

    pub const fn forget_process(&mut self) {
        self.pid = None;
        self.started = None;
    }
}

/// An instance as written to `instance.toml`; TOML requires scalars before lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    pub name: String,
    /// The reference as the user wrote it, kept for display.
    pub image: String,
    /// The digest the reference resolved to.
    pub digest: String,
    pub arch: String,
    /// Seconds since the epoch.
    pub created: u64,
    /// Mebibytes.
    pub memory: u64,
    pub cpus: u32,
    /// Defaults to BIOS when absent.
    #[serde(default, skip_serializing_if = "Firmware::is_default")]
    pub firmware: Firmware,
    /// Defaults to `max` when absent.
    #[serde(default = "default_cpu")]
    pub cpu: String,
    /// Defaults to q35 when absent.
    #[serde(default, skip_serializing_if = "Chipset::is_default")]
    pub machine: Chipset,
    /// Defaults to virtio when absent.
    #[serde(default, skip_serializing_if = "Disk::is_default")]
    pub disk: Disk,
    /// The account cloud-init was told to create, and the one `vm ssh` uses.
    pub user: String,
    /// Whether the image can be seeded at all, from its catalogue entry.
    pub seeded: bool,
    pub monitor: PathBuf,
    /// The host port forwarded to the guest's SSH port; absent when the image takes no key.
    pub ssh_port: Option<u16>,
    /// Absent until the hypervisor is launched, and again once it has gone.
    pub pid: Option<u32>,
    /// Paired with the pid so a reused pid is not mistaken for this one.
    pub started: Option<u64>,
    /// Seed rewrites, part of the cloud-init instance id; omitted while zero.
    #[serde(default, skip_serializing_if = "is_first")]
    pub generation: u32,
    /// The console password as a `$6$` hash, or `*` once removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// Whether the directory holds an SSH config entry for the machine.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ssh_config: bool,
    /// How the image was given: as the disk's base, or as a CD-ROM beside a blank disk.
    #[serde(default, skip_serializing_if = "Media::is_disk")]
    pub media: Media,
    /// The CD-ROM image in the drive, until it is ejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cdrom: Option<PathBuf>,
    /// Omitted when empty, because TOML cannot follow a table with a bare key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<Port>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shares: Vec<Share>,
}

fn default_cpu() -> String {
    machine::DEFAULT_CPU.to_owned()
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "the signature serde's skip_serializing_if requires"
)]
const fn is_first(generation: &u32) -> bool {
    *generation == 0
}

impl Instance {
    /// The recorded process, whether or not it still runs.
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

    /// Forgets a process that is no longer running.
    pub const fn forget_process(&mut self) {
        self.pid = None;
        self.started = None;
    }

    /// Whether the machine still needs its image: as its disk's base, or in its CD-ROM drive.
    pub const fn needs_image(&self) -> bool {
        !self.media.is_cdrom() || self.cdrom.is_some()
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

    pub fn runtime(&self) -> &Path {
        &self.runtime
    }

    /// Claims a name; creating the directory is the lock.
    pub fn create(&self, name: &str) -> Result<Directory> {
        check_name(name)?;
        let path = self.root.join(name);
        fs::create_dir_all(&self.root).map_err(|source| Error::State {
            path: self.root.clone(),
            action: "create the state directory",
            source,
        })?;
        match fs::create_dir(&path) {
            Ok(()) => {
                let directory = self.directory(name);
                directory.restrict()?;
                Ok(directory)
            }
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

    /// Every instance directory, in name order.
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

    /// The monitor socket, in the runtime directory to stay within the socket path limit.
    pub fn monitor(&self) -> &Path {
        &self.monitor
    }

    /// Makes the directory private, since it holds a private key.
    pub fn restrict(&self) -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            Error::State {
                path: self.path.clone(),
                action: "restrict the instance directory",
                source,
            }
        })
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

    /// The UEFI variable store, which holds the guest's boot entries.
    pub fn firmware_variables(&self) -> PathBuf {
        self.path.join("efivars.fd")
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

    /// The socket one share's `virtiofsd` listens on.
    pub fn share_socket(&self, index: usize) -> PathBuf {
        let mut path = self.monitor.clone();
        path.set_extension(format!("fs{index}"));
        path
    }

    /// The serial console socket.
    pub fn console_socket(&self) -> PathBuf {
        let mut path = self.monitor.clone();
        path.set_extension("console");
        path
    }

    /// The VNC server showing the machine's screen.
    pub fn screen_socket(&self) -> PathBuf {
        let mut path = self.monitor.clone();
        path.set_extension("vnc");
        path
    }

    /// Where that `virtiofsd` writes its complaints.
    pub fn share_log(&self, index: usize) -> PathBuf {
        self.path.join(format!("virtiofsd-{index}.log"))
    }

    /// The instance's own host-key file.
    pub fn known_hosts(&self) -> PathBuf {
        self.path.join("known_hosts")
    }

    /// The entry that lets plain `ssh` reach this machine by name.
    pub fn ssh_config(&self) -> PathBuf {
        self.path.join("ssh_config")
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

    /// Writes the record atomically.
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

    /// Every runtime file sharing this instance's socket id.
    pub fn runtime_files(&self) -> Vec<PathBuf> {
        let (Some(parent), Some(stem)) = (self.monitor.parent(), self.monitor.file_stem()) else {
            return Vec::new();
        };
        let mut prefix = stem.to_os_string();
        prefix.push(".");
        let Ok(entries) = fs::read_dir(parent) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .as_encoded_bytes()
                    .starts_with(prefix.as_encoded_bytes())
            })
            .map(|entry| entry.path())
            .collect();
        files.sort();
        files
    }

    /// Removes every runtime file sharing this instance's socket id.
    pub fn clear_runtime(&self) {
        for file in self.runtime_files() {
            let _ = fs::remove_file(file);
        }
    }

    /// Removes a stopped instance, runtime files included.
    pub fn remove(&self) -> Result<()> {
        self.clear_runtime();
        fs::remove_dir_all(&self.path).map_err(|source| Error::State {
            path: self.path.clone(),
            action: "remove the instance directory",
            source,
        })
    }
}

/// Seconds since the epoch, or zero before it.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// A short identifier derived from the name, keeping socket paths within the kernel limit.
pub(crate) fn socket_id(name: &str) -> String {
    use sha2::Digest as _;
    hexadecimal(&sha2::Sha256::digest(name.as_bytes()), 8)
}

/// Names double as guest hostnames, so they follow hostname rules.
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

/// Invents a name from the repository, as `docker run` does.
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

/// The cloud-init instance id, which changes with each seed rewrite.
fn instance_id(instance: &Instance) -> String {
    match instance.generation {
        0 => format!("{}-{}", instance.name, instance.created),
        generation => format!("{}-{}-{generation}", instance.name, instance.created),
    }
}

pub fn seed_for(instance: &Instance, authorized_key: &str) -> seed::Seed {
    seed::Seed {
        instance_id: instance_id(instance),
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
        password: instance.password.clone(),
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
            firmware: crate::machine::Firmware::Bios,
            cpu: "max".to_owned(),
            machine: crate::machine::Chipset::Q35,
            disk: crate::machine::Disk::Virtio,
            user: "vm".to_owned(),
            seeded: true,
            monitor: PathBuf::from("/run/user/1000/vm/0011223344556677.sock"),
            ssh_port: Some(2222),
            pid: None,
            started: None,
            generation: 0,
            ssh_config: false,
            media: crate::catalogue::Media::Disk,
            cdrom: None,
            password: None,
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
            pid: None,
            started: None,
        });
        directory.write(&held).unwrap();
        assert_eq!(directory.read().unwrap(), held);
    }

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
            pid: None,
            started: None,
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
            held.ssh_config = true;
            held.ports = ports;
            held.shares = shares;
            directory.write(&held).unwrap();
            assert_eq!(directory.read().unwrap(), held, "combination {index}");
        }
    }

    #[test]
    fn an_ssh_configuration_entry_is_recorded_only_when_there_is_one() {
        let scratch = Scratch::new("sshconfig");
        let directory = scratch.instances().create("one").unwrap();
        let held = instance("one");
        directory.write(&held).unwrap();
        let text = std::fs::read_to_string(directory.record()).unwrap();
        assert!(!text.contains("ssh_config"), "{text}");
        assert!(!directory.read().unwrap().ssh_config);
    }

    #[test]
    fn machine_settings_survive_the_round_trip() {
        let scratch = Scratch::new("machine");
        let directory = scratch.instances().create("one").unwrap();
        let mut held = instance("one");
        held.firmware = Firmware::Uefi;
        held.cpu = "Penryn,vendor=GenuineIntel,+avx".to_owned();
        held.shares.push(Share {
            tag: "work".to_owned(),
            source: PathBuf::from("/home/x/work"),
            target: "/mnt/work".to_owned(),
            pid: None,
            started: None,
        });
        directory.write(&held).unwrap();
        assert_eq!(directory.read().unwrap(), held);
    }

    #[test]
    fn a_chipset_and_disk_controller_survive_the_round_trip() {
        let scratch = Scratch::new("chipset");
        let directory = scratch.instances().create("one").unwrap();
        let mut held = instance("one");
        held.machine = Chipset::Pc;
        held.disk = Disk::Ide;
        directory.write(&held).unwrap();
        assert_eq!(directory.read().unwrap(), held);
        let text = fs::read_to_string(directory.record()).unwrap();
        assert!(text.contains("machine = \"pc\""), "{text}");
        assert!(text.contains("disk = \"ide\""), "{text}");
    }

    #[test]
    fn the_default_chipset_and_disk_are_left_out_and_read_back() {
        let text = basic_toml::to_string(&instance("one")).unwrap();
        assert!(!text.contains("machine ="), "{text}");
        assert!(!text.contains("disk ="), "{text}");
        let read: Instance = basic_toml::from_str(&text).unwrap();
        assert_eq!((read.machine, read.disk), (Chipset::Q35, Disk::Virtio));
    }

    #[test]
    fn a_record_without_machine_settings_reads_as_the_defaults() {
        let scratch = Scratch::new("oldrecord");
        let directory = scratch.instances().create("one").unwrap();
        let text: String = basic_toml::to_string(&instance("one"))
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with("cpu =") && !line.starts_with("firmware ="))
            .flat_map(|line| [line, "\n"])
            .collect();
        assert!(!text.contains("cpu ="), "{text}");
        fs::write(directory.record(), text).unwrap();
        let read = directory.read().unwrap();
        assert_eq!(read.firmware, Firmware::Bios);
        assert_eq!(read.cpu, "max");
    }

    #[test]
    fn default_firmware_is_left_out_of_the_record() {
        let text = basic_toml::to_string(&instance("one")).unwrap();
        assert!(!text.contains("firmware"), "{text}");
        let mut held = instance("one");
        held.firmware = Firmware::Uefi;
        let text = basic_toml::to_string(&held).unwrap();
        assert!(text.contains("firmware = \"uefi\""), "{text}");
    }

    #[test]
    fn a_cdrom_and_its_media_survive_the_round_trip_and_default_to_a_disk() {
        let text = basic_toml::to_string(&instance("one")).unwrap();
        assert!(!text.contains("media"), "{text}");
        assert!(!text.contains("cdrom"), "{text}");
        let read: Instance = basic_toml::from_str(&text).unwrap();
        assert_eq!(read.media, Media::Disk);
        let mut held = instance("one");
        held.shares.push(Share {
            tag: "t".to_owned(),
            source: PathBuf::from("/s"),
            target: "/t".to_owned(),
            pid: None,
            started: None,
        });
        held.media = Media::Cdrom;
        held.cdrom = Some(PathBuf::from("/store/disc"));
        let text = basic_toml::to_string(&held).unwrap();
        assert_eq!(basic_toml::from_str::<Instance>(&text).unwrap(), held);
    }

    #[test]
    fn only_an_ejected_cdrom_machine_stands_without_its_image() {
        let mut held = instance("one");
        assert!(held.needs_image());
        held.media = Media::Cdrom;
        held.cdrom = Some(PathBuf::from("/store/disc"));
        assert!(held.needs_image());
        held.cdrom = None;
        assert!(!held.needs_image());
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

    #[test]
    fn removing_an_instance_takes_the_share_sockets_and_what_they_leave_behind() {
        let scratch = Scratch::new("runtime");
        let instances = scratch.instances();
        let directory = instances.create("one").unwrap();
        let runtime = directory.monitor().parent().unwrap().to_owned();
        fs::create_dir_all(&runtime).unwrap();
        fs::write(directory.monitor(), b"").unwrap();
        fs::write(directory.console_socket(), b"").unwrap();
        fs::write(directory.screen_socket(), b"").unwrap();
        for index in 0..2 {
            let socket = directory.share_socket(index);
            fs::write(&socket, b"").unwrap();
            fs::write(format!("{}.pid", socket.display()), b"1").unwrap();
        }
        // Another machine's files, which are none of this one's business.
        let stranger = runtime.join("ffffffffffffffff.sock");
        fs::write(&stranger, b"").unwrap();
        directory.remove().unwrap();
        assert!(!directory.share_socket(0).exists());
        assert!(!directory.share_socket(1).exists());
        assert!(
            !PathBuf::from(format!("{}.pid", directory.share_socket(0).display())).exists(),
            "a pid file was left behind"
        );
        assert!(!directory.console_socket().exists());
        assert!(!directory.screen_socket().exists());
        assert!(stranger.exists(), "another machine's socket was removed");
    }

    #[test]
    fn the_console_and_screen_sockets_are_distinct_and_inside_the_kernels_limit() {
        let instances = Instances::at(
            "/home/somebody-with-a-long-name/.local/state/vm/instances",
            "/run/user/1000/vm",
        );
        let directory = instances.directory(&"a".repeat(63));
        let sockets = [
            directory.monitor().to_owned(),
            directory.console_socket(),
            directory.screen_socket(),
            directory.share_socket(0),
        ];
        for (index, socket) in sockets.iter().enumerate() {
            assert!(
                socket.as_os_str().len() <= crate::qmp::MAX_SOCKET_PATH,
                "{} is too long",
                socket.display()
            );
            assert!(
                !sockets[index + 1..].contains(socket),
                "{} is shared",
                socket.display()
            );
        }
    }

    /// A new directory holds a private key before anything else, so it starts private.
    #[test]
    fn a_new_instance_directory_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = Scratch::new("private");
        let directory = scratch.instances().create("one").unwrap();
        let mode = fs::metadata(directory.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    }

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
    fn each_share_has_a_socket_of_its_own_inside_the_kernels_limit() {
        let instances = Instances::at(
            "/home/somebody-with-a-long-name/.local/state/vm/instances",
            "/run/user/1000/vm",
        );
        let directory = instances.directory(&"a".repeat(63));
        assert_ne!(directory.share_socket(0), directory.share_socket(1));
        assert_ne!(directory.share_socket(0), directory.monitor());
        for index in 0..10 {
            assert!(
                directory.share_socket(index).as_os_str().len() <= crate::qmp::MAX_SOCKET_PATH,
                "{} is too long",
                directory.share_socket(index).display()
            );
        }
    }

    #[test]
    fn a_share_with_no_process_has_no_handle() {
        let mut share = Share {
            tag: "work".to_owned(),
            source: PathBuf::from("/home/x"),
            target: "/mnt/work".to_owned(),
            pid: Some(1),
            started: None,
        };
        assert!(share.handle().is_none());
        share.started = Some(2);
        assert_eq!(share.handle().map(|held| held.pid), Some(1));
        share.forget_process();
        assert!(share.handle().is_none());
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
            pid: None,
            started: None,
        });
        let seed = seed_for(&held, "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKq7YQ== vm");
        assert_eq!(seed.hostname, "demo");
        assert_eq!(seed.user, "vm");
        assert_eq!(seed.mounts.len(), 1);
        assert_eq!(seed.mounts[0].target, "/mnt/work");
        assert!(seed.image().is_ok(), "the seed should be buildable");
    }

    #[test]
    fn the_seed_identifier_is_stable_across_boots() {
        let held = instance("demo");
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKq7YQ== vm";
        assert_eq!(
            seed_for(&held, key).instance_id,
            seed_for(&held, key).instance_id
        );
    }

    #[test]
    fn a_new_generation_is_a_machine_cloud_init_has_not_met() {
        let held = instance("demo");
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKq7YQ== vm";
        let mut renewed = held.clone();
        renewed.generation = 1;
        assert_ne!(
            seed_for(&held, key).instance_id,
            seed_for(&renewed, key).instance_id
        );
    }

    #[test]
    fn a_record_without_a_generation_keeps_the_identity_it_had() {
        let held = instance("demo");
        assert_eq!(held.generation, 0);
        assert_eq!(instance_id(&held), "demo-1700000000");
    }

    #[test]
    fn a_first_generation_is_not_written_to_the_record() {
        let scratch = Scratch::new("generation");
        let instances = scratch.instances();
        let directory = instances.create("one").unwrap();
        let mut held = instance("one");
        directory.write(&held).unwrap();
        let text = fs::read_to_string(directory.record()).unwrap();
        assert!(!text.contains("generation"), "{text}");
        held.generation = 2;
        directory.write(&held).unwrap();
        assert_eq!(directory.read().unwrap().generation, 2);
    }
}

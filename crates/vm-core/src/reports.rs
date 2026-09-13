//! What each operation reports, for any front end to present.

use crate::catalogue::{Artifact, Entry, Kind, Media, Source};
use crate::compression::Compression;
use crate::config::{Key, Settings};
use crate::instance::{Instance, Port};
use crate::machine::{Chipset, Disk, Firmware};
use crate::process;
use std::path::PathBuf;

/// One architecture's build of one catalogue entry, as `vm images` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRow {
    pub name: String,
    pub tag: String,
    pub arch: String,
    pub description: String,
    pub held: bool,
    /// Size on disk as the catalogue records it, if it does.
    pub size: Option<u64>,
    pub catalogue: String,
}

/// Which kinds of catalogue an image listing reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    All,
    Local,
    Remote,
}

impl Origin {
    pub const fn admits(self, kind: Kind) -> bool {
        matches!(
            (self, kind),
            (Self::All, _) | (Self::Local, Kind::Local) | (Self::Remote, Kind::Remote)
        )
    }

    pub const fn of(local: bool, remote: bool) -> Self {
        match (local, remote) {
            (true, _) => Self::Local,
            (false, true) => Self::Remote,
            (false, false) => Self::All,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Images {
    pub rows: Vec<ImageRow>,
    pub origin: Origin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspect {
    pub name: String,
    pub tag: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub arch: String,
    pub format: String,
    /// The compression scheme's name, `none` where there is none.
    pub compression: String,
    /// The published format, when the image is converted on pull.
    pub source_format: Option<String>,
    pub media: Media,
    pub url: Option<String>,
    pub digest: String,
    pub seedable: bool,
    pub held: bool,
    pub size: Option<u64>,
    pub firmware: Firmware,
    pub cpu: String,
    pub machine: Chipset,
    pub disk: Disk,
    /// Every architecture the entry has a build for.
    pub architectures: Vec<String>,
    pub catalogue: String,
    pub kind: Kind,
    /// Catalogues whose entry of the same name this one hides.
    pub shadows: Vec<String>,
    /// The entry's file.
    pub entry: String,
    /// Where the image is in the store, when held.
    pub path: Option<String>,
    /// Machines that need the image.
    pub used_by: Vec<String>,
}

impl Inspect {
    pub fn new(entry: &Entry, artifact: &Artifact, held: bool) -> Self {
        Self {
            name: entry.name.clone(),
            tag: entry.tag.clone(),
            aliases: entry.aliases.clone(),
            description: entry.description.clone(),
            arch: artifact.arch.clone(),
            format: artifact.format.clone(),
            compression: artifact.compression.name().to_owned(),
            source_format: artifact.source_format.clone(),
            media: artifact.media,
            url: artifact.url.clone(),
            digest: artifact.digest.to_string(),
            seedable: entry.login.is_seedable(),
            held,
            size: artifact.size,
            firmware: artifact.firmware,
            cpu: artifact.cpu().to_owned(),
            machine: artifact.machine,
            disk: artifact.disk,
            architectures: entry.architectures(),
            catalogue: entry.catalogue.clone(),
            kind: entry.kind,
            shadows: entry.shadows.clone(),
            entry: entry.path.display().to_string(),
            path: None,
            used_by: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullStatus {
    Fetched,
    AlreadyPresent,
}

impl PullStatus {
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Fetched => "fetched",
            Self::AlreadyPresent => "already-present",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pull {
    pub name: String,
    pub tag: String,
    pub arch: String,
    pub digest: String,
    pub path: String,
    pub size: u64,
    pub status: PullStatus,
}

/// One remote catalogue's update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Updated {
    pub name: String,
    pub url: String,
    pub path: String,
    /// What was installed, or why nothing was.
    pub outcome: std::result::Result<Installed, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub version: String,
    pub archive: String,
    pub files: usize,
    pub entries: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub catalogues: Vec<Updated>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Created,
    Restarted,
    AlreadyRunning,
}

impl RunStatus {
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Restarted => "restarted",
            Self::AlreadyRunning => "already-running",
        }
    }
}

/// What `vm run` or `vm start` left running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub name: String,
    pub image: String,
    pub arch: String,
    pub memory: u64,
    pub cpus: u32,
    pub ports: Vec<Port>,
    pub ssh_port: Option<u16>,
    pub user: String,
    pub seeded: bool,
    pub pid: u32,
    /// Whether the machine got KVM; absent when the monitor would not say.
    pub accelerated: Option<bool>,
    pub console: String,
    pub screen: String,
    pub status: RunStatus,
    pub firmware: Firmware,
    pub cpu: String,
    pub machine: Chipset,
    pub disk: Disk,
    /// Set when this start moved the machine to other firmware.
    pub firmware_changed: bool,
    /// Whether plain `ssh` reaches the machine by name.
    pub ssh_config: bool,
    /// The user's SSH config, when the `Include` line was added to it.
    pub ssh_config_changed: Option<String>,
    /// The image in the CD-ROM drive.
    pub cdrom: Option<String>,
}

/// Where a machine's screen can be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub name: String,
    pub socket: String,
    /// This host's name, for the command that forwards the socket from elsewhere.
    pub host: String,
}

/// A saved picture of a machine's screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screenshot {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub width: u32,
    pub height: u32,
}

/// What a machine is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Stopped,
    /// The record could not be read, so nothing else is known.
    Damaged,
    /// The monitor's run state, passed through unmapped.
    Live(String),
}

impl State {
    pub fn slug(&self) -> &str {
        match self {
            Self::Stopped => "stopped",
            Self::Damaged => "damaged",
            Self::Live(held) => held,
        }
    }
}

/// Bytes to mebibytes, rounding down.
const fn mebibytes(bytes: u64) -> u64 {
    bytes >> 20
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRow {
    pub name: String,
    pub image: String,
    pub state: State,
    pub created: u64,
    pub ports: Vec<Port>,
    pub pid: Option<u32>,
    pub ssh_port: Option<u16>,
    pub user: String,
    /// In mebibytes.
    pub memory: Option<u64>,
    /// Memory the hypervisor holds now; absent when not running.
    pub memory_used: Option<u64>,
    pub disk: Option<u64>,
    pub disk_used: Option<u64>,
}

impl MachineRow {
    pub fn of(instance: &Instance, state: State, disk: crate::disk::Usage) -> Self {
        Self {
            name: instance.name.clone(),
            image: instance.image.clone(),
            state,
            created: instance.created,
            ports: instance.ports.clone(),
            pid: instance.pid,
            ssh_port: instance.ssh_port,
            user: instance.user.clone(),
            memory: Some(instance.memory),
            memory_used: instance
                .is_running()
                .then(|| instance.pid.and_then(process::resident).map(mebibytes))
                .flatten(),
            disk: disk.capacity.map(mebibytes),
            disk_used: disk.allocated.map(mebibytes),
        }
    }

    /// An instance whose record cannot be read, listed so it can be removed.
    pub fn damaged(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            image: String::new(),
            state: State::Damaged,
            created: 0,
            ports: Vec::new(),
            pid: None,
            ssh_port: None,
            user: String::new(),
            memory: None,
            memory_used: None,
            disk: None,
            disk_used: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machines {
    pub rows: Vec<MachineRow>,
    pub all: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// The guest took the power button and shut itself down.
    PoweredDown,
    /// It was taken down without being asked, as `vm kill` does.
    Killed,
    /// The guest ignored the power button.
    Unresponsive,
    /// There was no monitor to ask through, so it was signalled instead.
    Unreachable,
    AlreadyStopped,
}

impl StopOutcome {
    pub const fn slug(self) -> &'static str {
        match self {
            Self::PoweredDown => "powered-down",
            Self::Killed => "killed",
            Self::Unresponsive => "unresponsive",
            Self::Unreachable => "unreachable",
            Self::AlreadyStopped => "already-stopped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    pub name: String,
    pub outcome: StopOutcome,
    /// How long the guest was given, for the message when it took none of it.
    pub waited: u64,
}

/// How consistent a cloned disk is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consistency {
    /// The machine was not running.
    Stopped,
    /// The guest was paused first, so no write was half-done.
    Paused,
    /// Taken from a running guest, so unflushed writes are missing.
    Running,
}

impl Consistency {
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Paused => "paused",
            Self::Running => "running",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cloned {
    pub source: String,
    pub name: String,
    pub tag: String,
    pub arch: String,
    pub digest: String,
    pub size: u64,
    pub consistency: Consistency,
}

/// What `vm import` brought in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub source: String,
    pub outcome: crate::import::Imported,
    /// Catalogues whose entry of the same name this one hides.
    pub hides: Vec<String>,
}

/// What `vm export` wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exported {
    pub name: String,
    pub tag: String,
    pub arch: String,
    pub format: String,
    pub compression: Compression,
    pub cdrom: bool,
    pub digest: String,
    pub path: String,
    pub size: u64,
    /// The suffix added to the name given, if one was.
    pub suffixed: Option<String>,
    /// Whether the copy was checked against the image's digest.
    pub verified: bool,
}

/// What `vm rmi` took away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Untagged {
    pub name: String,
    pub tag: String,
    pub arch: String,
    pub digest: String,
    pub size: u64,
    /// Whether the catalogue entry was removed too.
    pub forgotten: bool,
    /// Machines left without a backing image by `--force`.
    pub broke: Vec<String>,
    /// Other names keeping the file.
    pub kept_by: Vec<String>,
}

/// The guest's console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Console {
    pub name: String,
    pub lines: Vec<String>,
}

/// What `vm pause` and `vm resume` did, or found already done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switched {
    pub name: String,
    /// What the machine is doing now.
    pub state: String,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pruned {
    pub items: Vec<crate::prune::Item>,
    pub dry_run: bool,
}

impl Pruned {
    pub fn total(&self) -> u64 {
        self.items.iter().map(|item| item.size).sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    pub name: String,
}

/// The config file's settings and catalogues, defaults included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub path: PathBuf,
    pub settings: Option<Settings>,
    pub sources: Vec<Source>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub path: PathBuf,
    pub exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Got {
    pub key: Key,
    /// The value, and whether it is the default.
    pub value: (String, bool),
}

/// What a change did to the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    pub path: PathBuf,
    pub created: bool,
    pub modified: bool,
    pub summary: String,
    pub notes: Vec<String>,
}

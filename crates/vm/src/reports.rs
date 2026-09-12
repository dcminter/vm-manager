use crate::style::Style;
use crate::table;
use crate::units::{human, human_pair};
use vm_core::catalogue::{Artifact, Entry};
use vm_core::instance::{self, Instance, Port};
use vm_core::machine::{Chipset, Disk, Firmware};
use vm_core::process;
use vm_core::value::Value;

use crate::output::Report;

/// One architecture's build of one catalogue entry, as `vm images` lists it.
pub struct ImageRow {
    pub name: String,
    pub tag: String,
    pub arch: String,
    pub description: String,
    pub held: bool,
    /// What the build takes on disk, as the catalogue records it. An entry
    /// that omits it leaves the column blank rather than claiming a zero.
    pub size: Option<u64>,
}

pub struct Images {
    pub rows: Vec<ImageRow>,
}

impl Report for Images {
    fn to_value(&self) -> Value {
        Value::list(self.rows.iter().map(|row| {
            Value::map([
                ("name", Value::string(row.name.clone())),
                ("tag", Value::string(row.tag.clone())),
                ("arch", Value::string(row.arch.clone())),
                ("description", Value::string(row.description.clone())),
                ("held", Value::Bool(row.held)),
                ("size", row.size.map_or(Value::Null, Value::Integer)),
            ])
        }))
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        if self.rows.is_empty() {
            return vec![style.dim("No images in the catalogue. Try 'vm update'.")];
        }
        let cells: Vec<Vec<String>> = self
            .rows
            .iter()
            .map(|row| {
                vec![
                    style.name(&row.name),
                    row.tag.clone(),
                    row.arch.clone(),
                    row.size.map(human).unwrap_or_default(),
                    if row.held {
                        "yes".to_owned()
                    } else {
                        String::new()
                    },
                    row.description.clone(),
                ]
            })
            .collect();
        let headings = ["REPOSITORY", "TAG", "ARCH", "SIZE", "PULLED", "DESCRIPTION"];
        let mut lines = table::render(&headings, &cells);
        if let Some(first) = lines.first_mut() {
            *first = style.heading(first);
        }
        lines
    }
}

pub struct Inspect {
    pub name: String,
    pub tag: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub arch: String,
    pub format: String,
    /// How the published file is wrapped, or "none".
    pub compression: String,
    pub url: Option<String>,
    pub digest: String,
    pub seedable: bool,
    pub held: bool,
    pub size: Option<u64>,
    pub firmware: Firmware,
    pub cpu: String,
    pub machine: Chipset,
    pub disk: Disk,
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
            url: artifact.url.clone(),
            digest: artifact.digest.to_string(),
            seedable: entry.login.is_seedable(),
            held,
            size: artifact.size,
            firmware: artifact.firmware,
            cpu: artifact.cpu().to_owned(),
            machine: artifact.machine,
            disk: artifact.disk,
        }
    }

    /// The image's own format, and the wrapper it arrives in where there is
    /// one. Written together because the second only qualifies the first.
    fn formatting(&self) -> String {
        if self.compression == "none" {
            self.format.clone()
        } else {
            format!("{}, published {}", self.format, self.compression)
        }
    }

    const fn access(&self) -> &'static str {
        if self.seedable {
            "cloud-init; volumes and generated keys are available"
        } else {
            "console only; volumes and key injection are unavailable"
        }
    }
}

impl Report for Inspect {
    fn to_value(&self) -> Value {
        Value::map([
            ("name", Value::string(self.name.clone())),
            ("tag", Value::string(self.tag.clone())),
            ("aliases", Value::strings(self.aliases.clone())),
            ("description", Value::string(self.description.clone())),
            ("arch", Value::string(self.arch.clone())),
            ("format", Value::string(self.format.clone())),
            ("compression", Value::string(self.compression.clone())),
            ("url", self.url.clone().map_or(Value::Null, Value::String)),
            ("digest", Value::string(self.digest.clone())),
            ("seedable", Value::Bool(self.seedable)),
            ("held", Value::Bool(self.held)),
            ("size", self.size.map_or(Value::Null, Value::Integer)),
            ("firmware", Value::string(self.firmware.name())),
            ("cpu", Value::string(self.cpu.clone())),
            ("machine", Value::string(self.machine.name())),
            ("disk", Value::string(self.disk.name())),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        let mut fields = vec![
            ("Image", format!("{}:{}", self.name, self.tag)),
            ("Description", self.description.clone()),
        ];
        if !self.aliases.is_empty() {
            fields.push(("Aliases", self.aliases.join(", ")));
        }
        fields.extend([
            ("Architecture", self.arch.clone()),
            ("Format", self.formatting()),
            (
                "Source",
                self.url
                    .clone()
                    .unwrap_or_else(|| "cloned here; nowhere to fetch it from".to_owned()),
            ),
            ("Digest", self.digest.clone()),
            (
                "Size",
                self.size.map_or_else(|| "unrecorded".to_owned(), human),
            ),
            ("Guest access", self.access().to_owned()),
            ("Pulled", if self.held { "yes" } else { "no" }.to_owned()),
        ]);
        if let Some(machine) = machine_text(self.firmware, self.machine, self.disk, &self.cpu) {
            fields.push(("Machine", machine));
        }
        let width = fields
            .iter()
            .map(|(label, _)| label.chars().count())
            .max()
            .unwrap_or(0);
        fields
            .into_iter()
            .map(|(label, value)| {
                let padding = " ".repeat(width - label.chars().count());
                format!("{}{padding}  {value}", style.heading(label))
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullStatus {
    Fetched,
    AlreadyPresent,
}

impl PullStatus {
    const fn slug(self) -> &'static str {
        match self {
            Self::Fetched => "fetched",
            Self::AlreadyPresent => "already-present",
        }
    }
}

pub struct Pull {
    pub name: String,
    pub tag: String,
    pub arch: String,
    pub digest: String,
    pub path: String,
    pub size: u64,
    pub status: PullStatus,
}

impl Report for Pull {
    fn to_value(&self) -> Value {
        Value::map([
            ("name", Value::string(self.name.clone())),
            ("tag", Value::string(self.tag.clone())),
            ("arch", Value::string(self.arch.clone())),
            ("digest", Value::string(self.digest.clone())),
            ("path", Value::string(self.path.clone())),
            ("size", Value::Integer(self.size)),
            ("status", Value::string(self.status.slug())),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        let reference = style.name(&format!("{}:{}", self.name, self.tag));
        vec![match self.status {
            PullStatus::Fetched => format!("Pulled {reference} ({})", human(self.size)),
            PullStatus::AlreadyPresent => format!("{reference} is already present"),
        }]
    }
}

pub struct Update {
    pub url: String,
    pub path: String,
    pub files: usize,
    pub entries: usize,
}

impl Report for Update {
    fn to_value(&self) -> Value {
        Value::map([
            ("url", Value::string(self.url.clone())),
            ("path", Value::string(self.path.clone())),
            ("files", Value::Integer(self.files as u64)),
            ("entries", Value::Integer(self.entries as u64)),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        vec![format!(
            "Updated from {} ({} entries in {} files)",
            style.name(&self.url),
            self.entries,
            self.files
        )]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Created,
    Restarted,
    AlreadyRunning,
}

impl RunStatus {
    const fn slug(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Restarted => "restarted",
            Self::AlreadyRunning => "already-running",
        }
    }
}

/// What `vm run` or `vm start` left running.
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
    /// The user's SSH configuration, when this run or start added the `Include` line to it.
    pub ssh_config_changed: Option<String>,
}

fn ports_value(ports: &[Port]) -> Value {
    Value::list(ports.iter().map(|port| {
        Value::map([
            ("host", Value::Integer(u64::from(port.host))),
            ("guest", Value::Integer(u64::from(port.guest))),
        ])
    }))
}

fn ports_text(ports: &[Port]) -> String {
    ports
        .iter()
        .map(|port| format!("{}->{}", port.host, port.guest))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Report for Run {
    fn to_value(&self) -> Value {
        Value::map([
            ("name", Value::string(self.name.clone())),
            ("image", Value::string(self.image.clone())),
            ("arch", Value::string(self.arch.clone())),
            ("memory", Value::Integer(self.memory)),
            ("cpus", Value::Integer(u64::from(self.cpus))),
            ("ports", ports_value(&self.ports)),
            (
                "ssh_port",
                self.ssh_port
                    .map_or(Value::Null, |port| Value::Integer(u64::from(port))),
            ),
            ("user", Value::string(self.user.clone())),
            ("seeded", Value::Bool(self.seeded)),
            ("status", Value::string(self.status.slug())),
            ("pid", Value::Integer(u64::from(self.pid))),
            (
                "accelerated",
                self.accelerated.map_or(Value::Null, Value::Bool),
            ),
            ("console", Value::string(self.console.clone())),
            ("screen", Value::string(self.screen.clone())),
            ("firmware", Value::string(self.firmware.name())),
            ("cpu", Value::string(self.cpu.clone())),
            ("machine", Value::string(self.machine.name())),
            ("disk", Value::string(self.disk.name())),
            ("firmware_changed", Value::Bool(self.firmware_changed)),
            ("ssh_config", Value::Bool(self.ssh_config)),
            (
                "ssh_config_changed",
                self.ssh_config_changed
                    .clone()
                    .map_or(Value::Null, Value::string),
            ),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        let name = style.name(&self.name);
        if self.status == RunStatus::AlreadyRunning {
            return vec![format!("{name} is already running")];
        }
        let mut lines = vec![format!(
            "{} {name} from {} ({} MiB, {} CPU{})",
            if self.status == RunStatus::Restarted {
                "Restarted"
            } else {
                "Started"
            },
            self.image,
            self.memory,
            self.cpus,
            if self.cpus == 1 { "" } else { "s" }
        )];
        if let Some(port) = self.ssh_port {
            if self.ssh_config {
                lines.push(format!(
                    "  ssh      vm ssh {0}, or ssh {0} (port {port})",
                    self.name
                ));
            } else {
                lines.push(format!("  ssh      vm ssh {} (port {port})", self.name));
            }
        }
        lines.push(format!("  console  vm console {}", self.name));
        if !self.seeded {
            lines.push(format!("  screen   vm screen {}", self.name));
        }
        if !self.ports.is_empty() {
            lines.push(format!("  ports    {}", ports_text(&self.ports)));
        }
        if let Some(machine) = machine_text(self.firmware, self.machine, self.disk, &self.cpu) {
            lines.push(format!("  machine  {machine}"));
        }
        if self.firmware_changed {
            lines.push(style.dim(&format!(
                "  The firmware is now {}; a disk prepared only for the other will not boot, \
                 and changing it back restores it.",
                self.firmware.name()
            )));
        }
        if let Some(path) = &self.ssh_config_changed {
            lines.push(style.dim(&format!(
                "  Added an Include line to {path}, so ssh reads the entries of machines that have one."
            )));
        }
        if self.accelerated == Some(false) {
            lines.push(
                style.dim(
                    "  This machine is emulated, not accelerated: /dev/kvm was not available.",
                ),
            );
        }
        if !self.seeded {
            lines.push(style.dim(
                "  This image takes no cloud-init seed, so it has no key and no user of ours.",
            ));
        }
        lines
    }
}

/// Where a machine's screen can be reached.
pub struct Screen {
    pub name: String,
    pub socket: String,
    /// This host's name, for the command that forwards the socket from elsewhere.
    pub host: String,
}

impl Report for Screen {
    fn to_value(&self) -> Value {
        Value::map([
            ("name", Value::string(self.name.clone())),
            ("socket", Value::string(self.socket.clone())),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        vec![
            format!(
                "{}'s screen is a VNC server at {}",
                style.name(&self.name),
                self.socket
            ),
            style.dim("  There is no display here to open a viewer on. From a machine with one:"),
            format!("  ssh -L 5900:{} {}", self.socket, self.host),
            style.dim("  then connect a VNC viewer to localhost:5900."),
        ]
    }
}

/// A saved picture of a machine's screen.
pub struct Screenshot {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub width: u32,
    pub height: u32,
}

impl Report for Screenshot {
    fn to_value(&self) -> Value {
        Value::map([
            ("name", Value::string(self.name.clone())),
            ("path", Value::string(self.path.clone())),
            ("size", Value::Integer(self.size)),
            ("width", Value::Integer(u64::from(self.width))),
            ("height", Value::Integer(u64::from(self.height))),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        vec![format!(
            "Saved {}'s screen to {} ({}x{}, {})",
            style.name(&self.name),
            self.path,
            self.width,
            self.height,
            human(self.size)
        )]
    }
}

/// The machine settings that differ from the defaults, or nothing when none do.
fn machine_text(firmware: Firmware, machine: Chipset, disk: Disk, cpu: &str) -> Option<String> {
    let mut parts = Vec::new();
    if !machine.is_default() {
        parts.push(format!("{} machine", machine.name()));
    }
    if !disk.is_default() {
        parts.push(format!("{} disk", disk.name()));
    }
    if !firmware.is_default() {
        parts.push(format!("{} firmware", firmware.name()));
    }
    if cpu != vm_core::machine::DEFAULT_CPU {
        parts.push(format!("cpu {cpu}"));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// One instance, as `vm ps` lists it.
/// What a machine is doing. Running and stopped are settled by looking for the
/// process; anything finer has to be asked of the machine itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Stopped,
    /// The record could not be read, so nothing else is known.
    Damaged,
    /// What the monitor says it is doing: `running`, `paused`, or a word this
    /// tool has never heard of. Passed through rather than mapped.
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

/// Bytes as the record keeps them. Rounding down is what a size is: 1.9 GiB
/// held is 1 GiB and a bit, not 2.
const fn mebibytes(bytes: u64) -> u64 {
    bytes >> 20
}

pub struct MachineRow {
    pub name: String,
    pub image: String,
    pub state: State,
    pub created: u64,
    pub ports: Vec<Port>,
    pub pid: Option<u32>,
    pub ssh_port: Option<u16>,
    pub user: String,
    /// Mebibytes throughout: what the record and the hypervisor both deal in,
    /// left as figures here and made readable only for the table.
    pub memory: Option<u64>,
    /// What the hypervisor is holding now. A guest is given its memory as an
    /// address space and takes it as it touches it, so this is the figure the
    /// host feels, and it is absent for a machine that is not running.
    pub memory_used: Option<u64>,
    pub disk: Option<u64>,
    pub disk_used: Option<u64>,
}

impl MachineRow {
    pub fn of(instance: &Instance, state: State, disk: vm_core::disk::Usage) -> Self {
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

    /// An instance whose record cannot be read is still listed, because the
    /// user needs to know it is there in order to remove it.
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

    /// One figure, or one against the other where both are known.
    fn sizes(used: Option<u64>, total: Option<u64>) -> String {
        match (used, total) {
            (Some(used), Some(total)) => human_pair(used << 20, total << 20),
            (None, Some(total)) => human(total << 20),
            (Some(used), None) => human(used << 20),
            (None, None) => String::new(),
        }
    }

    fn status(&self) -> &str {
        self.state.slug()
    }
}

pub struct Machines {
    pub rows: Vec<MachineRow>,
    pub all: bool,
}

/// Ages rather than timestamps: a list is read to see what is there now, and
/// a date needs a calendar to interpret.
fn age(created: u64) -> String {
    if created == 0 {
        return String::new();
    }
    let seconds = instance::now().saturating_sub(created);
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

impl Report for Machines {
    fn to_value(&self) -> Value {
        Value::list(self.rows.iter().map(|row| {
            Value::map([
                ("name", Value::string(row.name.clone())),
                ("image", Value::string(row.image.clone())),
                ("status", Value::string(row.status().to_owned())),
                ("created", Value::Integer(row.created)),
                ("ports", ports_value(&row.ports)),
                (
                    "pid",
                    row.pid
                        .map_or(Value::Null, |pid| Value::Integer(u64::from(pid))),
                ),
                // What a script needs to reach the guest without `vm ssh`,
                // which replaces this process and so reports nothing itself.
                (
                    "ssh_port",
                    row.ssh_port
                        .map_or(Value::Null, |port| Value::Integer(u64::from(port))),
                ),
                ("user", Value::string(row.user.clone())),
                ("memory", row.memory.map_or(Value::Null, Value::Integer)),
                (
                    "memory_used",
                    row.memory_used.map_or(Value::Null, Value::Integer),
                ),
                ("disk", row.disk.map_or(Value::Null, Value::Integer)),
                (
                    "disk_used",
                    row.disk_used.map_or(Value::Null, Value::Integer),
                ),
            ])
        }))
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        if self.rows.is_empty() {
            return vec![style.dim(if self.all {
                "No instances. Try 'vm run debian:trixie'."
            } else {
                "No instances running. Try 'vm ps --all'."
            })];
        }
        let cells: Vec<Vec<String>> = self
            .rows
            .iter()
            .map(|row| {
                vec![
                    style.name(&row.name),
                    row.image.clone(),
                    row.status().to_owned(),
                    MachineRow::sizes(row.memory_used, row.memory),
                    MachineRow::sizes(row.disk_used, row.disk),
                    age(row.created),
                    ports_text(&row.ports),
                ]
            })
            .collect();
        let headings = ["NAME", "IMAGE", "STATUS", "MEMORY", "DISK", "AGE", "PORTS"];
        let mut lines = table::render(&headings, &cells);
        if let Some(first) = lines.first_mut() {
            *first = style.heading(first);
        }
        lines
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// The guest took the power button and shut itself down.
    PoweredDown,
    /// It was taken down without being asked, as `vm kill` does.
    Killed,
    /// It was asked and did not answer. A machine still early in its boot has
    /// no ACPI handler yet, so this is what stopping one looks like.
    Unresponsive,
    /// There was no monitor to ask through, so it was signalled instead.
    Unreachable,
    AlreadyStopped,
}

impl StopOutcome {
    const fn slug(self) -> &'static str {
        match self {
            Self::PoweredDown => "powered-down",
            Self::Killed => "killed",
            Self::Unresponsive => "unresponsive",
            Self::Unreachable => "unreachable",
            Self::AlreadyStopped => "already-stopped",
        }
    }
}

pub struct Stopped {
    pub name: String,
    pub outcome: StopOutcome,
    /// How long the guest was given, for the message when it took none of it.
    pub waited: u64,
}

impl Report for Stopped {
    fn to_value(&self) -> Value {
        Value::map([
            ("name", Value::string(self.name.clone())),
            ("outcome", Value::string(self.outcome.slug())),
            ("waited", Value::Integer(self.waited)),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        let name = style.name(&self.name);
        vec![match self.outcome {
            StopOutcome::PoweredDown => format!("Stopped {name}"),
            StopOutcome::Killed => format!("Killed {name}"),
            StopOutcome::Unresponsive => format!(
                "Killed {name}: it ignored the power button for {} seconds. A machine \
                 still booting has no handler for it yet.",
                self.waited
            ),
            StopOutcome::Unreachable => {
                format!("Killed {name}: its monitor could not be reached")
            }
            StopOutcome::AlreadyStopped => format!("{name} was not running"),
        }]
    }
}

pub struct Cloned {
    pub source: String,
    pub name: String,
    pub tag: String,
    pub arch: String,
    pub digest: String,
    pub size: u64,
    pub consistency: crate::machines::Consistency,
}

impl Cloned {
    const fn consistency(&self) -> &'static str {
        match self.consistency {
            crate::machines::Consistency::Stopped => "stopped",
            crate::machines::Consistency::Paused => "paused",
            crate::machines::Consistency::Running => "running",
        }
    }
}

impl Report for Cloned {
    fn to_value(&self) -> Value {
        Value::map([
            ("source", Value::string(self.source.clone())),
            ("name", Value::string(self.name.clone())),
            ("tag", Value::string(self.tag.clone())),
            ("arch", Value::string(self.arch.clone())),
            ("digest", Value::string(self.digest.clone())),
            ("size", Value::Integer(self.size)),
            ("consistency", Value::string(self.consistency())),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        let mut lines = vec![format!(
            "Cloned {} as {} ({})",
            style.name(&self.source),
            style.name(&format!("{}:{}", self.name, self.tag)),
            human(self.size)
        )];
        if self.consistency == crate::machines::Consistency::Running {
            lines.push(style.dim(
                "  Taken from a running guest: anything it had not yet written is not in \
                 the image.",
            ));
        }
        lines
    }
}

/// What `vm rmi` took away.
pub struct Untagged {
    pub name: String,
    pub tag: String,
    pub arch: String,
    pub digest: String,
    pub size: u64,
    /// Whether the catalogue entry went with it, which is so for an image made
    /// here and not for one the catalogue provides.
    pub forgotten: bool,
    /// Machines whose disk was backed by it. Only ever non-empty under
    /// `--force`, and worth saying out loud because they will not start again.
    pub broke: Vec<String>,
    /// Other names for the same bytes, which is why the bytes are still there.
    /// An image made here cannot be fetched back, so the file goes only with
    /// the last name for it.
    pub kept_by: Vec<String>,
}

impl Report for Untagged {
    fn to_value(&self) -> Value {
        Value::map([
            ("name", Value::string(self.name.clone())),
            ("tag", Value::string(self.tag.clone())),
            ("arch", Value::string(self.arch.clone())),
            ("digest", Value::string(self.digest.clone())),
            ("size", Value::Integer(self.size)),
            ("forgotten", Value::Bool(self.forgotten)),
            (
                "broke",
                Value::List(self.broke.iter().map(Value::string).collect()),
            ),
            (
                "kept_by",
                Value::List(self.kept_by.iter().map(Value::string).collect()),
            ),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        let reference = style.name(&format!("{}:{}", self.name, self.tag));
        // Nothing reclaimed is not worth a figure: the bytes either stayed
        // because something else names them, or they were already gone.
        let mut lines = vec![if self.size > 0 {
            format!("Removed {reference} ({} reclaimed)", human(self.size))
        } else {
            format!("Removed {reference}")
        }];
        if !self.kept_by.is_empty() {
            let names = if self.kept_by.len() == 1 {
                "still names it"
            } else {
                "still name it"
            };
            lines.push(style.dim(&format!(
                "  The image itself stays: {} {names}.",
                self.kept_by.join(", ")
            )));
        }
        if self.forgotten {
            lines.push(style.dim("  It was made here, so its catalogue entry is gone too."));
        }
        if !self.broke.is_empty() {
            lines.push(style.dim(&format!(
                "  The disk behind {} is no longer there.",
                self.broke.join(", ")
            )));
        }
        lines
    }
}

/// The guest's console.
pub struct Console {
    pub name: String,
    pub lines: Vec<String>,
}

impl Report for Console {
    fn to_value(&self) -> Value {
        Value::map([
            ("name", Value::string(self.name.clone())),
            (
                "lines",
                Value::List(self.lines.iter().map(Value::string).collect()),
            ),
        ])
    }

    /// Verbatim: the guest wrote these, and this is not the place to decorate
    /// them.
    fn render_text(&self, _: Style) -> Vec<String> {
        self.lines.clone()
    }
}

/// What `vm pause` and `vm resume` did, or found already done.
pub struct Switched {
    pub name: String,
    /// What the machine is doing now.
    pub state: String,
    pub changed: bool,
}

impl Report for Switched {
    fn to_value(&self) -> Value {
        Value::map([
            ("name", Value::string(self.name.clone())),
            ("state", Value::string(self.state.clone())),
            ("changed", Value::Bool(self.changed)),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        let name = style.name(&self.name);
        if !self.changed {
            return vec![format!("{name} is already {}", self.state)];
        }
        let mut lines = vec![match self.state.as_str() {
            "paused" => format!("Paused {name}"),
            "running" => format!("Resumed {name}"),
            held => format!("{name} is {held}"),
        }];
        if self.state == "paused" {
            lines.push(style.dim(
                "  Its memory is held by a process that is still there, so this survives \
                 neither a host reboot nor 'vm kill'.",
            ));
        }
        lines
    }
}

pub struct Removed {
    pub name: String,
}

impl Report for Removed {
    fn to_value(&self) -> Value {
        Value::map([("name", Value::string(self.name.clone()))])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        vec![format!("Removed {}", style.name(&self.name))]
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use vm_core::value::{to_json, to_yaml};

    fn images() -> Images {
        Images {
            rows: vec![ImageRow {
                name: "debian".to_owned(),
                tag: "trixie".to_owned(),
                arch: "amd64".to_owned(),
                description: "Debian 13".to_owned(),
                held: true,
                size: Some(512 * 1024 * 1024),
            }],
        }
    }

    fn pull(status: PullStatus) -> Pull {
        Pull {
            name: "debian".to_owned(),
            tag: "trixie".to_owned(),
            arch: "amd64".to_owned(),
            digest: "sha512:abc".to_owned(),
            path: "/store/blobs/sha512/abc".to_owned(),
            size: 1024,
            status,
        }
    }

    #[test]
    fn a_pause_says_what_it_did_and_what_it_costs() {
        let report = Switched {
            name: "one".to_owned(),
            state: "paused".to_owned(),
            changed: true,
        };
        let text = report.render_text(Style::plain()).join("\n");
        assert!(text.starts_with("Paused one"), "{text}");
        assert!(text.contains("host reboot"), "{text}");
    }

    #[test]
    fn a_resume_says_only_that_it_carried_on() {
        let report = Switched {
            name: "one".to_owned(),
            state: "running".to_owned(),
            changed: true,
        };
        let text = report.render_text(Style::plain()).join("\n");
        assert_eq!(text, "Resumed one");
    }

    /// Asking for what a machine is already doing is the outcome that was
    /// wanted, so it reads as a statement rather than a refusal.
    #[test]
    fn a_machine_already_doing_it_is_not_a_failure() {
        let report = Switched {
            name: "one".to_owned(),
            state: "paused".to_owned(),
            changed: false,
        };
        assert_eq!(
            report.render_text(Style::plain()),
            ["one is already paused"]
        );
        let document = to_json(&report.to_value());
        assert!(document.contains(r#""changed": false"#), "{document}");
    }

    /// QEMU has more run states than this tool knows about, and one it has
    /// never heard of has to reach the user rather than be called running.
    #[test]
    fn a_state_this_tool_does_not_know_is_passed_through() {
        assert_eq!(
            State::Live("guest-panicked".to_owned()).slug(),
            "guest-panicked"
        );
        assert_eq!(State::Stopped.slug(), "stopped");
        assert_eq!(State::Damaged.slug(), "damaged");
    }

    fn machines() -> Machines {
        Machines {
            rows: vec![MachineRow {
                name: "one".to_owned(),
                image: "debian:trixie".to_owned(),
                state: State::Live("paused".to_owned()),
                created: instance::now(),
                ports: Vec::new(),
                pid: Some(42),
                ssh_port: Some(2222),
                user: "vm".to_owned(),
                memory: Some(2048),
                memory_used: Some(731),
                disk: Some(12 * 1024),
                disk_used: Some(1434),
            }],
            all: false,
        }
    }

    #[test]
    fn a_paused_machine_is_listed_as_paused() {
        let report = machines();
        let text = report.render_text(Style::plain()).join("\n");
        assert!(text.contains("paused"), "{text}");
        assert!(to_json(&report.to_value()).contains(r#""status": "paused""#));
    }

    /// The figure is the catalogue's, so it is there before the image is and
    /// says what pulling it would cost.
    #[test]
    fn an_image_is_listed_with_the_size_it_takes() {
        let report = images();
        let text = report.render_text(Style::plain()).join("\n");
        assert!(text.contains("SIZE"), "{text}");
        assert!(text.contains("512.0 MiB"), "{text}");
        assert!(
            to_json(&report.to_value()).contains(r#""size": 536870912"#),
            "{}",
            to_json(&report.to_value())
        );
    }

    /// An entry that records no size leaves the column empty rather than
    /// claiming the image takes nothing.
    #[test]
    fn an_image_of_unknown_size_says_nothing_about_it() {
        let mut report = images();
        report.rows[0].size = None;
        let lines = report.render_text(Style::plain());
        assert!(!lines[1].contains('B'), "{:?}", lines[1]);
        assert!(to_json(&report.to_value()).contains(r#""size": null"#));
    }

    /// Two figures on one scale: what the machine has taken of what it was
    /// promised, for memory and for its disk.
    #[test]
    fn a_machine_is_listed_with_what_it_holds_and_what_it_may() {
        let text = machines().render_text(Style::plain()).join("\n");
        assert!(text.contains("MEMORY"), "{text}");
        assert!(text.contains("0.7 of 2.0 GiB"), "{text}");
        assert!(text.contains("1.4 of 12.0 GiB"), "{text}");
    }

    /// Mebibytes in the document, whatever the table makes of them.
    #[test]
    fn the_sizes_are_documented_as_figures() {
        let document = to_yaml(&machines().to_value());
        for field in [
            "memory: 2048",
            "memory_used: 731",
            "disk: 12288",
            "disk_used: 1434",
        ] {
            assert!(document.contains(field), "{field} missing from {document}");
        }
    }

    /// A machine that is not running holds nothing, so there is one figure to
    /// give rather than two, and the disk it left behind is still there.
    #[test]
    fn a_stopped_machine_reports_only_what_it_was_given() {
        let mut report = machines();
        report.rows[0].state = State::Stopped;
        report.rows[0].memory_used = None;
        let text = report.render_text(Style::plain()).join("\n");
        assert!(text.contains("2.0 GiB"), "{text}");
        assert!(!text.contains("of 2.0 GiB"), "{text}");
        assert!(text.contains("1.4 of 12.0 GiB"), "{text}");
    }

    /// Its record could not be read, so there is nothing to say about either.
    #[test]
    fn a_damaged_machine_claims_no_sizes() {
        let report = Machines {
            rows: vec![MachineRow::damaged("one")],
            all: true,
        };
        let document = to_yaml(&report.to_value());
        assert!(document.contains("memory: null"), "{document}");
        assert!(document.contains("disk: null"), "{document}");
    }

    #[test]
    fn a_removed_image_says_what_it_reclaimed() {
        let report = Untagged {
            name: "debian".to_owned(),
            tag: "trixie".to_owned(),
            arch: "amd64".to_owned(),
            digest: "sha512:abc".to_owned(),
            size: 1024 * 1024,
            forgotten: false,
            broke: Vec::new(),
            kept_by: Vec::new(),
        };
        let text = report.render_text(Style::plain()).join("\n");
        assert!(text.contains("debian:trixie"), "{text}");
        assert!(text.contains("1.0 MiB"), "{text}");
        assert!(!text.contains("catalogue"), "{text}");
    }

    /// Under --force a machine is left without a disk, which is not something
    /// to find out later.
    #[test]
    fn a_removed_image_names_the_machines_it_broke() {
        let report = Untagged {
            name: "mine".to_owned(),
            tag: "latest".to_owned(),
            arch: "amd64".to_owned(),
            digest: "sha256:abc".to_owned(),
            size: 0,
            forgotten: true,
            broke: vec!["one".to_owned(), "two".to_owned()],
            kept_by: Vec::new(),
        };
        let text = report.render_text(Style::plain()).join("\n");
        assert!(text.contains("one, two"), "{text}");
        assert!(text.contains("catalogue entry is gone"), "{text}");
        let document = to_json(&report.to_value());
        assert!(document.contains(r#""forgotten": true"#), "{document}");
        assert!(document.contains(r#""one""#), "{document}");
    }

    /// Two clones of an unchanged disk are one file under two names. Taking
    /// one name away must not take the file, and saying nothing would leave
    /// the user thinking they had reclaimed the space.
    #[test]
    fn a_name_removed_from_shared_bytes_says_they_stayed() {
        let report = Untagged {
            name: "mine".to_owned(),
            tag: "one".to_owned(),
            arch: "amd64".to_owned(),
            digest: "sha256:abc".to_owned(),
            size: 0,
            forgotten: true,
            broke: Vec::new(),
            kept_by: vec!["mine:two".to_owned()],
        };
        let text = report.render_text(Style::plain()).join("\n");
        assert!(text.contains("mine:one"), "{text}");
        assert!(text.contains("mine:two still names it"), "{text}");
        assert!(!text.contains("reclaimed"), "{text}");
        assert!(to_json(&report.to_value()).contains(r#""mine:two""#));

        let mut more = report;
        more.kept_by.push("mine:three".to_owned());
        let text = more.render_text(Style::plain()).join("\n");
        assert!(
            text.contains("mine:two, mine:three still name it"),
            "{text}"
        );
    }

    /// The guest wrote these lines, so they are passed through as they are.
    #[test]
    fn a_console_is_rendered_verbatim() {
        let report = Console {
            name: "one".to_owned(),
            lines: vec!["[    0.000000] Linux".to_owned(), "login:".to_owned()],
        };
        assert_eq!(report.render_text(Style::plain()), report.lines);
        let document = to_yaml(&report.to_value());
        assert!(document.contains("lines:"), "{document}");
        assert!(document.contains("login:"), "{document}");
    }

    #[test]
    fn an_image_listing_is_a_json_array() {
        let text = to_json(&images().to_value());
        assert!(text.starts_with("[\n"), "{text}");
        assert!(text.contains(r#""held": true"#), "{text}");
    }

    #[test]
    fn an_empty_listing_is_an_empty_array_not_a_message() {
        let text = to_json(&Images { rows: Vec::new() }.to_value());
        assert_eq!(text, "[]\n");
    }

    #[test]
    fn an_empty_listing_says_so_to_a_human() {
        let lines = Images { rows: Vec::new() }.render_text(Style::plain());
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("No images"), "{lines:?}");
    }

    #[test]
    fn a_listing_renders_a_heading_and_a_row_per_image() {
        let lines = images().render_text(Style::plain());
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("REPOSITORY"), "{lines:?}");
        assert!(lines[1].starts_with("debian"), "{lines:?}");
    }

    #[test]
    fn a_held_image_is_marked_in_the_pulled_column() {
        assert!(images().render_text(Style::plain())[1].contains("yes"));
    }

    #[test]
    fn a_pull_reports_its_status_as_a_stable_slug() {
        assert!(to_yaml(&pull(PullStatus::Fetched).to_value()).contains("status: fetched"));
        let present = to_yaml(&pull(PullStatus::AlreadyPresent).to_value());
        assert!(present.contains("status: already-present"), "{present}");
    }

    #[test]
    fn a_pull_reads_differently_to_a_human_for_each_status() {
        let fetched = pull(PullStatus::Fetched).render_text(Style::plain());
        let present = pull(PullStatus::AlreadyPresent).render_text(Style::plain());
        assert!(
            fetched[0].starts_with("Pulled debian:trixie"),
            "{fetched:?}"
        );
        assert!(present[0].contains("already present"), "{present:?}");
    }

    #[test]
    fn an_inspection_without_aliases_omits_the_line() {
        let report = Inspect {
            name: "debian".to_owned(),
            tag: "trixie".to_owned(),
            aliases: Vec::new(),
            description: "Debian 13".to_owned(),
            arch: "amd64".to_owned(),
            format: "qcow2".to_owned(),
            compression: "none".to_owned(),
            url: Some("https://example.test/a.qcow2".to_owned()),
            digest: "sha512:abc".to_owned(),
            seedable: false,
            held: false,
            size: None,
            firmware: Firmware::Bios,
            cpu: "max".to_owned(),
            machine: Chipset::Q35,
            disk: Disk::Virtio,
        };
        let lines = report.render_text(Style::plain());
        assert!(
            !lines.iter().any(|line| line.starts_with("Aliases")),
            "{lines:?}"
        );
        assert!(to_json(&report.to_value()).contains(r#""aliases": []"#));
    }

    /// A wrapper is worth saying, because it explains why the file that
    /// arrives is not the size the entry records.
    #[test]
    fn an_inspection_names_the_compression_only_when_there_is_some() {
        let mut report = Inspect {
            name: "freebsd".to_owned(),
            tag: "14.3".to_owned(),
            aliases: Vec::new(),
            description: String::new(),
            arch: "amd64".to_owned(),
            format: "qcow2".to_owned(),
            compression: "xz".to_owned(),
            url: None,
            digest: String::new(),
            seedable: true,
            held: false,
            size: None,
            firmware: Firmware::Bios,
            cpu: "max".to_owned(),
            machine: Chipset::Q35,
            disk: Disk::Virtio,
        };
        let line = |report: &Inspect| {
            report
                .render_text(Style::plain())
                .into_iter()
                .find(|line| line.starts_with("Format"))
                .unwrap_or_default()
        };
        assert!(line(&report).contains("qcow2, published xz"));
        assert!(to_json(&report.to_value()).contains(r#""compression": "xz""#));
        report.compression = "none".to_owned();
        assert!(line(&report).ends_with("qcow2"));
    }

    fn run(firmware: Firmware, cpu: &str, firmware_changed: bool) -> Run {
        Run {
            name: "pd".to_owned(),
            image: "puredarwin:minimal".to_owned(),
            arch: "amd64".to_owned(),
            memory: 4096,
            cpus: 2,
            ports: Vec::new(),
            ssh_port: None,
            user: "vm".to_owned(),
            seeded: false,
            pid: 42,
            accelerated: Some(true),
            console: "/tmp/console.log".to_owned(),
            screen: "/run/user/1000/vm/0011.vnc".to_owned(),
            status: RunStatus::Restarted,
            firmware,
            cpu: cpu.to_owned(),
            machine: Chipset::Q35,
            disk: Disk::Virtio,
            firmware_changed,
            ssh_config: false,
            ssh_config_changed: None,
        }
    }

    #[test]
    fn a_machine_with_an_ssh_entry_says_plain_ssh_reaches_it() {
        let mut report = run(Firmware::Bios, "max", false);
        report.seeded = true;
        report.ssh_port = Some(2222);
        let text = report.render_text(Style::plain()).join("\n");
        assert!(text.contains("ssh      vm ssh pd (port 2222)"), "{text}");
        assert!(!text.contains("Include"), "{text}");
        report.ssh_config = true;
        report.ssh_config_changed = Some("/home/x/.ssh/config".to_owned());
        let text = report.render_text(Style::plain()).join("\n");
        assert!(text.contains("vm ssh pd, or ssh pd (port 2222)"), "{text}");
        assert!(
            text.contains("Include line to /home/x/.ssh/config"),
            "{text}"
        );
        let document = to_json(&report.to_value());
        assert!(document.contains(r#""ssh_config": true"#), "{document}");
        assert!(
            document.contains(r#""ssh_config_changed": "/home/x/.ssh/config""#),
            "{document}"
        );
    }

    #[test]
    fn a_run_says_how_to_reach_the_console_and_for_an_unseeded_image_the_screen() {
        let mut report = run(Firmware::Bios, "max", false);
        let text = report.render_text(Style::plain()).join("\n");
        assert!(text.contains("console  vm console pd"), "{text}");
        assert!(text.contains("screen   vm screen pd"), "{text}");
        report.seeded = true;
        report.ssh_port = Some(2222);
        let text = report.render_text(Style::plain()).join("\n");
        assert!(text.contains("vm console pd"), "{text}");
        assert!(!text.contains("vm screen"), "{text}");
        assert!(to_json(&report.to_value()).contains(r#""screen": "/run/user/1000/vm/0011.vnc""#));
    }

    #[test]
    fn a_screen_with_no_display_here_says_how_to_reach_it_from_elsewhere() {
        let report = Screen {
            name: "pd".to_owned(),
            socket: "/run/user/1000/vm/0011.vnc".to_owned(),
            host: "hal".to_owned(),
        };
        let text = report.render_text(Style::plain()).join("\n");
        assert!(
            text.contains("ssh -L 5900:/run/user/1000/vm/0011.vnc hal"),
            "{text}"
        );
        assert!(text.contains("localhost:5900"), "{text}");
        let document = to_json(&report.to_value());
        assert!(
            document.contains(r#""socket": "/run/user/1000/vm/0011.vnc""#),
            "{document}"
        );
        assert!(!document.contains("hal"), "{document}");
    }

    #[test]
    fn a_screenshot_reports_where_it_went_and_what_it_holds() {
        let report = Screenshot {
            name: "pd".to_owned(),
            path: "/home/x/pd.png".to_owned(),
            size: 20480,
            width: 1280,
            height: 800,
        };
        let text = report.render_text(Style::plain()).join("\n");
        assert_eq!(
            text,
            "Saved pd's screen to /home/x/pd.png (1280x800, 20.0 KiB)"
        );
        let document = to_json(&report.to_value());
        assert!(document.contains(r#""width": 1280"#), "{document}");
        assert!(document.contains(r#""size": 20480"#), "{document}");
    }

    #[test]
    fn a_default_machine_is_not_mentioned_in_text() {
        let text = run(Firmware::Bios, "max", false)
            .render_text(Style::plain())
            .join("\n");
        assert!(!text.contains("machine"), "{text}");
        assert!(!text.contains("firmware"), "{text}");
    }

    #[test]
    fn a_machine_that_differs_from_the_default_says_how() {
        let report = run(Firmware::Uefi, "Penryn,+avx", false);
        let text = report.render_text(Style::plain()).join("\n");
        assert!(
            text.contains("machine  uefi firmware, cpu Penryn,+avx"),
            "{text}"
        );
        let document = to_json(&report.to_value());
        assert!(document.contains(r#""firmware": "uefi""#), "{document}");
        assert!(document.contains(r#""cpu": "Penryn,+avx""#), "{document}");
        assert!(
            document.contains(r#""firmware_changed": false"#),
            "{document}"
        );
    }

    #[test]
    fn a_non_default_chipset_and_disk_are_named_in_the_machine_line() {
        let mut report = run(Firmware::Bios, "Penryn", false);
        report.machine = Chipset::Pc;
        report.disk = Disk::Ide;
        let text = report.render_text(Style::plain()).join("\n");
        assert!(
            text.contains("machine  pc machine, ide disk, cpu Penryn"),
            "{text}"
        );
        let document = to_json(&report.to_value());
        assert!(document.contains(r#""machine": "pc""#), "{document}");
        assert!(document.contains(r#""disk": "ide""#), "{document}");
    }

    #[test]
    fn a_firmware_change_is_explained_with_the_way_back() {
        let text = run(Firmware::Uefi, "max", true)
            .render_text(Style::plain())
            .join("\n");
        assert!(text.contains("The firmware is now uefi"), "{text}");
        assert!(text.contains("changing it back restores it"), "{text}");
    }

    #[test]
    fn an_inspection_names_a_machine_only_when_it_is_not_the_default() {
        let mut report = Inspect::new(
            &vm_core::catalogue::Entry {
                name: "puredarwin".to_owned(),
                tag: "minimal".to_owned(),
                aliases: Vec::new(),
                description: String::new(),
                login: vm_core::catalogue::Login::None,
                artifacts: Vec::new(),
            },
            &Artifact {
                arch: "amd64".to_owned(),
                format: "raw".to_owned(),
                url: None,
                digest: format!("sha256:{}", "a".repeat(64)).parse().unwrap(),
                size: None,
                compression: vm_core::compression::Compression::None,
                firmware: Firmware::Uefi,
                cpu: Some("Penryn".to_owned()),
                machine: Chipset::Q35,
                disk: Disk::Virtio,
            },
            false,
        );
        let machine = |report: &Inspect| {
            report
                .render_text(Style::plain())
                .into_iter()
                .find(|line| line.starts_with("Machine"))
        };
        assert!(
            machine(&report)
                .unwrap()
                .ends_with("uefi firmware, cpu Penryn")
        );
        report.firmware = Firmware::Bios;
        report.cpu = "max".to_owned();
        assert_eq!(machine(&report), None);
        assert!(to_json(&report.to_value()).contains(r#""cpu": "max""#));
    }

    #[test]
    fn an_update_counts_entries_and_files() {
        let report = Update {
            url: "https://example.test/c.tar.gz".to_owned(),
            path: "/home/x/.local/share/vm/catalogue".to_owned(),
            files: 4,
            entries: 3,
        };
        let text = to_json(&report.to_value());
        assert!(text.contains(r#""entries": 3"#), "{text}");
        assert!(text.contains(r#""files": 4"#), "{text}");
        let lines = report.render_text(Style::plain());
        assert!(lines[0].contains("3 entries in 4 files"), "{lines:?}");
    }

    #[test]
    fn guest_access_follows_the_seedable_flag() {
        let mut report = Inspect {
            name: "debian".to_owned(),
            tag: "trixie".to_owned(),
            aliases: Vec::new(),
            description: String::new(),
            arch: "amd64".to_owned(),
            format: "qcow2".to_owned(),
            compression: "none".to_owned(),
            url: None,
            digest: String::new(),
            seedable: true,
            held: false,
            size: Some(1024),
            firmware: Firmware::Bios,
            cpu: "max".to_owned(),
            machine: Chipset::Q35,
            disk: Disk::Virtio,
        };
        assert!(report.access().contains("cloud-init"));
        report.seedable = false;
        assert!(report.access().contains("console only"));
    }
}

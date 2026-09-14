//! What the window shows, decided without any widget.

use std::collections::BTreeMap;

use vm_core::catalogue::{Kind, Source};
use vm_core::instance::{Instance, Port};
use vm_core::reports::{ImageRow, Inspect, MachineRow, State};

/// Everything a refresh gathers.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub host: Host,
    pub machines: Vec<MachineRow>,
    pub records: BTreeMap<String, Instance>,
    pub images: Vec<ImageRow>,
    pub catalogues: Vec<Source>,
}

/// The host as the front end describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "a record of settings")]
pub struct Host {
    pub name: String,
    pub arch: String,
    pub hypervisor: Option<String>,
    pub accelerated: bool,
    pub store: String,
    pub instances: String,
    pub config: String,
    pub config_exists: bool,
    pub auto_pull: bool,
    pub add_ssh_config: bool,
    pub default_user: String,
}

/// What the sidebar and the tabs are keyed by.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeId {
    Host,
    Machines,
    Images,
    Machine(String),
    Image { name: String, tag: String },
}

impl NodeId {
    pub fn key(&self) -> String {
        match self {
            Self::Host => "host".to_owned(),
            Self::Machines => "machines".to_owned(),
            Self::Images => "images".to_owned(),
            Self::Machine(name) => format!("machine:{name}"),
            Self::Image { name, tag } => format!("image:{name}:{tag}"),
        }
    }

    pub fn parse(key: &str) -> Option<Self> {
        match key {
            "host" => Some(Self::Host),
            "machines" => Some(Self::Machines),
            "images" => Some(Self::Images),
            _ => {
                if let Some(name) = key.strip_prefix("machine:") {
                    Some(Self::Machine(name.to_owned()))
                } else {
                    let rest = key.strip_prefix("image:")?;
                    let (name, tag) = rest.rsplit_once(':')?;
                    Some(Self::Image {
                        name: name.to_owned(),
                        tag: tag.to_owned(),
                    })
                }
            }
        }
    }

    pub fn image(name: &str, tag: &str) -> Self {
        Self::Image {
            name: name.to_owned(),
            tag: tag.to_owned(),
        }
    }

    /// The `name:tag` reference of an image node.
    pub fn reference(&self) -> Option<String> {
        match self {
            Self::Image { name, tag } => Some(format!("{name}:{tag}")),
            _ => None,
        }
    }
}

/// A colour class for an icon, never the only carrier of meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Good,
    Warn,
    Bad,
    Machines,
    Images,
}

impl Tone {
    pub const fn class(self) -> &'static str {
        match self {
            Self::Neutral => "tone-neutral",
            Self::Good => "tone-good",
            Self::Warn => "tone-warn",
            Self::Bad => "tone-bad",
            Self::Machines => "tone-machines",
            Self::Images => "tone-images",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: NodeId,
    pub label: String,
    pub description: String,
    pub icon: &'static str,
    pub tone: Tone,
}

/// The state icon and tone of a machine row.
pub fn machine_glyph(state: &State) -> (&'static str, Tone) {
    match state {
        State::Stopped => ("media-playback-stop-symbolic", Tone::Neutral),
        State::Damaged => ("dialog-warning-symbolic", Tone::Bad),
        State::Live(held) if held == "paused" => ("media-playback-pause-symbolic", Tone::Warn),
        State::Live(_) => ("media-playback-start-symbolic", Tone::Good),
    }
}

pub fn host_node(host: &Host) -> Node {
    Node {
        id: NodeId::Host,
        label: host.name.clone(),
        description: "This host".to_owned(),
        icon: "computer-symbolic",
        tone: Tone::Neutral,
    }
}

pub fn machines_node(snapshot: &Snapshot) -> Node {
    let running = snapshot
        .machines
        .iter()
        .filter(|row| matches!(row.state, State::Live(_)))
        .count();
    Node {
        id: NodeId::Machines,
        label: "Machines".to_owned(),
        description: format!("{running} running of {}", snapshot.machines.len()),
        icon: "view-list-symbolic",
        tone: Tone::Machines,
    }
}

pub fn images_node(snapshot: &Snapshot) -> Node {
    let summaries = image_summaries(snapshot);
    let held = summaries.iter().filter(|image| image.held).count();
    Node {
        id: NodeId::Images,
        label: "Images".to_owned(),
        description: format!("{held} held of {}", summaries.len()),
        icon: "drive-harddisk-symbolic",
        tone: Tone::Images,
    }
}

pub fn machine_nodes(snapshot: &Snapshot) -> Vec<Node> {
    snapshot
        .machines
        .iter()
        .map(|row| {
            let (icon, tone) = machine_glyph(&row.state);
            Node {
                id: NodeId::Machine(row.name.clone()),
                label: row.name.clone(),
                description: format!("{}, {}", row.image, row.state.slug()),
                icon,
                tone,
            }
        })
        .collect()
}

/// Which catalogues the images table shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CatalogueFilter {
    #[default]
    All,
    Local,
    Remote,
    Named(String),
}

impl CatalogueFilter {
    /// The choices offered, in order: the kinds, then each catalogue by name.
    pub fn choices(catalogues: &[Source]) -> Vec<(Self, String)> {
        let mut choices = vec![
            (Self::All, "All catalogues".to_owned()),
            (Self::Local, "Local catalogues".to_owned()),
            (Self::Remote, "Remote catalogues".to_owned()),
        ];
        choices.extend(
            catalogues
                .iter()
                .map(|source| (Self::Named(source.name.clone()), source.name.clone())),
        );
        choices
    }

    pub fn admits(&self, catalogue: &str, catalogues: &[Source]) -> bool {
        let kind = catalogues
            .iter()
            .find(|source| source.name == catalogue)
            .map(|source| source.kind);
        match self {
            Self::All => true,
            Self::Local => kind == Some(Kind::Local),
            Self::Remote => kind == Some(Kind::Remote),
            Self::Named(name) => name == catalogue,
        }
    }
}

/// One catalogue entry across its architectures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSummary {
    pub name: String,
    pub tag: String,
    pub description: String,
    pub catalogue: String,
    pub architectures: Vec<String>,
    pub held: bool,
}

pub fn image_summaries(snapshot: &Snapshot) -> Vec<ImageSummary> {
    let mut summaries: Vec<ImageSummary> = Vec::new();
    for row in &snapshot.images {
        if let Some(summary) = summaries
            .iter_mut()
            .find(|held| held.name == row.name && held.tag == row.tag)
        {
            summary.architectures.push(row.arch.clone());
            summary.held |= row.held && row.arch == snapshot.host.arch;
        } else {
            summaries.push(ImageSummary {
                name: row.name.clone(),
                tag: row.tag.clone(),
                description: row.description.clone(),
                catalogue: row.catalogue.clone(),
                architectures: vec![row.arch.clone()],
                held: row.held && row.arch == snapshot.host.arch,
            });
        }
    }
    summaries
}

/// The held images; the rest are listed in the images table.
pub fn image_nodes(snapshot: &Snapshot) -> Vec<Node> {
    image_summaries(snapshot)
        .into_iter()
        .filter(|image| image.held)
        .map(|image| Node {
            id: NodeId::image(&image.name, &image.tag),
            label: format!("{}:{}", image.name, image.tag),
            description: image.description,
            icon: "drive-harddisk-symbolic",
            tone: Tone::Good,
        })
        .collect()
}

/// Something the user can do to what a page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Kill,
    Pause,
    Unpause,
    Console,
    Shell,
    RunCommand,
    Logs,
    Screen,
    Screenshot,
    Clone,
    CopyFiles,
    Settings,
    Eject,
    Remove,
    Pull,
    Run,
    Export,
    RemoveImage,
    Import,
    Update,
    Prune,
    Configure,
}

impl Action {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Stop => "Stop",
            Self::Kill => "Kill",
            Self::Pause => "Pause",
            Self::Unpause => "Unpause",
            Self::Console => "Console",
            Self::Shell => "Shell",
            Self::RunCommand => "Run Command…",
            Self::Logs => "Logs",
            Self::Screen => "Screen",
            Self::Screenshot => "Screenshot…",
            Self::Clone => "Clone…",
            Self::CopyFiles => "Copy Files…",
            Self::Settings => "Settings…",
            Self::Eject => "Eject CD-ROM",
            Self::Remove | Self::RemoveImage => "Remove…",
            Self::Pull => "Pull",
            Self::Run => "Run…",
            Self::Export => "Export…",
            Self::Import => "Import…",
            Self::Update => "Update Catalogues",
            Self::Prune => "Prune…",
            Self::Configure => "Configuration…",
        }
    }

    pub const fn icon(self) -> &'static str {
        match self {
            Self::Start | Self::Unpause => "media-playback-start-symbolic",
            Self::Stop => "system-shutdown-symbolic",
            Self::Kill => "process-stop-symbolic",
            Self::Pause => "media-playback-pause-symbolic",
            Self::Console | Self::Shell | Self::RunCommand => "utilities-terminal-symbolic",
            Self::Logs => "text-x-generic-symbolic",
            Self::Screen => "video-display-symbolic",
            Self::Screenshot => "camera-photo-symbolic",
            Self::Clone => "edit-copy-symbolic",
            Self::CopyFiles => "folder-symbolic",
            Self::Settings | Self::Configure => "emblem-system-symbolic",
            Self::Eject => "media-eject-symbolic",
            Self::Remove | Self::RemoveImage | Self::Prune => "user-trash-symbolic",
            Self::Pull => "folder-download-symbolic",
            Self::Run => "list-add-symbolic",
            Self::Export => "document-save-symbolic",
            Self::Import => "document-open-symbolic",
            Self::Update => "view-refresh-symbolic",
        }
    }

    pub const fn destructive(self) -> bool {
        matches!(
            self,
            Self::Kill | Self::Remove | Self::RemoveImage | Self::Prune | Self::Eject
        )
    }
}

/// One line of a detail group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub value: String,
    pub link: Option<NodeId>,
}

impl Row {
    pub fn new(label: &str, value: impl Into<String>) -> Self {
        Self {
            label: label.to_owned(),
            value: value.into(),
            link: None,
        }
    }

    pub fn linked(label: &str, value: impl Into<String>, link: NodeId) -> Self {
        Self {
            label: label.to_owned(),
            value: value.into(),
            link: Some(link),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub title: String,
    pub rows: Vec<Row>,
}

impl Group {
    fn new(title: &str, rows: Vec<Row>) -> Self {
        Self {
            title: title.to_owned(),
            rows,
        }
    }
}

/// Which table a page leads with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    Machines,
    Images,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub node: NodeId,
    pub title: String,
    pub subtitle: String,
    pub icon: &'static str,
    pub actions: Vec<Action>,
    pub groups: Vec<Group>,
    pub tables: Vec<TableKind>,
}

/// Bytes in the largest whole-ish unit.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut amount = bytes as f64;
    let mut unit = 0;
    while amount >= 1024.0 && unit < UNITS.len() - 1 {
        amount /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{amount:.1} {}", UNITS[unit])
    }
}

/// Mebibytes in the largest whole-ish unit.
pub fn mebibytes(amount: u64) -> String {
    human(amount << 20)
}

pub fn ports_text(ports: &[Port]) -> String {
    if ports.is_empty() {
        return "none".to_owned();
    }
    ports
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// How long ago, in the largest whole unit.
pub fn age(created: u64, now: u64) -> String {
    if created == 0 {
        return String::new();
    }
    let seconds = now.saturating_sub(created);
    match seconds {
        0..=59 => format!("{seconds}s ago"),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86_399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

pub fn host_page(snapshot: &Snapshot) -> Page {
    let host = &snapshot.host;
    let yes_no = |held: bool| if held { "yes" } else { "no" };
    let mut catalogues = Vec::new();
    for source in &snapshot.catalogues {
        let kind = match source.kind {
            Kind::Remote => "remote",
            Kind::Local => "local",
        };
        catalogues.push(Row::new(
            &source.name,
            format!("{kind}, {}", source.directory.display()),
        ));
    }
    Page {
        node: NodeId::Host,
        title: host.name.clone(),
        subtitle: "This host".to_owned(),
        icon: "computer-symbolic",
        actions: vec![
            Action::Run,
            Action::Import,
            Action::Update,
            Action::Prune,
            Action::Configure,
        ],
        groups: vec![
            Group::new(
                "Host",
                vec![
                    Row::new("Architecture", host.arch.clone()),
                    Row::new(
                        "Hypervisor",
                        host.hypervisor
                            .clone()
                            .unwrap_or_else(|| "not found".to_owned()),
                    ),
                    Row::new(
                        "KVM",
                        if host.accelerated {
                            "available"
                        } else {
                            "unavailable, machines are emulated"
                        },
                    ),
                ],
            ),
            Group::new(
                "Paths",
                vec![
                    Row::new("Image store", host.store.clone()),
                    Row::new("Machines", host.instances.clone()),
                    Row::new(
                        "Config file",
                        if host.config_exists {
                            host.config.clone()
                        } else {
                            format!("{} (not written)", host.config)
                        },
                    ),
                ],
            ),
            Group::new(
                "Settings",
                vec![
                    Row::new("Fetch missing images on run", yes_no(host.auto_pull)),
                    Row::new("Add SSH config entries", yes_no(host.add_ssh_config)),
                    Row::new("Default user", host.default_user.clone()),
                ],
            ),
            Group::new("Catalogues", catalogues),
        ],
        tables: vec![TableKind::Machines, TableKind::Images],
    }
}

/// What can be done to a machine in its state.
pub fn machine_actions(state: &State, record: Option<&Instance>) -> Vec<Action> {
    let mut actions = Vec::new();
    match state {
        State::Damaged => actions.push(Action::Remove),
        State::Stopped => {
            actions.extend([Action::Start, Action::Settings, Action::Logs, Action::Clone]);
            if record.is_some_and(|held| held.cdrom.is_some()) {
                actions.push(Action::Eject);
            }
            actions.push(Action::Remove);
        }
        State::Live(held) => {
            if held == "paused" {
                actions.push(Action::Unpause);
            } else {
                actions.push(Action::Pause);
            }
            actions.extend([Action::Stop, Action::Kill, Action::Console]);
            if record.is_some_and(|held| held.seeded) {
                actions.extend([Action::Shell, Action::RunCommand, Action::CopyFiles]);
            }
            actions.extend([
                Action::Logs,
                Action::Screen,
                Action::Screenshot,
                Action::Clone,
                Action::Remove,
            ]);
        }
    }
    actions
}

pub fn machine_page(row: &MachineRow, record: Option<&Instance>, now: u64) -> Page {
    let (icon, _) = machine_glyph(&row.state);
    let mut groups = vec![Group::new(
        "Machine",
        vec![
            Row::linked("Image", row.image.clone(), image_node_of(&row.image)),
            Row::new("State", row.state.slug()),
            Row::new("Created", age(row.created, now)),
            Row::new(
                "Process",
                row.pid
                    .map_or_else(|| "none".to_owned(), |pid| pid.to_string()),
            ),
        ],
    )];
    if let Some(held) = record {
        groups.push(access_group(held));
        groups.push(hardware_group(row, held));
        groups.push(ports_group(held));
        groups.push(volumes_group(held));
    }
    Page {
        node: NodeId::Machine(row.name.clone()),
        title: row.name.clone(),
        subtitle: format!("{}, {}", row.image, row.state.slug()),
        icon,
        actions: machine_actions(&row.state, record),
        groups,
        tables: Vec::new(),
    }
}

fn access_group(held: &Instance) -> Group {
    Group::new(
        "Access",
        vec![
            Row::new("User", held.user.clone()),
            Row::new(
                "SSH port",
                held.ssh_port
                    .map_or_else(|| "none".to_owned(), |port| port.to_string()),
            ),
            Row::new(
                "Guest access",
                if held.seeded {
                    "cloud-init; keys, volumes and ssh"
                } else {
                    "console only"
                },
            ),
            Row::new(
                "Console password",
                match held.password.as_deref() {
                    None => "none",
                    Some("*") => "disabled",
                    Some(_) => "set",
                },
            ),
            Row::new(
                "SSH config entry",
                if held.ssh_config { "yes" } else { "no" },
            ),
            Row::new(
                "Remove once stopped",
                if held.auto_remove { "yes" } else { "no" },
            ),
        ],
    )
}

fn hardware_group(row: &MachineRow, held: &Instance) -> Group {
    let disk = match (row.disk_used, row.disk) {
        (Some(used), Some(total)) => format!("{} of {}", mebibytes(used), mebibytes(total)),
        (_, Some(total)) => mebibytes(total),
        _ => "unknown".to_owned(),
    };
    let memory = row.memory_used.map_or_else(
        || mebibytes(held.memory),
        |used| format!("{} of {}", mebibytes(used), mebibytes(held.memory)),
    );
    Group::new(
        "Hardware",
        vec![
            Row::new("Architecture", held.arch.clone()),
            Row::new("Memory", memory),
            Row::new("Processors", held.cpus.to_string()),
            Row::new("Disk", disk),
            Row::new("Firmware", held.firmware.name()),
            Row::new("CPU model", held.cpu_model.clone()),
            Row::new("Chipset", held.machine.name()),
            Row::new("Disk controller", held.disk.name()),
            Row::new(
                "CD-ROM",
                held.cdrom
                    .as_ref()
                    .map_or_else(|| "none".to_owned(), |path| path.display().to_string()),
            ),
        ],
    )
}

fn ports_group(held: &Instance) -> Group {
    let ports = if held.ports.is_empty() {
        vec![Row::new("Forwarded", "none")]
    } else {
        held.ports
            .iter()
            .map(|port| {
                Row::new(
                    &format!("Host {}:{}", port.listen(), port.host),
                    format!("guest {}", port.guest),
                )
            })
            .collect()
    };
    Group::new("Ports", ports)
}

fn volumes_group(held: &Instance) -> Group {
    let shares = if held.shares.is_empty() {
        vec![Row::new("Shared", "none")]
    } else {
        held.shares
            .iter()
            .map(|share| {
                Row::new(
                    &share.source.display().to_string(),
                    format!(
                        "{} as {}{}",
                        share.target,
                        share.tag,
                        if share.readonly { ", read-only" } else { "" }
                    ),
                )
            })
            .collect()
    };
    Group::new("Volumes", shares)
}

/// The node an image reference names, taking the tag as latest when absent.
pub fn image_node_of(reference: &str) -> NodeId {
    let bare = reference.split('@').next().unwrap_or(reference);
    match bare.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => NodeId::image(name, tag),
        _ => NodeId::image(bare, "latest"),
    }
}

pub fn image_actions(inspect: Option<&Inspect>) -> Vec<Action> {
    let mut actions = vec![Action::Run];
    match inspect {
        Some(held) if held.held => actions.extend([Action::Export, Action::RemoveImage]),
        Some(held) if held.catalogue == vm_core::config::STORE_CATALOGUE => {
            actions.push(Action::RemoveImage);
        }
        Some(_) => actions.push(Action::Pull),
        None => {}
    }
    actions
}

pub fn image_page(summary: &ImageSummary, builds: &[Inspect]) -> Page {
    let first = builds.first();
    let mut image = vec![
        Row::new("Name", summary.name.clone()),
        Row::new("Tag", summary.tag.clone()),
        Row::new("Description", summary.description.clone()),
        Row::new("Catalogue", summary.catalogue.clone()),
        Row::new("Architectures", summary.architectures.join(", ")),
    ];
    if let Some(held) = first {
        if !held.aliases.is_empty() {
            image.push(Row::new("Aliases", held.aliases.join(", ")));
        }
        image.push(Row::new(
            "Guest access",
            if held.seedable {
                "cloud-init; keys, volumes and ssh"
            } else {
                "console only"
            },
        ));
        if !held.shadows.is_empty() {
            image.push(Row::new("Hides the entry in", held.shadows.join(", ")));
        }
        image.push(Row::new("Entry", held.entry.clone()));
    }
    let mut groups = vec![Group::new("Image", image)];
    for held in builds {
        let suffix = if builds.len() > 1 {
            format!(" for {}", held.arch)
        } else {
            String::new()
        };
        groups.push(build_group(held, &format!("Build{suffix}")));
        groups.push(Group::new(
            &format!("Hardware{suffix}"),
            vec![
                Row::new("Firmware", held.firmware.name()),
                Row::new("CPU model", held.cpu_model.clone()),
                Row::new("Chipset", held.machine.name()),
                Row::new("Disk controller", held.disk.name()),
            ],
        ));
    }
    if let Some(held) = first {
        let used = if held.used_by.is_empty() {
            vec![Row::new("Machines", "none")]
        } else {
            held.used_by
                .iter()
                .map(|name| Row::linked("Machine", name.clone(), NodeId::Machine(name.clone())))
                .collect()
        };
        groups.push(Group::new("Used by", used));
    }
    Page {
        node: NodeId::image(&summary.name, &summary.tag),
        title: format!("{}:{}", summary.name, summary.tag),
        subtitle: summary.description.clone(),
        icon: "drive-harddisk-symbolic",
        actions: image_actions(first),
        groups,
        tables: Vec::new(),
    }
}

fn build_group(held: &Inspect, title: &str) -> Group {
    let mut build = vec![
        Row::new("Architecture", held.arch.clone()),
        Row::new("Format", held.format.clone()),
        Row::new(
            "Media",
            if held.media.is_cdrom() {
                "CD-ROM image"
            } else {
                "disk image"
            },
        ),
        Row::new("Published compression", held.compression.clone()),
    ];
    if let Some(member) = &held.archive_member {
        build.push(Row::new("In a tar archive as", member.clone()));
    }
    if let Some(format) = &held.source_format {
        build.push(Row::new("Converted from", format.clone()));
    }
    build.push(Row::new(
        "URL",
        held.url
            .clone()
            .unwrap_or_else(|| "none; cannot be fetched again".to_owned()),
    ));
    build.push(Row::new("Digest", held.digest.clone()));
    build.push(Row::new(
        "Size",
        held.size.map_or_else(|| "unknown".to_owned(), human),
    ));
    build.push(Row::new(
        "Held locally",
        if held.held { "yes" } else { "no" },
    ));
    if let Some(path) = &held.path {
        build.push(Row::new("Path", path.clone()));
    }
    Group::new(title, build)
}

/// A sortable value behind a cell.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sort {
    Number(i128),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub sort: Sort,
    pub icon: Option<(&'static str, Tone)>,
}

impl Cell {
    pub fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            sort: Sort::Text(text.to_lowercase()),
            text,
            icon: None,
        }
    }

    pub fn number(text: impl Into<String>, value: impl Into<i128>) -> Self {
        Self {
            text: text.into(),
            sort: Sort::Number(value.into()),
            icon: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub title: &'static str,
    pub numeric: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRow {
    pub node: NodeId,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub id: &'static str,
    pub columns: Vec<Column>,
    pub rows: Vec<TableRow>,
    /// How many rows there would be with nothing filtered.
    pub total: usize,
}

const fn column(title: &'static str, numeric: bool) -> Column {
    Column { title, numeric }
}

pub fn machines_table(snapshot: &Snapshot, show_stopped: bool, now: u64) -> Table {
    let rows = snapshot
        .machines
        .iter()
        .filter(|row| show_stopped || matches!(row.state, State::Live(_)))
        .map(|row| {
            let (icon, tone) = machine_glyph(&row.state);
            let mut name = Cell::text(row.name.clone());
            name.icon = Some((icon, tone));
            let memory = row.memory.unwrap_or_default();
            let disk = row.disk_used.unwrap_or_default();
            TableRow {
                node: NodeId::Machine(row.name.clone()),
                cells: vec![
                    name,
                    Cell::text(row.image.clone()),
                    Cell::text(row.state.slug()),
                    Cell::number(
                        age(row.created, now),
                        i128::from(now.saturating_sub(row.created)),
                    ),
                    Cell::text(ports_text(&row.ports)),
                    Cell::number(
                        row.ssh_port
                            .map_or_else(String::new, |port| port.to_string()),
                        i128::from(row.ssh_port.unwrap_or_default()),
                    ),
                    Cell::text(row.user.clone()),
                    Cell::number(
                        row.memory.map_or_else(String::new, mebibytes),
                        i128::from(memory),
                    ),
                    Cell::number(
                        row.disk_used.map_or_else(String::new, mebibytes),
                        i128::from(disk),
                    ),
                ],
            }
        })
        .collect();
    Table {
        id: "machines",
        columns: vec![
            column("Name", false),
            column("Image", false),
            column("State", false),
            column("Age", true),
            column("Ports", false),
            column("SSH", true),
            column("User", false),
            column("Memory", true),
            column("Disk", true),
        ],
        rows,
        total: snapshot.machines.len(),
    }
}

pub fn images_table(
    snapshot: &Snapshot,
    show_remote: bool,
    all_architectures: bool,
    filter: &CatalogueFilter,
) -> Table {
    let rows = snapshot
        .images
        .iter()
        .filter(|row| show_remote || row.held)
        .filter(|row| all_architectures || row.arch == snapshot.host.arch)
        .filter(|row| filter.admits(&row.catalogue, &snapshot.catalogues))
        .map(|row| {
            let mut name = Cell::text(row.name.clone());
            name.icon = Some(if row.held {
                ("drive-harddisk-symbolic", Tone::Good)
            } else {
                ("folder-download-symbolic", Tone::Neutral)
            });
            TableRow {
                node: NodeId::image(&row.name, &row.tag),
                cells: vec![
                    name,
                    Cell::text(row.tag.clone()),
                    Cell::text(row.arch.clone()),
                    Cell::text(row.description.clone()),
                    Cell::text(if row.held { "held" } else { "" }),
                    Cell::number(
                        row.size.map_or_else(String::new, human),
                        i128::from(row.size.unwrap_or_default()),
                    ),
                    Cell::text(row.catalogue.clone()),
                ],
            }
        })
        .collect();
    Table {
        id: "images",
        columns: vec![
            column("Name", false),
            column("Tag", false),
            column("Arch", false),
            column("Description", false),
            column("Held", false),
            column("Size", true),
            column("Catalogue", false),
        ],
        rows,
        total: snapshot
            .images
            .iter()
            .filter(|row| all_architectures || row.arch == snapshot.host.arch)
            .filter(|row| filter.admits(&row.catalogue, &snapshot.catalogues))
            .count(),
    }
}

/// What a bulk action over checked rows can do.
pub fn bulk_actions(kind: TableKind) -> Vec<Action> {
    match kind {
        TableKind::Machines => vec![
            Action::Start,
            Action::Stop,
            Action::Pause,
            Action::Unpause,
            Action::Kill,
            Action::Remove,
        ],
        TableKind::Images => vec![Action::Pull, Action::RemoveImage],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vm_core::reports::ImageRow;

    fn row(name: &str, state: State) -> MachineRow {
        MachineRow {
            name: name.to_owned(),
            image: "debian:trixie".to_owned(),
            state,
            created: 100,
            ports: vec![Port {
                address: None,
                host: 8080,
                guest: 80,
            }],
            pid: Some(7),
            ssh_port: Some(2222),
            user: "dave".to_owned(),
            memory: Some(2048),
            memory_used: None,
            disk: Some(20_480),
            disk_used: Some(1024),
        }
    }

    fn image(name: &str, tag: &str, arch: &str, held: bool) -> ImageRow {
        ImageRow {
            name: name.to_owned(),
            tag: tag.to_owned(),
            arch: arch.to_owned(),
            description: format!("{name} {tag}"),
            held,
            size: Some(1 << 30),
            catalogue: "project".to_owned(),
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            host: Host {
                name: "box".to_owned(),
                arch: "amd64".to_owned(),
                ..Host::default()
            },
            machines: vec![
                row("one", State::Live("running".to_owned())),
                row("two", State::Stopped),
                row("three", State::Live("paused".to_owned())),
            ],
            records: BTreeMap::new(),
            images: vec![
                image("debian", "trixie", "amd64", true),
                image("debian", "trixie", "arm64", false),
                image("alpine", "3.22", "amd64", false),
            ],
            catalogues: Vec::new(),
        }
    }

    #[test]
    fn node_keys_round_trip() {
        for node in [
            NodeId::Host,
            NodeId::Machines,
            NodeId::Images,
            NodeId::Machine("demo".to_owned()),
            NodeId::image("debian", "trixie"),
            NodeId::image("ghcr.io/x", "v1.2"),
        ] {
            assert_eq!(NodeId::parse(&node.key()), Some(node));
        }
        assert_eq!(NodeId::parse("nonsense"), None);
    }

    #[test]
    fn the_machines_node_counts_what_is_live() {
        let node = machines_node(&snapshot());
        assert_eq!(node.description, "2 running of 3");
    }

    #[test]
    fn images_are_summarised_across_architectures() {
        let summaries = image_summaries(&snapshot());
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].architectures, ["amd64", "arm64"]);
        assert!(summaries[0].held);
        assert!(!summaries[1].held);
        assert_eq!(images_node(&snapshot()).description, "1 held of 2");
        let nodes = image_nodes(&snapshot());
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].label, "debian:trixie");
    }

    #[test]
    fn the_images_table_filters_by_holding_and_architecture() {
        let all = images_table(&snapshot(), true, true, &CatalogueFilter::All);
        assert_eq!(all.rows.len(), 3);
        let host = images_table(&snapshot(), true, false, &CatalogueFilter::All);
        assert_eq!(host.rows.len(), 2);
        assert_eq!(host.total, 2);
        let held = images_table(&snapshot(), false, false, &CatalogueFilter::All);
        assert_eq!(held.rows.len(), 1);
        assert_eq!(held.rows[0].cells[0].text, "debian");
    }

    #[test]
    fn the_images_table_filters_by_catalogue() {
        let mut held = snapshot();
        held.catalogues = vec![
            Source {
                name: "project".to_owned(),
                kind: Kind::Remote,
                directory: std::path::PathBuf::new(),
            },
            Source {
                name: "store".to_owned(),
                kind: Kind::Local,
                directory: std::path::PathBuf::new(),
            },
        ];
        held.images.push(ImageRow {
            catalogue: "store".to_owned(),
            ..image("mine", "1", "amd64", true)
        });
        assert_eq!(
            images_table(&held, true, true, &CatalogueFilter::Remote)
                .rows
                .len(),
            3
        );
        assert_eq!(
            images_table(&held, true, true, &CatalogueFilter::Local)
                .rows
                .len(),
            1
        );
        let named = CatalogueFilter::Named("store".to_owned());
        assert_eq!(
            images_table(&held, true, true, &named).rows[0].cells[0].text,
            "mine"
        );
        assert_eq!(CatalogueFilter::choices(&held.catalogues).len(), 5);
    }

    #[test]
    fn the_machines_table_hides_stopped_machines_on_request() {
        let shown = machines_table(&snapshot(), false, 160);
        assert_eq!(shown.rows.len(), 2);
        assert_eq!(shown.total, 3);
        assert_eq!(shown.rows[0].cells[3].text, "1m ago");
        assert_eq!(shown.rows[0].cells[4].text, "8080:80");
        assert_eq!(shown.rows[0].cells[7].sort, Sort::Number(2048));
    }

    #[test]
    fn a_machines_actions_follow_its_state() {
        let running = machine_actions(&State::Live("running".to_owned()), None);
        assert!(running.contains(&Action::Pause));
        assert!(running.contains(&Action::Stop));
        assert!(!running.contains(&Action::Shell));
        assert!(!running.contains(&Action::RunCommand));
        let paused = machine_actions(&State::Live("paused".to_owned()), None);
        assert!(paused.contains(&Action::Unpause));
        let stopped = machine_actions(&State::Stopped, None);
        assert_eq!(stopped[0], Action::Start);
        assert!(!stopped.contains(&Action::Eject));
        assert_eq!(machine_actions(&State::Damaged, None), [Action::Remove]);
    }

    #[test]
    fn a_running_machine_with_our_account_offers_a_shell_and_a_command() {
        let record = Instance {
            name: "one".to_owned(),
            image: "debian:trixie".to_owned(),
            digest: String::new(),
            arch: "amd64".to_owned(),
            created: 0,
            memory: 2048,
            cpus: 2,
            firmware: vm_core::machine::Firmware::Bios,
            cpu_model: "max".to_owned(),
            machine: vm_core::machine::Chipset::Q35,
            disk: vm_core::machine::Disk::Virtio,
            user: "vm".to_owned(),
            seeded: true,
            monitor: std::path::PathBuf::new(),
            ssh_port: Some(2222),
            pid: None,
            started: None,
            generation: 0,
            password: None,
            ssh_config: false,
            auto_remove: false,
            media: vm_core::catalogue::Media::Disk,
            cdrom: None,
            ports: Vec::new(),
            shares: Vec::new(),
        };
        let running = machine_actions(&State::Live("running".to_owned()), Some(&record));
        assert!(running.contains(&Action::Shell));
        assert!(running.contains(&Action::RunCommand));
        assert_eq!(Action::RunCommand.label(), "Run Command…");
        let stopped = machine_actions(&State::Stopped, Some(&record));
        assert!(!stopped.contains(&Action::RunCommand));
    }

    #[test]
    fn a_machine_page_links_to_its_image() {
        let page = machine_page(&row("one", State::Stopped), None, 160);
        assert_eq!(
            page.groups[0].rows[0].link,
            Some(NodeId::image("debian", "trixie"))
        );
        assert_eq!(image_node_of("debian"), NodeId::image("debian", "latest"));
        assert_eq!(
            image_node_of("debian:trixie@sha512:abc"),
            NodeId::image("debian", "trixie")
        );
    }

    #[test]
    fn sizes_read_in_whole_units() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(mebibytes(2048), "2.0 GiB");
        assert_eq!(age(0, 100), "");
        assert_eq!(age(10, 60), "50s ago");
        assert_eq!(age(10, 100), "1m ago");
        assert_eq!(age(10, 4000), "1h ago");
    }

    #[test]
    fn an_image_that_is_not_held_offers_a_pull() {
        assert_eq!(image_actions(None), [Action::Run]);
    }

    fn inspect(arch: &str, held: bool) -> Inspect {
        Inspect {
            name: "debian".to_owned(),
            tag: "trixie".to_owned(),
            aliases: Vec::new(),
            description: String::new(),
            arch: arch.to_owned(),
            format: "qcow2".to_owned(),
            compression: "none".to_owned(),
            source_format: None,
            archive_member: None,
            media: vm_core::catalogue::Media::Disk,
            url: None,
            digest: String::new(),
            seedable: true,
            held,
            size: None,
            firmware: vm_core::machine::Firmware::Bios,
            cpu_model: "max".to_owned(),
            machine: vm_core::machine::Chipset::Q35,
            disk: vm_core::machine::Disk::Virtio,
            architectures: vec!["amd64".to_owned(), "arm64".to_owned()],
            catalogue: "project".to_owned(),
            kind: vm_core::catalogue::Kind::Remote,
            shadows: Vec::new(),
            entry: String::new(),
            path: None,
            used_by: Vec::new(),
        }
    }

    #[test]
    fn an_image_page_shows_a_build_per_architecture() {
        let summary = ImageSummary {
            name: "debian".to_owned(),
            tag: "trixie".to_owned(),
            description: String::new(),
            catalogue: "project".to_owned(),
            architectures: vec!["amd64".to_owned(), "arm64".to_owned()],
            held: true,
        };
        let page = image_page(&summary, &[inspect("amd64", true), inspect("arm64", false)]);
        let titles: Vec<&str> = page
            .groups
            .iter()
            .map(|group| group.title.as_str())
            .collect();
        assert_eq!(
            titles,
            [
                "Image",
                "Build for amd64",
                "Hardware for amd64",
                "Build for arm64",
                "Hardware for arm64",
                "Used by"
            ]
        );
        assert!(page.actions.contains(&Action::Export));
        let single = image_page(&summary, &[inspect("amd64", false)]);
        assert_eq!(single.groups[1].title, "Build");
        assert!(single.actions.contains(&Action::Pull));
    }
}

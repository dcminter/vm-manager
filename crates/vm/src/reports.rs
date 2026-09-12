use crate::progress::human;
use crate::style::Style;
use crate::table;
use vm_core::catalogue::{Artifact, Entry};
use vm_core::instance::{self, Instance, Port};
use vm_core::value::Value;

use crate::output::Report;

/// One architecture's build of one catalogue entry, as `vm images` lists it.
pub struct ImageRow {
    pub name: String,
    pub tag: String,
    pub arch: String,
    pub description: String,
    pub held: bool,
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
                    if row.held {
                        "yes".to_owned()
                    } else {
                        String::new()
                    },
                    row.description.clone(),
                ]
            })
            .collect();
        let headings = ["REPOSITORY", "TAG", "ARCH", "PULLED", "DESCRIPTION"];
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
    pub url: String,
    pub digest: String,
    pub seedable: bool,
    pub held: bool,
    pub size: Option<u64>,
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
            url: artifact.url.clone(),
            digest: artifact.digest.to_string(),
            seedable: entry.login.is_seedable(),
            held,
            size: artifact.size,
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
            ("url", Value::string(self.url.clone())),
            ("digest", Value::string(self.digest.clone())),
            ("seedable", Value::Bool(self.seedable)),
            ("held", Value::Bool(self.held)),
            ("size", self.size.map_or(Value::Null, Value::Integer)),
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
            ("Format", self.format.clone()),
            ("URL", self.url.clone()),
            ("Digest", self.digest.clone()),
            ("Guest access", self.access().to_owned()),
            ("Pulled", if self.held { "yes" } else { "no" }.to_owned()),
        ]);
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
    pub status: RunStatus,
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
            lines.push(format!("  ssh      vm ssh {} (port {port})", self.name));
        }
        if !self.ports.is_empty() {
            lines.push(format!("  ports    {}", ports_text(&self.ports)));
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

/// One instance, as `vm ps` lists it.
pub struct MachineRow {
    pub name: String,
    pub image: String,
    pub running: bool,
    pub created: u64,
    pub ports: Vec<Port>,
    pub pid: Option<u32>,
    pub ssh_port: Option<u16>,
    pub user: String,
    pub damaged: bool,
}

impl MachineRow {
    pub fn of(instance: &Instance, running: bool) -> Self {
        Self {
            name: instance.name.clone(),
            image: instance.image.clone(),
            running,
            created: instance.created,
            ports: instance.ports.clone(),
            pid: instance.pid,
            ssh_port: instance.ssh_port,
            user: instance.user.clone(),
            damaged: false,
        }
    }

    /// An instance whose record cannot be read is still listed, because the
    /// user needs to know it is there in order to remove it.
    pub fn damaged(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            image: String::new(),
            running: false,
            created: 0,
            ports: Vec::new(),
            pid: None,
            ssh_port: None,
            user: String::new(),
            damaged: true,
        }
    }

    const fn status(&self) -> &'static str {
        if self.damaged {
            "damaged"
        } else if self.running {
            "running"
        } else {
            "stopped"
        }
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
                ("status", Value::string(row.status())),
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
                    age(row.created),
                    ports_text(&row.ports),
                ]
            })
            .collect();
        let headings = ["NAME", "IMAGE", "STATUS", "AGE", "PORTS"];
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
            url: "https://example.test/a.qcow2".to_owned(),
            digest: "sha512:abc".to_owned(),
            seedable: false,
            held: false,
            size: None,
        };
        let lines = report.render_text(Style::plain());
        assert!(
            !lines.iter().any(|line| line.starts_with("Aliases")),
            "{lines:?}"
        );
        assert!(to_json(&report.to_value()).contains(r#""aliases": []"#));
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
            url: String::new(),
            digest: String::new(),
            seedable: true,
            held: false,
            size: Some(1024),
        };
        assert!(report.access().contains("cloud-init"));
        report.seedable = false;
        assert!(report.access().contains("console only"));
    }
}

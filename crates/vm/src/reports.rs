//! Text and document renderings of what the operations report.

use crate::output::Report;
use crate::style::Style;
use crate::table;
use crate::units::{human, human_pair};
use vm_core::instance::{self, Port};
use vm_core::machine::{Chipset, Disk, Firmware};
use vm_core::value::Value;

pub use vm_core::reports::*;

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
                ("catalogue", Value::string(row.catalogue.clone())),
            ])
        }))
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        if self.rows.is_empty() {
            return vec![style.dim(match self.origin {
                Origin::Local => "No local images. Make one with 'vm clone' or 'vm import'.",
                Origin::All | Origin::Remote => "No images in the catalogue. Try 'vm update'.",
            })];
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
                    row.catalogue.clone(),
                    row.description.clone(),
                ]
            })
            .collect();
        let headings = [
            "REPOSITORY",
            "TAG",
            "ARCH",
            "SIZE",
            "PULLED",
            "CATALOGUE",
            "DESCRIPTION",
        ];
        let mut lines = table::render(&headings, &cells);
        if let Some(first) = lines.first_mut() {
            *first = style.heading(first);
        }
        lines
    }
}

/// Phrases an inspection needs.
trait InspectText {
    fn formatting(&self) -> String;
    fn architecture(&self) -> String;
    fn access(&self) -> &'static str;
}

impl InspectText for Inspect {
    /// The image format, with its compression where it has one.
    fn formatting(&self) -> String {
        use std::fmt::Write as _;
        let mut text = self.format.clone();
        if self.media.is_cdrom() {
            text.push_str(", a CD-ROM image");
        }
        if let Some(format) = &self.source_format {
            let _ = write!(text, ", converted from {format}");
        }
        if self.compression != "none" {
            let _ = write!(text, ", published {}", self.compression);
        }
        text
    }

    /// The architecture shown, with any others the entry has.
    fn architecture(&self) -> String {
        let others: Vec<&str> = self
            .architectures
            .iter()
            .map(String::as_str)
            .filter(|arch| *arch != self.arch)
            .collect();
        if others.is_empty() {
            self.arch.clone()
        } else {
            format!("{} (also {})", self.arch, others.join(", "))
        }
    }

    fn access(&self) -> &'static str {
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
            (
                "source_format",
                self.source_format
                    .clone()
                    .map_or(Value::Null, Value::String),
            ),
            ("media", Value::string(self.media.name())),
            ("url", self.url.clone().map_or(Value::Null, Value::String)),
            ("digest", Value::string(self.digest.clone())),
            ("seedable", Value::Bool(self.seedable)),
            ("held", Value::Bool(self.held)),
            ("size", self.size.map_or(Value::Null, Value::Integer)),
            ("firmware", Value::string(self.firmware.name())),
            ("cpu", Value::string(self.cpu.clone())),
            ("machine", Value::string(self.machine.name())),
            ("disk", Value::string(self.disk.name())),
            ("architectures", Value::strings(self.architectures.clone())),
            ("origin", Value::string(self.kind.name())),
            ("catalogue", Value::string(self.catalogue.clone())),
            ("shadows", Value::strings(self.shadows.clone())),
            ("entry", Value::string(self.entry.clone())),
            ("path", self.path.clone().map_or(Value::Null, Value::String)),
            ("used_by", Value::strings(self.used_by.clone())),
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
            ("Architecture", self.architecture()),
            (
                "Origin",
                format!("{} ({})", self.kind.name(), self.catalogue),
            ),
            ("Entry", self.entry.clone()),
            ("Format", self.formatting()),
            (
                "Source",
                self.url
                    .clone()
                    .unwrap_or_else(|| "none; it cannot be fetched".to_owned()),
            ),
            ("Digest", self.digest.clone()),
            (
                "Size",
                self.size.map_or_else(|| "unrecorded".to_owned(), human),
            ),
            ("Guest access", self.access().to_owned()),
            (
                "Pulled",
                self.path
                    .as_ref()
                    .filter(|_| self.held)
                    .map_or_else(|| "no".to_owned(), |path| format!("yes, at {path}")),
            ),
        ]);
        if !self.shadows.is_empty() {
            fields.push(("Shadows", self.shadows.join(", ")));
        }
        if !self.used_by.is_empty() {
            fields.push(("Used by", self.used_by.join(", ")));
        }
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

impl Report for Update {
    fn to_value(&self) -> Value {
        Value::list(self.catalogues.iter().map(|held| {
            let (files, entries, error) = match &held.outcome {
                Ok((files, entries)) => (
                    Value::Integer(*files as u64),
                    Value::Integer(*entries as u64),
                    Value::Null,
                ),
                Err(reason) => (Value::Null, Value::Null, Value::string(reason.clone())),
            };
            Value::map([
                ("name", Value::string(held.name.clone())),
                ("url", Value::string(held.url.clone())),
                ("path", Value::string(held.path.clone())),
                ("files", files),
                ("entries", entries),
                ("error", error),
            ])
        }))
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        self.catalogues
            .iter()
            .map(|held| match &held.outcome {
                Ok((files, entries)) => format!(
                    "Updated {} from {} ({entries} entries in {files} files)",
                    style.name(&held.name),
                    held.url
                ),
                Err(reason) => format!(
                    "Could not update {}; its previous copy stays in use: {reason}",
                    style.name(&held.name)
                ),
            })
            .collect()
    }

    fn succeeded(&self) -> bool {
        self.catalogues.iter().all(|held| held.outcome.is_ok())
    }
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
            (
                "cdrom",
                self.cdrom.clone().map_or(Value::Null, Value::string),
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
        if self.cdrom.is_some() {
            lines.push(format!(
                "  cdrom    {}, until vm start {} --eject",
                self.image, self.name
            ));
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

/// Phrases a machine listing needs.
trait MachineRowText {
    fn sizes(used: Option<u64>, total: Option<u64>) -> String;
    fn status(&self) -> &str;
}

impl MachineRowText for MachineRow {
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
                // What a script needs to reach the guest without `vm ssh`.
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

impl Report for Cloned {
    fn to_value(&self) -> Value {
        Value::map([
            ("source", Value::string(self.source.clone())),
            ("name", Value::string(self.name.clone())),
            ("tag", Value::string(self.tag.clone())),
            ("arch", Value::string(self.arch.clone())),
            ("digest", Value::string(self.digest.clone())),
            ("size", Value::Integer(self.size)),
            ("consistency", Value::string(self.consistency.slug())),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        let mut lines = vec![format!(
            "Cloned {} as {} ({})",
            style.name(&self.source),
            style.name(&format!("{}:{}", self.name, self.tag)),
            human(self.size)
        )];
        if self.consistency == Consistency::Running {
            lines.push(style.dim(
                "  Taken from a running guest: anything it had not yet written is not in \
                 the image.",
            ));
        }
        lines
    }
}

/// The format before and after an import, when they differ.
fn imported_formatting(report: &Imported) -> String {
    let artifact = &report.outcome.artifact;
    if artifact.media.is_cdrom() {
        format!("{}, kept as a CD-ROM image", report.outcome.format)
    } else if report.outcome.format == artifact.format {
        artifact.format.clone()
    } else {
        format!(
            "{}, converted to {}",
            report.outcome.format, artifact.format
        )
    }
}

impl Report for Imported {
    fn to_value(&self) -> Value {
        let imported = &self.outcome;
        let artifact = &imported.artifact;
        Value::map([
            ("source", Value::string(self.source.clone())),
            ("name", Value::string(imported.name.clone())),
            ("tag", Value::string(imported.tag.clone())),
            ("arch", Value::string(artifact.arch.clone())),
            ("original_format", Value::string(imported.format.clone())),
            ("format", Value::string(artifact.format.clone())),
            ("media", Value::string(artifact.media.name())),
            ("digest", Value::string(artifact.digest.to_string())),
            ("size", artifact.size.map_or(Value::Null, Value::Integer)),
            (
                "url",
                artifact.url.clone().map_or(Value::Null, Value::String),
            ),
            ("path", Value::string(imported.path.display().to_string())),
            ("entry", Value::string(imported.entry.display().to_string())),
            ("replaced", Value::Bool(imported.replaced)),
            ("hides", Value::strings(self.hides.clone())),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        let imported = &self.outcome;
        let artifact = &imported.artifact;
        let mut lines = vec![
            format!(
                "Imported {} ({}, {})",
                style.name(&format!("{}:{}", imported.name, imported.tag)),
                artifact.arch,
                human(artifact.size.unwrap_or_default())
            ),
            format!("  from     {}", self.source),
            format!("  format   {}", imported_formatting(self)),
        ];
        match &artifact.url {
            Some(_) => lines.push("  fetch    again from its source when not held".to_owned()),
            None => lines.push("  fetch    never; the store holds the only copy".to_owned()),
        }
        if !self.hides.is_empty() {
            lines.push(format!("  hides    {}", self.hides.join(", ")));
        }
        if imported.replaced {
            lines.push(style.dim("  It replaces the image of the same name."));
        }
        if artifact.media.is_cdrom() {
            lines.push(style.dim(
                "  A machine run from it boots the CD-ROM until its blank disk holds a system.",
            ));
        }
        lines
    }
}

impl Report for Exported {
    fn to_value(&self) -> Value {
        Value::map([
            ("name", Value::string(self.name.clone())),
            ("tag", Value::string(self.tag.clone())),
            ("arch", Value::string(self.arch.clone())),
            ("format", Value::string(self.format.clone())),
            ("compression", Value::string(self.compression.name())),
            (
                "media",
                Value::string(if self.cdrom { "cdrom" } else { "disk" }),
            ),
            ("digest", Value::string(self.digest.clone())),
            ("path", Value::string(self.path.clone())),
            ("size", Value::Integer(self.size)),
            ("suffixed", Value::Bool(self.suffixed.is_some())),
            ("verified", Value::Bool(self.verified)),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        let mut lines = vec![
            format!(
                "Exported {} ({}, {})",
                style.name(&format!("{}:{}", self.name, self.tag)),
                self.arch,
                human(self.size)
            ),
            format!("  to       {}", self.path),
            format!(
                "  format   {}{}{}",
                self.format,
                if self.cdrom { ", a CD-ROM image" } else { "" },
                match self.compression {
                    vm_core::compression::Compression::None => String::new(),
                    scheme => format!(", compressed with {}", scheme.name()),
                }
            ),
        ];
        if let Some(suffix) = &self.suffixed {
            lines.push(style.dim(&format!(
                "  The name given lacked a suffix for it, so .{suffix} was added."
            )));
        }
        lines
    }
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
                "  The image {} used is no longer there.",
                self.broke.join(", ")
            )));
        }
        lines
    }
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

    /// Verbatim.
    fn render_text(&self, _: Style) -> Vec<String> {
        self.lines.clone()
    }
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

impl Report for Pruned {
    fn to_value(&self) -> Value {
        Value::map([
            ("dry_run", Value::Bool(self.dry_run)),
            (
                "items",
                Value::list(self.items.iter().map(|item| {
                    Value::map([
                        ("kind", Value::string(item.kind.name())),
                        ("name", Value::string(item.name.clone())),
                        ("reason", Value::string(item.reason.name())),
                        ("size", Value::Integer(item.size)),
                    ])
                })),
            ),
            ("size", Value::Integer(self.total())),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        if self.items.is_empty() {
            return vec![style.dim("Nothing to prune.")];
        }
        let cells: Vec<Vec<String>> = self
            .items
            .iter()
            .map(|item| {
                vec![
                    item.kind.name().to_owned(),
                    style.name(&item.name),
                    item.reason.describe().to_owned(),
                    human(item.size),
                ]
            })
            .collect();
        let mut lines = table::render(&["KIND", "NAME", "REASON", "SIZE"], &cells);
        if let Some(first) = lines.first_mut() {
            *first = style.heading(first);
        }
        let count = self.items.len();
        let noun = if count == 1 { "item" } else { "items" };
        let verb = if self.dry_run {
            "Would remove"
        } else {
            "Removed"
        };
        lines.push(format!(
            "{verb} {count} {noun}, {}",
            if self.dry_run {
                format!("freeing {}", human(self.total()))
            } else {
                format!("freed {}", human(self.total()))
            }
        ));
        lines
    }
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
    use vm_core::catalogue::{Artifact, Kind};
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
                catalogue: "project".to_owned(),
            }],
            origin: Origin::All,
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

    #[test]
    fn an_image_of_unknown_size_says_nothing_about_it() {
        let mut report = images();
        report.rows[0].size = None;
        let lines = report.render_text(Style::plain());
        assert!(!lines[1].contains('B'), "{:?}", lines[1]);
        assert!(to_json(&report.to_value()).contains(r#""size": null"#));
    }

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
        let text = to_json(
            &Images {
                rows: Vec::new(),
                origin: Origin::Local,
            }
            .to_value(),
        );
        assert_eq!(text, "[]\n");
    }

    #[test]
    fn an_empty_listing_says_so_to_a_human() {
        let lines = Images {
            rows: Vec::new(),
            origin: Origin::All,
        }
        .render_text(Style::plain());
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("No images"), "{lines:?}");
    }

    fn pruned(dry_run: bool) -> Pruned {
        Pruned {
            items: vec![
                vm_core::prune::Item {
                    kind: vm_core::prune::Kind::Machine,
                    name: "old".to_owned(),
                    reason: vm_core::prune::Reason::Stopped,
                    size: 1024 * 1024,
                    paths: vec!["/state/old".into()],
                },
                vm_core::prune::Item {
                    kind: vm_core::prune::Kind::Image,
                    name: "debian:trixie".to_owned(),
                    reason: vm_core::prune::Reason::Unused,
                    size: 3 * 1024 * 1024,
                    paths: vec!["/store/blob".into()],
                },
            ],
            dry_run,
        }
    }

    #[test]
    fn a_prune_lists_each_item_and_the_total() {
        let lines = pruned(false).render_text(Style::plain());
        assert!(lines[0].starts_with("KIND"), "{lines:?}");
        assert!(
            lines[1].contains("old") && lines[1].contains("stopped"),
            "{lines:?}"
        );
        assert!(lines[2].contains("no machine uses it"), "{lines:?}");
        assert_eq!(lines[3], "Removed 2 items, freed 4.0 MiB");
        let lines = pruned(true).render_text(Style::plain());
        assert_eq!(lines[3], "Would remove 2 items, freeing 4.0 MiB");
    }

    #[test]
    fn a_prune_document_carries_kinds_reasons_and_sizes_but_not_paths() {
        let text = to_json(&pruned(true).to_value());
        for expected in [
            r#""dry_run": true"#,
            r#""kind": "machine""#,
            r#""reason": "unused""#,
            r#""size": 4194304"#,
        ] {
            assert!(text.contains(expected), "{expected} in {text}");
        }
        assert!(!text.contains("/store/blob"), "{text}");
    }

    #[test]
    fn an_empty_prune_says_so() {
        let empty = Pruned {
            items: Vec::new(),
            dry_run: false,
        };
        assert_eq!(empty.render_text(Style::plain()), ["Nothing to prune."]);
        assert!(to_json(&empty.to_value()).contains(r#""items": []"#));
    }

    #[test]
    fn an_empty_local_listing_points_at_clone() {
        let empty = |origin| {
            Images {
                rows: Vec::new(),
                origin,
            }
            .render_text(Style::plain())
        };
        assert!(empty(Origin::Local)[0].contains("vm clone"));
        assert!(empty(Origin::Remote)[0].contains("vm update"));
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
            source_format: None,
            media: vm_core::catalogue::Media::Disk,
            url: Some("https://example.test/a.qcow2".to_owned()),
            digest: "sha512:abc".to_owned(),
            seedable: false,
            held: false,
            size: None,
            firmware: Firmware::Bios,
            cpu: "max".to_owned(),
            machine: Chipset::Q35,
            disk: Disk::Virtio,
            architectures: vec!["amd64".to_owned()],
            catalogue: "project".to_owned(),
            kind: Kind::Remote,
            shadows: Vec::new(),
            entry: "/catalogue/entry.toml".to_owned(),
            path: None,
            used_by: Vec::new(),
        };
        let lines = report.render_text(Style::plain());
        assert!(
            !lines.iter().any(|line| line.starts_with("Aliases")),
            "{lines:?}"
        );
        assert!(to_json(&report.to_value()).contains(r#""aliases": []"#));
    }

    #[test]
    fn an_inspection_shows_other_architectures_origin_location_and_users() {
        let mut report = Inspect {
            name: "debian".to_owned(),
            tag: "trixie".to_owned(),
            aliases: Vec::new(),
            description: "Debian 13".to_owned(),
            arch: "arm64".to_owned(),
            format: "qcow2".to_owned(),
            compression: "none".to_owned(),
            source_format: None,
            media: vm_core::catalogue::Media::Disk,
            url: None,
            digest: "sha512:abc".to_owned(),
            seedable: true,
            held: true,
            size: None,
            firmware: Firmware::Bios,
            cpu: "max".to_owned(),
            machine: Chipset::Q35,
            disk: Disk::Virtio,
            architectures: vec!["amd64".to_owned(), "arm64".to_owned()],
            catalogue: "team".to_owned(),
            kind: Kind::Local,
            shadows: vec!["internal".to_owned(), "project".to_owned()],
            entry: "/local/debian/trixie.toml".to_owned(),
            path: Some("/store/sha512/abc".to_owned()),
            used_by: vec!["one".to_owned(), "two".to_owned()],
        };
        let field = |report: &Inspect, label: &str| {
            report
                .render_text(Style::plain())
                .into_iter()
                .find(|line| line.starts_with(label))
        };
        assert!(
            field(&report, "Architecture")
                .unwrap()
                .ends_with("arm64 (also amd64)")
        );
        assert!(field(&report, "Origin").unwrap().contains("local"));
        assert!(
            field(&report, "Entry")
                .unwrap()
                .ends_with("/local/debian/trixie.toml")
        );
        assert!(
            field(&report, "Pulled")
                .unwrap()
                .ends_with("yes, at /store/sha512/abc")
        );
        assert!(field(&report, "Used by").unwrap().ends_with("one, two"));
        let json = to_json(&report.to_value());
        for expected in [
            r#""origin": "local""#,
            r#""catalogue": "team""#,
            r#""internal""#,
            r#""path": "/store/sha512/abc""#,
            r#""entry": "/local/debian/trixie.toml""#,
            r#""arm64""#,
            r#""two""#,
        ] {
            assert!(json.contains(expected), "{expected} in {json}");
        }

        report.architectures = vec!["arm64".to_owned()];
        report.kind = Kind::Remote;
        report.catalogue = "internal".to_owned();
        report.shadows = Vec::new();
        report.held = false;
        report.path = None;
        report.used_by = Vec::new();
        assert!(field(&report, "Architecture").unwrap().ends_with("  arm64"));
        assert!(
            field(&report, "Origin")
                .unwrap()
                .ends_with("remote (internal)")
        );
        assert!(field(&report, "Pulled").unwrap().ends_with("  no"));
        assert_eq!(field(&report, "Used by"), None);
        assert_eq!(field(&report, "Shadows"), None);
        let json = to_json(&report.to_value());
        assert!(json.contains(r#""path": null"#), "{json}");
        assert!(json.contains(r#""used_by": []"#), "{json}");
    }

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
            source_format: None,
            media: vm_core::catalogue::Media::Disk,
            url: None,
            digest: String::new(),
            seedable: true,
            held: false,
            size: None,
            firmware: Firmware::Bios,
            cpu: "max".to_owned(),
            machine: Chipset::Q35,
            disk: Disk::Virtio,
            architectures: vec!["amd64".to_owned()],
            catalogue: "project".to_owned(),
            kind: Kind::Remote,
            shadows: Vec::new(),
            entry: "/catalogue/entry.toml".to_owned(),
            path: None,
            used_by: Vec::new(),
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
        report.source_format = Some("vmdk".to_owned());
        assert!(line(&report).ends_with("qcow2, converted from vmdk, published xz"));
        assert!(to_json(&report.to_value()).contains(r#""source_format": "vmdk""#));
        report.source_format = None;
        report.compression = "none".to_owned();
        assert!(line(&report).ends_with("qcow2"));
        assert!(to_json(&report.to_value()).contains(r#""media": "disk""#));
        report.media = vm_core::catalogue::Media::Cdrom;
        report.format = "raw".to_owned();
        assert!(line(&report).ends_with("raw, a CD-ROM image"));
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
            cdrom: None,
        }
    }

    #[test]
    fn a_machine_with_a_cdrom_says_how_to_eject_it() {
        let mut report = run(Firmware::Bios, "max", false);
        let text = report.render_text(Style::plain()).join("\n");
        assert!(!text.contains("cdrom"), "{text}");
        assert!(to_json(&report.to_value()).contains(r#""cdrom": null"#));
        report.cdrom = Some("/store/disc".to_owned());
        let text = report.render_text(Style::plain()).join("\n");
        assert!(
            text.contains("cdrom    puredarwin:minimal, until vm start pd --eject"),
            "{text}"
        );
        assert!(to_json(&report.to_value()).contains(r#""cdrom": "/store/disc""#));
    }

    fn imported(format: &str, artifact: Artifact) -> Imported {
        Imported {
            source: "/home/x/disk".to_owned(),
            outcome: vm_core::import::Imported {
                name: "mine".to_owned(),
                tag: "1".to_owned(),
                format: format.to_owned(),
                path: std::path::PathBuf::from("/store/blob"),
                entry: std::path::PathBuf::from("/local/mine/1.toml"),
                replaced: false,
                artifact,
            },
            hides: Vec::new(),
        }
    }

    fn imported_artifact() -> Artifact {
        Artifact {
            arch: "amd64".to_owned(),
            format: "qcow2".to_owned(),
            url: None,
            digest: format!("sha256:{}", "a".repeat(64)).parse().unwrap(),
            size: Some(1024 * 1024),
            compression: vm_core::compression::Compression::None,
            source_format: None,
            media: vm_core::catalogue::Media::Disk,
            firmware: Firmware::Bios,
            cpu: None,
            machine: Chipset::Q35,
            disk: Disk::Virtio,
        }
    }

    #[test]
    fn an_import_says_what_it_converted_and_whether_it_can_be_fetched_again() {
        let mut report = imported("vmdk", imported_artifact());
        let text = report.render_text(Style::plain()).join("\n");
        assert!(
            text.starts_with("Imported mine:1 (amd64, 1.0 MiB)"),
            "{text}"
        );
        assert!(text.contains("from     /home/x/disk"), "{text}");
        assert!(text.contains("format   vmdk, converted to qcow2"), "{text}");
        assert!(text.contains("fetch    never"), "{text}");
        assert!(!text.contains("hides"), "{text}");
        assert!(!text.contains("replaces"), "{text}");
        report.outcome.artifact.url = Some("https://example.invalid/x".to_owned());
        report.outcome.replaced = true;
        report.hides = vec!["project".to_owned()];
        let text = report.render_text(Style::plain()).join("\n");
        assert!(text.contains("fetch    again from its source"), "{text}");
        assert!(text.contains("hides    project"), "{text}");
        assert!(
            text.contains("replaces the image of the same name"),
            "{text}"
        );
        let document = to_json(&report.to_value());
        for field in [
            r#""original_format": "vmdk""#,
            r#""format": "qcow2""#,
            r#""media": "disk""#,
            r#""url": "https://example.invalid/x""#,
            r#""replaced": true"#,
            r#""entry": "/local/mine/1.toml""#,
        ] {
            assert!(document.contains(field), "{field}: {document}");
        }
    }

    fn exported() -> Exported {
        Exported {
            name: "debian".to_owned(),
            tag: "trixie".to_owned(),
            arch: "amd64".to_owned(),
            format: "qcow2".to_owned(),
            compression: vm_core::compression::Compression::None,
            cdrom: false,
            digest: "sha512:abc".to_owned(),
            path: "/out/disk.qcow2".to_owned(),
            size: 2 * 1024 * 1024,
            suffixed: None,
            verified: true,
        }
    }

    #[test]
    fn an_export_says_where_it_went_and_any_suffix_it_added() {
        let mut report = exported();
        let text = report.render_text(Style::plain()).join("\n");
        assert!(
            text.starts_with("Exported debian:trixie (amd64, 2.0 MiB)"),
            "{text}"
        );
        assert!(text.contains("to       /out/disk.qcow2"), "{text}");
        assert!(text.contains("format   qcow2"), "{text}");
        assert!(!text.contains("suffix"), "{text}");
        report.suffixed = Some("iso.gz".to_owned());
        report.cdrom = true;
        report.format = "iso".to_owned();
        report.compression = vm_core::compression::Compression::Gzip;
        let text = report.render_text(Style::plain()).join("\n");
        assert!(text.contains(".iso.gz was added"), "{text}");
        assert!(
            text.contains("format   iso, a CD-ROM image, compressed with gzip"),
            "{text}"
        );
        assert!(to_json(&report.to_value()).contains(r#""compression": "gzip""#));
        let document = to_json(&report.to_value());
        for field in [
            r#""suffixed": true"#,
            r#""verified": true"#,
            r#""media": "cdrom""#,
            r#""path": "/out/disk.qcow2""#,
        ] {
            assert!(document.contains(field), "{field}: {document}");
        }
    }

    #[test]
    fn an_imported_cdrom_image_says_it_is_kept_as_it_is() {
        let report = imported(
            "iso",
            Artifact {
                format: "raw".to_owned(),
                media: vm_core::catalogue::Media::Cdrom,
                ..imported_artifact()
            },
        );
        let text = report.render_text(Style::plain()).join("\n");
        assert!(
            text.contains("format   iso, kept as a CD-ROM image"),
            "{text}"
        );
        assert!(text.contains("boots the CD-ROM"), "{text}");
        let same = imported("qcow2", imported_artifact());
        let text = same.render_text(Style::plain()).join("\n");
        assert!(text.contains("format   qcow2\n"), "{text}");
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
                path: std::path::PathBuf::from("/catalogue/puredarwin/minimal.toml"),
                catalogue: "project".to_owned(),
                kind: Kind::Remote,
                shadows: Vec::new(),
            },
            &Artifact {
                arch: "amd64".to_owned(),
                format: "raw".to_owned(),
                url: None,
                digest: format!("sha256:{}", "a".repeat(64)).parse().unwrap(),
                size: None,
                compression: vm_core::compression::Compression::None,
                source_format: None,
                media: vm_core::catalogue::Media::Disk,
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
    fn an_update_reports_each_catalogue_and_fails_if_any_did() {
        let mut report = Update {
            catalogues: vec![
                Updated {
                    name: "project".to_owned(),
                    url: "https://example.test/c.tar.gz".to_owned(),
                    path: "/home/x/.local/share/vm/catalogues/project".to_owned(),
                    outcome: Ok((4, 3)),
                },
                Updated {
                    name: "internal".to_owned(),
                    url: "https://internal.test/c.tar.gz".to_owned(),
                    path: "/home/x/.local/share/vm/catalogues/internal".to_owned(),
                    outcome: Err("connection refused".to_owned()),
                },
            ],
        };
        let text = to_json(&report.to_value());
        for expected in [
            r#""entries": 3"#,
            r#""files": 4"#,
            r#""error": null"#,
            r#""error": "connection refused""#,
            r#""name": "internal""#,
        ] {
            assert!(text.contains(expected), "{expected} in {text}");
        }
        let lines = report.render_text(Style::plain());
        assert!(
            lines[0].contains("project") && lines[0].contains("3 entries in 4 files"),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("internal") && lines[1].contains("previous copy"),
            "{lines:?}"
        );
        assert!(!report.succeeded());
        report.catalogues.pop();
        assert!(report.succeeded());
    }

    #[test]
    fn an_image_listing_names_each_image_catalogue() {
        let lines = images().render_text(Style::plain());
        assert!(lines[0].contains("CATALOGUE"), "{lines:?}");
        assert!(lines[1].contains("project"), "{lines:?}");
        assert!(to_json(&images().to_value()).contains(r#""catalogue": "project""#));
    }

    #[test]
    fn a_kind_filter_admits_only_its_kind() {
        assert!(Origin::All.admits(Kind::Local) && Origin::All.admits(Kind::Remote));
        assert!(Origin::Local.admits(Kind::Local) && !Origin::Local.admits(Kind::Remote));
        assert!(Origin::Remote.admits(Kind::Remote) && !Origin::Remote.admits(Kind::Local));
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
            source_format: None,
            media: vm_core::catalogue::Media::Disk,
            url: None,
            digest: String::new(),
            seedable: true,
            held: false,
            size: Some(1024),
            firmware: Firmware::Bios,
            cpu: "max".to_owned(),
            machine: Chipset::Q35,
            disk: Disk::Virtio,
            architectures: vec!["amd64".to_owned()],
            catalogue: "project".to_owned(),
            kind: Kind::Remote,
            shadows: Vec::new(),
            entry: "/catalogue/entry.toml".to_owned(),
            path: None,
            used_by: Vec::new(),
        };
        assert!(report.access().contains("cloud-init"));
        report.seedable = false;
        assert!(report.access().contains("console only"));
    }
}

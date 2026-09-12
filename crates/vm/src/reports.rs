use crate::progress::human;
use crate::style::Style;
use crate::table;
use crate::value::Value;
use vm_core::catalogue::{Artifact, Entry};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{to_json, to_yaml};

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

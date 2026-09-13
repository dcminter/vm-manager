//! The `vm config` command.

use crate::completion;
use crate::output::Report;
use crate::style::Style;
use crate::table;
use clap::Subcommand;
use clap_complete::engine::ArgValueCandidates;
use std::path::{Path, PathBuf};
use vm_core::config::{
    CURRENT_USER, Config, FALLBACK_USER, Key, Local, PROJECT_CATALOGUE, Remote, STORE_CATALOGUE,
    Settings,
};
use vm_core::value::Value;
use vm_core::{Error, Result};

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Print where the config file is
    Path,
    /// Print a setting's value
    Get {
        #[arg(value_enum)]
        key: Setting,
    },
    /// Set a setting
    Set {
        #[arg(value_enum)]
        key: Setting,
        value: String,
    },
    /// Remove a setting so it takes its default
    Unset {
        #[arg(value_enum)]
        key: Setting,
    },
    /// Add a catalogue
    Add {
        #[command(subcommand)]
        catalogue: Added,
    },
    /// Remove a catalogue
    Remove {
        #[command(subcommand)]
        catalogue: Removed,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Setting {
    #[value(name = "auto_pull")]
    AutoPull,
    #[value(name = "add_ssh_config")]
    AddSshConfig,
    #[value(name = "default_user")]
    DefaultUser,
}

impl Setting {
    const fn key(self) -> Key {
        match self {
            Self::AutoPull => Key::AutoPull,
            Self::AddSshConfig => Key::AddSshConfig,
            Self::DefaultUser => Key::DefaultUser,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Added {
    /// A catalogue fetched by vm update, placed last unless --before is given
    Remote {
        name: String,
        /// A gzipped tar archive over http or https
        url: String,
        /// The directory within the archive holding the entries
        #[arg(long)]
        path: Option<String>,
        /// Place it before this remote catalogue
        #[arg(long, add = ArgValueCandidates::new(completion::configured_remote))]
        before: Option<String>,
    },
    /// A directory of catalogue entries, placed last unless --before is given
    Local {
        name: String,
        #[arg(value_hint = clap::ValueHint::DirPath)]
        path: PathBuf,
        /// Place it before this local catalogue
        #[arg(long, add = ArgValueCandidates::new(completion::configured_local))]
        before: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum Removed {
    /// Remove a remote catalogue
    Remote {
        #[arg(add = ArgValueCandidates::new(completion::configured_remote))]
        name: String,
    },
    /// Remove a local catalogue
    Local {
        #[arg(add = ArgValueCandidates::new(completion::configured_local))]
        name: String,
    },
}

pub fn run(action: Option<&Action>) -> Result<Box<dyn Report>> {
    let path = Config::path().ok_or(Error::NoConfigDirectory)?;
    let settings = Settings::read(&path)?;
    Ok(match action {
        None => Box::new(Shown {
            path,
            sources: vm_core::paths::catalogue_sources(
                &settings.clone().unwrap_or_default().config(),
            ),
            settings,
        }),
        Some(Action::Path) => Box::new(Located {
            exists: settings.is_some(),
            path,
        }),
        Some(Action::Get { key }) => Box::new(Got {
            key: key.key(),
            value: value(&settings.unwrap_or_default(), key.key()),
        }),
        Some(action) => Box::new(change(action, &path, settings)?),
    })
}

/// A setting's value as written, or its default.
fn value(settings: &Settings, key: Key) -> (String, bool) {
    let config = settings.config();
    let text = match key {
        Key::AutoPull => config.auto_pull.to_string(),
        Key::AddSshConfig => config.add_ssh_config.to_string(),
        Key::DefaultUser => config
            .default_user
            .unwrap_or_else(|| FALLBACK_USER.to_owned()),
    };
    (text, !settings.is_set(key))
}

/// What an edit did: a change to write, with notes, or why nothing changed.
enum Edit {
    Made(String, Vec<String>),
    Unchanged(String),
}

/// Applies a change, writing the file only when something changed.
fn change(action: &Action, path: &Path, existing: Option<Settings>) -> Result<Changed> {
    let exists = existing.is_some();
    let mut settings = existing.unwrap_or_default();
    let invalid = |reason: String| Error::Config {
        path: path.to_owned(),
        reason,
    };
    let edit = match action {
        Action::Set { key, value } => set(&mut settings, key.key(), value).map_err(invalid)?,
        Action::Unset { key } => unset(&mut settings, key.key()),
        Action::Add { catalogue } => add(&mut settings, catalogue).map_err(invalid)?,
        Action::Remove { catalogue } => remove(&mut settings, catalogue)?,
        Action::Path | Action::Get { .. } => Edit::Unchanged(String::new()),
    };
    match edit {
        Edit::Unchanged(summary) => Ok(Changed {
            path: path.to_owned(),
            created: false,
            modified: false,
            summary,
            notes: Vec::new(),
        }),
        Edit::Made(summary, mut notes) => {
            if has_comments(path) {
                notes.push("comments in the file are not kept".to_owned());
            }
            settings.write(path)?;
            Ok(Changed {
                path: path.to_owned(),
                created: !exists,
                modified: true,
                summary,
                notes,
            })
        }
    }
}

fn set(settings: &mut Settings, key: Key, value: &str) -> std::result::Result<Edit, String> {
    let before = value_if_set(settings, key);
    settings.set(key, value)?;
    if before.as_deref() == Some(value) {
        return Ok(Edit::Unchanged(format!(
            "{} is already {value}",
            key.name()
        )));
    }
    let notes = if key == Key::DefaultUser && value == CURRENT_USER {
        vec![format!("{CURRENT_USER} stands for the user running vm")]
    } else {
        Vec::new()
    };
    Ok(Edit::Made(format!("Set {} to {value}", key.name()), notes))
}

fn unset(settings: &mut Settings, key: Key) -> Edit {
    if settings.unset(key) {
        Edit::Made(
            format!(
                "Removed {}; it is {}, the default",
                key.name(),
                value(settings, key).0
            ),
            Vec::new(),
        )
    } else {
        Edit::Unchanged(format!(
            "{} is not set; it is {}",
            key.name(),
            value(settings, key).0
        ))
    }
}

fn add(settings: &mut Settings, catalogue: &Added) -> std::result::Result<Edit, String> {
    match catalogue {
        Added::Remote {
            name,
            url,
            path,
            before,
        } => {
            let first = settings.remotes.is_empty();
            let remote = Remote {
                name: name.clone(),
                url: url.clone(),
                path: path
                    .clone()
                    .unwrap_or_else(|| Config::default().remotes().remove(0).path),
            };
            settings.add_remote(remote, before.as_deref())?;
            let mut notes = Vec::new();
            if first && name != PROJECT_CATALOGUE {
                notes.push(format!(
                    "{PROJECT_CATALOGUE} is listed first so it stays in use; \
                     'vm config remove remote {PROJECT_CATALOGUE}' drops it"
                ));
            }
            notes.push(format!("'vm update {name}' fetches it"));
            Ok(Edit::Made(format!("Added remote catalogue {name}"), notes))
        }
        Added::Local { name, path, before } => {
            let directory = std::fs::canonicalize(path)
                .map_err(|_| format!("{} is not a directory", path.display()))?;
            let local = Local {
                name: name.clone(),
                path: directory,
            };
            settings.add_local(local, before.as_deref())?;
            Ok(Edit::Made(
                format!("Added local catalogue {name}"),
                Vec::new(),
            ))
        }
    }
}

fn remove(settings: &mut Settings, catalogue: &Removed) -> Result<Edit> {
    match catalogue {
        Removed::Remote { name } => {
            if !settings.remove_remote(name) {
                return Err(unknown(
                    name,
                    settings.remotes.iter().map(|held| &held.name),
                ));
            }
            let notes = if settings.remotes.is_empty() {
                vec![format!(
                    "no remote catalogue is listed, so {PROJECT_CATALOGUE} is used"
                )]
            } else {
                Vec::new()
            };
            Ok(Edit::Made(
                format!("Removed remote catalogue {name}"),
                notes,
            ))
        }
        Removed::Local { name } => {
            if !settings.remove_local(name) {
                return Err(unknown(name, settings.locals.iter().map(|held| &held.name)));
            }
            Ok(Edit::Made(
                format!("Removed local catalogue {name}"),
                Vec::new(),
            ))
        }
    }
}

/// Whether an existing config file holds comments a rewrite would drop.
fn has_comments(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .is_ok_and(|text| text.lines().any(|line| line.trim_start().starts_with('#')))
}

fn value_if_set(settings: &Settings, key: Key) -> Option<String> {
    settings.is_set(key).then(|| value(settings, key).0)
}

fn unknown<'a>(name: &str, names: impl Iterator<Item = &'a String>) -> Error {
    Error::UnknownCatalogue {
        name: name.to_owned(),
        available: names.cloned().collect(),
    }
}

/// The config file's settings and catalogues, defaults included.
pub struct Shown {
    pub path: PathBuf,
    pub settings: Option<Settings>,
    pub sources: Vec<vm_core::catalogue::Source>,
}

impl Shown {
    fn remotes(&self) -> Vec<(Remote, bool)> {
        let settings = self.settings.clone().unwrap_or_default();
        let listed = !settings.remotes.is_empty();
        settings
            .config()
            .remotes()
            .into_iter()
            .map(|remote| (remote, !listed))
            .collect()
    }

    fn locals(&self) -> Vec<(String, PathBuf, bool)> {
        self.sources
            .iter()
            .filter(|source| source.kind == vm_core::catalogue::Kind::Local)
            .map(|source| {
                (
                    source.name.clone(),
                    source.directory.clone(),
                    source.name == STORE_CATALOGUE,
                )
            })
            .collect()
    }
}

impl Report for Shown {
    fn to_value(&self) -> Value {
        let settings = self.settings.clone().unwrap_or_default();
        Value::map([
            ("path", Value::string(self.path.display().to_string())),
            ("exists", Value::Bool(self.settings.is_some())),
            (
                "settings",
                Value::list(Key::ALL.into_iter().map(|key| {
                    let (text, default) = value(&settings, key);
                    Value::map([
                        ("name", Value::string(key.name())),
                        ("value", Value::string(text)),
                        ("default", Value::Bool(default)),
                    ])
                })),
            ),
            (
                "remotes",
                Value::list(self.remotes().into_iter().map(|(remote, default)| {
                    Value::map([
                        ("name", Value::string(remote.name)),
                        ("url", Value::string(remote.url)),
                        ("path", Value::string(remote.path)),
                        ("default", Value::Bool(default)),
                    ])
                })),
            ),
            (
                "locals",
                Value::list(self.locals().into_iter().map(|(name, path, built_in)| {
                    Value::map([
                        ("name", Value::string(name)),
                        ("path", Value::string(path.display().to_string())),
                        ("built_in", Value::Bool(built_in)),
                    ])
                })),
            ),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        let settings = self.settings.clone().unwrap_or_default();
        let marked = |default: bool, text: &str| {
            if default {
                style.dim(text)
            } else {
                String::new()
            }
        };
        let mut lines = vec![match &self.settings {
            Some(_) => format!(
                "Config file {}",
                style.name(&self.path.display().to_string())
            ),
            None => format!(
                "No config file; one is created at {} when a setting is added",
                style.name(&self.path.display().to_string())
            ),
        }];
        let mut section = |headings: &[&str], rows: Vec<Vec<String>>| {
            lines.push(String::new());
            let mut rendered = table::render(headings, &rows);
            if let Some(first) = rendered.first_mut() {
                *first = style.heading(first);
            }
            lines.extend(rendered);
        };
        section(
            &["SETTING", "VALUE", ""],
            Key::ALL
                .into_iter()
                .map(|key| {
                    let (text, default) = value(&settings, key);
                    vec![key.name().to_owned(), text, marked(default, "default")]
                })
                .collect(),
        );
        section(
            &["REMOTE", "URL", "PATH", ""],
            self.remotes()
                .into_iter()
                .map(|(remote, default)| {
                    vec![
                        remote.name,
                        remote.url,
                        remote.path,
                        marked(default, "default"),
                    ]
                })
                .collect(),
        );
        section(
            &["LOCAL", "PATH", ""],
            self.locals()
                .into_iter()
                .map(|(name, path, built_in)| {
                    vec![
                        name,
                        path.display().to_string(),
                        marked(built_in, "built in"),
                    ]
                })
                .collect(),
        );
        lines
            .into_iter()
            .map(|line| line.trim_end().to_owned())
            .collect()
    }
}

pub struct Located {
    pub path: PathBuf,
    pub exists: bool,
}

impl Report for Located {
    fn to_value(&self) -> Value {
        Value::map([
            ("path", Value::string(self.path.display().to_string())),
            ("exists", Value::Bool(self.exists)),
        ])
    }

    fn render_text(&self, _: Style) -> Vec<String> {
        vec![self.path.display().to_string()]
    }
}

pub struct Got {
    pub key: Key,
    /// The value, and whether it is the default.
    pub value: (String, bool),
}

impl Report for Got {
    fn to_value(&self) -> Value {
        Value::map([
            ("name", Value::string(self.key.name())),
            ("value", Value::string(self.value.0.clone())),
            ("default", Value::Bool(self.value.1)),
        ])
    }

    fn render_text(&self, _: Style) -> Vec<String> {
        vec![self.value.0.clone()]
    }
}

/// What a change did to the config file.
#[derive(Debug)]
pub struct Changed {
    pub path: PathBuf,
    pub created: bool,
    pub modified: bool,
    pub summary: String,
    pub notes: Vec<String>,
}

impl Report for Changed {
    fn to_value(&self) -> Value {
        Value::map([
            ("path", Value::string(self.path.display().to_string())),
            ("created", Value::Bool(self.created)),
            ("changed", Value::Bool(self.modified)),
            ("summary", Value::string(self.summary.clone())),
            ("notes", Value::strings(self.notes.clone())),
        ])
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        let mut lines = vec![self.summary.clone()];
        if self.created {
            lines.push(style.dim(&format!("  Created {}", self.path.display())));
        }
        lines.extend(
            self.notes
                .iter()
                .map(|note| style.dim(&format!("  {note}"))),
        );
        lines
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use vm_core::value::to_json;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("vm-configuration-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn config(&self) -> PathBuf {
            self.0.join("vm/config.toml")
        }

        fn apply(&self, action: &Action) -> Result<Changed> {
            change(
                action,
                &self.config(),
                Settings::read(&self.config()).unwrap(),
            )
        }

        fn text(&self) -> String {
            std::fs::read_to_string(self.config()).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn set(key: Setting, value: &str) -> Action {
        Action::Set {
            key,
            value: value.to_owned(),
        }
    }

    fn remote(name: &str, before: Option<&str>) -> Action {
        Action::Add {
            catalogue: Added::Remote {
                name: name.to_owned(),
                url: format!("https://{name}.test/c.tar.gz"),
                path: None,
                before: before.map(str::to_owned),
            },
        }
    }

    #[test]
    fn the_first_setting_creates_the_file() {
        let scratch = Scratch::new("create");
        let changed = scratch.apply(&set(Setting::AutoPull, "false")).unwrap();
        assert!(changed.created && changed.modified);
        assert_eq!(changed.summary, "Set auto_pull to false");
        assert_eq!(scratch.text(), "auto_pull = false\n");
        let changed = scratch.apply(&set(Setting::AddSshConfig, "true")).unwrap();
        assert!(!changed.created);
        assert_eq!(scratch.text(), "auto_pull = false\nadd_ssh_config = true\n");
    }

    #[test]
    fn nothing_that_changes_nothing_creates_the_file() {
        let scratch = Scratch::new("nocreate");
        let changed = scratch
            .apply(&Action::Unset {
                key: Setting::DefaultUser,
            })
            .unwrap();
        assert!(!changed.modified);
        assert_eq!(changed.summary, "default_user is not set; it is vm");
        let removal = Action::Remove {
            catalogue: Removed::Local {
                name: "team".to_owned(),
            },
        };
        assert_eq!(
            scratch.apply(&removal).unwrap_err().kind(),
            "unknown-catalogue"
        );
        assert_eq!(
            scratch
                .apply(&set(Setting::AutoPull, "maybe"))
                .unwrap_err()
                .kind(),
            "config-invalid"
        );
        assert!(!scratch.config().exists());
    }

    #[test]
    fn a_rewrite_says_that_comments_are_dropped() {
        let scratch = Scratch::new("comments");
        std::fs::create_dir_all(scratch.config().parent().unwrap()).unwrap();
        std::fs::write(scratch.config(), "# mine\nauto_pull = false\n").unwrap();
        let changed = scratch.apply(&set(Setting::AddSshConfig, "true")).unwrap();
        assert_eq!(changed.notes, ["comments in the file are not kept"]);
        assert!(!scratch.text().contains('#'));
        let changed = scratch.apply(&set(Setting::AutoPull, "true")).unwrap();
        assert!(changed.notes.is_empty());
    }

    #[test]
    fn setting_the_value_already_set_leaves_the_file_alone() {
        let scratch = Scratch::new("same");
        scratch.apply(&set(Setting::DefaultUser, "$USER")).unwrap();
        let changed = scratch.apply(&set(Setting::DefaultUser, "$USER")).unwrap();
        assert!(!changed.modified);
        assert_eq!(changed.summary, "default_user is already $USER");
    }

    #[test]
    fn unsetting_names_the_default_it_returns_to() {
        let scratch = Scratch::new("unset");
        scratch.apply(&set(Setting::AutoPull, "false")).unwrap();
        let changed = scratch
            .apply(&Action::Unset {
                key: Setting::AutoPull,
            })
            .unwrap();
        assert!(changed.modified);
        assert_eq!(
            changed.summary,
            "Removed auto_pull; it is true, the default"
        );
        assert_eq!(scratch.text(), "");
    }

    #[test]
    fn remotes_are_added_in_order_keeping_the_project_catalogue() {
        let scratch = Scratch::new("remotes");
        let changed = scratch.apply(&remote("internal", None)).unwrap();
        assert!(
            changed.notes[0].contains("project is listed first"),
            "{:?}",
            changed.notes
        );
        assert!(
            changed.notes[1].contains("vm update internal"),
            "{:?}",
            changed.notes
        );
        let changed = scratch.apply(&remote("mirror", Some("internal"))).unwrap();
        assert_eq!(changed.notes.len(), 1);
        let names: Vec<String> = Settings::read(&scratch.config())
            .unwrap()
            .unwrap()
            .remotes
            .into_iter()
            .map(|held| held.name)
            .collect();
        assert_eq!(names, ["project", "mirror", "internal"]);
        assert!(scratch.apply(&remote("mirror", None)).is_err());
    }

    #[test]
    fn removing_the_last_remote_says_the_project_catalogue_returns() {
        let scratch = Scratch::new("lastremote");
        scratch.apply(&remote("project", None)).unwrap();
        let changed = scratch
            .apply(&Action::Remove {
                catalogue: Removed::Remote {
                    name: "project".to_owned(),
                },
            })
            .unwrap();
        assert_eq!(changed.summary, "Removed remote catalogue project");
        assert!(
            changed.notes[0].contains("project is used"),
            "{:?}",
            changed.notes
        );
    }

    #[test]
    fn a_local_catalogue_is_stored_as_an_absolute_path() {
        let scratch = Scratch::new("local");
        let directory = scratch.0.join("team");
        std::fs::create_dir_all(&directory).unwrap();
        let add = |path: PathBuf| Action::Add {
            catalogue: Added::Local {
                name: "team".to_owned(),
                path,
                before: None,
            },
        };
        let dotted = scratch.0.join("team/../team");
        scratch.apply(&add(dotted)).unwrap();
        let settings = Settings::read(&scratch.config()).unwrap().unwrap();
        assert_eq!(
            settings.locals[0].path,
            std::fs::canonicalize(&directory).unwrap()
        );
        let missing = scratch.apply(&add(scratch.0.join("absent"))).unwrap_err();
        assert!(missing.to_string().contains("not a directory"), "{missing}");
    }

    #[test]
    fn showing_without_a_file_says_where_one_would_go_and_marks_defaults() {
        let shown = Shown {
            path: PathBuf::from("/home/x/.config/vm/config.toml"),
            settings: None,
            sources: vec![vm_core::catalogue::Source {
                name: STORE_CATALOGUE.to_owned(),
                kind: vm_core::catalogue::Kind::Local,
                directory: PathBuf::from("/home/x/.local/share/vm/local"),
            }],
        };
        let lines = shown.render_text(Style::plain());
        assert!(lines[0].starts_with("No config file"), "{lines:?}");
        assert!(
            lines[0].contains("/home/x/.config/vm/config.toml"),
            "{lines:?}"
        );
        let text = lines.join("\n");
        assert!(text.contains("auto_pull       true   default"), "{text}");
        assert!(text.contains("project"), "{text}");
        assert!(
            text.contains("store  /home/x/.local/share/vm/local  built in"),
            "{text}"
        );
        let json = to_json(&shown.to_value());
        assert!(json.contains(r#""exists": false"#), "{json}");
        assert!(json.contains(r#""default": true"#), "{json}");
    }

    #[test]
    fn showing_a_file_marks_only_what_it_does_not_set() {
        let mut settings = Settings::default();
        settings.set(Key::AutoPull, "false").unwrap();
        let shown = Shown {
            path: PathBuf::from("/c/config.toml"),
            settings: Some(settings),
            sources: Vec::new(),
        };
        let text = shown.render_text(Style::plain()).join("\n");
        assert!(text.starts_with("Config file /c/config.toml"), "{text}");
        assert!(text.contains("\nauto_pull       false\n"), "{text}");
        assert!(text.contains("add_ssh_config  false  default"), "{text}");
    }

    #[test]
    fn path_and_get_print_bare_values() {
        let located = Located {
            path: PathBuf::from("/c/config.toml"),
            exists: false,
        };
        assert_eq!(located.render_text(Style::plain()), ["/c/config.toml"]);
        assert!(to_json(&located.to_value()).contains(r#""exists": false"#));
        let got = Got {
            key: Key::DefaultUser,
            value: value(&Settings::default(), Key::DefaultUser),
        };
        assert_eq!(got.render_text(Style::plain()), ["vm"]);
        assert!(to_json(&got.to_value()).contains(r#""default": true"#));
    }

    #[test]
    fn the_command_line_takes_settings_and_catalogues() {
        use clap::Parser as _;
        let parse = |arguments: &[&str]| crate::Cli::try_parse_from(arguments);
        assert!(parse(&["vm", "config"]).is_ok());
        assert!(parse(&["vm", "config", "set", "auto_pull", "false"]).is_ok());
        assert!(parse(&["vm", "config", "set", "auto_pul", "false"]).is_err());
        assert!(
            parse(&[
                "vm",
                "config",
                "add",
                "remote",
                "a",
                "https://a.test/c.tar.gz",
                "--before",
                "b",
                "--path",
                "images"
            ])
            .is_ok()
        );
        assert!(parse(&["vm", "config", "add", "local", "team", "/srv/team"]).is_ok());
        assert!(parse(&["vm", "config", "remove", "remote", "a"]).is_ok());
        assert!(parse(&["vm", "config", "remove", "store"]).is_err());
    }
}

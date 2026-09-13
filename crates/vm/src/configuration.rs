//! The `vm config` command.

use crate::completion;
use crate::output::Report;
use crate::style::Style;
use crate::table;
use clap::Subcommand;
use clap_complete::engine::ArgValueCandidates;
use std::path::PathBuf;
use vm_core::Result;
use vm_core::config::{Key, Remote, STORE_CATALOGUE};
use vm_core::configuration::{self, Outcome, value};
use vm_core::reports::{Changed, Got, Located, Shown};
use vm_core::value::Value;

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
        /// A catalogue pointer file over http or https
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

impl Action {
    fn plain(&self) -> configuration::Action {
        match self {
            Self::Path => configuration::Action::Path,
            Self::Get { key } => configuration::Action::Get { key: key.key() },
            Self::Set { key, value } => configuration::Action::Set {
                key: key.key(),
                value: value.clone(),
            },
            Self::Unset { key } => configuration::Action::Unset { key: key.key() },
            Self::Add { catalogue } => configuration::Action::Add(match catalogue {
                Added::Remote {
                    name,
                    url,
                    path,
                    before,
                } => configuration::Added::Remote {
                    name: name.clone(),
                    url: url.clone(),
                    path: path.clone(),
                    before: before.clone(),
                },
                Added::Local { name, path, before } => configuration::Added::Local {
                    name: name.clone(),
                    path: path.clone(),
                    before: before.clone(),
                },
            }),
            Self::Remove { catalogue } => configuration::Action::Remove(match catalogue {
                Removed::Remote { name } => configuration::Removed::Remote { name: name.clone() },
                Removed::Local { name } => configuration::Removed::Local { name: name.clone() },
            }),
        }
    }
}

pub fn run(action: Option<&Action>) -> Result<Box<dyn Report>> {
    let plain = action.map(Action::plain);
    Ok(match configuration::run(plain.as_ref())? {
        Outcome::Shown(report) => Box::new(report),
        Outcome::Located(report) => Box::new(report),
        Outcome::Got(report) => Box::new(report),
        Outcome::Changed(report) => Box::new(report),
    })
}

/// Every remote catalogue in use, and whether it is there by default.
fn remotes(shown: &Shown) -> Vec<(Remote, bool)> {
    let settings = shown.settings.clone().unwrap_or_default();
    let listed = !settings.remotes.is_empty();
    settings
        .config()
        .remotes()
        .into_iter()
        .map(|remote| (remote, !listed))
        .collect()
}

/// Every local catalogue in use, and whether it is built in.
fn locals(shown: &Shown) -> Vec<(String, PathBuf, bool)> {
    shown
        .sources
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
                Value::list(remotes(self).into_iter().map(|(remote, default)| {
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
                Value::list(locals(self).into_iter().map(|(name, path, built_in)| {
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
            remotes(self)
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
            locals(self)
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
    use std::path::PathBuf;
    use vm_core::config::Settings;
    use vm_core::value::to_json;

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

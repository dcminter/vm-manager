//! Changes to the config file, and what they did.

use crate::config::{
    CURRENT_USER, Config, FALLBACK_USER, Key, Local, PROJECT_CATALOGUE, Remote, Settings,
};
use crate::reports::{Changed, Got, Located, Shown};
use crate::{Error, Result};
use std::path::{Path, PathBuf};

/// What `vm config` is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Say where the config file is.
    Path,
    /// Give a setting's value.
    Get {
        key: Key,
    },
    Set {
        key: Key,
        value: String,
    },
    /// Remove a setting so it takes its default.
    Unset {
        key: Key,
    },
    Add(Added),
    Remove(Removed),
}

/// A catalogue to add, placed last unless `before` names the one it precedes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Added {
    Remote {
        name: String,
        url: String,
        path: Option<String>,
        before: Option<String>,
    },
    Local {
        name: String,
        path: PathBuf,
        before: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removed {
    Remote { name: String },
    Local { name: String },
}

/// What `vm config` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Shown(Shown),
    Located(Located),
    Got(Got),
    Changed(Changed),
}

pub fn run(action: Option<&Action>) -> Result<Outcome> {
    let path = Config::path().ok_or(Error::NoConfigDirectory)?;
    let settings = Settings::read(&path)?;
    Ok(match action {
        None => Outcome::Shown(Shown {
            path,
            sources: crate::paths::catalogue_sources(
                &settings.clone().unwrap_or_default().config(),
            ),
            settings,
        }),
        Some(Action::Path) => Outcome::Located(Located {
            exists: settings.is_some(),
            path,
        }),
        Some(Action::Get { key }) => Outcome::Got(Got {
            key: *key,
            value: value(&settings.unwrap_or_default(), *key),
        }),
        Some(action) => Outcome::Changed(change(action, &path, settings)?),
    })
}

/// A setting's value as written, or its default, and whether it is the default.
pub fn value(settings: &Settings, key: Key) -> (String, bool) {
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
        Action::Set { key, value } => set(&mut settings, *key, value).map_err(invalid)?,
        Action::Unset { key } => unset(&mut settings, *key),
        Action::Add(catalogue) => add(&mut settings, catalogue).map_err(invalid)?,
        Action::Remove(catalogue) => remove(&mut settings, catalogue)?,
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

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

    fn set(key: Key, value: &str) -> Action {
        Action::Set {
            key,
            value: value.to_owned(),
        }
    }

    fn remote(name: &str, before: Option<&str>) -> Action {
        Action::Add(Added::Remote {
            name: name.to_owned(),
            url: format!("https://{name}.test/c.tar.gz"),
            path: None,
            before: before.map(str::to_owned),
        })
    }

    #[test]
    fn the_first_setting_creates_the_file() {
        let scratch = Scratch::new("create");
        let changed = scratch.apply(&set(Key::AutoPull, "false")).unwrap();
        assert!(changed.created && changed.modified);
        assert_eq!(changed.summary, "Set auto_pull to false");
        assert_eq!(scratch.text(), "auto_pull = false\n");
        let changed = scratch.apply(&set(Key::AddSshConfig, "true")).unwrap();
        assert!(!changed.created);
        assert_eq!(scratch.text(), "auto_pull = false\nadd_ssh_config = true\n");
    }

    #[test]
    fn nothing_that_changes_nothing_creates_the_file() {
        let scratch = Scratch::new("nocreate");
        let changed = scratch
            .apply(&Action::Unset {
                key: Key::DefaultUser,
            })
            .unwrap();
        assert!(!changed.modified);
        assert_eq!(changed.summary, "default_user is not set; it is vm");
        let removal = Action::Remove(Removed::Local {
            name: "team".to_owned(),
        });
        assert_eq!(
            scratch.apply(&removal).unwrap_err().kind(),
            "unknown-catalogue"
        );
        assert_eq!(
            scratch
                .apply(&set(Key::AutoPull, "maybe"))
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
        let changed = scratch.apply(&set(Key::AddSshConfig, "true")).unwrap();
        assert_eq!(changed.notes, ["comments in the file are not kept"]);
        assert!(!scratch.text().contains('#'));
        let changed = scratch.apply(&set(Key::AutoPull, "true")).unwrap();
        assert!(changed.notes.is_empty());
    }

    #[test]
    fn setting_the_value_already_set_leaves_the_file_alone() {
        let scratch = Scratch::new("same");
        scratch.apply(&set(Key::DefaultUser, "$USER")).unwrap();
        let changed = scratch.apply(&set(Key::DefaultUser, "$USER")).unwrap();
        assert!(!changed.modified);
        assert_eq!(changed.summary, "default_user is already $USER");
    }

    #[test]
    fn unsetting_names_the_default_it_returns_to() {
        let scratch = Scratch::new("unset");
        scratch.apply(&set(Key::AutoPull, "false")).unwrap();
        let changed = scratch
            .apply(&Action::Unset { key: Key::AutoPull })
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
            .apply(&Action::Remove(Removed::Remote {
                name: "project".to_owned(),
            }))
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
        let add = |path: PathBuf| {
            Action::Add(Added::Local {
                name: "team".to_owned(),
                path,
                before: None,
            })
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
}

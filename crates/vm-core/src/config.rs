use crate::error::{Error, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

/// The project's own catalogue archive.
pub const PROJECT_CATALOGUE_URL: &str =
    "https://codeload.github.com/dcminter/vm-manager/tar.gz/refs/heads/main";
const DEFAULT_CATALOGUE_PATH: &str = "catalogue";

/// The name of the project's catalogue when no remote is configured.
pub const PROJECT_CATALOGUE: &str = "project";

/// The name of the local catalogue `vm clone` writes to.
pub const CLONES_CATALOGUE: &str = "clones";

/// A catalogue fetched by `vm update`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Remote {
    pub name: String,
    /// A gzipped tar archive.
    pub url: String,
    /// The directory inside the archive holding the entries.
    #[serde(default = "default_catalogue_path")]
    pub path: String,
}

/// A catalogue read where it is.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Local {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Remote catalogues, lowest precedence first.
    #[serde(default, rename = "remote")]
    pub remotes: Vec<Remote>,
    /// Local catalogues, lowest precedence first.
    #[serde(default, rename = "local")]
    pub locals: Vec<Local>,
    /// Whether `vm run` fetches an image it does not hold.
    #[serde(default = "default_auto_pull")]
    pub auto_pull: bool,
    /// Whether `vm run` adds an SSH configuration entry without being asked to.
    #[serde(default)]
    pub add_ssh_config: bool,
    /// The account `vm run` creates without `--user`; [`CURRENT_USER`] means the user running `vm`.
    #[serde(default)]
    pub default_user: Option<String>,
}

/// The account `vm run` creates when neither `--user` nor the config names one.
pub const FALLBACK_USER: &str = "vm";

/// The `default_user` value standing for the user running `vm`.
pub const CURRENT_USER: &str = "$USER";

/// Top-level settings refused with a pointer to `[[remote]]`.
const OBSOLETE: [&str; 2] = ["catalogue_url", "catalogue_path"];

const fn default_auto_pull() -> bool {
    true
}

fn default_catalogue_path() -> String {
    DEFAULT_CATALOGUE_PATH.to_owned()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            remotes: Vec::new(),
            locals: Vec::new(),
            auto_pull: default_auto_pull(),
            add_ssh_config: false,
            default_user: None,
        }
    }
}

impl Config {
    /// Reads the user's config; an absent file gives the defaults.
    pub fn load() -> Result<Self> {
        crate::paths::config_directory().map_or_else(
            || Ok(Self::default()),
            |directory| Self::read(&directory.join("config.toml")),
        )
    }

    /// The remote catalogues in precedence order, or the project's when none is configured.
    pub fn remotes(&self) -> Vec<Remote> {
        if self.remotes.is_empty() {
            vec![Remote {
                name: PROJECT_CATALOGUE.to_owned(),
                url: PROJECT_CATALOGUE_URL.to_owned(),
                path: default_catalogue_path(),
            }]
        } else {
            self.remotes.clone()
        }
    }

    /// The account to create when `--user` is not given.
    pub fn user(&self) -> Result<String> {
        self.user_with(current_user)
    }

    fn user_with(&self, current: impl FnOnce() -> Result<String>) -> Result<String> {
        let (name, current) = match self.default_user.as_deref() {
            None => return Ok(FALLBACK_USER.to_owned()),
            Some(CURRENT_USER) => (current()?, true),
            Some(name) => (name.to_owned(), false),
        };
        if crate::seed::is_username(&name) {
            Ok(name)
        } else {
            Err(Error::UnusableDefaultUser { name, current })
        }
    }

    pub fn read(path: &Path) -> Result<Self> {
        let invalid = |reason: String| Error::Config {
            path: path.to_owned(),
            reason,
        };
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(Error::CatalogueRead {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        if let Some(setting) = obsolete_setting(&text) {
            return Err(invalid(format!(
                "{setting} is no longer a setting; name each catalogue in a [[remote]] table \
                 with name, url and path"
            )));
        }
        let config: Self =
            basic_toml::from_str(&text).map_err(|source| invalid(source.to_string()))?;
        config.check().map_err(invalid)?;
        Ok(config)
    }

    /// Refuses catalogue names that are unusable or used twice.
    fn check(&self) -> std::result::Result<(), String> {
        let names: Vec<&str> = self
            .remotes
            .iter()
            .map(|remote| remote.name.as_str())
            .chain(self.locals.iter().map(|local| local.name.as_str()))
            .collect();
        for (index, name) in names.iter().enumerate() {
            if !is_catalogue_name(name) {
                return Err(format!(
                    "'{name}' is not a usable catalogue name; use lowercase letters, digits and dashes"
                ));
            }
            if *name == CLONES_CATALOGUE {
                return Err(format!("'{name}' is reserved for images made by vm clone"));
            }
            if names[..index].contains(name) {
                return Err(format!("the catalogue name '{name}' is used twice"));
            }
        }
        if let Some(local) = self.locals.iter().find(|local| !local.path.is_absolute()) {
            return Err(format!(
                "the path of local catalogue '{}' must be absolute",
                local.name
            ));
        }
        Ok(())
    }
}

/// The first top-level line setting an obsolete key.
fn obsolete_setting(text: &str) -> Option<&'static str> {
    text.lines()
        .map(str::trim_start)
        .take_while(|line| !line.starts_with('['))
        .find_map(|line| {
            OBSOLETE.into_iter().find(|setting| {
                line.strip_prefix(setting)
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
            })
        })
}

/// Whether a name is usable as a catalogue's directory name.
pub fn is_catalogue_name(name: &str) -> bool {
    name.len() <= 64
        && name.starts_with(|character: char| {
            character.is_ascii_lowercase() || character.is_ascii_digit()
        })
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The login name of the user running this process.
fn current_user() -> Result<String> {
    let output = std::process::Command::new("id")
        .arg("-un")
        .output()
        .map_err(|source| Error::NoCurrentUser {
            reason: source.to_string(),
        })?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() && !name.is_empty() {
        Ok(name)
    } else {
        Err(Error::NoCurrentUser {
            reason: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("vm-config-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, body: &str) -> PathBuf {
            let path = self.0.join("config.toml");
            fs::write(&path, body).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn an_absent_file_yields_the_defaults() {
        let config = Config::read(Path::new("/nonexistent/vm/config.toml")).unwrap();
        assert_eq!(config, Config::default());
        let remotes = config.remotes();
        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes[0].name, PROJECT_CATALOGUE);
        assert_eq!(remotes[0].url, PROJECT_CATALOGUE_URL);
        assert_eq!(remotes[0].path, DEFAULT_CATALOGUE_PATH);
        assert!(config.locals.is_empty());
    }

    #[test]
    fn remotes_and_locals_are_read_in_order() {
        let scratch = Scratch::new("catalogues");
        let path = scratch.write(
            "[[remote]]\nname = \"public\"\nurl = \"https://example.test/a.tar.gz\"\n\n\
             [[remote]]\nname = \"internal\"\nurl = \"https://example.test/b.tar.gz\"\npath = \"images\"\n\n\
             [[local]]\nname = \"team\"\npath = \"/srv/team\"\n",
        );
        let config = Config::read(&path).unwrap();
        let remotes = config.remotes();
        assert_eq!(
            remotes
                .iter()
                .map(|remote| remote.name.as_str())
                .collect::<Vec<_>>(),
            ["public", "internal"]
        );
        assert_eq!(remotes[0].path, DEFAULT_CATALOGUE_PATH);
        assert_eq!(remotes[1].path, "images");
        assert_eq!(config.locals[0].path, PathBuf::from("/srv/team"));
    }

    #[test]
    fn the_obsolete_catalogue_settings_say_what_replaces_them() {
        let scratch = Scratch::new("obsolete");
        for body in [
            "catalogue_url = \"https://example.test/c.tar.gz\"\n",
            "auto_pull = true\n  catalogue_path=\"images\"\n",
        ] {
            let error = Config::read(&scratch.write(body)).unwrap_err();
            assert_eq!(error.kind(), "config-invalid");
            assert!(error.to_string().contains("[[remote]]"), "{error}");
        }
    }

    #[test]
    fn catalogue_names_must_be_usable_distinct_and_not_reserved() {
        let scratch = Scratch::new("names");
        for (body, expected) in [
            (
                "[[remote]]\nname = \"Bad Name\"\nurl = \"u\"\n",
                "not a usable",
            ),
            (
                "[[remote]]\nname = \"a\"\nurl = \"u\"\n[[local]]\nname = \"a\"\npath = \"/x\"\n",
                "used twice",
            ),
            ("[[local]]\nname = \"clones\"\npath = \"/x\"\n", "reserved"),
            (
                "[[local]]\nname = \"team\"\npath = \"relative\"\n",
                "absolute",
            ),
            ("[[remote]]\nname = \"a\"\n", "url"),
        ] {
            let error = Config::read(&scratch.write(body)).unwrap_err();
            assert!(error.to_string().contains(expected), "{expected}: {error}");
        }
        assert!(is_catalogue_name("internal-2"));
        assert!(!is_catalogue_name("-lead"));
        assert!(!is_catalogue_name("../up"));
    }

    #[test]
    fn an_empty_file_yields_the_defaults() {
        let scratch = Scratch::new("empty");
        let path = scratch.write("");
        assert_eq!(Config::read(&path).unwrap(), Config::default());
    }

    #[test]
    fn an_unknown_setting_is_reported_rather_than_ignored() {
        let scratch = Scratch::new("unknown");
        let path = scratch.write("auto_pul = false\n");
        let error = Config::read(&path).unwrap_err();
        assert!(error.to_string().contains("config.toml"), "{error}");
    }

    #[test]
    fn pulling_on_demand_is_the_default_and_can_be_turned_off() {
        assert!(Config::default().auto_pull);
        let scratch = Scratch::new("autopull");
        let path = scratch.write("auto_pull = false\n");
        assert!(!Config::read(&path).unwrap().auto_pull);
    }

    #[test]
    fn ssh_configuration_entries_are_off_by_default_and_can_be_turned_on() {
        assert!(!Config::default().add_ssh_config);
        let scratch = Scratch::new("sshconfig");
        let path = scratch.write("add_ssh_config = true\n");
        let config = Config::read(&path).unwrap();
        assert!(config.add_ssh_config);
        assert!(config.auto_pull);
    }

    #[test]
    fn the_default_user_is_vm_unless_configured() {
        assert_eq!(Config::default().user().unwrap(), "vm");
        let scratch = Scratch::new("defaultuser");
        let path = scratch.write("default_user = \"dave\"\n");
        let config = Config::read(&path).unwrap();
        assert_eq!(config.default_user.as_deref(), Some("dave"));
        assert_eq!(config.user().unwrap(), "dave");
    }

    #[test]
    fn the_current_user_is_used_when_asked_for() {
        let scratch = Scratch::new("currentuser");
        let path = scratch.write("default_user = \"$USER\"\n");
        let config = Config::read(&path).unwrap();
        assert_eq!(
            config.user_with(|| Ok("alice".to_owned())).unwrap(),
            "alice"
        );
    }

    #[test]
    fn the_current_user_is_not_consulted_otherwise() {
        let config = Config {
            default_user: Some("dave".to_owned()),
            ..Config::default()
        };
        let user = config.user_with(|| panic!("consulted")).unwrap();
        assert_eq!(user, "dave");
        assert_eq!(
            Config::default().user_with(|| panic!("consulted")).unwrap(),
            "vm"
        );
    }

    #[test]
    fn an_unusable_configured_user_is_refused() {
        let config = Config {
            default_user: Some("Has Space".to_owned()),
            ..Config::default()
        };
        let error = config.user().unwrap_err();
        assert_eq!(error.kind(), "unusable-default-user");
        assert!(error.to_string().contains("Has Space"), "{error}");
    }

    #[test]
    fn an_unusable_current_user_is_refused_and_says_where_it_came_from() {
        let config = Config {
            default_user: Some(CURRENT_USER.to_owned()),
            ..Config::default()
        };
        let error = config
            .user_with(|| Ok("Dave.Minter".to_owned()))
            .unwrap_err();
        assert_eq!(error.kind(), "unusable-default-user");
        assert!(error.to_string().contains("running vm"), "{error}");
    }

    #[test]
    fn a_failure_to_find_the_current_user_is_reported() {
        let config = Config {
            default_user: Some(CURRENT_USER.to_owned()),
            ..Config::default()
        };
        let error = config
            .user_with(|| {
                Err(Error::NoCurrentUser {
                    reason: "no such user".to_owned(),
                })
            })
            .unwrap_err();
        assert_eq!(error.kind(), "no-current-user");
    }

    #[test]
    fn the_user_running_the_tests_has_a_login_name() {
        let name = current_user().unwrap();
        assert!(!name.is_empty());
        assert!(!name.contains('\n'));
    }

    #[test]
    fn unparseable_toml_is_reported() {
        let scratch = Scratch::new("broken");
        let path = scratch.write("this is not toml");
        assert!(Config::read(&path).is_err());
    }
}

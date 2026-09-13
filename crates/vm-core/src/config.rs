use crate::error::{Error, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;

const DEFAULT_CATALOGUE_URL: &str =
    "https://codeload.github.com/dcminter/vm-manager/tar.gz/refs/heads/main";
const DEFAULT_CATALOGUE_PATH: &str = "catalogue";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Where `vm update` fetches the catalogue from.
    #[serde(default = "default_catalogue_url")]
    pub catalogue_url: String,
    /// The directory inside that archive holding the entries.
    #[serde(default = "default_catalogue_path")]
    pub catalogue_path: String,
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

const fn default_auto_pull() -> bool {
    true
}

fn default_catalogue_url() -> String {
    DEFAULT_CATALOGUE_URL.to_owned()
}

fn default_catalogue_path() -> String {
    DEFAULT_CATALOGUE_PATH.to_owned()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            catalogue_url: default_catalogue_url(),
            catalogue_path: default_catalogue_path(),
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
        match fs::read_to_string(path) {
            Ok(text) => basic_toml::from_str(&text).map_err(|source| Error::CatalogueParse {
                path: path.to_owned(),
                source,
            }),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(Error::CatalogueRead {
                path: path.to_owned(),
                source,
            }),
        }
    }
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
        assert!(config.catalogue_url.starts_with("https://"));
    }

    #[test]
    fn a_setting_overrides_only_itself() {
        let scratch = Scratch::new("partial");
        let path = scratch.write(r#"catalogue_url = "https://example.test/c.tar.gz""#);
        let config = Config::read(&path).unwrap();
        assert_eq!(config.catalogue_url, "https://example.test/c.tar.gz");
        assert_eq!(config.catalogue_path, DEFAULT_CATALOGUE_PATH);
    }

    #[test]
    fn an_empty_file_yields_the_defaults() {
        let scratch = Scratch::new("empty");
        let path = scratch.write("");
        assert_eq!(Config::read(&path).unwrap(), Config::default());
    }

    #[test]
    fn both_settings_can_be_given() {
        let scratch = Scratch::new("both");
        let path = scratch.write(
            "catalogue_url = \"https://example.test/c.tar.gz\"\ncatalogue_path = \"images\"\n",
        );
        let config = Config::read(&path).unwrap();
        assert_eq!(config.catalogue_path, "images");
    }

    #[test]
    fn an_unknown_setting_is_reported_rather_than_ignored() {
        let scratch = Scratch::new("unknown");
        let path = scratch.write("catalogue_yurl = \"typo\"\n");
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

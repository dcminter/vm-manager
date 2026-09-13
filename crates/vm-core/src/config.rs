use crate::error::{Error, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

/// The pointer to the project's own catalogue.
pub const PROJECT_CATALOGUE_URL: &str = "https://vm-manager.com/catalogue.toml";
const DEFAULT_CATALOGUE_PATH: &str = "catalogue";

/// The name of the project's catalogue when no remote is configured.
pub const PROJECT_CATALOGUE: &str = "project";

/// The name of the local catalogue `vm clone` and `vm import` write to.
pub const STORE_CATALOGUE: &str = "store";

/// A catalogue fetched by `vm update`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Remote {
    pub name: String,
    /// A pointer file naming the current gzipped tar archive.
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
        Ok(Settings::read(path)?.unwrap_or_default().config())
    }

    /// Where the user's config file is, whether or not it exists.
    pub fn path() -> Option<PathBuf> {
        crate::paths::config_directory().map(|directory| directory.join("config.toml"))
    }
}

/// A setting that holds one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    AutoPull,
    AddSshConfig,
    DefaultUser,
}

impl Key {
    pub const ALL: [Self; 3] = [Self::AutoPull, Self::AddSshConfig, Self::DefaultUser];

    pub const fn name(self) -> &'static str {
        match self {
            Self::AutoPull => "auto_pull",
            Self::AddSshConfig => "add_ssh_config",
            Self::DefaultUser => "default_user",
        }
    }
}

/// The config file as written, where an absent setting takes its default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub auto_pull: Option<bool>,
    pub add_ssh_config: Option<bool>,
    pub default_user: Option<String>,
    #[serde(default, rename = "remote")]
    pub remotes: Vec<Remote>,
    #[serde(default, rename = "local")]
    pub locals: Vec<Local>,
}

impl Settings {
    /// Reads a config file, or `None` if there is none.
    pub fn read(path: &Path) -> Result<Option<Self>> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, path).map(Some),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(Error::CatalogueRead {
                path: path.to_owned(),
                source,
            }),
        }
    }

    fn parse(text: &str, path: &Path) -> Result<Self> {
        let invalid = |reason: String| Error::Config {
            path: path.to_owned(),
            reason,
        };
        if let Some(setting) = obsolete_setting(text) {
            return Err(invalid(format!(
                "{setting} is no longer a setting; name each catalogue in a [[remote]] table \
                 with name, url and path"
            )));
        }
        let settings: Self =
            basic_toml::from_str(text).map_err(|source| invalid(source.to_string()))?;
        settings.check().map_err(invalid)?;
        Ok(settings)
    }

    /// The configuration these settings make, defaults filled in.
    pub fn config(&self) -> Config {
        Config {
            remotes: self.remotes.clone(),
            locals: self.locals.clone(),
            auto_pull: self.auto_pull.unwrap_or_else(default_auto_pull),
            add_ssh_config: self.add_ssh_config.unwrap_or(false),
            default_user: self.default_user.clone(),
        }
    }

    /// Whether a setting is given rather than defaulted.
    pub const fn is_set(&self, key: Key) -> bool {
        match key {
            Key::AutoPull => self.auto_pull.is_some(),
            Key::AddSshConfig => self.add_ssh_config.is_some(),
            Key::DefaultUser => self.default_user.is_some(),
        }
    }

    /// Refuses catalogues with unusable or repeated names, relative paths or unfetchable URLs.
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
            if *name == STORE_CATALOGUE {
                return Err(format!(
                    "'{name}' is reserved for images made by vm clone and vm import"
                ));
            }
            if names[..index].contains(name) {
                return Err(format!("the catalogue name '{name}' is used twice"));
            }
        }
        if let Some(remote) = self
            .remotes
            .iter()
            .find(|remote| !is_fetchable(&remote.url))
        {
            return Err(format!(
                "the url of remote catalogue '{}' must start with http:// or https://",
                remote.name
            ));
        }
        if let Some(local) = self.locals.iter().find(|local| !local.path.is_absolute()) {
            return Err(format!(
                "the path of local catalogue '{}' must be absolute",
                local.name
            ));
        }
        if let Some(local) = self
            .locals
            .iter()
            .find(|local| local.path.to_str().is_none())
        {
            return Err(format!(
                "the path of local catalogue '{}' is not valid UTF-8",
                local.name
            ));
        }
        Ok(())
    }

    /// Sets a setting from its text form.
    pub fn set(&mut self, key: Key, value: &str) -> std::result::Result<(), String> {
        let boolean = || match value {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(format!("{} is true or false, not '{value}'", key.name())),
        };
        match key {
            Key::AutoPull => self.auto_pull = Some(boolean()?),
            Key::AddSshConfig => self.add_ssh_config = Some(boolean()?),
            Key::DefaultUser => {
                if value != CURRENT_USER && !crate::seed::is_username(value) {
                    return Err(format!(
                        "'{value}' is not a usable account name; give a lowercase name or {CURRENT_USER}"
                    ));
                }
                self.default_user = Some(value.to_owned());
            }
        }
        Ok(())
    }

    /// Removes a setting so it takes its default, returning whether it was set.
    pub fn unset(&mut self, key: Key) -> bool {
        let was = self.is_set(key);
        match key {
            Key::AutoPull => self.auto_pull = None,
            Key::AddSshConfig => self.add_ssh_config = None,
            Key::DefaultUser => self.default_user = None,
        }
        was
    }

    /// Adds a remote catalogue last or before the one named, keeping the project's first if none was listed.
    pub fn add_remote(
        &mut self,
        remote: Remote,
        before: Option<&str>,
    ) -> std::result::Result<(), String> {
        if self.remotes.is_empty() && remote.name != PROJECT_CATALOGUE {
            self.remotes = Config::default().remotes();
        }
        let index = position(before, self.remotes.iter().map(|held| &held.name), "remote")?;
        self.remotes.insert(index, remote);
        self.check()
    }

    /// Adds a local catalogue last, or before the one named.
    pub fn add_local(
        &mut self,
        local: Local,
        before: Option<&str>,
    ) -> std::result::Result<(), String> {
        if !local.path.is_dir() {
            return Err(format!("{} is not a directory", local.path.display()));
        }
        let index = position(before, self.locals.iter().map(|held| &held.name), "local")?;
        self.locals.insert(index, local);
        self.check()
    }

    /// Removes the remote catalogue named, returning `false` if there is none.
    pub fn remove_remote(&mut self, name: &str) -> bool {
        let count = self.remotes.len();
        self.remotes.retain(|remote| remote.name != name);
        self.remotes.len() < count
    }

    /// Removes the local catalogue named, returning `false` if there is none.
    pub fn remove_local(&mut self, name: &str) -> bool {
        let count = self.locals.len();
        self.locals.retain(|local| local.name != name);
        self.locals.len() < count
    }

    pub fn to_toml(&self) -> String {
        use crate::value::toml_string;
        use std::fmt::Write as _;
        let mut text = String::new();
        if let Some(value) = self.auto_pull {
            let _ = writeln!(text, "auto_pull = {value}");
        }
        if let Some(value) = self.add_ssh_config {
            let _ = writeln!(text, "add_ssh_config = {value}");
        }
        if let Some(value) = &self.default_user {
            let _ = writeln!(text, "default_user = {}", toml_string(value));
        }
        for remote in &self.remotes {
            let _ = write!(
                text,
                "\n[[remote]]\nname = {}\nurl = {}\n",
                toml_string(&remote.name),
                toml_string(&remote.url)
            );
            if remote.path != DEFAULT_CATALOGUE_PATH {
                let _ = writeln!(text, "path = {}", toml_string(&remote.path));
            }
        }
        for local in &self.locals {
            let _ = write!(
                text,
                "\n[[local]]\nname = {}\npath = {}\n",
                toml_string(&local.name),
                toml_string(&local.path.to_string_lossy())
            );
        }
        text.trim_start_matches('\n').to_owned()
    }

    /// Writes the settings through a temporary file, creating the directory if needed.
    pub fn write(&self, path: &Path) -> Result<()> {
        let text = self.to_toml();
        Self::parse(&text, path)?;
        let store = |target: &Path, action: &'static str, source| Error::Store {
            path: target.to_owned(),
            action,
            source,
        };
        if let Some(directory) = path.parent() {
            fs::create_dir_all(directory).map_err(|source| store(directory, "create", source))?;
        }
        let staging = path.with_extension("toml.new");
        fs::write(&staging, text).map_err(|source| store(&staging, "write", source))?;
        fs::rename(&staging, path).map_err(|source| store(path, "replace", source))
    }
}

/// Where to insert into a list: before the entry named, or at the end.
fn position<'a>(
    before: Option<&str>,
    names: impl Iterator<Item = &'a String>,
    kind: &str,
) -> std::result::Result<usize, String> {
    let names: Vec<&String> = names.collect();
    before.map_or(Ok(names.len()), |wanted| {
        names
            .iter()
            .position(|name| *name == wanted)
            .ok_or_else(|| format!("there is no {kind} catalogue named '{wanted}' to add before"))
    })
}

/// Whether a URL names something `vm update` can fetch.
pub(crate) fn is_fetchable(url: &str) -> bool {
    ["http://", "https://"].iter().any(|scheme| {
        url.strip_prefix(scheme)
            .is_some_and(|rest| !rest.is_empty())
    })
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
            ("[[local]]\nname = \"store\"\npath = \"/x\"\n", "reserved"),
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

    fn remote(name: &str) -> Remote {
        Remote {
            name: name.to_owned(),
            url: format!("https://{name}.test/c.tar.gz"),
            path: DEFAULT_CATALOGUE_PATH.to_owned(),
        }
    }

    fn remote_names(settings: &Settings) -> Vec<&str> {
        settings
            .remotes
            .iter()
            .map(|held| held.name.as_str())
            .collect()
    }

    #[test]
    fn an_absent_file_has_no_settings() {
        assert_eq!(
            Settings::read(Path::new("/nonexistent/vm/config.toml")).unwrap(),
            None
        );
    }

    #[test]
    fn settings_distinguish_what_is_given_from_what_is_defaulted() {
        let scratch = Scratch::new("given");
        let settings = Settings::read(&scratch.write("auto_pull = true\n"))
            .unwrap()
            .unwrap();
        assert!(settings.is_set(Key::AutoPull));
        assert!(!settings.is_set(Key::AddSshConfig));
        assert_eq!(settings.config(), Config::default());
    }

    #[test]
    fn a_setting_is_set_from_text_only_when_valid() {
        let mut settings = Settings::default();
        settings.set(Key::AutoPull, "false").unwrap();
        settings.set(Key::DefaultUser, "$USER").unwrap();
        settings.set(Key::DefaultUser, "dave").unwrap();
        assert_eq!(settings.auto_pull, Some(false));
        assert_eq!(settings.default_user.as_deref(), Some("dave"));
        assert!(
            settings
                .set(Key::AddSshConfig, "yes")
                .unwrap_err()
                .contains("true or false")
        );
        assert!(settings.set(Key::DefaultUser, "Bad Name").is_err());
        assert!(!settings.is_set(Key::AddSshConfig));
        assert_eq!(settings.default_user.as_deref(), Some("dave"));
    }

    #[test]
    fn unsetting_restores_the_default_and_says_whether_anything_changed() {
        let mut settings = Settings::default();
        settings.set(Key::AutoPull, "false").unwrap();
        assert!(settings.unset(Key::AutoPull));
        assert!(!settings.unset(Key::AutoPull));
        assert!(settings.config().auto_pull);
    }

    #[test]
    fn the_first_remote_added_keeps_the_project_catalogue_ahead_of_it() {
        let mut settings = Settings::default();
        settings.add_remote(remote("internal"), None).unwrap();
        assert_eq!(remote_names(&settings), [PROJECT_CATALOGUE, "internal"]);
        settings
            .add_remote(remote("mirror"), Some("internal"))
            .unwrap();
        assert_eq!(
            remote_names(&settings),
            [PROJECT_CATALOGUE, "mirror", "internal"]
        );
        assert!(settings.remove_remote(PROJECT_CATALOGUE));
        assert_eq!(remote_names(&settings), ["mirror", "internal"]);
        assert!(!settings.remove_remote("absent"));
    }

    #[test]
    fn a_first_remote_named_project_replaces_the_default_rather_than_repeating_it() {
        let mut settings = Settings::default();
        settings
            .add_remote(remote(PROJECT_CATALOGUE), None)
            .unwrap();
        assert_eq!(remote_names(&settings), [PROJECT_CATALOGUE]);
        assert_eq!(settings.remotes[0].url, "https://project.test/c.tar.gz");
    }

    #[test]
    fn an_invalid_catalogue_is_not_added() {
        let mut settings = Settings::default();
        settings.add_remote(remote("internal"), None).unwrap();
        let before = settings.clone();
        let mut attempt = settings.clone();
        assert!(
            attempt
                .add_remote(remote("internal"), None)
                .unwrap_err()
                .contains("twice")
        );
        let mut attempt = settings.clone();
        assert!(
            attempt
                .add_remote(remote("x"), Some("absent"))
                .unwrap_err()
                .contains("absent")
        );
        let mut attempt = settings.clone();
        let mut ftp = remote("ftp");
        ftp.url = "ftp://example.test/c.tar.gz".to_owned();
        assert!(attempt.add_remote(ftp, None).unwrap_err().contains("http"));
        let mut attempt = settings.clone();
        let missing = Local {
            name: "team".to_owned(),
            path: PathBuf::from("/nonexistent/vm/team"),
        };
        assert!(
            attempt
                .add_local(missing, None)
                .unwrap_err()
                .contains("not a directory")
        );
        assert_eq!(settings, before);
    }

    #[test]
    fn locals_are_added_in_order_and_removed_by_name() {
        let scratch = Scratch::new("locals");
        let local = |name: &str| Local {
            name: name.to_owned(),
            path: scratch.0.clone(),
        };
        let mut settings = Settings::default();
        settings.add_local(local("team"), None).unwrap();
        settings.add_local(local("mine"), Some("team")).unwrap();
        let names: Vec<&str> = settings
            .locals
            .iter()
            .map(|held| held.name.as_str())
            .collect();
        assert_eq!(names, ["mine", "team"]);
        assert!(settings.remotes.is_empty());
        assert!(settings.remove_local("mine"));
        assert!(!settings.remove_local("mine"));
    }

    #[test]
    fn written_settings_read_back_the_same() {
        let scratch = Scratch::new("roundtrip");
        let mut settings = Settings::default();
        settings.set(Key::AddSshConfig, "true").unwrap();
        settings.set(Key::DefaultUser, "$USER").unwrap();
        let mut internal = remote("internal");
        internal.path = "images".to_owned();
        settings.add_remote(internal, None).unwrap();
        settings
            .add_local(
                Local {
                    name: "team".to_owned(),
                    path: scratch.0.clone(),
                },
                None,
            )
            .unwrap();
        let path = scratch.0.join("nested/config.toml");
        settings.write(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("add_ssh_config = true\n"), "{text}");
        assert!(text.contains("path = \"images\""), "{text}");
        assert_eq!(Settings::read(&path).unwrap().unwrap(), settings);
        assert!(!scratch.0.join("nested/config.toml.new").exists());
    }

    #[test]
    fn empty_settings_write_an_empty_file() {
        assert_eq!(Settings::default().to_toml(), "");
    }
}

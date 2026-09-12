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
    /// Whether `vm run` fetches an image it does not hold. Turning this off
    /// makes a missing image an error rather than a download.
    #[serde(default = "default_auto_pull")]
    pub auto_pull: bool,
}

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
        }
    }
}

impl Config {
    /// Reads the user's configuration. An absent file is the default, not an
    /// error; an unreadable or invalid one is reported rather than ignored.
    pub fn load() -> Result<Self> {
        crate::paths::config_directory().map_or_else(
            || Ok(Self::default()),
            |directory| Self::read(&directory.join("config.toml")),
        )
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
    fn unparseable_toml_is_reported() {
        let scratch = Scratch::new("broken");
        let path = scratch.write("this is not toml");
        assert!(Config::read(&path).is_err());
    }
}

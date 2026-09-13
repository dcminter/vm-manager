use crate::catalogue::{Kind, Source};
use crate::config::{Config, PROJECT_CATALOGUE_URL, STORE_CATALOGUE};
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Replaces every remote catalogue with one directory.
pub const CATALOGUE_ENV: &str = "VM_CATALOGUE";

/// The name of the catalogue [`CATALOGUE_ENV`] supplies.
pub const OVERRIDE_CATALOGUE: &str = "override";

const PACKAGED_CATALOGUE: &str = "/usr/share/vm/catalogue";

/// Environment lookup, so paths are testable without changing the process environment.
pub trait Environment {
    fn var(&self, name: &str) -> Option<OsString>;
}

pub struct SystemEnvironment;

impl Environment for SystemEnvironment {
    fn var(&self, name: &str) -> Option<OsString> {
        env::var_os(name)
    }
}

fn xdg(environment: &impl Environment, variable: &str, fallback: &str) -> Option<PathBuf> {
    environment
        .var(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            environment
                .var("HOME")
                .map(|home| PathBuf::from(home).join(fallback))
        })
}

pub fn data_directory_in(environment: &impl Environment) -> Option<PathBuf> {
    xdg(environment, "XDG_DATA_HOME", ".local/share").map(|path| path.join("vm"))
}

pub fn state_directory_in(environment: &impl Environment) -> Option<PathBuf> {
    xdg(environment, "XDG_STATE_HOME", ".local/state").map(|path| path.join("vm"))
}

pub fn config_directory_in(environment: &impl Environment) -> Option<PathBuf> {
    xdg(environment, "XDG_CONFIG_HOME", ".config").map(|path| path.join("vm"))
}

pub fn images_directory_in(environment: &impl Environment) -> Option<PathBuf> {
    data_directory_in(environment).map(|path| path.join("images"))
}

/// Where sockets live, short enough for the kernel's socket path limit.
pub fn runtime_directory_in(environment: &impl Environment) -> PathBuf {
    environment
        .var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map_or_else(
            || PathBuf::from(format!("/tmp/vm-{}", user_id())),
            |path| path.join("vm"),
        )
}

pub fn runtime_directory() -> PathBuf {
    runtime_directory_in(&SystemEnvironment)
}

/// The user id, read from `/proc` to avoid unsafe code.
fn user_id() -> u32 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata("/proc/self").map_or(0, |data| data.uid())
}

/// Where `vm clone` and `vm import` write catalogue entries.
pub fn local_catalogue_directory_in(environment: &impl Environment) -> Option<PathBuf> {
    data_directory_in(environment).map(|path| path.join("local"))
}

pub fn local_catalogue_directory() -> Option<PathBuf> {
    local_catalogue_directory_in(&SystemEnvironment)
}

pub fn instances_directory_in(environment: &impl Environment) -> Option<PathBuf> {
    state_directory_in(environment).map(|path| path.join("instances"))
}

pub fn data_directory() -> Option<PathBuf> {
    data_directory_in(&SystemEnvironment)
}

pub fn state_directory() -> Option<PathBuf> {
    state_directory_in(&SystemEnvironment)
}

pub fn config_directory() -> Option<PathBuf> {
    config_directory_in(&SystemEnvironment)
}

pub fn images_directory() -> Option<PathBuf> {
    images_directory_in(&SystemEnvironment)
}

pub fn instances_directory() -> Option<PathBuf> {
    instances_directory_in(&SystemEnvironment)
}

/// The user's own SSH configuration, where `ssh` looks for it.
pub fn ssh_config_in(environment: &impl Environment) -> Option<PathBuf> {
    environment
        .var("HOME")
        .map(|home| PathBuf::from(home).join(".ssh").join("config"))
}

pub fn ssh_config() -> Option<PathBuf> {
    ssh_config_in(&SystemEnvironment)
}

/// Where `vm update` keeps a remote catalogue.
pub fn remote_catalogue_directory_in(
    environment: &impl Environment,
    name: &str,
) -> Option<PathBuf> {
    data_directory_in(environment).map(|path| path.join("catalogues").join(name))
}

pub fn remote_catalogue_directory(name: &str) -> Option<PathBuf> {
    remote_catalogue_directory_in(&SystemEnvironment, name)
}

/// Every catalogue, lowest precedence first: remotes, configured locals, then the store catalogue.
pub fn catalogue_sources(config: &Config) -> Vec<Source> {
    catalogue_sources_in(&SystemEnvironment, config, &|path| path.is_dir())
}

fn catalogue_sources_in(
    environment: &impl Environment,
    config: &Config,
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<Source> {
    let mut sources = environment.var(CATALOGUE_ENV).map_or_else(
        || remote_sources(environment, config, exists),
        |path| {
            vec![Source {
                name: OVERRIDE_CATALOGUE.to_owned(),
                kind: Kind::Remote,
                directory: PathBuf::from(path),
            }]
        },
    );
    sources.extend(config.locals.iter().map(|local| Source {
        name: local.name.clone(),
        kind: Kind::Local,
        directory: local.path.clone(),
    }));
    sources.extend(
        local_catalogue_directory_in(environment).map(|directory| Source {
            name: STORE_CATALOGUE.to_owned(),
            kind: Kind::Local,
            directory,
        }),
    );
    sources
}

/// The configured remotes, each where `vm update` keeps it.
fn remote_sources(
    environment: &impl Environment,
    config: &Config,
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<Source> {
    config
        .remotes()
        .into_iter()
        .map(|remote| {
            let fetched = remote_catalogue_directory_in(environment, &remote.name);
            let directory = match fetched {
                Some(fetched) if exists(&fetched) => fetched,
                _ if remote.url == PROJECT_CATALOGUE_URL => bundled_catalogue(exists),
                fetched => fetched.unwrap_or_default(),
            };
            Source {
                name: remote.name,
                kind: Kind::Remote,
                directory,
            }
        })
        .collect()
}

/// The project catalogue shipped in the package, or beside a development build.
fn bundled_catalogue(exists: &dyn Fn(&Path) -> bool) -> PathBuf {
    let packaged = PathBuf::from(PACKAGED_CATALOGUE);
    if exists(&packaged) {
        return packaged;
    }
    development_catalogue(exists).unwrap_or(packaged)
}

/// Finds the catalogue beside a binary run out of a build directory.
fn development_catalogue(exists: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut directory = env::current_exe().ok()?;
    while directory.pop() {
        let candidate = directory.join("catalogue");
        if exists(&candidate.join("debian")) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::config::{Local, Remote};

    struct Fixed(&'static [(&'static str, &'static str)]);

    impl Environment for Fixed {
        fn var(&self, name: &str) -> Option<OsString> {
            self.0
                .iter()
                .find(|(held, _)| *held == name)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    const fn environment(pairs: &'static [(&'static str, &'static str)]) -> Fixed {
        Fixed(pairs)
    }

    fn nothing_exists(_: &Path) -> bool {
        false
    }

    #[test]
    fn xdg_variables_win_over_the_home_fallback() {
        let held = environment(&[("XDG_DATA_HOME", "/xdg/data"), ("HOME", "/home/x")]);
        assert_eq!(
            data_directory_in(&held).unwrap(),
            PathBuf::from("/xdg/data/vm")
        );
    }

    #[test]
    fn a_relative_xdg_variable_is_ignored_as_the_specification_requires() {
        let held = environment(&[("XDG_STATE_HOME", "relative/path"), ("HOME", "/home/x")]);
        assert_eq!(
            state_directory_in(&held).unwrap(),
            PathBuf::from("/home/x/.local/state/vm")
        );
    }

    #[test]
    fn the_home_fallback_supplies_the_conventional_paths() {
        let held = environment(&[("HOME", "/home/x")]);
        assert_eq!(
            images_directory_in(&held).unwrap(),
            PathBuf::from("/home/x/.local/share/vm/images")
        );
        assert_eq!(
            instances_directory_in(&held).unwrap(),
            PathBuf::from("/home/x/.local/state/vm/instances")
        );
        assert_eq!(
            config_directory_in(&held).unwrap(),
            PathBuf::from("/home/x/.config/vm")
        );
        assert_eq!(
            ssh_config_in(&held).unwrap(),
            PathBuf::from("/home/x/.ssh/config")
        );
    }

    #[test]
    fn without_home_or_xdg_there_is_no_path_to_offer() {
        let held = environment(&[]);
        assert!(data_directory_in(&held).is_none());
        assert!(instances_directory_in(&held).is_none());
        assert!(ssh_config_in(&held).is_none());
    }

    fn names(sources: &[Source]) -> Vec<(&str, Kind, &Path)> {
        sources
            .iter()
            .map(|source| {
                (
                    source.name.as_str(),
                    source.kind,
                    source.directory.as_path(),
                )
            })
            .collect()
    }

    fn configured() -> Config {
        Config {
            remotes: vec![
                Remote {
                    name: "public".to_owned(),
                    url: PROJECT_CATALOGUE_URL.to_owned(),
                    path: "catalogue".to_owned(),
                },
                Remote {
                    name: "internal".to_owned(),
                    url: "https://example.test/c.tar.gz".to_owned(),
                    path: "catalogue".to_owned(),
                },
            ],
            locals: vec![Local {
                name: "team".to_owned(),
                path: PathBuf::from("/srv/team"),
            }],
            ..Config::default()
        }
    }

    #[test]
    fn catalogues_are_ordered_remotes_then_locals_then_the_store() {
        let held = environment(&[("HOME", "/home/x")]);
        let sources = catalogue_sources_in(&held, &configured(), &|_| true);
        assert_eq!(
            names(&sources),
            [
                (
                    "public",
                    Kind::Remote,
                    Path::new("/home/x/.local/share/vm/catalogues/public")
                ),
                (
                    "internal",
                    Kind::Remote,
                    Path::new("/home/x/.local/share/vm/catalogues/internal")
                ),
                ("team", Kind::Local, Path::new("/srv/team")),
                (
                    "store",
                    Kind::Local,
                    Path::new("/home/x/.local/share/vm/local")
                ),
            ]
        );
    }

    #[test]
    fn without_configured_remotes_the_project_catalogue_is_used() {
        let held = environment(&[("HOME", "/home/x")]);
        let sources = catalogue_sources_in(&held, &Config::default(), &|_| true);
        assert_eq!(
            names(&sources)[0],
            (
                "project",
                Kind::Remote,
                Path::new("/home/x/.local/share/vm/catalogues/project")
            )
        );
        assert_eq!(sources.len(), 2);
    }

    #[test]
    fn an_unfetched_project_catalogue_falls_back_to_the_packaged_one() {
        let held = environment(&[("HOME", "/home/x")]);
        let sources = catalogue_sources_in(&held, &configured(), &|path| {
            path == Path::new(PACKAGED_CATALOGUE)
        });
        assert_eq!(sources[0].directory, PathBuf::from(PACKAGED_CATALOGUE));
        assert_eq!(
            sources[1].directory,
            PathBuf::from("/home/x/.local/share/vm/catalogues/internal")
        );
    }

    #[test]
    fn the_packaged_path_is_the_answer_of_last_resort() {
        let held = environment(&[("HOME", "/home/x")]);
        let sources = catalogue_sources_in(&held, &Config::default(), &nothing_exists);
        assert_eq!(sources[0].directory, PathBuf::from(PACKAGED_CATALOGUE));
    }

    #[test]
    fn the_catalogue_override_replaces_every_remote() {
        let held = environment(&[(CATALOGUE_ENV, "/override"), ("HOME", "/home/x")]);
        let sources = catalogue_sources_in(&held, &configured(), &|_| true);
        assert_eq!(
            names(&sources),
            [
                ("override", Kind::Remote, Path::new("/override")),
                ("team", Kind::Local, Path::new("/srv/team")),
                (
                    "store",
                    Kind::Local,
                    Path::new("/home/x/.local/share/vm/local")
                ),
            ]
        );
    }

    #[test]
    fn the_runtime_directory_follows_its_variable() {
        let held = environment(&[("XDG_RUNTIME_DIR", "/run/user/1000")]);
        assert_eq!(
            runtime_directory_in(&held),
            PathBuf::from("/run/user/1000/vm")
        );
    }

    #[test]
    fn a_missing_runtime_variable_falls_back_to_a_private_directory() {
        let held = environment(&[("HOME", "/home/x")]);
        let path = runtime_directory_in(&held);
        assert!(
            path.to_string_lossy().starts_with("/tmp/vm-"),
            "{}",
            path.display()
        );
    }

    #[test]
    fn a_relative_runtime_variable_is_ignored() {
        let held = environment(&[("XDG_RUNTIME_DIR", "run/user/1000")]);
        assert!(
            runtime_directory_in(&held)
                .to_string_lossy()
                .starts_with("/tmp/vm-")
        );
    }

    #[test]
    fn the_runtime_directory_leaves_room_for_a_socket_name() {
        for held in [
            environment(&[("XDG_RUNTIME_DIR", "/run/user/1000")]),
            environment(&[("HOME", "/home/somebody-with-a-long-name")]),
        ] {
            let path = runtime_directory_in(&held);
            assert!(
                path.as_os_str().len() + "/0123456789abcdef.sock".len()
                    <= crate::qmp::MAX_SOCKET_PATH,
                "{} is already too long",
                path.display()
            );
        }
    }
}

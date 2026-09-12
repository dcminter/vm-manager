use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Overrides the catalogue location; set by tests and by anyone pointing the
/// tool at a catalogue of their own.
pub const CATALOGUE_ENV: &str = "VM_CATALOGUE";

const PACKAGED_CATALOGUE: &str = "/usr/share/vm/catalogue";

/// Environment lookup, abstracted so paths are testable without mutating the
/// process environment, which is shared and unsafe to change.
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

/// The catalogue to read: an explicit override, then the copy `vm update`
/// fetches, then the copy shipped with the package, then a checkout's own.
pub fn catalogue_directory() -> PathBuf {
    catalogue_directory_in(&SystemEnvironment, &|path| path.is_dir())
}

fn catalogue_directory_in(
    environment: &impl Environment,
    exists: &dyn Fn(&Path) -> bool,
) -> PathBuf {
    if let Some(path) = environment.var(CATALOGUE_ENV) {
        return PathBuf::from(path);
    }
    if let Some(fetched) = data_directory_in(environment).map(|path| path.join("catalogue"))
        && exists(&fetched)
    {
        return fetched;
    }
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
    }

    #[test]
    fn without_home_or_xdg_there_is_no_path_to_offer() {
        let held = environment(&[]);
        assert!(data_directory_in(&held).is_none());
        assert!(instances_directory_in(&held).is_none());
    }

    #[test]
    fn the_catalogue_override_is_taken_verbatim() {
        let held = environment(&[(CATALOGUE_ENV, "/tmp/some/catalogue")]);
        assert_eq!(
            catalogue_directory_in(&held, &nothing_exists),
            PathBuf::from("/tmp/some/catalogue")
        );
    }

    #[test]
    fn the_override_beats_a_fetched_catalogue() {
        let held = environment(&[(CATALOGUE_ENV, "/override"), ("HOME", "/home/x")]);
        assert_eq!(
            catalogue_directory_in(&held, &|_| true),
            PathBuf::from("/override")
        );
    }

    #[test]
    fn a_fetched_catalogue_beats_the_packaged_one() {
        let held = environment(&[("HOME", "/home/x")]);
        let fetched = PathBuf::from("/home/x/.local/share/vm/catalogue");
        let found = catalogue_directory_in(&held, &|path| path == fetched);
        assert_eq!(found, fetched);
    }

    #[test]
    fn the_packaged_catalogue_is_used_when_nothing_has_been_fetched() {
        let held = environment(&[("HOME", "/home/x")]);
        let found = catalogue_directory_in(&held, &|path| path == Path::new(PACKAGED_CATALOGUE));
        assert_eq!(found, PathBuf::from(PACKAGED_CATALOGUE));
    }

    #[test]
    fn the_packaged_path_is_the_answer_of_last_resort() {
        let held = environment(&[("HOME", "/home/x")]);
        assert_eq!(
            catalogue_directory_in(&held, &nothing_exists),
            PathBuf::from(PACKAGED_CATALOGUE)
        );
    }
}

//! Per-machine SSH config entries, read through one `Include` line in the user's config.

use crate::access;
use crate::error::{Error, Result};
use crate::instance::{Directory, Instance};
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

/// What is written above the `Include` line, so its origin is not a mystery.
const INCLUDE_COMMENT: &str = "# Added by vm, to reach machines run with --add-ssh-config by name.";

/// The entry for one machine, or nothing when it has no port to reach.
pub fn entry(instance: &Instance, directory: &Directory) -> Option<String> {
    let port = instance.ssh_port.filter(|_| instance.seeded)?;
    let mut text = format!(
        "# Written by vm, rewritten when the machine starts and removed with it.\nHost {}\n    HostName 127.0.0.1\n",
        instance.name
    );
    for (key, value) in access::settings(instance, directory, port) {
        let _ = writeln!(text, "    {key} {}", quote(&value));
    }
    Some(text)
}

/// The line that reads every machine's entry.
pub fn include_line(instances: &Path) -> String {
    format!(
        "Include {}",
        quote(&instances.join("*").join("ssh_config").display().to_string())
    )
}

/// SSH splits values on whitespace, so a path holding a space has to be quoted.
fn quote(value: &str) -> String {
    if value.contains(char::is_whitespace) || value.contains('#') {
        format!("\"{value}\"")
    } else {
        value.to_owned()
    }
}

/// A config line's keyword and arguments, in `Key value` or `Key=value` form.
fn words(line: &str) -> Option<(&str, Vec<&str>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let at = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let (keyword, rest) = line.split_at(at);
    let rest = rest.trim_start().strip_prefix('=').unwrap_or(rest);
    Some((
        keyword,
        rest.split_whitespace()
            .map(|word| word.trim_matches('"'))
            .collect(),
    ))
}

/// Whether the line appears before any `Host` or `Match`, where it applies globally.
fn includes(text: &str, line: &str) -> bool {
    for held in text.lines() {
        if held.trim() == line {
            return true;
        }
        if let Some((keyword, _)) = words(held)
            && (keyword.eq_ignore_ascii_case("host") || keyword.eq_ignore_ascii_case("match"))
        {
            return false;
        }
    }
    false
}

/// The config with the `Include` line prepended, or `None` if already present.
pub fn with_include(text: &str, line: &str) -> Option<String> {
    if includes(text, line) {
        return None;
    }
    let separator = if text.is_empty() { "" } else { "\n" };
    Some(format!("{INCLUDE_COMMENT}\n{line}\n{separator}{text}"))
}

/// Whether a `Host` line already matches this name.
pub fn names_host(text: &str, name: &str) -> bool {
    text.lines().filter_map(words).any(|(keyword, patterns)| {
        keyword.eq_ignore_ascii_case("host")
            && patterns
                .iter()
                .any(|pattern| pattern.eq_ignore_ascii_case(name))
    })
}

/// Refuses a machine name that the user's configuration already uses for another host.
pub fn check_name(user_config: &Path, name: &str) -> Result<()> {
    match fs::read_to_string(user_config) {
        Ok(text) if names_host(&text, name) => Err(Error::SshHostTaken {
            name: name.to_owned(),
            path: user_config.to_owned(),
        }),
        Ok(_) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::State {
            path: user_config.to_owned(),
            action: "read the SSH configuration",
            source,
        }),
    }
}

/// Writes the machine's entry, or removes it when there is nothing to reach.
pub fn write(instance: &Instance, directory: &Directory) -> Result<()> {
    let Some(text) = entry(instance, directory) else {
        return remove(directory);
    };
    replace(&directory.ssh_config(), &text, 0o600)
}

pub fn remove(directory: &Directory) -> Result<()> {
    match fs::remove_file(directory.ssh_config()) {
        Err(source) if source.kind() != std::io::ErrorKind::NotFound => Err(Error::State {
            path: directory.ssh_config(),
            action: "remove the SSH configuration entry",
            source,
        }),
        _ => Ok(()),
    }
}

/// Adds the `Include` line to the user's config, returning whether it was missing.
pub fn ensure_include(user_config: &Path, instances: &Path) -> Result<bool> {
    let failure = |path: &Path, action| {
        let path = path.to_owned();
        move |source| Error::State {
            path,
            action,
            source,
        }
    };
    let (text, target, mode) = match fs::read_to_string(user_config) {
        Ok(text) => {
            // Write through a symlink rather than replacing it.
            let target = fs::canonicalize(user_config)
                .map_err(failure(user_config, "resolve the SSH configuration"))?;
            let mode = fs::metadata(&target)
                .map_err(failure(&target, "read the SSH configuration"))?
                .permissions()
                .mode();
            (text, target, mode & 0o7777)
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = user_config.parent()
                && !parent.exists()
            {
                fs::create_dir_all(parent).map_err(failure(parent, "create the SSH directory"))?;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                    .map_err(failure(parent, "restrict the SSH directory"))?;
            }
            (String::new(), user_config.to_owned(), 0o600)
        }
        Err(source) => return Err(failure(user_config, "read the SSH configuration")(source)),
    };
    with_include(&text, &include_line(instances)).map_or(Ok(false), |changed| {
        replace(&target, &changed, mode).map(|()| true)
    })
}

/// Writes a file atomically with the given mode.
fn replace(path: &Path, text: &str, mode: u32) -> Result<()> {
    let mut staging = path.as_os_str().to_owned();
    staging.push(".vm-new");
    let staging = std::path::PathBuf::from(staging);
    let state = |path: &Path, action| {
        let path = path.to_owned();
        move |source| Error::State {
            path,
            action,
            source,
        }
    };
    fs::write(&staging, text).map_err(state(&staging, "write the SSH configuration"))?;
    fs::set_permissions(&staging, fs::Permissions::from_mode(mode))
        .map_err(state(&staging, "restrict the SSH configuration"))?;
    fs::rename(&staging, path).map_err(state(path, "replace the SSH configuration"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::instance::Instances;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("vm-sshconfig-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn instances(&self) -> PathBuf {
            self.0.join("instances")
        }

        fn directory(&self, name: &str) -> Directory {
            Instances::at(self.instances(), self.0.join("run"))
                .create(name)
                .unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn instance(name: &str) -> Instance {
        Instance {
            name: name.to_owned(),
            image: "debian:trixie".to_owned(),
            digest: "sha512:abc".to_owned(),
            arch: "amd64".to_owned(),
            created: 1_700_000_000,
            memory: 2048,
            cpus: 2,
            firmware: crate::machine::Firmware::Bios,
            cpu: "max".to_owned(),
            machine: crate::machine::Chipset::Q35,
            disk: crate::machine::Disk::Virtio,
            user: "operator".to_owned(),
            seeded: true,
            monitor: PathBuf::new(),
            ssh_port: Some(40_022),
            pid: None,
            started: None,
            generation: 0,
            ssh_config: true,
            password: None,
            ports: Vec::new(),
            shares: Vec::new(),
        }
    }

    #[test]
    fn an_entry_names_the_machine_and_holds_what_vm_ssh_passes() {
        let scratch = Scratch::new("entry");
        let directory = scratch.directory("mytestvm");
        let text = entry(&instance("mytestvm"), &directory).unwrap();
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        assert!(lines.contains(&"Host mytestvm"), "{text}");
        assert!(lines.contains(&"HostName 127.0.0.1"), "{text}");
        assert!(lines.contains(&"Port 40022"), "{text}");
        assert!(lines.contains(&"User operator"), "{text}");
        assert!(lines.contains(&"IdentitiesOnly yes"), "{text}");
        assert!(
            lines.contains(&"StrictHostKeyChecking accept-new"),
            "{text}"
        );
        let key = format!("IdentityFile {}", directory.key().display());
        assert!(lines.contains(&key.as_str()), "{text}");
        let hosts = format!("UserKnownHostsFile {}", directory.known_hosts().display());
        assert!(lines.contains(&hosts.as_str()), "{text}");
    }

    #[test]
    fn an_entry_holds_every_setting_the_command_line_does() {
        let scratch = Scratch::new("every");
        let directory = scratch.directory("one");
        let held = instance("one");
        let text = entry(&held, &directory).unwrap();
        for (key, _) in access::settings(&held, &directory, 40_022) {
            assert!(
                text.lines()
                    .any(|line| line.trim().starts_with(&format!("{key} "))),
                "{key} missing from {text}"
            );
        }
    }

    #[test]
    fn a_machine_with_no_way_in_has_no_entry() {
        let scratch = Scratch::new("noway");
        let directory = scratch.directory("one");
        let mut held = instance("one");
        held.ssh_port = None;
        assert!(entry(&held, &directory).is_none());
        let mut held = instance("one");
        held.seeded = false;
        assert!(entry(&held, &directory).is_none());
    }

    #[test]
    fn a_path_holding_a_space_is_quoted() {
        assert_eq!(
            include_line(Path::new("/home/a b/.local/state/vm/instances")),
            "Include \"/home/a b/.local/state/vm/instances/*/ssh_config\""
        );
        assert_eq!(
            include_line(Path::new("/home/x/.local/state/vm/instances")),
            "Include /home/x/.local/state/vm/instances/*/ssh_config"
        );
    }

    #[test]
    fn the_include_goes_in_front_of_what_was_there() {
        let line = "Include /i/*/ssh_config";
        let changed = with_include("Host work\n    User me\n", line).unwrap();
        assert!(changed.starts_with("# Added by vm"), "{changed}");
        assert!(
            changed.ends_with(&format!("{line}\n\nHost work\n    User me\n")),
            "{changed}"
        );
        assert_eq!(
            with_include("", line).unwrap(),
            format!("{INCLUDE_COMMENT}\n{line}\n")
        );
    }

    #[test]
    fn an_include_already_in_front_is_not_added_again() {
        let line = "Include /i/*/ssh_config";
        let once = with_include("Host work\n", line).unwrap();
        assert!(with_include(&once, line).is_none());
        assert!(with_include(&format!("  {line}\nHost work\n"), line).is_none());
    }

    /// Inside a `Host` block the line would apply only to that block's hosts.
    #[test]
    fn an_include_inside_a_host_block_does_not_count() {
        let line = "Include /i/*/ssh_config";
        let text = format!("Host work\n    {line}\n");
        assert!(with_include(&text, line).is_some());
        let text = format!("Match host work\n{line}\n");
        assert!(with_include(&text, line).is_some());
    }

    #[test]
    fn a_host_already_named_is_found_in_either_spelling_and_among_patterns() {
        assert!(names_host("Host mytestvm\n", "mytestvm"));
        assert!(names_host("host=MyTestVM\n", "mytestvm"));
        assert!(names_host("  Host work mytestvm *.example\n", "mytestvm"));
        assert!(names_host("Host \"mytestvm\"\n", "mytestvm"));
        assert!(!names_host("Host mytestvm2 *\n", "mytestvm"));
        assert!(!names_host("# Host mytestvm\n", "mytestvm"));
        assert!(!names_host("HostName mytestvm\n", "mytestvm"));
        assert!(!names_host("Match host mytestvm\n", "mytestvm"));
    }

    #[test]
    fn a_name_is_refused_only_when_the_configuration_uses_it() {
        let scratch = Scratch::new("check");
        let config = scratch.0.join("config");
        check_name(&config, "work").unwrap();
        fs::write(&config, "Host work\n    User me\n").unwrap();
        assert_eq!(
            check_name(&config, "work").unwrap_err().kind(),
            "ssh-host-taken"
        );
        check_name(&config, "play").unwrap();
    }

    #[test]
    fn writing_an_entry_makes_a_private_file_and_removing_it_twice_is_fine() {
        let scratch = Scratch::new("write");
        let directory = scratch.directory("one");
        write(&instance("one"), &directory).unwrap();
        let mode = fs::metadata(directory.ssh_config())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        remove(&directory).unwrap();
        assert!(!directory.ssh_config().exists());
        remove(&directory).unwrap();
    }

    #[test]
    fn a_machine_that_lost_its_way_in_loses_its_entry() {
        let scratch = Scratch::new("lost");
        let directory = scratch.directory("one");
        let mut held = instance("one");
        write(&held, &directory).unwrap();
        held.ssh_port = None;
        write(&held, &directory).unwrap();
        assert!(!directory.ssh_config().exists());
    }

    #[test]
    fn a_missing_configuration_is_created_privately_with_its_directory() {
        let scratch = Scratch::new("create");
        let config = scratch.0.join("home/.ssh/config");
        assert!(ensure_include(&config, &scratch.instances()).unwrap());
        let text = fs::read_to_string(&config).unwrap();
        assert!(text.contains(&include_line(&scratch.instances())), "{text}");
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&config), 0o600);
        assert_eq!(mode(config.parent().unwrap()), 0o700);
        assert!(!ensure_include(&config, &scratch.instances()).unwrap());
    }

    #[test]
    fn an_existing_configuration_keeps_its_contents_and_permissions() {
        let scratch = Scratch::new("existing");
        let config = scratch.0.join("config");
        fs::write(&config, "Host work\n    User me\n").unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(ensure_include(&config, &scratch.instances()).unwrap());
        let text = fs::read_to_string(&config).unwrap();
        assert!(text.ends_with("Host work\n    User me\n"), "{text}");
        assert_eq!(
            fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert!(!scratch.0.join("config.vm-new").exists());
    }

    /// A configuration kept elsewhere and linked into place stays linked.
    #[test]
    fn a_linked_configuration_is_changed_where_it_lives() {
        let scratch = Scratch::new("linked");
        let real = scratch.0.join("dotfiles-config");
        let config = scratch.0.join("config");
        fs::write(&real, "Host work\n").unwrap();
        std::os::unix::fs::symlink(&real, &config).unwrap();
        assert!(ensure_include(&config, &scratch.instances()).unwrap());
        assert!(
            fs::symlink_metadata(&config)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::read_to_string(&real)
                .unwrap()
                .contains(&include_line(&scratch.instances()))
        );
    }

    /// What `ssh` itself makes of the entry, through the line added to a configuration.
    #[test]
    fn ssh_reaches_the_machine_by_name_through_the_include() {
        let scratch = Scratch::new("resolve");
        let directory = scratch.directory("mytestvm");
        write(&instance("mytestvm"), &directory).unwrap();
        let config = scratch.0.join("config");
        fs::write(&config, "Host elsewhere\n    Port 1\n").unwrap();
        ensure_include(&config, &scratch.instances()).unwrap();
        let Ok(output) = std::process::Command::new("ssh")
            .arg("-F")
            .arg(&config)
            .args(["-G", "mytestvm"])
            .output()
        else {
            eprintln!("ssh is not here, so its reading of the entry goes unchecked");
            return;
        };
        let text = String::from_utf8(output.stdout).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.contains(&"hostname 127.0.0.1"), "{text}");
        assert!(lines.contains(&"port 40022"), "{text}");
        assert!(lines.contains(&"user operator"), "{text}");
        assert!(lines.contains(&"identitiesonly yes"), "{text}");
        let key = format!("identityfile {}", directory.key().display());
        assert!(lines.contains(&key.as_str()), "{text}");
    }

    #[test]
    fn the_include_pattern_matches_where_an_entry_is_written() {
        let scratch = Scratch::new("pattern");
        let directory = scratch.directory("one");
        let pattern = include_line(&scratch.instances());
        let pattern = pattern.strip_prefix("Include ").unwrap();
        let (before, after) = pattern.split_once('*').unwrap();
        let path = directory.ssh_config().display().to_string();
        assert!(path.starts_with(before) && path.ends_with(after), "{path}");
        assert!(!path[before.len()..path.len() - after.len()].contains('/'));
    }
}

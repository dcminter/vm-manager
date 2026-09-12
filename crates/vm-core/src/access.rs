//! Reaching a guest, by wrapping `ssh` and `scp` rather than replacing them.
//!
//! An in-process SSH client would quietly lose the user's `~/.ssh/config`,
//! their agent, their jump hosts and their `ProxyCommand`. What is built here
//! is the argument list, which is the part worth testing; running it is the
//! caller's business.

use crate::error::{Error, Result};
use crate::instance::{Directory, Instance};

/// Where a file is, for `vm cp`. A path with no instance in front of it is on
/// this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Host(String),
    Guest { instance: String, path: String },
}

impl Location {
    /// Reads `name:path` as a guest location and anything else as a host one.
    ///
    /// A Windows-style `C:\` is not a concern, but a relative path holding a
    /// colon is, so only a leading segment that could be an instance name is
    /// read as one.
    pub fn parse(text: &str) -> Self {
        match text.split_once(':') {
            Some((name, path)) if is_name(name) => Self::Guest {
                instance: name.to_owned(),
                path: path.to_owned(),
            },
            _ => Self::Host(text.to_owned()),
        }
    }

    pub const fn instance(&self) -> Option<&String> {
        match self {
            Self::Guest { instance, .. } => Some(instance),
            Self::Host(_) => None,
        }
    }

    /// How `scp` should be told about it.
    fn render(&self, user: &str) -> String {
        match self {
            Self::Host(path) => path.clone(),
            Self::Guest { path, .. } => format!("{user}@127.0.0.1:{path}"),
        }
    }
}

fn is_name(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with('-')
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// The options both commands share.
///
/// The instance keeps its own `known_hosts`, so rebuilding a machine never
/// provokes a warning about the user's, and `IdentitiesOnly` stops an agent
/// holding many keys from offering all of them and being refused for it.
fn options(instance: &Instance, directory: &Directory, port: u16) -> Vec<String> {
    [
        "-i".to_owned(),
        directory.key().display().to_string(),
        "-o".to_owned(),
        "IdentitiesOnly=yes".to_owned(),
        "-o".to_owned(),
        format!("UserKnownHostsFile={}", directory.known_hosts().display()),
        "-o".to_owned(),
        "GlobalKnownHostsFile=/dev/null".to_owned(),
        "-o".to_owned(),
        "StrictHostKeyChecking=accept-new".to_owned(),
        "-o".to_owned(),
        format!("Port={port}"),
        "-o".to_owned(),
        format!("User={}", instance.user),
    ]
    .into()
}

/// The arguments for `ssh`, optionally running a command rather than a shell.
pub fn ssh(instance: &Instance, directory: &Directory, command: &[String]) -> Result<Vec<String>> {
    let port = reachable(instance)?;
    let mut arguments = options(instance, directory, port);
    arguments.push("127.0.0.1".to_owned());
    arguments.extend(command.iter().cloned());
    Ok(arguments)
}

/// The arguments for `scp`. Exactly one side names the guest; copying between
/// two guests would need each to reach the other, which neither can.
pub fn scp(
    instance: &Instance,
    directory: &Directory,
    from: &Location,
    to: &Location,
) -> Result<Vec<String>> {
    let port = reachable(instance)?;
    let mut arguments = options(instance, directory, port);
    arguments.push("-r".to_owned());
    arguments.push(from.render(&instance.user));
    arguments.push(to.render(&instance.user));
    Ok(arguments)
}

/// Whether the guest can be reached at all, and on what port.
fn reachable(instance: &Instance) -> Result<u16> {
    if !instance.seeded {
        return Err(Error::NoGuestAccess {
            name: instance.name.clone(),
        });
    }
    if !instance.is_running() {
        return Err(Error::InstanceStopped {
            name: instance.name.clone(),
        });
    }
    instance.ssh_port.ok_or_else(|| Error::NoGuestAccess {
        name: instance.name.clone(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::instance::Instances;
    use crate::process::Handle;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("vm-access-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn directory(&self, name: &str) -> Directory {
            Instances::at(self.0.join("instances"), self.0.join("run"))
                .create(name)
                .unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Running, because this process is.
    fn instance(name: &str) -> Instance {
        let handle = Handle::of(std::process::id()).unwrap();
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
            user: "vm".to_owned(),
            seeded: true,
            monitor: PathBuf::new(),
            ssh_port: Some(2222),
            pid: Some(handle.pid),
            started: Some(handle.started),
            generation: 0,
            password: None,
            ports: Vec::new(),
            shares: Vec::new(),
        }
    }

    fn value(arguments: &[String], flag: &str, prefix: &str) -> Option<String> {
        arguments
            .windows(2)
            .filter(|pair| pair[0] == flag && pair[1].starts_with(prefix))
            .map(|pair| pair[1].clone())
            .next()
    }

    #[test]
    fn the_instance_key_is_the_only_one_offered() {
        let scratch = Scratch::new("key");
        let directory = scratch.directory("one");
        let arguments = ssh(&instance("one"), &directory, &[]).unwrap();
        assert_eq!(
            value(&arguments, "-i", ""),
            Some(directory.key().display().to_string())
        );
        assert!(arguments.iter().any(|held| held == "IdentitiesOnly=yes"));
    }

    /// A rebuilt machine presents a new host key on the same port, which
    /// against the user's own file is a warning they have to clear by hand.
    #[test]
    fn the_instance_keeps_its_own_host_keys() {
        let scratch = Scratch::new("hosts");
        let directory = scratch.directory("one");
        let arguments = ssh(&instance("one"), &directory, &[]).unwrap();
        assert_eq!(
            value(&arguments, "-o", "UserKnownHostsFile="),
            Some(format!(
                "UserKnownHostsFile={}",
                directory.known_hosts().display()
            ))
        );
        assert!(
            arguments
                .iter()
                .any(|held| held == "GlobalKnownHostsFile=/dev/null")
        );
    }

    #[test]
    fn the_port_and_the_user_come_from_the_record() {
        let scratch = Scratch::new("port");
        let mut held = instance("one");
        held.ssh_port = Some(40_022);
        held.user = "operator".to_owned();
        let arguments = ssh(&held, &scratch.directory("one"), &[]).unwrap();
        assert_eq!(
            value(&arguments, "-o", "Port="),
            Some("Port=40022".to_owned())
        );
        assert_eq!(
            value(&arguments, "-o", "User="),
            Some("User=operator".to_owned())
        );
    }

    #[test]
    fn with_no_command_the_destination_is_the_last_argument() {
        let scratch = Scratch::new("shell");
        let arguments = ssh(&instance("one"), &scratch.directory("one"), &[]).unwrap();
        assert_eq!(arguments.last().unwrap(), "127.0.0.1");
    }

    #[test]
    fn a_command_follows_the_destination() {
        let scratch = Scratch::new("command");
        let command = vec!["uname".to_owned(), "-a".to_owned()];
        let arguments = ssh(&instance("one"), &scratch.directory("one"), &command).unwrap();
        let at = arguments
            .iter()
            .position(|held| held == "127.0.0.1")
            .unwrap();
        assert_eq!(&arguments[at + 1..], command.as_slice());
    }

    #[test]
    fn an_instance_that_is_not_running_cannot_be_reached() {
        let scratch = Scratch::new("stopped");
        let mut held = instance("one");
        held.forget_process();
        let error = ssh(&held, &scratch.directory("one"), &[]).unwrap_err();
        assert_eq!(error.kind(), "instance-stopped");
    }

    /// An image with no cloud-init took no key, so there is nothing to offer
    /// and no account to offer it to. Saying so beats a password prompt.
    #[test]
    fn an_unseeded_instance_has_no_way_in() {
        let scratch = Scratch::new("unseeded");
        let mut held = instance("one");
        held.seeded = false;
        let error = ssh(&held, &scratch.directory("one"), &[]).unwrap_err();
        assert_eq!(error.kind(), "no-guest-access");
    }

    #[test]
    fn an_instance_with_no_forwarded_port_has_no_way_in() {
        let scratch = Scratch::new("noport");
        let mut held = instance("one");
        held.ssh_port = None;
        assert_eq!(
            ssh(&held, &scratch.directory("one"), &[])
                .unwrap_err()
                .kind(),
            "no-guest-access"
        );
    }

    #[test]
    fn a_location_with_an_instance_in_front_of_it_is_a_guest_path() {
        assert_eq!(
            Location::parse("demo:/etc/hostname"),
            Location::Guest {
                instance: "demo".to_owned(),
                path: "/etc/hostname".to_owned()
            }
        );
    }

    #[test]
    fn a_plain_path_is_on_this_machine() {
        for text in ["/etc/hostname", "./notes.txt", "notes.txt", ""] {
            assert_eq!(Location::parse(text), Location::Host(text.to_owned()));
            assert!(Location::parse(text).instance().is_none());
        }
    }

    /// A colon in a path is not an instance name, and reading it as one would
    /// send a local copy to a machine that does not exist.
    #[test]
    fn a_path_holding_a_colon_is_not_an_instance() {
        for text in ["/a/b:c", "a b:c", "-x:y", ":/etc/hostname"] {
            assert_eq!(
                Location::parse(text),
                Location::Host(text.to_owned()),
                "{text}"
            );
        }
    }

    #[test]
    fn a_copy_out_of_a_guest_names_the_user_and_the_address() {
        let scratch = Scratch::new("copyout");
        let arguments = scp(
            &instance("one"),
            &scratch.directory("one"),
            &Location::parse("one:/etc/hostname"),
            &Location::parse("./hostname"),
        )
        .unwrap();
        assert_eq!(arguments[arguments.len() - 2], "vm@127.0.0.1:/etc/hostname");
        assert_eq!(arguments[arguments.len() - 1], "./hostname");
    }

    #[test]
    fn a_copy_into_a_guest_is_the_same_arguments_the_other_way_round() {
        let scratch = Scratch::new("copyin");
        let arguments = scp(
            &instance("one"),
            &scratch.directory("one"),
            &Location::parse("./notes.txt"),
            &Location::parse("one:/home/vm/"),
        )
        .unwrap();
        assert_eq!(arguments[arguments.len() - 2], "./notes.txt");
        assert_eq!(arguments[arguments.len() - 1], "vm@127.0.0.1:/home/vm/");
    }

    #[test]
    fn a_copy_is_recursive_so_that_a_directory_is_not_a_surprise() {
        let scratch = Scratch::new("recursive");
        let arguments = scp(
            &instance("one"),
            &scratch.directory("one"),
            &Location::parse("./tree"),
            &Location::parse("one:/tmp/"),
        )
        .unwrap();
        assert!(arguments.iter().any(|held| held == "-r"));
    }
}

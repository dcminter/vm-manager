//! Candidates for tab completion of instance names, image references and machine settings.

use clap_complete::engine::{CompletionCandidate, PathCompleter, ValueCompleter as _};
use std::ffi::OsStr;
use vm_core::catalogue::Catalogue;
use vm_core::host_architecture;
use vm_core::instance::Instances;
use vm_core::machine::{Chipset, Disk, Firmware};
use vm_core::store::Store;

/// Which instances a command can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wanted {
    Any,
    Running,
    Stopped,
}

pub fn any_instance() -> Vec<CompletionCandidate> {
    instances_or_nothing(Wanted::Any)
}

pub fn running_instance() -> Vec<CompletionCandidate> {
    instances_or_nothing(Wanted::Running)
}

pub fn stopped_instance() -> Vec<CompletionCandidate> {
    instances_or_nothing(Wanted::Stopped)
}

fn instances_or_nothing(wanted: Wanted) -> Vec<CompletionCandidate> {
    Instances::discover().map_or_else(|_| Vec::new(), |held| instances(&held, wanted))
}

/// Instance names, each described by its image and whether it is running.
pub fn instances(instances: &Instances, wanted: Wanted) -> Vec<CompletionCandidate> {
    instances
        .names()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|name| {
            let held = instances.open(&name).ok()?.read().ok()?;
            let running = held.is_running();
            let fits = match wanted {
                Wanted::Any => true,
                Wanted::Running => running,
                Wanted::Stopped => !running,
            };
            let state = if running { "running" } else { "stopped" };
            fits.then(|| {
                CompletionCandidate::new(name).help(Some(format!("{}, {state}", held.image).into()))
            })
        })
        .collect()
}

/// Every reference the catalogue resolves on this host.
pub fn catalogue_image() -> Vec<CompletionCandidate> {
    loaded().map_or_else(Vec::new, |(catalogue, _)| {
        references(&catalogue, Some(host_architecture()), &|_| true)
    })
}

/// Every reference the catalogue holds, for any architecture.
pub fn any_catalogue_image() -> Vec<CompletionCandidate> {
    loaded().map_or_else(Vec::new, |(catalogue, _)| {
        references(&catalogue, None, &|_| true)
    })
}

/// Every architecture the catalogue has a build for.
pub fn architecture() -> Vec<CompletionCandidate> {
    loaded().map_or_else(Vec::new, |(catalogue, _)| architectures(&catalogue))
}

fn architectures(catalogue: &Catalogue) -> Vec<CompletionCandidate> {
    let mut found: Vec<String> = catalogue
        .entries()
        .into_iter()
        .flat_map(vm_core::catalogue::Entry::architectures)
        .collect();
    found.sort();
    found.dedup();
    found.into_iter().map(CompletionCandidate::new).collect()
}

/// References to images this host holds, or wrote itself.
pub fn held_image() -> Vec<CompletionCandidate> {
    loaded().map_or_else(Vec::new, |(catalogue, store)| {
        references(&catalogue, Some(host_architecture()), &|artifact| {
            artifact.url.is_none() || store.contains(&artifact.digest)
        })
    })
}

fn loaded() -> Option<(Catalogue, Store)> {
    let catalogue = Catalogue::load_layered(&crate::catalogue_sources().ok()?).ok()?;
    Some((catalogue, Store::discover().ok()?))
}

pub fn catalogue_name() -> Vec<CompletionCandidate> {
    crate::catalogue_sources().map_or_else(
        |_| Vec::new(),
        |sources| {
            sources
                .into_iter()
                .map(|source| {
                    CompletionCandidate::new(source.name).help(Some(source.kind.name().into()))
                })
                .collect()
        },
    )
}

pub fn configured_remote() -> Vec<CompletionCandidate> {
    configured(|settings| {
        settings
            .remotes
            .into_iter()
            .map(|remote| remote.name)
            .collect()
    })
}

pub fn configured_local() -> Vec<CompletionCandidate> {
    configured(|settings| {
        settings
            .locals
            .into_iter()
            .map(|local| local.name)
            .collect()
    })
}

/// Names the config file itself lists.
fn configured(names: fn(vm_core::config::Settings) -> Vec<String>) -> Vec<CompletionCandidate> {
    vm_core::config::Config::path()
        .and_then(|path| vm_core::config::Settings::read(&path).ok().flatten())
        .map_or_else(Vec::new, |settings| {
            names(settings)
                .into_iter()
                .map(CompletionCandidate::new)
                .collect()
        })
}

pub fn remote_catalogue_name() -> Vec<CompletionCandidate> {
    vm_core::config::Config::load().map_or_else(
        |_| Vec::new(),
        |config| {
            config
                .remotes()
                .into_iter()
                .map(|remote| CompletionCandidate::new(remote.name).help(Some(remote.url.into())))
                .collect()
        },
    )
}

/// Each entry's `name:tag` and aliases, for the entries with a fitting build for `arch`, or any.
pub fn references(
    catalogue: &Catalogue,
    arch: Option<&str>,
    keep: &dyn Fn(&vm_core::catalogue::Artifact) -> bool,
) -> Vec<CompletionCandidate> {
    catalogue
        .entries()
        .into_iter()
        .filter(|entry| {
            entry
                .artifacts
                .iter()
                .any(|artifact| arch.is_none_or(|wanted| artifact.arch == wanted) && keep(artifact))
        })
        .flat_map(|entry| {
            let help = Some(entry.description.clone().into());
            std::iter::once(&entry.tag)
                .chain(&entry.aliases)
                .map(move |tag| {
                    CompletionCandidate::new(format!("{}:{tag}", entry.name)).help(help.clone())
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

pub fn firmware() -> Vec<CompletionCandidate> {
    Firmware::ALL
        .into_iter()
        .map(|held| CompletionCandidate::new(held.name()))
        .collect()
}

pub fn compression() -> Vec<CompletionCandidate> {
    vm_core::compression::Compression::SCHEMES
        .into_iter()
        .map(|held| CompletionCandidate::new(held.name()))
        .collect()
}

pub fn login() -> Vec<CompletionCandidate> {
    vm_core::catalogue::Login::ALL
        .into_iter()
        .map(|held| CompletionCandidate::new(held.name()))
        .collect()
}

pub fn known_architecture() -> Vec<CompletionCandidate> {
    vm_core::ARCHITECTURES
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

pub fn chipset() -> Vec<CompletionCandidate> {
    Chipset::ALL
        .into_iter()
        .map(|held| CompletionCandidate::new(held.name()))
        .collect()
}

pub fn disk() -> Vec<CompletionCandidate> {
    Disk::ALL
        .into_iter()
        .map(|held| CompletionCandidate::new(held.name()))
        .collect()
}

/// A side of `vm cp`: a local path, or a running instance's name followed by a colon.
pub fn copy_side(current: &OsStr) -> Vec<CompletionCandidate> {
    let running =
        Instances::discover().map_or_else(|_| Vec::new(), |held| instances(&held, Wanted::Running));
    copy_candidates(current, running, &|typed| {
        PathCompleter::any().complete(typed)
    })
}

fn copy_candidates(
    current: &OsStr,
    running: Vec<CompletionCandidate>,
    paths: &dyn Fn(&OsStr) -> Vec<CompletionCandidate>,
) -> Vec<CompletionCandidate> {
    let Some(typed) = current.to_str() else {
        return paths(current);
    };
    if typed.contains(':') {
        return Vec::new();
    }
    let mut found: Vec<CompletionCandidate> = running
        .into_iter()
        .map(|candidate| {
            let name = candidate.get_value().to_string_lossy().into_owned();
            CompletionCandidate::new(format!("{name}:")).help(candidate.get_help().cloned())
        })
        .filter(|candidate| candidate.get_value().to_string_lossy().starts_with(typed))
        .collect();
    found.extend(paths(current));
    found
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use clap::CommandFactory as _;
    use std::path::PathBuf;
    use vm_core::instance::Instance;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("vm-completion-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn instances(&self) -> Instances {
            Instances::at(self.0.join("instances"), self.0.join("run"))
        }

        /// An instance whose recorded process is this test, so it reads as running.
        fn instance(&self, name: &str, running: bool) {
            let instances = self.instances();
            let directory = instances.create(name).unwrap();
            let handle = running
                .then(|| vm_core::process::Handle::of(std::process::id()))
                .flatten();
            directory
                .write(&Instance {
                    name: name.to_owned(),
                    image: "debian:trixie".to_owned(),
                    digest: "sha512:abc".to_owned(),
                    arch: "amd64".to_owned(),
                    created: 1_700_000_000,
                    memory: 2048,
                    cpus: 2,
                    firmware: Firmware::Bios,
                    cpu: "max".to_owned(),
                    machine: Chipset::Q35,
                    disk: Disk::Virtio,
                    user: "vm".to_owned(),
                    seeded: true,
                    monitor: directory.monitor().to_owned(),
                    ssh_port: Some(2222),
                    pid: handle.as_ref().map(|held| held.pid),
                    started: handle.as_ref().map(|held| held.started),
                    generation: 0,
                    ssh_config: false,
                    media: vm_core::catalogue::Media::Disk,
                    cdrom: None,
                    password: None,
                    ports: Vec::new(),
                    shares: Vec::new(),
                })
                .unwrap();
        }

        fn entry(&self, file: &str, body: &str) {
            let path = self.0.join("catalogue").join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn values(candidates: &[CompletionCandidate]) -> Vec<String> {
        candidates
            .iter()
            .map(|candidate| candidate.get_value().to_string_lossy().into_owned())
            .collect()
    }

    fn entry(name: &str, tag: &str, aliases: &str, arch: &str, url: bool) -> String {
        let url = if url {
            "url = \"https://example.invalid/image.qcow2\"\n"
        } else {
            ""
        };
        format!(
            "name = \"{name}\"\ntag = \"{tag}\"\naliases = [{aliases}]\ndescription = \"{name} for tests\"\nlogin = \"none\"\n\n[[image]]\narch = \"{arch}\"\nformat = \"qcow2\"\n{url}digest = \"sha512:{}\"\n",
            "b".repeat(128)
        )
    }

    #[test]
    fn instances_are_offered_by_whether_the_command_can_use_them() {
        let scratch = Scratch::new("instances");
        scratch.instance("awake", true);
        scratch.instance("asleep", false);
        let instances = scratch.instances();
        assert_eq!(
            values(&super::instances(&instances, Wanted::Any)),
            ["asleep", "awake"]
        );
        assert_eq!(
            values(&super::instances(&instances, Wanted::Running)),
            ["awake"]
        );
        assert_eq!(
            values(&super::instances(&instances, Wanted::Stopped)),
            ["asleep"]
        );
    }

    #[test]
    fn an_instance_is_described_by_its_image_and_state() {
        let scratch = Scratch::new("described");
        scratch.instance("awake", true);
        let found = super::instances(&scratch.instances(), Wanted::Any);
        assert_eq!(
            found[0].get_help().unwrap().to_string(),
            "debian:trixie, running"
        );
    }

    /// A damaged instance must not stop the others being offered.
    #[test]
    fn a_directory_without_a_readable_record_is_left_out() {
        let scratch = Scratch::new("damaged");
        scratch.instance("good", false);
        std::fs::create_dir_all(scratch.0.join("instances/broken")).unwrap();
        assert_eq!(
            values(&super::instances(&scratch.instances(), Wanted::Any)),
            ["good"]
        );
    }

    #[test]
    fn no_state_directory_offers_nothing() {
        let scratch = Scratch::new("empty");
        assert!(super::instances(&scratch.instances(), Wanted::Any).is_empty());
    }

    #[test]
    fn references_include_aliases_and_only_this_architecture() {
        let scratch = Scratch::new("references");
        scratch.entry(
            "debian/trixie.toml",
            &entry("debian", "trixie", "\"13\", \"latest\"", "amd64", true),
        );
        scratch.entry("arm/only.toml", &entry("armonly", "1", "", "arm64", true));
        let catalogue = Catalogue::load(&scratch.0.join("catalogue")).unwrap();
        let found = references(&catalogue, Some("amd64"), &|_| true);
        let mut offered = values(&found);
        offered.sort();
        assert_eq!(offered, ["debian:13", "debian:latest", "debian:trixie"]);
        assert_eq!(found[0].get_help().unwrap().to_string(), "debian for tests");
        assert_eq!(
            values(&references(&catalogue, Some("arm64"), &|_| true)),
            ["armonly:1"]
        );
        let mut every = values(&references(&catalogue, None, &|_| true));
        every.sort();
        assert_eq!(
            every,
            ["armonly:1", "debian:13", "debian:latest", "debian:trixie"]
        );
        assert_eq!(values(&architectures(&catalogue)), ["amd64", "arm64"]);
    }

    #[test]
    fn references_can_be_narrowed_to_what_is_held() {
        let scratch = Scratch::new("held");
        scratch.entry("fetched/1.toml", &entry("fetched", "1", "", "amd64", true));
        scratch.entry("local/1.toml", &entry("local", "1", "", "amd64", false));
        let catalogue = Catalogue::load(&scratch.0.join("catalogue")).unwrap();
        let found = references(&catalogue, Some("amd64"), &|artifact| {
            artifact.url.is_none()
        });
        assert_eq!(values(&found), ["local:1"]);
    }

    #[test]
    fn machine_settings_offer_every_accepted_value() {
        assert_eq!(values(&firmware()), ["bios", "uefi"]);
        assert_eq!(values(&chipset()), ["q35", "pc"]);
        assert_eq!(values(&disk()), ["virtio", "ide", "sata"]);
        for value in values(&firmware()) {
            vm_core::settings::parse_firmware(&value).unwrap();
        }
        for value in values(&chipset()) {
            vm_core::settings::parse_machine(&value).unwrap();
        }
        for value in values(&disk()) {
            vm_core::settings::parse_disk(&value).unwrap();
        }
    }

    fn no_paths(_: &OsStr) -> Vec<CompletionCandidate> {
        Vec::new()
    }

    #[test]
    fn a_copy_offers_running_names_with_a_colon_alongside_paths() {
        let running = || {
            vec![
                CompletionCandidate::new("web"),
                CompletionCandidate::new("db"),
            ]
        };
        let paths = |_: &OsStr| vec![CompletionCandidate::new("docs/")];
        assert_eq!(
            values(&copy_candidates(OsStr::new(""), running(), &paths)),
            ["web:", "db:", "docs/"]
        );
        assert_eq!(
            values(&copy_candidates(OsStr::new("w"), running(), &no_paths)),
            ["web:"]
        );
    }

    /// The part after the colon is a path inside the guest, which is not ours to list.
    #[test]
    fn a_copy_offers_nothing_once_an_instance_is_named() {
        let running = vec![CompletionCandidate::new("web")];
        let paths = |_: &OsStr| vec![CompletionCandidate::new("docs/")];
        assert!(copy_candidates(OsStr::new("web:/et"), running, &paths).is_empty());
    }

    fn complete(line: &[&str]) -> Vec<String> {
        let args = line.iter().map(std::ffi::OsString::from).collect();
        let found =
            clap_complete::engine::complete(&mut crate::Cli::command(), args, line.len() - 1, None)
                .unwrap();
        values(&found)
    }

    #[test]
    fn the_command_line_offers_machine_settings() {
        assert_eq!(
            complete(&["vm", "run", "debian:trixie", "--firmware", ""]),
            ["bios", "uefi"]
        );
        assert_eq!(complete(&["vm", "start", "x", "--machine", "p"]), ["pc"]);
        assert_eq!(
            complete(&["vm", "run", "x", "--disk", ""]),
            ["virtio", "ide", "sata"]
        );
    }

    /// Each command taking a name or reference has a completer, not the path fallback.
    #[test]
    fn every_name_and_reference_has_a_completer() {
        let command = crate::Cli::command();
        let mut missing = Vec::new();
        for subcommand in command.get_subcommands() {
            for arg in subcommand.get_arguments() {
                let id = arg.get_id().as_str();
                if !["name", "reference", "from", "to"].contains(&id) || !arg.is_positional() {
                    continue;
                }
                if arg
                    .get::<clap_complete::engine::ArgValueCandidates>()
                    .is_none()
                    && arg
                        .get::<clap_complete::engine::ArgValueCompleter>()
                        .is_none()
                {
                    missing.push(format!("{} {id}", subcommand.get_name()));
                }
            }
        }
        assert!(missing.is_empty(), "{missing:?}");
    }
}

//! Flattening a machine's disk into a new image.

use crate::catalogue::{Artifact, Login, Media, NewEntry};
use crate::compression::Compression;
use crate::conversion::{self, Conversion};
use crate::error::{Error, Result};
use crate::instance::Instance;
use crate::reference::{Algorithm, Digest, Reference};
use crate::store::Store;
use std::fs;
use std::path::{Path, PathBuf};

/// What a clone is to be called.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    pub reference: &'a Reference,
    /// Replaces the description naming the source machine.
    pub description: Option<&'a str>,
}

/// A clone's pass: flattening, then hashing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Converting,
    Hashing,
}

impl Stage {
    /// The pass's name for display.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Converting => "converting",
            Self::Hashing => "verifying",
        }
    }
}

/// Receives the current pass and its percentage.
pub type Reporter<'a> = &'a mut dyn FnMut(Stage, u8);

/// The digest algorithm for local images.
pub const ALGORITHM: Algorithm = Algorithm::Sha256;

/// What a clone produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cloned {
    pub name: String,
    pub tag: String,
    pub arch: String,
    pub digest: Digest,
    pub size: u64,
    pub path: PathBuf,
    pub entry: PathBuf,
}

/// Whole percent of a known total, or zero when the total is unknown.
pub fn proportion(progress: &crate::store::Progress) -> u8 {
    let Some(total) = progress.total.filter(|total| *total > 0) else {
        return 0;
    };
    let percent = progress.received.saturating_mul(100) / total;
    u8::try_from(percent.min(100)).unwrap_or(100)
}

/// Clones an instance's disk into the store as `name:tag`.
pub fn image(
    store: &Store,
    local: &Path,
    instance: &Instance,
    overlay: &Path,
    target: Target<'_>,
    in_use: bool,
    mut report: Option<Reporter<'_>>,
) -> Result<Cloned> {
    let (name, tag) = (target.reference.repository(), target.reference.tag());
    let staged = store.staging(&format!("{name}-{tag}"))?;
    let _ = fs::remove_file(&staged);
    let mut say = |stage, percent| {
        if let Some(report) = report.as_deref_mut() {
            report(stage, percent);
        }
    };
    conversion::convert(
        &Conversion {
            source: overlay,
            format: "qcow2",
            destination: &staged,
            in_use,
            operation: "cloning a virtual machine",
        },
        Some(&mut |percent| say(Stage::Converting, percent)),
    )
    .map_err(|error| match error {
        Error::Convert { reason } => Error::Clone { reason },
        other => other,
    })?;
    let digest = store.adopt(&staged, ALGORITHM, &mut |progress| {
        say(Stage::Hashing, proportion(&progress));
    })?;
    let path = store.path_for(&digest);
    let size = fs::metadata(&path).map_or(0, |data| data.len());
    let entry = write_entry(
        local,
        instance,
        name,
        tag,
        &digest,
        size,
        target.description,
    )?;
    Ok(Cloned {
        name: name.to_owned(),
        tag: tag.to_owned(),
        arch: instance.arch.clone(),
        digest,
        size,
        path,
        entry,
    })
}

/// Writes the catalogue entry naming a cloned image.
fn write_entry(
    local: &Path,
    instance: &Instance,
    name: &str,
    tag: &str,
    digest: &Digest,
    size: u64,
    description: Option<&str>,
) -> Result<PathBuf> {
    let description =
        description.map_or_else(|| format!("Cloned from '{}'", instance.name), str::to_owned);
    // The disk was made to boot on this machine, so a clone asks for the same one.
    let artifact = Artifact {
        arch: instance.arch.clone(),
        format: "qcow2".to_owned(),
        url: None,
        digest: digest.clone(),
        size: Some(size),
        compression: Compression::None,
        source_format: None,
        archive_member: None,
        media: Media::Disk,
        firmware: instance.firmware,
        cpu_model: (instance.cpu_model != crate::machine::DEFAULT_CPU_MODEL)
            .then(|| instance.cpu_model.clone()),
        machine: instance.machine,
        disk: instance.disk,
    };
    NewEntry {
        name,
        tag,
        description: &description,
        login: if instance.seeded {
            Login::CloudInit
        } else {
            Login::None
        },
        artifact: &artifact,
    }
    .write(local)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::catalogue::Catalogue;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("vm-clone-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
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
            cpu_model: "max".to_owned(),
            machine: crate::machine::Chipset::Q35,
            disk: crate::machine::Disk::Virtio,
            user: "vm".to_owned(),
            seeded: true,
            monitor: PathBuf::new(),
            ssh_port: None,
            pid: None,
            started: None,
            generation: 0,
            ssh_config: false,
            auto_remove: false,
            media: crate::catalogue::Media::Disk,
            cdrom: None,
            password: None,
            ports: Vec::new(),
            shares: Vec::new(),
        }
    }

    fn entry_is_readable(local: &Path) -> Catalogue {
        Catalogue::load(local).unwrap()
    }

    #[test]
    fn each_pass_is_named_for_what_it_is_doing() {
        assert_eq!(Stage::Converting.label(), "converting");
        assert_eq!(Stage::Hashing.label(), "verifying");
    }

    #[test]
    fn a_pass_of_known_length_reports_how_far_through_it_is() {
        use crate::store::Progress;
        let at = |received, total| {
            proportion(&Progress {
                received,
                total: Some(total),
            })
        };
        assert_eq!(at(0, 1000), 0);
        assert_eq!(at(500, 1000), 50);
        assert_eq!(at(1000, 1000), 100);
    }

    #[test]
    fn a_pass_of_unknown_length_reports_nothing() {
        use crate::store::Progress;
        assert_eq!(
            proportion(&Progress {
                received: 42,
                total: None
            }),
            0
        );
        assert_eq!(
            proportion(&Progress {
                received: 42,
                total: Some(0)
            }),
            0
        );
        assert_eq!(
            proportion(&Progress {
                received: 4000,
                total: Some(10)
            }),
            100
        );
    }

    #[test]
    fn a_written_entry_is_one_the_catalogue_can_read() {
        let scratch = Scratch::new("entry");
        let digest = Digest::new(ALGORITHM, &"a".repeat(64));
        write_entry(
            &scratch.0,
            &instance("demo"),
            "mine",
            "latest",
            &digest,
            4096,
            None,
        )
        .unwrap();
        let catalogue = entry_is_readable(&scratch.0);
        let reference: Reference = "mine:latest".parse().unwrap();
        let (entry, artifact) = catalogue.resolve(&reference, "amd64").unwrap();
        assert_eq!(entry.name, "mine");
        assert!(entry.description.contains("demo"), "{}", entry.description);
        assert_eq!(artifact.digest, digest);
        assert_eq!(artifact.size, Some(4096));
    }

    #[test]
    fn a_given_description_replaces_the_default() {
        let scratch = Scratch::new("description");
        let digest = Digest::new(ALGORITHM, &"d".repeat(64));
        let text = "Trixie with \"tools\" at C:\\dev,\ttabbed and multi\nline";
        write_entry(
            &scratch.0,
            &instance("demo"),
            "mine",
            "latest",
            &digest,
            1,
            Some(text),
        )
        .unwrap();
        let catalogue = entry_is_readable(&scratch.0);
        let reference: Reference = "mine:latest".parse().unwrap();
        let (entry, _) = catalogue.resolve(&reference, "amd64").unwrap();
        assert_eq!(entry.description, text);
    }

    #[test]
    fn a_clone_asks_for_the_machine_its_disk_was_made_on() {
        let scratch = Scratch::new("machine");
        let digest = Digest::new(ALGORITHM, &"c".repeat(64));
        let mut held = instance("demo");
        held.firmware = crate::machine::Firmware::Uefi;
        held.cpu_model = "Penryn,vendor=GenuineIntel,+avx".to_owned();
        held.machine = crate::machine::Chipset::Pc;
        held.disk = crate::machine::Disk::Ide;
        write_entry(&scratch.0, &held, "mine", "latest", &digest, 1, None).unwrap();
        let catalogue = entry_is_readable(&scratch.0);
        let reference: Reference = "mine:latest".parse().unwrap();
        let (_, artifact) = catalogue.resolve(&reference, "amd64").unwrap();
        assert_eq!(artifact.firmware, crate::machine::Firmware::Uefi);
        assert_eq!(artifact.cpu_model(), "Penryn,vendor=GenuineIntel,+avx");
        assert_eq!(artifact.machine, crate::machine::Chipset::Pc);
        assert_eq!(artifact.disk, crate::machine::Disk::Ide);
    }

    #[test]
    fn a_clone_of_a_default_machine_states_no_machine() {
        let scratch = Scratch::new("defaultmachine");
        let digest = Digest::new(ALGORITHM, &"d".repeat(64));
        let path = write_entry(
            &scratch.0,
            &instance("demo"),
            "mine",
            "latest",
            &digest,
            1,
            None,
        )
        .unwrap();
        let text = fs::read_to_string(path).unwrap();
        assert!(!text.contains("firmware"), "{text}");
        assert!(!text.contains("cpu"), "{text}");
        assert!(!text.contains("machine"), "{text}");
        assert!(!text.contains("disk ="), "{text}");
    }

    #[test]
    fn a_cloned_entry_has_no_address_to_fetch_from() {
        let scratch = Scratch::new("nourl");
        let digest = Digest::new(ALGORITHM, &"b".repeat(64));
        write_entry(
            &scratch.0,
            &instance("demo"),
            "mine",
            "latest",
            &digest,
            1,
            None,
        )
        .unwrap();
        let catalogue = entry_is_readable(&scratch.0);
        let reference: Reference = "mine:latest".parse().unwrap();
        let (_, artifact) = catalogue.resolve(&reference, "amd64").unwrap();
        assert_eq!(artifact.url, None);
    }

    #[test]
    fn an_instance_that_takes_no_seed_clones_to_an_image_that_takes_none() {
        let scratch = Scratch::new("login");
        let mut held = instance("demo");
        held.seeded = false;
        let digest = Digest::new(ALGORITHM, &"c".repeat(64));
        write_entry(&scratch.0, &held, "mine", "latest", &digest, 1, None).unwrap();
        let catalogue = entry_is_readable(&scratch.0);
        let reference: Reference = "mine:latest".parse().unwrap();
        let (entry, _) = catalogue.resolve(&reference, "amd64").unwrap();
        assert!(!entry.login.is_seedable());
    }

    #[test]
    fn cloning_again_over_the_same_tag_replaces_the_entry() {
        let scratch = Scratch::new("again");
        let held = instance("demo");
        let first = Digest::new(ALGORITHM, &"d".repeat(64));
        let second = Digest::new(ALGORITHM, &"e".repeat(64));
        write_entry(&scratch.0, &held, "mine", "latest", &first, 1, None).unwrap();
        let path = write_entry(&scratch.0, &held, "mine", "latest", &second, 2, None).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap().matches("digest").count(),
            1
        );
        let catalogue = entry_is_readable(&scratch.0);
        let reference: Reference = "mine:latest".parse().unwrap();
        let (_, artifact) = catalogue.resolve(&reference, "amd64").unwrap();
        assert_eq!(artifact.digest, second);
    }

    #[test]
    fn two_tags_of_one_name_sit_beside_each_other() {
        let scratch = Scratch::new("tags");
        let held = instance("demo");
        let digest = Digest::new(ALGORITHM, &"f".repeat(64));
        write_entry(&scratch.0, &held, "mine", "one", &digest, 1, None).unwrap();
        write_entry(&scratch.0, &held, "mine", "two", &digest, 1, None).unwrap();
        assert_eq!(entry_is_readable(&scratch.0).entries().len(), 2);
    }
}

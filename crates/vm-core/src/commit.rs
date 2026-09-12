//! Turning a machine's disk back into an image.
//!
//! An instance writes to an overlay over the image it was built from. A commit
//! flattens the two into one standalone file, hashes it, and puts it in the
//! store under a name of the user's choosing.

use crate::error::{Error, Result};
use crate::instance::Instance;
use crate::reference::{Algorithm, Digest};
use crate::store::Store;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Local images are hashed with SHA-256. Nothing external publishes a sum for
/// them, so the only requirement is that it names the bytes.
pub const ALGORITHM: Algorithm = Algorithm::Sha256;

/// What a commit produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    pub name: String,
    pub tag: String,
    pub arch: String,
    pub digest: Digest,
    pub size: u64,
    pub path: PathBuf,
    pub entry: PathBuf,
}

/// Flattens an overlay and everything behind it into one image.
///
/// `qemu-img convert` reads through the backing chain, so what comes out
/// stands on its own and does not refer to the store copy it grew from.
///
/// The input format is stated rather than left to be probed. Left to guess,
/// `qemu-img` falls back to raw, so a damaged overlay converts successfully
/// into an image of its own wreckage instead of being refused.
pub fn convert(overlay: &Path, destination: &Path, in_use: bool) -> Result<()> {
    let mut command = Command::new("qemu-img");
    command
        .arg("convert")
        .arg("-q")
        .arg("-f")
        .arg("qcow2")
        .arg("-O")
        .arg("qcow2");
    if in_use {
        // A running hypervisor holds a write lock on the disk, and reading it
        // means saying so. This is only asked for when the machine is known to
        // be running: a stopped one keeps the lock check, so a disk something
        // else is still writing to is refused rather than read.
        command.arg("-U");
    }
    let output = command
        .arg(overlay)
        .arg(destination)
        .output()
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::MissingTool {
                    binary: "qemu-img",
                    package: "qemu-utils",
                    operation: "committing a virtual machine",
                }
            } else {
                Error::Launch {
                    program: "qemu-img".to_owned(),
                    source,
                }
            }
        })?;
    if output.status.success() {
        return Ok(());
    }
    let _ = fs::remove_file(destination);
    Err(Error::Commit {
        reason: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

/// Commits an instance's disk into the store under `name:tag`.
///
/// Whether the guest was paused first is the caller's business; by the time
/// this runs, the disk is as consistent as it is going to be.
pub fn commit(
    store: &Store,
    local: &Path,
    instance: &Instance,
    overlay: &Path,
    name: &str,
    tag: &str,
    in_use: bool,
) -> Result<Committed> {
    let staged = store.staging(&format!("{name}-{tag}"))?;
    let _ = fs::remove_file(&staged);
    convert(overlay, &staged, in_use)?;
    let digest = store.adopt(&staged, ALGORITHM)?;
    let path = store.path_for(&digest);
    let size = fs::metadata(&path).map_or(0, |data| data.len());
    let entry = write_entry(local, instance, name, tag, &digest, size)?;
    Ok(Committed {
        name: name.to_owned(),
        tag: tag.to_owned(),
        arch: instance.arch.clone(),
        digest,
        size,
        path,
        entry,
    })
}

/// Writes the catalogue entry that makes a committed image nameable.
///
/// The scalars come before the `[[image]]` table because TOML has no way to
/// write a bare key after one.
fn write_entry(
    local: &Path,
    instance: &Instance,
    name: &str,
    tag: &str,
    digest: &Digest,
    size: u64,
) -> Result<PathBuf> {
    let directory = local.join(name);
    fs::create_dir_all(&directory).map_err(|source| Error::Store {
        path: directory.clone(),
        action: "create",
        source,
    })?;
    let login = if instance.seeded {
        "cloud-init"
    } else {
        "none"
    };
    let body = format!(
        "name = \"{name}\"\n\
         tag = \"{tag}\"\n\
         description = \"Committed from '{}'\"\n\
         login = \"{login}\"\n\
         \n\
         [[image]]\n\
         arch = \"{}\"\n\
         format = \"qcow2\"\n\
         digest = \"{digest}\"\n\
         size = {size}\n",
        instance.name, instance.arch
    );
    let path = directory.join(format!("{tag}.toml"));
    fs::write(&path, body).map_err(|source| Error::Store {
        path: path.clone(),
        action: "write",
        source,
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::catalogue::Catalogue;
    use crate::reference::Reference;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("vm-commit-{label}-{}", std::process::id()));
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
            user: "vm".to_owned(),
            seeded: true,
            monitor: PathBuf::new(),
            ssh_port: None,
            pid: None,
            started: None,
            ports: Vec::new(),
            shares: Vec::new(),
        }
    }

    fn entry_is_readable(local: &Path) -> Catalogue {
        Catalogue::load(local).unwrap()
    }

    /// The flag that lets a disk be read while a hypervisor holds it. Asking
    /// for it when nothing holds the disk would hide a genuine clash.
    #[test]
    fn a_disk_in_use_is_read_without_taking_the_lock() {
        let scratch = Scratch::new("lock");
        let image = scratch.0.join("image.qcow2");
        let made = Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2"])
            .arg(&image)
            .arg("16M")
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return;
        }
        for in_use in [false, true] {
            let out = scratch.0.join(format!("out-{in_use}.qcow2"));
            convert(&image, &out, in_use).unwrap();
            assert!(out.is_file());
        }
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

    /// There is nowhere to fetch a local image from, and the absence has to
    /// survive being written out and read back.
    #[test]
    fn a_committed_entry_has_no_address_to_fetch_from() {
        let scratch = Scratch::new("nourl");
        let digest = Digest::new(ALGORITHM, &"b".repeat(64));
        write_entry(&scratch.0, &instance("demo"), "mine", "latest", &digest, 1).unwrap();
        let catalogue = entry_is_readable(&scratch.0);
        let reference: Reference = "mine:latest".parse().unwrap();
        let (_, artifact) = catalogue.resolve(&reference, "amd64").unwrap();
        assert_eq!(artifact.url, None);
    }

    #[test]
    fn an_instance_that_takes_no_seed_commits_to_an_image_that_takes_none() {
        let scratch = Scratch::new("login");
        let mut held = instance("demo");
        held.seeded = false;
        let digest = Digest::new(ALGORITHM, &"c".repeat(64));
        write_entry(&scratch.0, &held, "mine", "latest", &digest, 1).unwrap();
        let catalogue = entry_is_readable(&scratch.0);
        let reference: Reference = "mine:latest".parse().unwrap();
        let (entry, _) = catalogue.resolve(&reference, "amd64").unwrap();
        assert!(!entry.login.is_seedable());
    }

    #[test]
    fn committing_again_over_the_same_tag_replaces_the_entry() {
        let scratch = Scratch::new("again");
        let held = instance("demo");
        let first = Digest::new(ALGORITHM, &"d".repeat(64));
        let second = Digest::new(ALGORITHM, &"e".repeat(64));
        write_entry(&scratch.0, &held, "mine", "latest", &first, 1).unwrap();
        let path = write_entry(&scratch.0, &held, "mine", "latest", &second, 2).unwrap();
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
        write_entry(&scratch.0, &held, "mine", "one", &digest, 1).unwrap();
        write_entry(&scratch.0, &held, "mine", "two", &digest, 1).unwrap();
        assert_eq!(entry_is_readable(&scratch.0).entries().len(), 2);
    }

    /// Without an explicit input format this passes: `qemu-img` falls back to
    /// reading anything at all as a raw disk.
    #[test]
    fn converting_something_that_is_not_a_disk_is_refused() {
        let scratch = Scratch::new("notadisk");
        let overlay = scratch.0.join("not-a-disk");
        fs::write(&overlay, b"certainly not a qcow2").unwrap();
        let outcome = convert(&overlay, &scratch.0.join("out.qcow2"), false);
        match outcome {
            Err(error) => assert!(
                matches!(error.kind(), "commit-failed" | "missing-tool"),
                "{error}"
            ),
            Ok(()) => panic!("a text file should not convert to an image"),
        }
        assert!(
            !scratch.0.join("out.qcow2").exists(),
            "a failed conversion left a file behind"
        );
    }
}

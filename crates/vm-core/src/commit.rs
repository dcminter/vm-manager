//! Turning a machine's disk back into an image.
//!
//! An instance writes to an overlay over the image it was built from. A commit
//! flattens the two into one standalone file, hashes it, and puts it in the
//! store under a name of the user's choosing.

use crate::error::{Error, Result};
use crate::instance::Instance;
use crate::reference::{Algorithm, Digest, Reference};
use crate::store::Store;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How far along a conversion is, in whole percent.
pub type Converting<'a> = &'a mut dyn FnMut(u8);

/// Which pass a commit is in the middle of.
///
/// There are two, and they take comparable time on a large image: the disk is
/// flattened, and then read back to be hashed, because an image is addressed
/// by its digest and the digest cannot be known before the bytes exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Converting,
    Hashing,
}

impl Stage {
    /// What to call the pass to a user. The second one is hashing, but what it
    /// is for is establishing that the image is the bytes it claims to be, and
    /// that is the part worth waiting through.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Converting => "converting",
            Self::Hashing => "verifying",
        }
    }
}

/// How far along a commit is: which pass, and how much of it is done.
pub type Reporter<'a> = &'a mut dyn FnMut(Stage, u8);

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
pub fn convert(
    overlay: &Path,
    destination: &Path,
    in_use: bool,
    report: Option<Converting<'_>>,
) -> Result<()> {
    let mut command = Command::new("qemu-img");
    command
        .arg("convert")
        // `-p` writes a percentage whether or not it is talking to a terminal,
        // rewriting one line with a carriage return. It is asked for either
        // way so that there is only one code path to have got right.
        .arg("-p")
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
    let mut child = command
        .arg(overlay)
        .arg(destination)
        .stdout(std::process::Stdio::piped())
        // Held rather than inherited so that a failure is reported as an error
        // of ours rather than printed over whatever else is on the terminal.
        // Nothing here writes enough of it to fill a pipe.
        .stderr(std::process::Stdio::piped())
        .spawn()
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
    if let (Some(stdout), Some(report)) = (child.stdout.take(), report) {
        watch(stdout, report);
    }
    let output = child.wait_with_output().map_err(|source| Error::Launch {
        program: "qemu-img".to_owned(),
        source,
    })?;
    if output.status.success() {
        return Ok(());
    }
    let _ = fs::remove_file(destination);
    Err(Error::Commit {
        reason: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

/// Reads the conversion's progress until it stops writing any.
///
/// This is also what waits for the child to get on with it: nothing else is
/// read until its output ends, which it does when the process does.
fn watch(stdout: std::process::ChildStdout, report: Converting<'_>) {
    use std::io::BufRead as _;
    let mut reader = std::io::BufReader::new(stdout);
    let mut buffer = Vec::new();
    while reader
        .read_until(b'\r', &mut buffer)
        .is_ok_and(|read| read > 0)
    {
        if let Some(percent) = percentage(&String::from_utf8_lossy(&buffer)) {
            report(percent);
        }
        buffer.clear();
    }
}

/// The whole percent out of `    (37.50/100%)`.
///
/// The fraction is dropped rather than rounded, because this is read only to
/// decide what to draw and a bar has nowhere to put it.
fn percentage(text: &str) -> Option<u8> {
    let open = text.rfind('(')?;
    let slash = text.get(open..)?.find('/')? + open;
    if !text.get(slash..)?.starts_with("/100%") {
        return None;
    }
    let figure = text.get(open + 1..slash)?;
    let whole = figure.split_once('.').map_or(figure, |(whole, _)| whole);
    whole.parse().ok()
}

/// How much of a known total has been read, in whole percent. A pass whose
/// length is not known reports nothing rather than a figure it made up.
fn proportion(progress: &crate::store::Progress) -> u8 {
    let Some(total) = progress.total.filter(|total| *total > 0) else {
        return 0;
    };
    let percent = progress.received.saturating_mul(100) / total;
    u8::try_from(percent.min(100)).unwrap_or(100)
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
    target: &Reference,
    in_use: bool,
    mut report: Option<Reporter<'_>>,
) -> Result<Committed> {
    let (name, tag) = (target.repository(), target.tag());
    let staged = store.staging(&format!("{name}-{tag}"))?;
    let _ = fs::remove_file(&staged);
    let mut say = |stage, percent| {
        if let Some(report) = report.as_deref_mut() {
            report(stage, percent);
        }
    };
    convert(
        overlay,
        &staged,
        in_use,
        Some(&mut |percent| say(Stage::Converting, percent)),
    )?;
    let digest = store.adopt(&staged, ALGORITHM, &mut |progress| {
        say(Stage::Hashing, proportion(&progress));
    })?;
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
            generation: 0,
            ports: Vec::new(),
            shares: Vec::new(),
        }
    }

    fn entry_is_readable(local: &Path) -> Catalogue {
        Catalogue::load(local).unwrap()
    }

    #[test]
    fn a_progress_line_yields_its_whole_percent() {
        assert_eq!(percentage("    (37.50/100%)"), Some(37));
        assert_eq!(percentage("    (0.00/100%)"), Some(0));
        assert_eq!(percentage("    (100.00/100%)"), Some(100));
    }

    /// Anything else on the stream is not progress and must not be read as it.
    #[test]
    fn a_line_that_is_not_progress_yields_nothing() {
        for text in ["", "(", "()", "(x.00/100%)", "(50.00/50%)", "(50.00)"] {
            assert_eq!(percentage(text), None, "{text}");
        }
    }

    /// The digits alone would parse out of a byte count as readily as out of a
    /// percentage; the shape is what identifies it.
    #[test]
    fn a_figure_too_large_for_a_percentage_is_refused() {
        assert_eq!(percentage("(300.00/100%)"), None);
    }

    /// A conversion that runs reports at least that it finished. `qemu-img`
    /// writes a first line before any work and a last one after all of it.
    #[test]
    fn a_conversion_reports_its_progress() {
        let scratch = Scratch::new("progress");
        let image = scratch.0.join("image.qcow2");
        let made = Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2"])
            .arg(&image)
            .arg("64M")
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return;
        }
        let mut seen = Vec::new();
        convert(
            &image,
            &scratch.0.join("out.qcow2"),
            false,
            Some(&mut |percent| seen.push(percent)),
        )
        .unwrap();
        assert_eq!(seen.last(), Some(&100), "{seen:?}");
        assert!(seen.windows(2).all(|pair| pair[0] <= pair[1]), "{seen:?}");
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

    /// Nothing to measure against, so there is no figure to give. Neither a
    /// zero total nor a runaway count may produce one out of range.
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
            convert(&image, &out, in_use, None).unwrap();
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
        let outcome = convert(&overlay, &scratch.0.join("out.qcow2"), false, None);
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

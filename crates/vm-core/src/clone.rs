//! Flattening a machine's disk into a new image.

use crate::error::{Error, Result};
use crate::instance::Instance;
use crate::reference::{Algorithm, Digest, Reference};
use crate::store::Store;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How far along a conversion is, in whole percent.
pub type Converting<'a> = &'a mut dyn FnMut(u8);

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

/// Flattens an overlay and its backing chain into one image, stating the input format so damage is refused.
pub fn convert(
    overlay: &Path,
    destination: &Path,
    in_use: bool,
    report: Option<Converting<'_>>,
) -> Result<()> {
    let mut command = Command::new("qemu-img");
    command
        .arg("convert")
        // Progress, on one line rewritten by carriage returns.
        .arg("-p")
        .arg("-f")
        .arg("qcow2")
        .arg("-O")
        .arg("qcow2");
    if in_use {
        // Bypasses the lock a running hypervisor holds.
        command.arg("-U");
    }
    let mut child = command
        .arg(overlay)
        .arg(destination)
        .stdout(std::process::Stdio::piped())
        // Captured to report as an error.
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::MissingTool {
                    binary: "qemu-img",
                    package: "qemu-utils",
                    operation: "cloning a virtual machine",
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
    Err(Error::Clone {
        reason: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

/// Reads the conversion's progress until the process ends.
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

/// The whole percent from `    (37.50/100%)`.
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

/// Whole percent of a known total, or zero when the total is unknown.
fn proportion(progress: &crate::store::Progress) -> u8 {
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
    let description =
        description.map_or_else(|| format!("Cloned from '{}'", instance.name), str::to_owned);
    let mut body = format!(
        "name = \"{name}\"\n\
         tag = \"{tag}\"\n\
         description = {}\n\
         login = \"{login}\"\n\
         \n\
         [[image]]\n\
         arch = \"{}\"\n\
         format = \"qcow2\"\n\
         digest = \"{digest}\"\n\
         size = {size}\n",
        toml_string(&description),
        instance.arch
    );
    // The disk was made to boot on this machine, so a clone asks for the same one.
    if !instance.firmware.is_default() {
        let _ = std::fmt::Write::write_fmt(
            &mut body,
            format_args!("firmware = \"{}\"\n", instance.firmware.name()),
        );
    }
    if !instance.machine.is_default() {
        let _ = std::fmt::Write::write_fmt(
            &mut body,
            format_args!("machine = \"{}\"\n", instance.machine.name()),
        );
    }
    if !instance.disk.is_default() {
        let _ = std::fmt::Write::write_fmt(
            &mut body,
            format_args!("disk = \"{}\"\n", instance.disk.name()),
        );
    }
    if instance.cpu != crate::machine::DEFAULT_CPU {
        let _ = std::fmt::Write::write_fmt(&mut body, format_args!("cpu = \"{}\"\n", instance.cpu));
    }
    let path = directory.join(format!("{tag}.toml"));
    fs::write(&path, body).map_err(|source| Error::Store {
        path: path.clone(),
        action: "write",
        source,
    })?;
    Ok(path)
}

/// A TOML basic string holding `text`.
fn toml_string(text: &str) -> String {
    let mut quoted = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            control if control.is_control() => {
                let _ = std::fmt::Write::write_fmt(
                    &mut quoted,
                    format_args!("\\u{:04X}", u32::from(control)),
                );
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
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
            cpu: "max".to_owned(),
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
            password: None,
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

    #[test]
    fn a_figure_too_large_for_a_percentage_is_refused() {
        assert_eq!(percentage("(300.00/100%)"), None);
    }

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
    fn a_toml_string_escapes_what_would_end_or_break_it() {
        assert_eq!(toml_string("plain"), r#""plain""#);
        assert_eq!(toml_string(r#"a "b" \c"#), r#""a \"b\" \\c""#);
        assert_eq!(toml_string("one\ntwo"), r#""one\u000Atwo""#);
    }

    #[test]
    fn a_clone_asks_for_the_machine_its_disk_was_made_on() {
        let scratch = Scratch::new("machine");
        let digest = Digest::new(ALGORITHM, &"c".repeat(64));
        let mut held = instance("demo");
        held.firmware = crate::machine::Firmware::Uefi;
        held.cpu = "Penryn,vendor=GenuineIntel,+avx".to_owned();
        held.machine = crate::machine::Chipset::Pc;
        held.disk = crate::machine::Disk::Ide;
        write_entry(&scratch.0, &held, "mine", "latest", &digest, 1, None).unwrap();
        let catalogue = entry_is_readable(&scratch.0);
        let reference: Reference = "mine:latest".parse().unwrap();
        let (_, artifact) = catalogue.resolve(&reference, "amd64").unwrap();
        assert_eq!(artifact.firmware, crate::machine::Firmware::Uefi);
        assert_eq!(artifact.cpu(), "Penryn,vendor=GenuineIntel,+avx");
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

    #[test]
    fn converting_something_that_is_not_a_disk_is_refused() {
        let scratch = Scratch::new("notadisk");
        let overlay = scratch.0.join("not-a-disk");
        fs::write(&overlay, b"certainly not a qcow2").unwrap();
        let outcome = convert(&overlay, &scratch.0.join("out.qcow2"), false, None);
        match outcome {
            Err(error) => assert!(
                matches!(error.kind(), "clone-failed" | "missing-tool"),
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

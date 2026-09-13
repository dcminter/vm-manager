//! Rewriting images as qcow2, and finding out what an image file is.

use crate::compression::Compression;
use crate::error::{Error, Result};
use crate::value::Value;
use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;
use std::process::{Command, Stdio};

/// How far along a conversion is, in whole percent.
pub type Converting<'a> = &'a mut dyn FnMut(u8);

/// One file to rewrite as qcow2.
#[derive(Debug, Clone, Copy)]
pub struct Conversion<'a> {
    pub source: &'a Path,
    /// Stated rather than probed, so a damaged file is refused instead of guessed at.
    pub format: &'a str,
    pub destination: &'a Path,
    /// Whether a running hypervisor holds the source's lock.
    pub in_use: bool,
    /// What `qemu-img` is needed for, should it be missing.
    pub operation: &'static str,
}

/// Rewrites an image, and any backing chain it has, as one qcow2 file.
pub fn convert(conversion: &Conversion<'_>, report: Option<Converting<'_>>) -> Result<()> {
    let mut command = Command::new("qemu-img");
    command
        .arg("convert")
        // Progress, on one line rewritten by carriage returns.
        .arg("-p")
        .arg("-f")
        .arg(conversion.format)
        .arg("-O")
        .arg("qcow2");
    if conversion.in_use {
        // Bypasses the lock a running hypervisor holds.
        command.arg("-U");
    }
    let mut child = command
        .arg(conversion.source)
        .arg(conversion.destination)
        .stdout(Stdio::piped())
        // Captured to report as an error.
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| launch_failure(source, conversion.operation))?;
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
    let _ = fs::remove_file(conversion.destination);
    Err(Error::Convert {
        reason: complaint(&output),
    })
}

fn launch_failure(source: std::io::Error, operation: &'static str) -> Error {
    if source.kind() == std::io::ErrorKind::NotFound {
        Error::MissingTool {
            binary: "qemu-img",
            package: "qemu-utils",
            operation,
        }
    } else {
        Error::Launch {
            program: "qemu-img".to_owned(),
            source,
        }
    }
}

fn complaint(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_owned()
}

/// What `qemu-img` makes of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub format: String,
    /// Other files the image reads: a backing file, or extents beside a descriptor.
    pub external: Vec<String>,
}

/// Asks `qemu-img` what format a file is in.
pub fn probe(path: &Path, operation: &'static str) -> Result<Probe> {
    let output = Command::new("qemu-img")
        .args(["info", "--output=json"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| launch_failure(source, operation))?;
    if !output.status.success() {
        return Err(Error::Convert {
            reason: complaint(&output),
        });
    }
    parse_probe(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| Error::Convert {
        reason: "qemu-img did not say what format the file is in".to_owned(),
    })
}

fn parse_probe(json: &str) -> Option<Probe> {
    let value = crate::value::from_json(json).ok()?;
    let format = value.get("format")?.as_str()?.to_owned();
    let itself = value.get("filename").and_then(Value::as_str);
    let children = match value.get("children") {
        Some(Value::List(children)) => children.as_slice(),
        _ => &[],
    };
    let mut external: Vec<String> = Vec::new();
    let named = value.get("backing-filename").into_iter().chain(
        children
            .iter()
            .filter_map(|child| child.get("info")?.get("filename")),
    );
    for name in named.filter_map(Value::as_str) {
        if Some(name) != itself && !external.iter().any(|held| held == name) {
            external.push(name.to_owned());
        }
    }
    Some(Probe { format, external })
}

/// Where an ISO 9660 filesystem keeps its identifier.
const ISO_MARK_OFFSET: u64 = 32769;

/// Whether a file holds an ISO 9660 filesystem, as a CD-ROM image does.
pub fn is_iso(path: &Path) -> bool {
    let mut mark = [0_u8; 5];
    fs::File::open(path)
        .and_then(|mut file| {
            file.seek(SeekFrom::Start(ISO_MARK_OFFSET))?;
            file.read_exact(&mut mark)
        })
        .is_ok_and(|()| &mark == b"CD001")
}

/// The compression a file's first bytes show.
pub fn sniff(head: &[u8]) -> Compression {
    if head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        Compression::Xz
    } else if head.starts_with(&[0x1f, 0x8b]) {
        Compression::Gzip
    } else if head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Compression::Zstd
    } else {
        Compression::None
    }
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("vm-conversion-{label}-{}", std::process::id()));
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

    /// Makes an image with `qemu-img`, or `None` where it is not installed.
    fn made(scratch: &Scratch, name: &str, format: &str, size: &str) -> Option<PathBuf> {
        let image = scratch.0.join(name);
        Command::new("qemu-img")
            .args(["create", "-q", "-f", format])
            .arg(&image)
            .arg(size)
            .status()
            .is_ok_and(|status| status.success())
            .then_some(image)
    }

    fn conversion<'a>(source: &'a Path, format: &'a str, destination: &'a Path) -> Conversion<'a> {
        Conversion {
            source,
            format,
            destination,
            in_use: false,
            operation: "testing",
        }
    }

    #[test]
    fn a_conversion_reports_its_progress() {
        let scratch = Scratch::new("progress");
        let Some(image) = made(&scratch, "image.qcow2", "qcow2", "64M") else {
            return;
        };
        let mut seen = Vec::new();
        let out = scratch.0.join("out.qcow2");
        convert(
            &conversion(&image, "qcow2", &out),
            Some(&mut |percent| seen.push(percent)),
        )
        .unwrap();
        assert_eq!(seen.last(), Some(&100), "{seen:?}");
        assert!(seen.windows(2).all(|pair| pair[0] <= pair[1]), "{seen:?}");
    }

    #[test]
    fn a_disk_in_use_is_read_without_taking_the_lock() {
        let scratch = Scratch::new("lock");
        let Some(image) = made(&scratch, "image.qcow2", "qcow2", "16M") else {
            return;
        };
        for in_use in [false, true] {
            let out = scratch.0.join(format!("out-{in_use}.qcow2"));
            convert(
                &Conversion {
                    in_use,
                    ..conversion(&image, "qcow2", &out)
                },
                None,
            )
            .unwrap();
            assert!(out.is_file());
        }
    }

    #[test]
    fn other_formats_become_qcow2() {
        let scratch = Scratch::new("formats");
        for format in ["raw", "vmdk", "vdi", "vpc", "vhdx"] {
            let Some(image) = made(&scratch, &format!("image.{format}"), format, "8M") else {
                return;
            };
            assert_eq!(probe(&image, "testing").unwrap().format, format);
            let out = scratch.0.join(format!("{format}.qcow2"));
            convert(&conversion(&image, format, &out), None).unwrap();
            let probed = probe(&out, "testing").unwrap();
            assert_eq!(probed.format, "qcow2", "{format}");
            assert!(probed.external.is_empty(), "{format}");
        }
    }

    #[test]
    fn a_layered_image_names_what_it_is_layered_over_and_converts_to_one_file() {
        let scratch = Scratch::new("backing");
        let Some(base) = made(&scratch, "base.qcow2", "qcow2", "8M") else {
            return;
        };
        let top = scratch.0.join("top.qcow2");
        let layered = Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2", "-F", "qcow2", "-b"])
            .arg(&base)
            .arg(&top)
            .status()
            .unwrap();
        assert!(layered.success());
        let probed = probe(&top, "testing").unwrap();
        assert_eq!(probed.external, [base.display().to_string()]);
        let out = scratch.0.join("flat.qcow2");
        convert(&conversion(&top, "qcow2", &out), None).unwrap();
        assert!(probe(&out, "testing").unwrap().external.is_empty());
    }

    #[test]
    fn a_descriptor_names_the_extents_it_reads() {
        let scratch = Scratch::new("extents");
        let descriptor = scratch.0.join("split.vmdk");
        let made = Command::new("qemu-img")
            .args([
                "create",
                "-q",
                "-f",
                "vmdk",
                "-o",
                "subformat=monolithicFlat",
            ])
            .arg(&descriptor)
            .arg("1M")
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return;
        }
        let probed = probe(&descriptor, "testing").unwrap();
        assert_eq!(probed.format, "vmdk");
        assert_eq!(
            probed.external,
            [scratch.0.join("split-flat.vmdk").display().to_string()]
        );
    }

    #[test]
    fn converting_something_that_is_not_a_disk_is_refused() {
        let scratch = Scratch::new("notadisk");
        let overlay = scratch.0.join("not-a-disk");
        fs::write(&overlay, b"certainly not a qcow2").unwrap();
        let out = scratch.0.join("out.qcow2");
        let outcome = convert(&conversion(&overlay, "qcow2", &out), None);
        match outcome {
            Err(error) => assert!(
                matches!(error.kind(), "conversion-failed" | "missing-tool"),
                "{error}"
            ),
            Ok(()) => panic!("a text file should not convert to an image"),
        }
        assert!(!out.exists(), "a failed conversion left a file behind");
    }

    #[test]
    fn probing_what_is_not_there_is_refused() {
        let scratch = Scratch::new("probenone");
        assert!(probe(&scratch.0.join("absent"), "testing").is_err());
    }

    #[test]
    fn a_probe_reads_the_format_and_any_backing_file() {
        let probed = parse_probe(
            r#"{"format": "vmdk", "filename": "a", "children": [{"name": "file", "info": {"filename": "a"}}]}"#,
        )
        .unwrap();
        assert_eq!(probed.format, "vmdk");
        assert!(probed.external.is_empty());
        let probed = parse_probe(
            r#"{"format": "qcow2", "filename": "a", "backing-filename": "/etc/x", "children": [{"name": "extents.0", "info": {"filename": "/etc/x"}}]}"#,
        )
        .unwrap();
        assert_eq!(probed.external, ["/etc/x"]);
        assert_eq!(parse_probe(r#"{"virtual-size": 1}"#), None);
        assert_eq!(parse_probe("not json"), None);
    }

    #[test]
    fn an_iso_is_known_by_its_filesystem_mark() {
        let scratch = Scratch::new("iso");
        let iso = scratch.0.join("disc.iso");
        let mut bytes = vec![0_u8; 40_000];
        bytes[32769..32774].copy_from_slice(b"CD001");
        fs::write(&iso, &bytes).unwrap();
        assert!(is_iso(&iso));
        bytes[32769] = b'X';
        fs::write(&iso, &bytes).unwrap();
        assert!(!is_iso(&iso));
        fs::write(&iso, b"CD001").unwrap();
        assert!(!is_iso(&iso));
        assert!(!is_iso(&scratch.0.join("absent")));
    }

    #[test]
    fn compression_is_known_by_its_first_bytes() {
        assert_eq!(
            sniff(&[0xfd, b'7', b'z', b'X', b'Z', 0, 1]),
            Compression::Xz
        );
        assert_eq!(sniff(&[0x1f, 0x8b, 8]), Compression::Gzip);
        assert_eq!(sniff(&[0x28, 0xb5, 0x2f, 0xfd, 0]), Compression::Zstd);
        assert_eq!(sniff(b"QFI\xfb"), Compression::None);
        assert_eq!(sniff(&[0x1f]), Compression::None);
        assert_eq!(sniff(&[]), Compression::None);
    }
}

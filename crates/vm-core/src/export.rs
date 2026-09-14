//! Copying an image out of the store.

use crate::catalogue::Artifact;
use crate::compression::Compression;
use crate::digest::Hashing;
use crate::error::{Error, Result};
use crate::store::{Progress, Reporter};
use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Suffixes and the stored format each names, with archives naming none.
const SUFFIXES: [(&str, Option<&str>); 17] = [
    ("qcow2", Some("qcow2")),
    ("qcow", Some("qcow2")),
    ("img", Some("raw")),
    ("raw", Some("raw")),
    ("iso", Some("iso")),
    ("vmdk", Some("vmdk")),
    ("vdi", Some("vdi")),
    ("vhdx", Some("vhdx")),
    ("vhd", Some("vpc")),
    ("vpc", Some("vpc")),
    ("qed", Some("qed")),
    ("gz", None),
    ("xz", None),
    ("zst", None),
    ("zip", None),
    ("tar", None),
    ("ova", None),
];

/// The format an artifact is stored in, as its suffixes name it.
pub fn stored_format(artifact: &Artifact) -> &str {
    if artifact.media.is_cdrom() {
        "iso"
    } else if artifact.source_format.is_some() {
        "qcow2"
    } else {
        &artifact.format
    }
}

/// The suffix given to an exported artifact.
pub fn suffix(artifact: &Artifact) -> &'static str {
    let format = stored_format(artifact);
    SUFFIXES
        .iter()
        .find(|(_, named)| *named == Some(format))
        .map_or("img", |(suffix, _)| suffix)
}

/// How a file name's suffix bears on an export.
enum Named {
    /// No suffix an image file carries.
    Unknown,
    /// A suffix for the stored format.
    Image,
    /// The suffix of the compression asked for.
    Compressed,
    /// A suffix for anything else.
    Other(String),
}

fn named(
    extension: Option<&std::ffi::OsStr>,
    artifact: &Artifact,
    compression: Compression,
) -> Named {
    let Some(extension) = extension
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
    else {
        return Named::Unknown;
    };
    if compression.suffix() == Some(extension.as_str()) {
        return Named::Compressed;
    }
    match SUFFIXES.iter().find(|(suffix, _)| *suffix == extension) {
        None => Named::Unknown,
        Some((_, Some(format))) if *format == stored_format(artifact) => Named::Image,
        Some(_) => Named::Other(extension),
    }
}

/// Where to write, and any suffix added where the name given lacks one.
pub fn destination(
    given: &Path,
    artifact: &Artifact,
    compression: Compression,
    name: &str,
    tag: &str,
) -> Result<(PathBuf, Option<String>)> {
    let image = suffix(artifact);
    let full = compression
        .suffix()
        .map_or_else(|| image.to_owned(), |outer| format!("{image}.{outer}"));
    if given.is_dir() {
        return Ok((given.join(format!("{name}-{tag}.{full}")), None));
    }
    let appended = |suffix: &str| {
        let mut path = given.as_os_str().to_owned();
        path.push(format!(".{suffix}"));
        (PathBuf::from(path), Some(suffix.to_owned()))
    };
    let refused = |suffix: String| Error::SuffixMismatch {
        path: given.to_owned(),
        suffix,
        exported: describe(artifact, compression),
        expected: full.clone(),
    };
    let inner = || {
        named(
            Path::new(given.file_stem().unwrap_or_default()).extension(),
            artifact,
            Compression::None,
        )
    };
    match (
        named(given.extension(), artifact, compression),
        compression.suffix(),
    ) {
        (Named::Unknown, _) => Ok(appended(&full)),
        (Named::Image, None) => Ok((given.to_owned(), None)),
        (Named::Image, Some(outer)) => Ok(appended(outer)),
        (Named::Compressed, _) => match inner() {
            Named::Other(suffix) => Err(refused(suffix)),
            _ => Ok((given.to_owned(), None)),
        },
        (Named::Other(suffix), _) => Err(refused(suffix)),
    }
}

fn describe(artifact: &Artifact, compression: Compression) -> String {
    let stored = if artifact.media.is_cdrom() {
        "an ISO CD-ROM image".to_owned()
    } else {
        format!("a {} image", stored_format(artifact))
    };
    match compression {
        Compression::None => stored,
        scheme => format!("{stored} compressed with {}", scheme.name()),
    }
}

/// What an export wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exported {
    pub path: PathBuf,
    /// Bytes of image read, before any compression.
    pub size: u64,
    /// Whether the stored file was checked against the digest it is held under.
    pub verified: bool,
}

/// Bytes copied at a time, and the smallest hole left.
const PIECE: usize = 1 << 20;

/// Copies a stored image, compressed or with holes where it is empty.
pub fn copy(
    stored: &Path,
    artifact: &Artifact,
    destination: &Path,
    compression: Compression,
    force: bool,
    report: Reporter<'_>,
) -> Result<Exported> {
    if !force && fs::symlink_metadata(destination).is_ok() {
        return Err(Error::OutputExists {
            path: destination.to_owned(),
        });
    }
    let mut partial = destination.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    let outcome = write(stored, artifact, &partial, compression, report).and_then(|exported| {
        fs::rename(&partial, destination).map_err(|source| Error::Export {
            path: destination.to_owned(),
            source,
        })?;
        Ok(Exported {
            path: destination.to_owned(),
            ..exported
        })
    });
    if outcome.is_err() {
        let _ = fs::remove_file(&partial);
    }
    outcome
}

/// Where the copied bytes go.
enum Sink {
    Sparse(fs::File),
    Compressor {
        child: std::process::Child,
        input: std::process::ChildStdin,
        scheme: Compression,
    },
}

impl Sink {
    fn open(partial: &Path, compression: Compression) -> Result<Self> {
        let file = fs::File::create(partial).map_err(|source| Error::Export {
            path: partial.to_owned(),
            source,
        })?;
        let Some(mut command) = compression.compressor() else {
            return Ok(Self::Sparse(file));
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::from(file))
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| compression.missing_compressor(&source))?;
        let input = child.stdin.take().ok_or_else(|| Error::Compress {
            scheme: compression.name(),
            reason: "the compressor was given no input pipe".to_owned(),
        })?;
        Ok(Self::Compressor {
            child,
            input,
            scheme: compression,
        })
    }

    fn put(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        match self {
            Self::Sparse(file) if bytes.iter().all(|byte| *byte == 0) => file
                .seek(SeekFrom::Current(
                    i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                ))
                .map(|_| ()),
            Self::Sparse(file) => file.write_all(bytes),
            Self::Compressor { input, .. } => input.write_all(bytes),
        }
    }

    fn finish(self, partial: &Path, length: u64) -> Result<()> {
        let unwritable = |source| Error::Export {
            path: partial.to_owned(),
            source,
        };
        match self {
            Self::Sparse(file) => {
                file.set_len(length).map_err(unwritable)?;
                file.sync_all().map_err(unwritable)
            }
            Self::Compressor {
                child,
                input,
                scheme,
            } => {
                // The compressor finishes only once its input is closed.
                drop(input);
                let finished = child.wait_with_output().map_err(unwritable)?;
                if !finished.status.success() {
                    return Err(Error::Compress {
                        scheme: scheme.name(),
                        reason: String::from_utf8_lossy(&finished.stderr).trim().to_owned(),
                    });
                }
                fs::File::open(partial)
                    .and_then(|file| file.sync_all())
                    .map_err(unwritable)
            }
        }
    }

    /// Why a write failed, preferring what a compressor that stopped reading said.
    fn failure(self, partial: &Path, source: std::io::Error) -> Error {
        match self {
            Self::Compressor {
                child,
                input,
                scheme,
            } => {
                drop(input);
                let said = child
                    .wait_with_output()
                    .map(|finished| String::from_utf8_lossy(&finished.stderr).trim().to_owned())
                    .unwrap_or_default();
                Error::Compress {
                    scheme: scheme.name(),
                    reason: if said.is_empty() {
                        source.to_string()
                    } else {
                        said
                    },
                }
            }
            Self::Sparse(_) => Error::Export {
                path: partial.to_owned(),
                source,
            },
        }
    }
}

fn write(
    stored: &Path,
    artifact: &Artifact,
    partial: &Path,
    compression: Compression,
    report: Reporter<'_>,
) -> Result<Exported> {
    let unreadable = |source| Error::Store {
        path: stored.to_owned(),
        action: "read",
        source,
    };
    let mut input = fs::File::open(stored).map_err(unreadable)?;
    let total = input.metadata().map_err(unreadable)?.len();
    let mut sink = Sink::open(partial, compression)?;
    // The digest covers the stored file only when it is held as published.
    let verified = artifact.compression.is_none() && artifact.source_format.is_none();
    let mut hashing = Hashing::new(std::io::sink(), artifact.digest.algorithm());
    let mut piece = vec![0_u8; PIECE];
    let mut copied = 0_u64;
    loop {
        let count = read_piece(&mut input, &mut piece).map_err(unreadable)?;
        if count == 0 {
            break;
        }
        let bytes = &piece[..count];
        if verified {
            hashing.write_all(bytes).map_err(unreadable)?;
        }
        if let Err(source) = sink.put(bytes) {
            return Err(sink.failure(partial, source));
        }
        copied += count as u64;
        report(Progress {
            received: copied,
            total: Some(total),
        });
    }
    sink.finish(partial, copied)?;
    if verified {
        let (_, hash) = hashing.finish();
        if !crate::digest::matches(&artifact.digest, &hash) {
            return Err(Error::DamagedImage {
                path: stored.to_owned(),
                expected: artifact.digest.to_string(),
            });
        }
    }
    Ok(Exported {
        path: partial.to_owned(),
        size: copied,
        verified,
    })
}

/// Fills `piece` unless the file ends first, returning how much was read.
fn read_piece(input: &mut fs::File, piece: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < piece.len() {
        match input.read(&mut piece[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::catalogue::Media;
    use crate::compression::Compression;
    use crate::machine::{Chipset, Disk, Firmware};
    use crate::reference::{Algorithm, Digest};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("vm-export-{label}-{}", std::process::id()));
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

    fn sha256(bytes: &[u8]) -> Digest {
        let mut ignored = |_| {};
        let (_, hash) = crate::digest::copy_hashing(
            &mut &bytes[..],
            std::io::sink(),
            Algorithm::Sha256,
            &mut ignored,
        )
        .unwrap();
        Digest::new(Algorithm::Sha256, &hash)
    }

    fn artifact(format: &str) -> Artifact {
        Artifact {
            arch: "amd64".to_owned(),
            format: format.to_owned(),
            url: None,
            digest: sha256(b""),
            size: None,
            compression: Compression::None,
            source_format: None,
            media: Media::Disk,
            firmware: Firmware::Bios,
            cpu_model: None,
            machine: Chipset::Q35,
            disk: Disk::Virtio,
        }
    }

    fn cdrom() -> Artifact {
        Artifact {
            media: Media::Cdrom,
            ..artifact("raw")
        }
    }

    fn resolved(given: &str, artifact: &Artifact) -> Result<(PathBuf, bool)> {
        destination(
            Path::new(given),
            artifact,
            Compression::None,
            "debian",
            "trixie",
        )
        .map(|(path, added)| (path, added.is_some()))
    }

    fn compressed(
        given: &str,
        artifact: &Artifact,
        scheme: Compression,
    ) -> Result<(PathBuf, Option<String>)> {
        destination(Path::new(given), artifact, scheme, "debian", "trixie")
    }

    #[test]
    fn each_stored_format_has_its_suffix() {
        assert_eq!(suffix(&artifact("qcow2")), "qcow2");
        assert_eq!(suffix(&artifact("raw")), "img");
        assert_eq!(suffix(&artifact("vmdk")), "vmdk");
        assert_eq!(suffix(&artifact("vpc")), "vhd");
        assert_eq!(suffix(&cdrom()), "iso");
        let converted = Artifact {
            source_format: Some("vmdk".to_owned()),
            ..artifact("qcow2")
        };
        assert_eq!(suffix(&converted), "qcow2");
        assert_eq!(suffix(&artifact("parallels")), "img");
    }

    #[test]
    fn a_name_without_an_image_suffix_gains_one() {
        assert_eq!(
            resolved("/out/disk", &artifact("qcow2")).unwrap(),
            (PathBuf::from("/out/disk.qcow2"), true)
        );
        assert_eq!(
            resolved("/out/trixie.13", &artifact("qcow2")).unwrap(),
            (PathBuf::from("/out/trixie.13.qcow2"), true)
        );
        assert_eq!(
            resolved("installer", &cdrom()).unwrap(),
            (PathBuf::from("installer.iso"), true)
        );
    }

    #[test]
    fn a_suffix_for_the_stored_format_is_kept_whatever_its_case() {
        for (given, stored) in [
            ("/out/disk.qcow2", artifact("qcow2")),
            ("/out/disk.QCOW2", artifact("qcow2")),
            ("/out/disk.qcow", artifact("qcow2")),
            ("/out/disk.raw", artifact("raw")),
            ("/out/disk.img", artifact("raw")),
            ("/out/disc.iso", cdrom()),
            ("/out/pd.vmdk", artifact("vmdk")),
        ] {
            assert_eq!(
                resolved(given, &stored).unwrap(),
                (PathBuf::from(given), false),
                "{given}"
            );
        }
    }

    #[test]
    fn a_suffix_for_another_format_is_refused() {
        for (given, stored, expected) in [
            ("/out/disk.vmdk", artifact("qcow2"), ".qcow2"),
            ("/out/disk.iso", artifact("raw"), ".img"),
            ("/out/disc.img", cdrom(), ".iso"),
            ("/out/disk.qcow2.gz", artifact("qcow2"), ".qcow2"),
            ("/out/disk.ova", artifact("qcow2"), ".qcow2"),
            ("/out/disk.vhdx", artifact("vpc"), ".vhd"),
        ] {
            let error = resolved(given, &stored).unwrap_err();
            assert_eq!(error.kind(), "suffix-mismatch", "{given}");
            assert!(error.to_string().contains(expected), "{given}: {error}");
        }
    }

    #[test]
    fn a_compressed_export_gains_what_its_name_lacks() {
        let qcow2 = artifact("qcow2");
        for (given, scheme, path, added) in [
            (
                "/out/disk",
                Compression::Xz,
                "/out/disk.qcow2.xz",
                Some("qcow2.xz"),
            ),
            (
                "/out/disk.qcow2",
                Compression::Xz,
                "/out/disk.qcow2.xz",
                Some("xz"),
            ),
            (
                "/out/disk.QCOW2",
                Compression::Gzip,
                "/out/disk.QCOW2.gz",
                Some("gz"),
            ),
            (
                "/out/disk.qcow2.zst",
                Compression::Zstd,
                "/out/disk.qcow2.zst",
                None,
            ),
            ("/out/disk.XZ", Compression::Xz, "/out/disk.XZ", None),
            (
                "/out/trixie.13.xz",
                Compression::Xz,
                "/out/trixie.13.xz",
                None,
            ),
            (
                "/out/trixie.13",
                Compression::Zstd,
                "/out/trixie.13.qcow2.zst",
                Some("qcow2.zst"),
            ),
        ] {
            assert_eq!(
                compressed(given, &qcow2, scheme).unwrap(),
                (PathBuf::from(path), added.map(ToOwned::to_owned)),
                "{given}"
            );
        }
        assert_eq!(
            compressed("/out/disc", &cdrom(), Compression::Gzip)
                .unwrap()
                .0,
            PathBuf::from("/out/disc.iso.gz")
        );
    }

    #[test]
    fn a_compressed_export_refuses_a_name_for_another_format_or_scheme() {
        let qcow2 = artifact("qcow2");
        for (given, scheme) in [
            ("/out/disk.gz", Compression::Xz),
            ("/out/disk.qcow2.gz", Compression::Zstd),
            ("/out/disk.vmdk.xz", Compression::Xz),
            ("/out/disk.vmdk", Compression::Xz),
            ("/out/disk.tar.xz", Compression::Xz),
        ] {
            let error = compressed(given, &qcow2, scheme).unwrap_err();
            assert_eq!(error.kind(), "suffix-mismatch", "{given}");
            let expected = format!(".qcow2.{}", scheme.suffix().unwrap());
            assert!(error.to_string().contains(&expected), "{given}: {error}");
            assert!(
                error.to_string().contains(scheme.name()),
                "{given}: {error}"
            );
        }
        let error = resolved("/out/disk.qcow2.xz", &qcow2).unwrap_err();
        assert!(error.to_string().contains(".qcow2,"), "{error}");
    }

    #[test]
    fn a_directory_gains_a_file_named_for_the_image_and_its_compression() {
        let scratch = Scratch::new("compresseddirectory");
        assert_eq!(
            destination(
                &scratch.0,
                &artifact("raw"),
                Compression::Xz,
                "debian",
                "trixie"
            )
            .unwrap(),
            (scratch.0.join("debian-trixie.img.xz"), None)
        );
    }

    #[test]
    fn each_scheme_writes_what_expands_to_the_stored_file() {
        let scratch = Scratch::new("compress");
        let stored = scratch.0.join("blob");
        fs::write(&stored, image()).unwrap();
        let held = Artifact {
            digest: sha256(&image()),
            ..artifact("raw")
        };
        for scheme in Compression::SCHEMES {
            let out = scratch
                .0
                .join(format!("disk.img.{}", scheme.suffix().unwrap()));
            let exported = copy(&stored, &held, &out, scheme, false, &mut |_| {}).unwrap();
            assert!(exported.verified);
            assert_eq!(exported.size, image().len() as u64);
            let packed = fs::read(&out).unwrap();
            assert!(packed.len() < image().len() / 100, "{scheme:?}");
            assert_eq!(crate::conversion::sniff(&packed), scheme);
            let expanded = std::process::Command::new(scheme.command().unwrap().get_program())
                .arg("-dc")
                .arg(&out)
                .output()
                .unwrap();
            assert_eq!(expanded.stdout, image(), "{scheme:?}");
            assert!(
                !scratch
                    .0
                    .join(format!("disk.img.{}.partial", scheme.suffix().unwrap()))
                    .exists()
            );
        }
    }

    #[test]
    fn a_damaged_image_leaves_no_compressed_file() {
        let scratch = Scratch::new("compressdamaged");
        let stored = scratch.0.join("blob");
        fs::write(&stored, image()).unwrap();
        let held = Artifact {
            digest: sha256(b"something else"),
            ..artifact("raw")
        };
        let out = scratch.0.join("disk.img.xz");
        let error = copy(&stored, &held, &out, Compression::Xz, false, &mut |_| {}).unwrap_err();
        assert_eq!(error.kind(), "image-damaged");
        assert!(!out.exists());
        assert!(!scratch.0.join("disk.img.xz.partial").exists());
    }

    #[test]
    fn a_directory_gains_a_file_named_for_the_image() {
        let scratch = Scratch::new("directory");
        assert_eq!(
            destination(
                &scratch.0,
                &artifact("qcow2"),
                Compression::None,
                "debian",
                "trixie"
            )
            .unwrap(),
            (scratch.0.join("debian-trixie.qcow2"), None)
        );
    }

    /// An image with empty stretches, as disks mostly are.
    fn image() -> Vec<u8> {
        let mut bytes = vec![0_u8; 3 * PIECE + 123];
        bytes[..5].copy_from_slice(b"start");
        bytes[2 * PIECE + 7] = 9;
        let end = bytes.len() - 1;
        bytes[end] = 1;
        bytes
    }

    #[test]
    fn a_copy_is_the_stored_file_with_its_empty_stretches_left_as_holes() {
        use std::os::unix::fs::MetadataExt as _;
        let scratch = Scratch::new("copy");
        let stored = scratch.0.join("blob");
        let mut bytes = image();
        let end = bytes.len() - 1;
        bytes[end] = 0;
        fs::write(&stored, &bytes).unwrap();
        let held = Artifact {
            digest: sha256(&bytes),
            ..artifact("raw")
        };
        let out = scratch.0.join("disk.img");
        let mut seen = Vec::new();
        let exported = copy(
            &stored,
            &held,
            &out,
            Compression::None,
            false,
            &mut |progress| {
                seen.push(progress.received);
            },
        )
        .unwrap();
        assert_eq!(fs::read(&out).unwrap(), bytes);
        assert_eq!(exported.size, bytes.len() as u64);
        assert!(exported.verified);
        assert_eq!(seen.last().copied(), Some(bytes.len() as u64));
        let allocated = fs::metadata(&out).unwrap().blocks() * 512;
        assert!(allocated <= 2 * PIECE as u64, "{allocated} bytes allocated");
        assert!(!scratch.0.join("disk.img.partial").exists());
    }

    #[test]
    fn a_stored_file_that_does_not_match_its_digest_is_not_exported() {
        let scratch = Scratch::new("damaged");
        let stored = scratch.0.join("blob");
        fs::write(&stored, image()).unwrap();
        let held = Artifact {
            digest: sha256(b"something else"),
            ..artifact("raw")
        };
        let out = scratch.0.join("disk.img");
        let error = copy(&stored, &held, &out, Compression::None, false, &mut |_| {}).unwrap_err();
        assert_eq!(error.kind(), "image-damaged");
        assert!(!out.exists());
        assert!(!scratch.0.join("disk.img.partial").exists());
    }

    #[test]
    fn a_file_held_other_than_as_published_is_copied_unverified() {
        let scratch = Scratch::new("unverified");
        let stored = scratch.0.join("blob");
        fs::write(&stored, image()).unwrap();
        for held in [
            Artifact {
                compression: Compression::Xz,
                ..artifact("raw")
            },
            Artifact {
                source_format: Some("vmdk".to_owned()),
                ..artifact("qcow2")
            },
        ] {
            let out = scratch.0.join(format!("disk.{}", suffix(&held)));
            let exported =
                copy(&stored, &held, &out, Compression::None, false, &mut |_| {}).unwrap();
            assert!(!exported.verified);
            assert_eq!(fs::read(&out).unwrap(), image());
        }
    }

    #[test]
    fn an_existing_file_is_replaced_only_when_asked() {
        let scratch = Scratch::new("exists");
        let stored = scratch.0.join("blob");
        fs::write(&stored, b"image").unwrap();
        let held = Artifact {
            digest: sha256(b"image"),
            ..artifact("raw")
        };
        let out = scratch.0.join("disk.img");
        fs::write(&out, b"precious").unwrap();
        let error = copy(&stored, &held, &out, Compression::None, false, &mut |_| {}).unwrap_err();
        assert_eq!(error.kind(), "output-exists");
        assert_eq!(fs::read(&out).unwrap(), b"precious");
        copy(&stored, &held, &out, Compression::None, true, &mut |_| {}).unwrap();
        assert_eq!(fs::read(&out).unwrap(), b"image");
    }

    #[test]
    fn a_destination_that_cannot_be_written_is_refused() {
        let scratch = Scratch::new("unwritable");
        let stored = scratch.0.join("blob");
        fs::write(&stored, b"image").unwrap();
        let held = Artifact {
            digest: sha256(b"image"),
            ..artifact("raw")
        };
        let out = scratch.0.join("missing/disk.img");
        let error = copy(&stored, &held, &out, Compression::None, false, &mut |_| {}).unwrap_err();
        assert_eq!(error.kind(), "export-failed");
        assert!(error.to_string().contains("missing"), "{error}");
    }
}

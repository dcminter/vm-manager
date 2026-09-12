//! Compression of published images.
//!
//! Several projects publish their disk images compressed, and the saving is
//! large enough that they are unlikely to stop: a FreeBSD image is a third of
//! its size in xz, and 9front's is a tenth. An image has to be expanded before
//! it can be a backing file, so the expansion happens on the way in, once,
//! rather than on every boot.
//!
//! The work is handed to the tools that already do it. They are present on any
//! Debian system worth the name, they stream, and they are faster than
//! anything this tree would carry of its own.

use crate::error::Error;
use serde::Deserialize;

/// How an artifact is published, as distinct from what it holds. The format
/// field describes the image inside; this describes the wrapper around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Compression {
    /// Published as it is used.
    #[default]
    None,
    #[serde(alias = "lzma")]
    Xz,
    #[serde(alias = "gz")]
    Gzip,
    #[serde(alias = "zst")]
    Zstd,
}

impl Compression {
    pub const fn is_none(self) -> bool {
        matches!(self, Self::None)
    }

    /// The name the catalogue uses, so that a report can say what it read.
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Xz => "xz",
            Self::Gzip => "gzip",
            Self::Zstd => "zstd",
        }
    }

    /// The program that expands this, and the package that carries it.
    const fn tool(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::None => None,
            Self::Xz => Some(("xz", "xz-utils")),
            Self::Gzip => Some(("gzip", "gzip")),
            Self::Zstd => Some(("zstd", "zstd")),
        }
    }

    /// A decompressor reading standard input and writing standard output.
    ///
    /// `-c` is given alongside `-d` because gzip and xz otherwise look for a
    /// file to replace and refuse a stream.
    pub fn command(self) -> Option<std::process::Command> {
        let (binary, _) = self.tool()?;
        let mut command = std::process::Command::new(binary);
        command.arg("-dc");
        Some(command)
    }

    /// Says which package to install, rather than reporting that a program
    /// nobody asked for by name was not found.
    pub fn missing(self, source: &std::io::Error) -> Error {
        match (self.tool(), source.kind()) {
            (Some((binary, package)), std::io::ErrorKind::NotFound) => Error::MissingTool {
                binary,
                package,
                operation: "expanding a compressed image",
            },
            _ => Error::Launch {
                program: self.name().to_owned(),
                source: std::io::Error::new(source.kind(), source.to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[derive(Deserialize)]
    struct Held {
        #[serde(default)]
        compression: Compression,
    }

    fn read(text: &str) -> Compression {
        basic_toml::from_str::<Held>(text).unwrap().compression
    }

    #[test]
    fn an_entry_that_says_nothing_is_uncompressed() {
        assert_eq!(read(""), Compression::None);
        assert!(Compression::default().is_none());
    }

    #[test]
    fn the_published_names_are_read() {
        assert_eq!(read(r#"compression = "xz""#), Compression::Xz);
        assert_eq!(read(r#"compression = "gzip""#), Compression::Gzip);
        assert_eq!(read(r#"compression = "zstd""#), Compression::Zstd);
        assert_eq!(read(r#"compression = "none""#), Compression::None);
    }

    /// The suffixes people actually type are worth accepting, because the
    /// alternative is a catalogue entry that fails to load over a spelling.
    #[test]
    fn the_common_abbreviations_are_read_too() {
        assert_eq!(read(r#"compression = "gz""#), Compression::Gzip);
        assert_eq!(read(r#"compression = "zst""#), Compression::Zstd);
        assert_eq!(read(r#"compression = "lzma""#), Compression::Xz);
    }

    #[test]
    fn a_name_that_is_not_a_scheme_is_refused() {
        assert!(basic_toml::from_str::<Held>(r#"compression = "rar""#).is_err());
    }

    #[test]
    fn nothing_uncompressed_has_a_command_and_everything_else_does() {
        assert!(Compression::None.command().is_none());
        for held in [Compression::Xz, Compression::Gzip, Compression::Zstd] {
            let command = held.command().unwrap();
            assert_eq!(command.get_args().collect::<Vec<_>>(), ["-dc"], "{held:?}");
        }
    }

    /// The three of them are Debian priority standard or better, so a run that
    /// cannot find one has a broken host rather than a missing catalogue.
    #[test]
    fn the_decompressors_are_present_and_expand_what_they_made() {
        for (held, packer) in [
            (Compression::Xz, "xz"),
            (Compression::Gzip, "gzip"),
            (Compression::Zstd, "zstd"),
        ] {
            let packed = std::process::Command::new(packer)
                .arg("-c")
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(packed.status.success(), "{packer} would not run");
            let mut command = held.command().unwrap();
            assert!(command.output().is_ok(), "{held:?} would not run");
        }
    }

    #[test]
    fn a_missing_decompressor_names_its_package() {
        let absent = std::io::Error::from(std::io::ErrorKind::NotFound);
        let error = Compression::Xz.missing(&absent);
        assert_eq!(error.kind(), "missing-tool");
        assert!(error.to_string().contains("xz-utils"), "{error}");
    }

    /// A failure that is not absence is a failure to start, and saying a
    /// package is missing would send the reader after the wrong thing.
    #[test]
    fn another_failure_is_reported_as_itself() {
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(Compression::Gzip.missing(&denied).kind(), "launch-failed");
    }

    #[test]
    fn every_scheme_has_a_name() {
        assert_eq!(Compression::None.name(), "none");
        assert_eq!(Compression::Zstd.name(), "zstd");
    }
}

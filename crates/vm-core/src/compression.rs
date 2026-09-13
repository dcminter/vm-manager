//! Decompression of published images with the system's own tools.

use crate::error::Error;
use serde::Deserialize;

/// How a published file is compressed.
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
    /// Every scheme that compresses.
    pub const SCHEMES: [Self; 3] = [Self::Xz, Self::Gzip, Self::Zstd];

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

    /// The suffix of a file compressed this way.
    pub const fn suffix(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Xz => Some("xz"),
            Self::Gzip => Some("gz"),
            Self::Zstd => Some("zst"),
        }
    }

    /// A decompressor from standard input to standard output.
    pub fn command(self) -> Option<std::process::Command> {
        let (binary, _) = self.tool()?;
        let mut command = std::process::Command::new(binary);
        command.arg("-dc");
        Some(command)
    }

    /// A compressor from standard input to standard output, using every processor it can.
    pub fn compressor(self) -> Option<std::process::Command> {
        let (binary, _) = self.tool()?;
        let mut command = std::process::Command::new(binary);
        match self {
            Self::Xz => command.args(["-T0", "-c"]),
            Self::Zstd => command.args(["-T0", "-q", "-c"]),
            Self::Gzip | Self::None => command.arg("-c"),
        };
        Some(command)
    }

    /// An error naming the package to install.
    pub fn missing(self, source: &std::io::Error) -> Error {
        self.unavailable(source, "expanding a compressed image")
    }

    /// An error naming the package to install, for compressing.
    pub fn missing_compressor(self, source: &std::io::Error) -> Error {
        self.unavailable(source, "compressing an image")
    }

    fn unavailable(self, source: &std::io::Error, operation: &'static str) -> Error {
        match (self.tool(), source.kind()) {
            (Some((binary, package)), std::io::ErrorKind::NotFound) => Error::MissingTool {
                binary,
                package,
                operation,
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

    #[test]
    fn another_failure_is_reported_as_itself() {
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(Compression::Gzip.missing(&denied).kind(), "launch-failed");
    }

    #[test]
    fn what_each_compressor_makes_its_decompressor_expands() {
        use std::io::Write as _;
        let plain = b"an image, more or less".repeat(1000);
        for held in Compression::SCHEMES {
            let mut child = held
                .compressor()
                .unwrap()
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(&plain).unwrap();
            let packed = child.wait_with_output().unwrap().stdout;
            assert!(packed.len() < plain.len(), "{held:?}");
            assert_eq!(crate::conversion::sniff(&packed), held);
            let mut child = held
                .command()
                .unwrap()
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(&packed).unwrap();
            assert_eq!(child.wait_with_output().unwrap().stdout, plain, "{held:?}");
        }
        assert!(Compression::None.compressor().is_none());
    }

    #[test]
    fn each_scheme_has_its_suffix() {
        assert_eq!(Compression::None.suffix(), None);
        assert_eq!(Compression::Xz.suffix(), Some("xz"));
        assert_eq!(Compression::Gzip.suffix(), Some("gz"));
        assert_eq!(Compression::Zstd.suffix(), Some("zst"));
    }

    #[test]
    fn a_missing_compressor_says_it_was_wanted_for_compressing() {
        let absent = std::io::Error::from(std::io::ErrorKind::NotFound);
        let error = Compression::Zstd.missing_compressor(&absent);
        assert!(error.to_string().starts_with("compressing"), "{error}");
    }

    #[test]
    fn every_scheme_has_a_name() {
        assert_eq!(Compression::None.name(), "none");
        assert_eq!(Compression::Zstd.name(), "zstd");
    }
}

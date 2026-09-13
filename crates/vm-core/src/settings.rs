//! Parsing of the settings a front end takes as text.

use crate::compression::Compression;
use crate::error::Error;
use crate::instance::{Port, Share};
use crate::machine::{self, Chipset, Disk, Firmware};
use crate::{ARCHITECTURES, seed};
use std::path::Path;

/// Turns `2G`, `512M` or a bare number of mebibytes into mebibytes.
pub fn parse_memory(text: &str) -> std::result::Result<u64, String> {
    let text = text.trim();
    let (digits, scale) = match text.chars().last() {
        Some('G' | 'g') => (&text[..text.len() - 1], 1024),
        Some('M' | 'm') => (&text[..text.len() - 1], 1),
        _ => (text, 1),
    };
    let amount: u64 = digits
        .parse()
        .map_err(|_| format!("'{text}' is not an amount of memory"))?;
    let total = amount * scale;
    if total < 128 {
        return Err("a machine needs at least 128M".to_owned());
    }
    Ok(total)
}

/// Parses `/host/path:/guest/path`, tagging the share with the host directory's name.
pub fn parse_share(text: &str) -> std::result::Result<Share, String> {
    let (source, target) = text
        .rsplit_once(':')
        .ok_or_else(|| format!("'{text}' is not a share; write it as host:guest"))?;
    if source.is_empty() || target.is_empty() {
        return Err(format!("'{text}' is not a share; write it as host:guest"));
    }
    if !target.starts_with('/') {
        return Err(format!("'{target}' is not an absolute path in the guest"));
    }
    let source = std::path::PathBuf::from(source);
    let source = source
        .canonicalize()
        .map_err(|error| format!("{}: {error}", source.display()))?;
    if !source.is_dir() {
        return Err(format!("{} is not a directory", source.display()));
    }
    Ok(Share {
        tag: tag_for(&source),
        source,
        target: target.to_owned(),
        pid: None,
        started: None,
    })
}

/// A virtiofs tag of at most 36 bytes, in characters the seed allows.
fn tag_for(source: &Path) -> String {
    let stem: String = source
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .take(30)
        .collect();
    if stem.is_empty() {
        "share".to_owned()
    } else {
        stem
    }
}

/// Turns `2222:22` into a forward.
pub fn parse_port(text: &str) -> std::result::Result<Port, String> {
    let (host, guest) = text
        .split_once(':')
        .ok_or_else(|| format!("'{text}' is not a port mapping; write it as host:guest"))?;
    Ok(Port {
        host: host
            .parse()
            .map_err(|_| format!("'{host}' is not a port number"))?,
        guest: guest
            .parse()
            .map_err(|_| format!("'{guest}' is not a port number"))?,
    })
}

/// A description on one line of text.
pub fn parse_description(text: &str) -> std::result::Result<String, String> {
    if text.trim().is_empty() {
        Err("a description needs some text".to_owned())
    } else if text.chars().any(char::is_control) {
        Err("a description is one line, without control characters".to_owned())
    } else {
        Ok(text.to_owned())
    }
}

pub fn parse_firmware(text: &str) -> std::result::Result<Firmware, String> {
    text.parse().map_err(|error: Error| error.to_string())
}

pub fn parse_machine(text: &str) -> std::result::Result<Chipset, String> {
    text.parse().map_err(|error: Error| error.to_string())
}

pub fn parse_disk(text: &str) -> std::result::Result<Disk, String> {
    text.parse().map_err(|error: Error| error.to_string())
}

pub fn parse_cpu(text: &str) -> std::result::Result<String, String> {
    machine::check_cpu(text)
        .map(|()| text.to_owned())
        .map_err(|error| error.to_string())
}

pub fn parse_user(text: &str) -> std::result::Result<String, String> {
    let trial = seed::Seed {
        instance_id: "check".to_owned(),
        hostname: "check".to_owned(),
        user: text.to_owned(),
        authorized_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKq7YQ== check".to_owned(),
        mounts: Vec::new(),
        password: None,
    };
    trial
        .image()
        .map(|_| text.to_owned())
        .map_err(|_| format!("'{text}' is not a usable user name"))
}

/// A source as given on the command line.
pub fn parse_source(text: &str) -> std::result::Result<crate::import::Source, String> {
    if text.is_empty() {
        Err("a source needs a path or URL".to_owned())
    } else {
        Ok(crate::import::Source::parse(text))
    }
}

pub fn parse_login(text: &str) -> std::result::Result<crate::catalogue::Login, String> {
    text.parse().map_err(|error: Error| error.to_string())
}

pub fn parse_digest(text: &str) -> std::result::Result<crate::reference::Digest, String> {
    text.parse().map_err(|error: Error| error.to_string())
}

pub fn parse_arch(text: &str) -> std::result::Result<String, String> {
    if ARCHITECTURES.contains(&text) {
        Ok(text.to_owned())
    } else {
        Err(format!(
            "'{text}' is not an architecture; use {}",
            ARCHITECTURES.join(", ")
        ))
    }
}

/// A compression scheme to export with.
pub fn parse_compression(text: &str) -> std::result::Result<Compression, String> {
    Compression::SCHEMES
        .into_iter()
        .find(|scheme| scheme.name() == text)
        .ok_or_else(|| format!("'{text}' is not a compression scheme; use xz, gzip or zstd"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn memory_is_read_in_mebibytes_by_default() {
        assert_eq!(parse_memory("2048"), Ok(2048));
        assert_eq!(parse_memory("512M"), Ok(512));
        assert_eq!(parse_memory("2G"), Ok(2048));
        assert_eq!(parse_memory(" 4g "), Ok(4096));
    }

    #[test]
    fn memory_that_is_not_an_amount_is_refused() {
        assert!(parse_memory("lots").is_err());
        assert!(parse_memory("").is_err());
        assert!(parse_memory("2T").is_err());
    }

    #[test]
    fn memory_below_what_will_boot_is_refused() {
        assert!(parse_memory("2").is_err());
        assert!(parse_memory("127").is_err());
        assert!(parse_memory("128").is_ok());
    }

    #[test]
    fn a_port_mapping_is_host_then_guest() {
        assert_eq!(
            parse_port("2222:22"),
            Ok(Port {
                host: 2222,
                guest: 22
            })
        );
    }

    #[test]
    fn a_port_mapping_that_is_not_one_is_refused() {
        assert!(parse_port("2222").is_err());
        assert!(parse_port("2222:").is_err());
        assert!(parse_port("http:22").is_err());
        assert!(parse_port("99999:22").is_err());
    }

    #[test]
    fn a_share_takes_its_tag_from_the_host_directory() {
        let share = parse_share(&format!("{}:/mnt/tmp", std::env::temp_dir().display())).unwrap();
        assert_eq!(share.target, "/mnt/tmp");
        assert_eq!(share.tag, "tmp");
    }

    #[test]
    fn a_share_that_is_not_one_is_refused() {
        assert!(parse_share("/tmp").is_err());
        assert!(parse_share("/tmp:").is_err());
        assert!(parse_share(":/mnt").is_err());
        assert!(
            parse_share("/tmp:mnt").is_err(),
            "the guest path must be absolute"
        );
        assert!(parse_share("/nonexistent/directory:/mnt").is_err());
    }

    #[test]
    fn a_share_of_something_that_is_not_a_directory_is_refused() {
        let mut path = std::env::temp_dir();
        path.push(format!("vm-share-{}.txt", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        let outcome = parse_share(&format!("{}:/mnt/x", path.display()));
        let _ = std::fs::remove_file(&path);
        assert!(outcome.is_err());
    }

    #[test]
    fn a_user_name_the_guest_would_refuse_is_refused_here() {
        assert_eq!(
            parse_description("Trixie with \"tools\""),
            Ok("Trixie with \"tools\"".to_owned())
        );
        assert!(parse_description("  ").is_err());
        assert!(parse_description("two\nlines").is_err());
        assert!(parse_description("tab\there").is_err());
        assert_eq!(parse_user("vm"), Ok("vm".to_owned()));
        assert!(parse_user("Capitals").is_err());
        assert!(parse_user("has space").is_err());
        assert!(parse_user("").is_err());
    }

    #[test]
    fn an_architecture_must_be_one_a_machine_can_have() {
        assert_eq!(parse_arch("arm64").unwrap(), "arm64");
        assert!(parse_arch("x86_64").unwrap_err().contains("amd64"));
    }

    #[test]
    fn a_login_and_digest_are_checked_as_they_are_parsed() {
        assert!(parse_login("cloud-init").unwrap().is_seedable());
        assert!(parse_login("ssh").is_err());
        assert!(parse_digest(&format!("sha256:{}", "a".repeat(64))).is_ok());
        assert!(parse_digest("sha256:short").is_err());
        assert!(parse_source("").is_err());
        assert!(parse_source("https://example.invalid/x").unwrap().is_url());
    }

    #[test]
    fn only_the_compressing_schemes_are_accepted_by_name() {
        assert_eq!(parse_compression("xz"), Ok(Compression::Xz));
        assert_eq!(parse_compression("gzip"), Ok(Compression::Gzip));
        assert_eq!(parse_compression("zstd"), Ok(Compression::Zstd));
        for refused in ["none", "gz", "zst", "bzip2", ""] {
            assert!(parse_compression(refused).is_err(), "{refused}");
        }
    }
}

use crate::error::{Error, Result};
use std::fmt;
use std::str::FromStr;

pub const DEFAULT_TAG: &str = "latest";

/// A digest with its algorithm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digest {
    algorithm: Algorithm,
    hex: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Sha256,
    Sha512,
}

impl Algorithm {
    const fn name(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }

    const fn hex_len(self) -> usize {
        match self {
            Self::Sha256 => 64,
            Self::Sha512 => 128,
        }
    }
}

impl Digest {
    /// A digest of bytes hashed locally.
    pub fn new(algorithm: Algorithm, hex: &str) -> Self {
        Self {
            algorithm,
            hex: hex.to_ascii_lowercase(),
        }
    }

    pub const fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    pub const fn algorithm_name(&self) -> &'static str {
        self.algorithm.name()
    }

    pub fn hex(&self) -> &str {
        &self.hex
    }
}

impl FromStr for Digest {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        let reject = |reason| Error::Reference {
            input: text.to_owned(),
            reason,
        };
        let (name, hex) = text
            .split_once(':')
            .ok_or_else(|| reject("expected <algorithm>:<hex>"))?;
        let algorithm = match name {
            "sha256" => Algorithm::Sha256,
            "sha512" => Algorithm::Sha512,
            _ => {
                return Err(reject(
                    "unknown digest algorithm; expected sha256 or sha512",
                ));
            }
        };
        if hex.len() != algorithm.hex_len() {
            return Err(reject("digest has the wrong length for its algorithm"));
        }
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(reject("digest is not hexadecimal"));
        }
        Ok(Self {
            algorithm,
            hex: hex.to_ascii_lowercase(),
        })
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm.name(), self.hex)
    }
}

/// An image reference: `repository[:tag][@digest]`, as in `debian:trixie`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    repository: String,
    tag: String,
    digest: Option<Digest>,
}

impl Reference {
    pub fn repository(&self) -> &str {
        &self.repository
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub const fn digest(&self) -> Option<&Digest> {
        self.digest.as_ref()
    }
}

fn valid_repository(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with('/')
        && !text.ends_with('/')
        && !text.contains("//")
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-/".contains(&b))
}

fn valid_tag(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

impl FromStr for Reference {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        let reject = |reason| Error::Reference {
            input: text.to_owned(),
            reason,
        };
        let (name, digest) = match text.split_once('@') {
            Some((name, digest)) => (name, Some(digest.parse::<Digest>()?)),
            None => (text, None),
        };
        let (repository, tag) = match name.rsplit_once(':') {
            Some((repository, tag)) => (repository, tag),
            None => (name, DEFAULT_TAG),
        };
        if !valid_repository(repository) {
            return Err(reject(
                "repository must be lowercase letters, digits, '.', '-', '_' or '/'",
            ));
        }
        if !valid_tag(tag) {
            return Err(reject("tag must be letters, digits, '.', '-' or '_'"));
        }
        Ok(Self {
            repository: repository.to_owned(),
            tag: tag.to_owned(),
            digest,
        })
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.repository, self.tag)?;
        if let Some(digest) = &self.digest {
            write!(f, "@{digest}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn parse(text: &str) -> Reference {
        text.parse().unwrap()
    }

    #[test]
    fn bare_name_takes_the_default_tag() {
        let reference = parse("debian");
        assert_eq!(reference.repository(), "debian");
        assert_eq!(reference.tag(), "latest");
        assert!(reference.digest().is_none());
    }

    #[test]
    fn explicit_tag_is_kept() {
        assert_eq!(parse("debian:trixie").tag(), "trixie");
    }

    #[test]
    fn namespaced_repository_is_kept_whole() {
        let reference = parse("dcminter/debian:trixie");
        assert_eq!(reference.repository(), "dcminter/debian");
        assert_eq!(reference.tag(), "trixie");
    }

    #[test]
    fn digest_may_accompany_a_tag() {
        let reference = parse(&format!("debian:trixie@sha256:{}", "a".repeat(64)));
        assert_eq!(reference.tag(), "trixie");
        assert_eq!(reference.digest().unwrap().algorithm(), Algorithm::Sha256);
    }

    #[test]
    fn digest_without_a_tag_still_defaults_the_tag() {
        let reference = parse(&format!("debian@sha512:{}", "b".repeat(128)));
        assert_eq!(reference.tag(), "latest");
        assert_eq!(reference.digest().unwrap().algorithm(), Algorithm::Sha512);
    }

    #[test]
    fn a_colon_in_the_digest_does_not_become_the_tag() {
        let reference = parse(&format!("debian@sha256:{}", "c".repeat(64)));
        assert_eq!(reference.repository(), "debian");
        assert_eq!(reference.tag(), "latest");
    }

    #[test]
    fn display_round_trips() {
        for text in ["debian:trixie", "dcminter/debian:13"] {
            assert_eq!(parse(text).to_string(), text);
        }
        let pinned = format!("debian:trixie@sha256:{}", "d".repeat(64));
        assert_eq!(parse(&pinned).to_string(), pinned);
    }

    #[test]
    fn display_makes_the_default_tag_explicit() {
        assert_eq!(parse("debian").to_string(), "debian:latest");
    }

    #[test]
    fn digest_hex_is_normalised_to_lowercase() {
        let reference = parse(&format!("debian@sha256:{}", "AB".repeat(32)));
        assert_eq!(reference.digest().unwrap().hex(), "ab".repeat(32));
    }

    #[test]
    fn uppercase_repositories_are_rejected() {
        assert!("Debian".parse::<Reference>().is_err());
    }

    #[test]
    fn empty_components_are_rejected() {
        for text in ["", ":trixie", "debian:", "/debian", "debian/", "deb//ian"] {
            assert!(
                text.parse::<Reference>().is_err(),
                "{text} should be rejected"
            );
        }
    }

    #[test]
    fn malformed_digests_are_rejected() {
        let cases = [
            format!("debian@{}", "a".repeat(64)),
            format!("debian@md5:{}", "a".repeat(32)),
            format!("debian@sha256:{}", "a".repeat(63)),
            format!("debian@sha512:{}", "a".repeat(64)),
            format!("debian@sha256:{}", "z".repeat(64)),
        ];
        for text in &cases {
            assert!(
                text.parse::<Reference>().is_err(),
                "{text} should be rejected"
            );
        }
    }

    #[test]
    fn tags_may_not_contain_a_slash() {
        assert!("debian:tri/xie".parse::<Reference>().is_err());
    }
}

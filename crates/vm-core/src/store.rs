use crate::catalogue::{Artifact, Entry};
use crate::digest;
use crate::error::{Error, Result};
use crate::reference::Digest;
use std::fs;
use std::path::{Path, PathBuf};

/// How much of a download has arrived, reported as it goes.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub received: u64,
    pub total: Option<u64>,
}

pub type Reporter<'a> = &'a mut dyn FnMut(Progress);

/// What a pull did, so the caller can say so without guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pulled {
    AlreadyPresent,
    Fetched,
}

/// Images on disk, addressed by digest so that two tags naming the same build
/// share one file.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn discover() -> Result<Self> {
        crate::paths::images_directory()
            .map(Self::new)
            .ok_or(Error::NoImageStore)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where a given build lives, whether or not it has been fetched.
    pub fn path_for(&self, digest: &Digest) -> PathBuf {
        self.root
            .join("blobs")
            .join(digest.algorithm_name())
            .join(digest.hex())
    }

    pub fn contains(&self, digest: &Digest) -> bool {
        self.path_for(digest).is_file()
    }

    /// Fetches the artifact unless it is already held. The digest is verified
    /// in the same pass that writes the file, and a mismatch keeps nothing.
    pub fn pull(
        &self,
        artifact: &Artifact,
        agent: &ureq::Agent,
        report: Reporter<'_>,
    ) -> Result<Pulled> {
        let destination = self.path_for(&artifact.digest);
        if destination.is_file() {
            return Ok(Pulled::AlreadyPresent);
        }
        let directory = destination.parent().unwrap_or(&self.root);
        create_directory(directory)?;
        let partial = directory.join(format!("{}.partial", artifact.digest.hex()));
        let outcome = Self::fetch(artifact, agent, &partial, report);
        match outcome {
            Ok(()) => {
                rename(&partial, &destination)?;
                Ok(Pulled::Fetched)
            }
            Err(error) => {
                let _ = fs::remove_file(&partial);
                Err(error)
            }
        }
    }

    fn fetch(
        artifact: &Artifact,
        agent: &ureq::Agent,
        partial: &Path,
        report: Reporter<'_>,
    ) -> Result<()> {
        let response = agent
            .get(&artifact.url)
            .call()
            .map_err(|source| Error::Download {
                url: artifact.url.clone(),
                source: Box::new(source),
            })?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(Error::HttpStatus {
                url: artifact.url.clone(),
                status,
            });
        }
        let total = content_length(&response).or(artifact.size);
        let mut body = response.into_body().into_reader();
        let file = fs::File::create(partial).map_err(|source| Error::Store {
            path: partial.to_owned(),
            action: "create",
            source,
        })?;
        let mut sink = std::io::BufWriter::new(file);
        let mut observe = |received| report(Progress { received, total });
        let (_, actual) = digest::copy_hashing(
            &mut body,
            &mut sink,
            artifact.digest.algorithm(),
            &mut observe,
        )
        .map_err(|source| Error::Store {
            path: partial.to_owned(),
            action: "write",
            source,
        })?;
        if !digest::matches(&artifact.digest, &actual) {
            return Err(Error::DigestMismatch {
                url: artifact.url.clone(),
                expected: artifact.digest.to_string(),
                actual: format!("{}:{actual}", artifact.digest.algorithm_name()),
            });
        }
        Ok(())
    }

    /// Records which reference a build was fetched for, so listings and later
    /// removal have something to read.
    pub fn record(&self, entry: &Entry, artifact: &Artifact) -> Result<()> {
        let directory = self.root.join("refs").join(&entry.name);
        create_directory(&directory)?;
        let path = directory.join(format!("{}-{}.toml", entry.tag, artifact.arch));
        let body = format!(
            "name = \"{}\"\ntag = \"{}\"\narch = \"{}\"\ndigest = \"{}\"\nurl = \"{}\"\n",
            entry.name, entry.tag, artifact.arch, artifact.digest, artifact.url
        );
        fs::write(&path, body).map_err(|source| Error::Store {
            path,
            action: "write",
            source,
        })
    }
}

fn content_length(response: &ureq::http::Response<ureq::Body>) -> Option<u64> {
    response
        .headers()
        .get("content-length")?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

fn create_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|source| Error::Store {
        path: path.to_owned(),
        action: "create",
        source,
    })
}

fn rename(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to).map_err(|source| Error::Store {
        path: to.to_owned(),
        action: "move",
        source,
    })
}

/// An agent that trusts the system's certificate store, so an internal CA
/// works without configuration.
pub fn http_agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder().tls_config(tls).build().into()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::catalogue::Login;
    use std::str::FromStr;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("vm-store-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn store(&self) -> Store {
            Store::new(self.0.clone())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn digest() -> Digest {
        Digest::from_str(&format!("sha512:{}", "a".repeat(128))).unwrap()
    }

    fn artifact() -> Artifact {
        Artifact {
            arch: "amd64".to_owned(),
            format: "qcow2".to_owned(),
            url: "https://example.invalid/image.qcow2".to_owned(),
            digest: digest(),
            size: Some(1024),
        }
    }

    fn entry() -> Entry {
        Entry {
            name: "debian".to_owned(),
            tag: "trixie".to_owned(),
            aliases: vec!["latest".to_owned()],
            description: "test entry".to_owned(),
            login: Login::CloudInit,
            artifacts: vec![artifact()],
        }
    }

    #[test]
    fn a_build_is_addressed_by_its_digest() {
        let scratch = Scratch::new("address");
        let path = scratch.store().path_for(&digest());
        assert!(
            path.ends_with(format!("blobs/sha512/{}", "a".repeat(128))),
            "{}",
            path.display()
        );
    }

    #[test]
    fn two_tags_of_one_build_share_a_path() {
        let scratch = Scratch::new("shared");
        let store = scratch.store();
        assert_eq!(
            store.path_for(&digest()),
            store.path_for(&artifact().digest)
        );
    }

    #[test]
    fn an_absent_build_is_not_reported_as_held() {
        let scratch = Scratch::new("absent");
        assert!(!scratch.store().contains(&digest()));
    }

    #[test]
    fn a_present_build_is_reported_as_held() {
        let scratch = Scratch::new("present");
        let store = scratch.store();
        let path = store.path_for(&digest());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"payload").unwrap();
        assert!(store.contains(&digest()));
    }

    #[test]
    fn a_directory_is_not_mistaken_for_a_build() {
        let scratch = Scratch::new("directory");
        let store = scratch.store();
        fs::create_dir_all(store.path_for(&digest())).unwrap();
        assert!(!store.contains(&digest()));
    }

    #[test]
    fn a_record_names_the_reference_it_was_fetched_for() {
        let scratch = Scratch::new("record");
        let store = scratch.store();
        store.record(&entry(), &artifact()).unwrap();
        let path = scratch.0.join("refs/debian/trixie-amd64.toml");
        let body = fs::read_to_string(path).unwrap();
        assert!(body.contains(r#"tag = "trixie""#), "{body}");
        assert!(body.contains(r#"arch = "amd64""#), "{body}");
        assert!(body.contains(&digest().to_string()), "{body}");
    }

    #[test]
    fn a_record_is_rewritten_rather_than_duplicated() {
        let scratch = Scratch::new("rewrite");
        let store = scratch.store();
        store.record(&entry(), &artifact()).unwrap();
        store.record(&entry(), &artifact()).unwrap();
        let count = fs::read_dir(scratch.0.join("refs/debian")).unwrap().count();
        assert_eq!(count, 1);
    }

    #[test]
    fn a_held_build_is_not_fetched_again() {
        let scratch = Scratch::new("skip");
        let store = scratch.store();
        let path = store.path_for(&digest());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"payload").unwrap();
        // The agent is never reached, so an unroutable URL is safe here.
        let outcome = store.pull(&artifact(), &http_agent(), &mut |_| {}).unwrap();
        assert_eq!(outcome, Pulled::AlreadyPresent);
    }
}

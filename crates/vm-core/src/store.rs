use crate::catalogue::{Artifact, Entry};
use crate::digest;
use crate::error::{Error, Result};
use crate::reference::{Algorithm, Digest};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;

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

/// Images on disk, addressed by digest.
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

    /// The address to fetch an artifact from.
    fn source(artifact: &Artifact) -> Result<&String> {
        artifact.url.as_ref().ok_or(Error::NotFetchable)
    }

    /// Fetches the artifact unless held, verifying its digest as it is written.
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
        let url = Self::source(artifact)?;
        let response = agent.get(url).call().map_err(|source| Error::Download {
            url: url.clone(),
            source: Box::new(source),
        })?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(Error::HttpStatus {
                url: url.clone(),
                status,
            });
        }
        // For a compressed artifact the entry's size is the expanded size, not the download's.
        let stated = artifact
            .compression
            .is_none()
            .then_some(artifact.size)
            .flatten();
        let total = content_length(&response).or(stated);
        let mut body = response.into_body().into_reader();
        let file = fs::File::create(partial).map_err(|source| Error::Store {
            path: partial.to_owned(),
            action: "create",
            source,
        })?;
        let mut observe = |received| report(Progress { received, total });
        let actual = if let Some(command) = artifact.compression.command() {
            expand(command, artifact, &mut body, file, &mut observe)?
        } else {
            let mut sink = std::io::BufWriter::new(file);
            let (_, hash) = digest::copy_hashing(
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
            hash
        };
        if !digest::matches(&artifact.digest, &actual) {
            return Err(Error::DigestMismatch {
                url: url.clone(),
                expected: artifact.digest.to_string(),
                actual: format!("{}:{actual}", artifact.digest.algorithm_name()),
            });
        }
        Ok(())
    }

    /// A path inside the store to build an image at, so adopting it is a rename.
    pub fn staging(&self, label: &str) -> Result<PathBuf> {
        let directory = self.root.join("blobs");
        create_directory(&directory)?;
        Ok(directory.join(format!(".building-{label}")))
    }

    /// Moves a file built at [`Store::staging`] into the store under its digest.
    pub fn adopt(
        &self,
        staged: &Path,
        algorithm: Algorithm,
        report: Reporter<'_>,
    ) -> Result<Digest> {
        let mut file = fs::File::open(staged).map_err(|source| Error::Store {
            path: staged.to_owned(),
            action: "read",
            source,
        })?;
        // Hashing an image takes long enough to need progress.
        let total = file.metadata().ok().map(|data| data.len());
        let mut observe = |received| report(Progress { received, total });
        let mut sink = std::io::sink();
        let (_, hash) = digest::copy_hashing(&mut file, &mut sink, algorithm, &mut observe)
            .map_err(|source| Error::Store {
                path: staged.to_owned(),
                action: "read",
                source,
            })?;
        let digest = Digest::new(algorithm, &hash);
        let destination = self.path_for(&digest);
        if destination.exists() {
            // The same bytes are already held, so the new copy is redundant.
            let _ = fs::remove_file(staged);
            return Ok(digest);
        }
        if let Some(parent) = destination.parent() {
            create_directory(parent)?;
        }
        rename(staged, &destination)?;
        Ok(digest)
    }

    /// Removes a build if held, returning the bytes freed.
    pub fn discard(&self, digest: &Digest) -> Result<u64> {
        let path = self.path_for(digest);
        let size = fs::metadata(&path).map_or(0, |data| data.len());
        match fs::remove_file(&path) {
            Ok(()) => Ok(size),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(source) => Err(Error::Store {
                path,
                action: "remove",
                source,
            }),
        }
    }

    /// Removes a reference's record, and its directory once empty.
    pub fn forget(&self, name: &str, tag: &str, arch: &str) {
        let directory = self.root.join("refs").join(name);
        let _ = fs::remove_file(directory.join(format!("{tag}-{arch}.toml")));
        let _ = fs::remove_dir(&directory);
    }

    /// Records which reference a build was fetched for.
    pub fn record(&self, entry: &Entry, artifact: &Artifact) -> Result<()> {
        let directory = self.root.join("refs").join(&entry.name);
        create_directory(&directory)?;
        let path = directory.join(format!("{}-{}.toml", entry.tag, artifact.arch));
        let body = format!(
            "name = \"{}\"\ntag = \"{}\"\narch = \"{}\"\ndigest = \"{}\"\nurl = \"{}\"\n",
            entry.name,
            entry.tag,
            artifact.arch,
            artifact.digest,
            artifact.url.clone().unwrap_or_default()
        );
        fs::write(&path, body).map_err(|source| Error::Store {
            path,
            action: "write",
            source,
        })
    }
}

/// Streams a download through a decompressor, hashing the compressed bytes.
fn expand(
    mut command: std::process::Command,
    artifact: &Artifact,
    body: &mut impl Read,
    file: fs::File,
    observe: &mut dyn FnMut(u64),
) -> Result<String> {
    let scheme = artifact.compression.name();
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::from(file))
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| artifact.compression.missing(&source))?;
    let Some(mut stdin) = child.stdin.take() else {
        return Err(Error::Decompress {
            scheme,
            reason: "the decompressor was given no input pipe".to_owned(),
        });
    };
    let outcome = digest::copy_hashing(body, &mut stdin, artifact.digest.algorithm(), observe)
        .map(|(_, hash)| hash);
    // The decompressor finishes only once its input is closed.
    drop(stdin);
    let finished = child
        .wait_with_output()
        .map_err(|source| Error::Decompress {
            scheme,
            reason: source.to_string(),
        })?;
    // The decompressor's own error explains a broken pipe, so it is read first.
    if !finished.status.success() {
        return Err(Error::Decompress {
            scheme,
            reason: complaint(&finished),
        });
    }
    outcome.map_err(|source| Error::Decompress {
        scheme,
        reason: source.to_string(),
    })
}

/// What the decompressor said, or failing that what it returned.
fn complaint(finished: &std::process::Output) -> String {
    String::from_utf8_lossy(&finished.stderr)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map_or_else(
            || format!("exited with {}", finished.status),
            ToOwned::to_owned,
        )
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

/// An HTTP agent that trusts the system certificate store.
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
    use crate::compression::Compression;
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
            url: Some("https://example.invalid/image.qcow2".to_owned()),
            digest: digest(),
            size: Some(1024),
            compression: Compression::None,
            firmware: crate::machine::Firmware::Bios,
            cpu: None,
            machine: crate::machine::Chipset::Q35,
            disk: crate::machine::Disk::Virtio,
        }
    }

    /// Compresses with the real tool.
    fn packed(scheme: Compression, plain: &[u8]) -> Vec<u8> {
        let packer = match scheme {
            Compression::Xz => "xz",
            Compression::Gzip => "gzip",
            Compression::Zstd => "zstd",
            Compression::None => unreachable!("nothing to pack"),
        };
        let mut child = std::process::Command::new(packer)
            .arg("-c")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        {
            use std::io::Write as _;
            child.stdin.take().unwrap().write_all(plain).unwrap();
        }
        let finished = child.wait_with_output().unwrap();
        assert!(finished.status.success(), "{packer} would not pack");
        finished.stdout
    }

    fn hex(bytes: &[u8]) -> String {
        let mut ignored = |_| {};
        let (_, hash) =
            digest::copy_hashing(&mut &bytes[..], Vec::new(), Algorithm::Sha256, &mut ignored)
                .unwrap();
        hash
    }

    fn compressed(scheme: Compression, bytes: &[u8]) -> Artifact {
        Artifact {
            compression: scheme,
            digest: Digest::from_str(&format!("sha256:{}", hex(bytes))).unwrap(),
            ..artifact()
        }
    }

    fn through(scheme: Compression, source: &[u8], destination: &Path) -> Result<String> {
        let mut ignored = |_| {};
        expand(
            scheme.command().unwrap(),
            &compressed(scheme, source),
            &mut &source[..],
            fs::File::create(destination).unwrap(),
            &mut ignored,
        )
    }

    fn entry() -> Entry {
        Entry {
            name: "debian".to_owned(),
            tag: "trixie".to_owned(),
            aliases: vec!["latest".to_owned()],
            description: "test entry".to_owned(),
            login: Login::CloudInit,
            artifacts: vec![artifact()],
            path: PathBuf::from("/catalogue/debian/trixie.toml"),
            catalogue: "project".to_owned(),
            kind: crate::catalogue::Kind::Remote,
            shadows: Vec::new(),
        }
    }

    #[test]
    fn a_discarded_build_is_gone_and_its_size_is_reported() {
        let scratch = Scratch::new("discard");
        let store = scratch.store();
        let path = store.path_for(&digest());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![0u8; 4096]).unwrap();
        assert_eq!(store.discard(&digest()).unwrap(), 4096);
        assert!(!store.contains(&digest()));
    }

    #[test]
    fn discarding_what_is_not_held_reclaims_nothing() {
        let scratch = Scratch::new("discardnone");
        assert_eq!(scratch.store().discard(&digest()).unwrap(), 0);
    }

    #[test]
    fn forgetting_a_reference_takes_its_record_and_the_empty_directory() {
        let scratch = Scratch::new("forget");
        let store = scratch.store();
        store.record(&entry(), &artifact()).unwrap();
        let directory = scratch.0.join("refs").join("debian");
        assert!(directory.join("trixie-amd64.toml").is_file());
        store.forget("debian", "trixie", "amd64");
        assert!(!directory.exists());
    }

    /// One tag going does not take another tag's record with it.
    #[test]
    fn forgetting_one_reference_leaves_the_others() {
        let scratch = Scratch::new("forgetone");
        let store = scratch.store();
        let mut other = entry();
        other.tag = "bookworm".to_owned();
        store.record(&entry(), &artifact()).unwrap();
        store.record(&other, &artifact()).unwrap();
        store.forget("debian", "trixie", "amd64");
        let directory = scratch.0.join("refs").join("debian");
        assert!(directory.join("bookworm-amd64.toml").is_file());
        assert!(!directory.join("trixie-amd64.toml").exists());
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

    #[test]
    fn each_scheme_is_expanded_and_the_published_digest_is_what_is_checked() {
        let scratch = Scratch::new("expand");
        // Larger than the copy buffer, and compressible.
        let plain: Vec<u8> = (0..400_000u32).map(|held| (held % 251) as u8).collect();
        for scheme in [Compression::Gzip, Compression::Xz, Compression::Zstd] {
            let source = packed(scheme, &plain);
            assert_ne!(source, plain, "{scheme:?} did not compress");
            let destination = scratch.0.join(format!("{}.img", scheme.name()));
            let hash = through(scheme, &source, &destination).unwrap();
            assert_eq!(hash, hex(&source), "{scheme:?} hashed the wrong stream");
            assert_eq!(fs::read(&destination).unwrap(), plain, "{scheme:?}");
        }
    }

    #[test]
    fn a_stream_that_is_not_what_it_claims_is_refused_in_its_own_words() {
        let scratch = Scratch::new("corrupt");
        for scheme in [Compression::Gzip, Compression::Xz, Compression::Zstd] {
            let source = b"this was never compressed".to_vec();
            let outcome = through(scheme, &source, &scratch.0.join("out.img"));
            match outcome {
                Err(error) => {
                    assert_eq!(error.kind(), "decompression-failed", "{error}");
                    assert!(error.to_string().contains(scheme.name()), "{error}");
                }
                Ok(_) => panic!("{scheme:?} accepted something that was not its own"),
            }
        }
    }

    #[test]
    fn expanding_reports_the_bytes_that_arrived() {
        let scratch = Scratch::new("progress");
        let plain = vec![7u8; 300_000];
        let source = packed(Compression::Gzip, &plain);
        let mut seen = Vec::new();
        let mut observe = |received| seen.push(received);
        let hash = expand(
            Compression::Gzip.command().unwrap(),
            &compressed(Compression::Gzip, &source),
            &mut &source[..],
            fs::File::create(scratch.0.join("out.img")).unwrap(),
            &mut observe,
        )
        .unwrap();
        assert_eq!(hash, hex(&source));
        assert_eq!(seen.last().copied(), Some(source.len() as u64));
        assert!(seen.iter().is_sorted(), "{seen:?}");
    }

    #[test]
    fn an_uncompressed_artifact_asks_for_no_decompressor() {
        assert!(artifact().compression.command().is_none());
    }
}

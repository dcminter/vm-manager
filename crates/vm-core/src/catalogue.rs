use crate::compression::Compression;
use crate::error::{Error, Result};
use crate::machine::{self, Chipset, Disk, Firmware};
use crate::reference::{Digest, Reference};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// How a guest can be reached once it boots, as claimed by the catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Login {
    /// Seedable with keys, accounts and shares.
    CloudInit,
    /// Boots with console access only.
    None,
}

impl Login {
    pub const fn is_seedable(self) -> bool {
        matches!(self, Self::CloudInit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RawEntry {
    name: String,
    tag: String,
    #[serde(default)]
    aliases: Vec<String>,
    description: String,
    login: Login,
    #[serde(rename = "image")]
    images: Vec<RawImage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RawImage {
    arch: String,
    format: String,
    /// Absent for a local image.
    url: Option<String>,
    digest: String,
    size: Option<u64>,
    /// Absent when uncompressed.
    #[serde(default)]
    compression: Compression,
    #[serde(default)]
    firmware: Firmware,
    cpu: Option<String>,
    #[serde(default)]
    machine: Chipset,
    #[serde(default)]
    disk: Disk,
}

/// One architecture's build of a catalogue entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub arch: String,
    /// The uncompressed image's format, as given to `qemu-img`.
    pub format: String,
    /// Where to fetch it, or nothing if it was made here by `vm clone`.
    pub url: Option<String>,
    /// The digest of the file as published, before decompression.
    pub digest: Digest,
    /// What the expanded image occupies, where the entry says.
    pub size: Option<u64>,
    pub compression: Compression,
    pub firmware: Firmware,
    /// The CPU model this image needs, where the default will not boot it.
    pub cpu: Option<String>,
    pub machine: Chipset,
    pub disk: Disk,
}

impl Artifact {
    /// The CPU model to present, falling back to the default.
    pub fn cpu(&self) -> &str {
        self.cpu.as_deref().unwrap_or(machine::DEFAULT_CPU)
    }
}

/// A named, tagged image as the catalogue describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub tag: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub login: Login,
    pub artifacts: Vec<Artifact>,
    /// The file the entry was read from.
    pub path: PathBuf,
}

impl Entry {
    pub fn artifact_for(&self, arch: &str) -> Option<&Artifact> {
        self.artifacts.iter().find(|artifact| artifact.arch == arch)
    }

    pub fn architectures(&self) -> Vec<String> {
        self.artifacts
            .iter()
            .map(|artifact| artifact.arch.clone())
            .collect()
    }
}

/// Entries loaded from a catalogue directory, indexed by `name:tag`.
#[derive(Debug, Default, Clone)]
pub struct Catalogue {
    entries: BTreeMap<String, Entry>,
}

fn key(name: &str, tag: &str) -> String {
    format!("{name}:{tag}")
}

impl Catalogue {
    /// Reads every `.toml` file below `root`; a missing directory is empty.
    pub fn load(root: &Path) -> Result<Self> {
        let mut catalogue = Self::default();
        if !root.exists() {
            return Ok(catalogue);
        }
        for path in toml_files(root)? {
            catalogue.insert(read_entry(&path)?, &path)?;
        }
        Ok(catalogue)
    }

    /// Reads directories in order, later entries shadowing earlier ones.
    pub fn load_layered(roots: &[PathBuf]) -> Result<Self> {
        let mut catalogue = Self::default();
        for root in roots {
            let layer = Self::load(root)?;
            for entry in layer.entries() {
                catalogue.replace(entry);
            }
        }
        Ok(catalogue)
    }

    /// Inserts an entry, displacing any with the same names.
    fn replace(&mut self, entry: &Entry) {
        let mut keys = vec![key(&entry.name, &entry.tag)];
        keys.extend(entry.aliases.iter().map(|alias| key(&entry.name, alias)));
        for candidate in keys {
            self.entries.insert(candidate, entry.clone());
        }
    }

    fn insert(&mut self, entry: Entry, path: &Path) -> Result<()> {
        let invalid = |reason: String| Error::CatalogueEntry {
            path: path.to_owned(),
            reason,
        };
        if entry.artifacts.is_empty() {
            return Err(invalid("no [[image]] sections".to_owned()));
        }
        let mut keys = vec![key(&entry.name, &entry.tag)];
        keys.extend(entry.aliases.iter().map(|alias| key(&entry.name, alias)));
        for candidate in &keys {
            if self.entries.contains_key(candidate) {
                return Err(invalid(format!("'{candidate}' is already defined")));
            }
        }
        let last = keys.pop().unwrap_or_default();
        for candidate in keys {
            self.entries.insert(candidate, entry.clone());
        }
        self.entries.insert(last, entry);
        Ok(())
    }

    /// Resolves a reference to the artifact for `arch`.
    pub fn resolve(&self, reference: &Reference, arch: &str) -> Result<(&Entry, &Artifact)> {
        let entry = self
            .entries
            .get(&key(reference.repository(), reference.tag()))
            .ok_or_else(|| Error::UnknownImage {
                reference: reference.to_string(),
            })?;
        let artifact = entry
            .artifact_for(arch)
            .ok_or_else(|| Error::UnsupportedArchitecture {
                reference: reference.to_string(),
                wanted: arch.to_owned(),
                available: entry.architectures(),
            })?;
        if let Some(pinned) = reference.digest()
            && *pinned != artifact.digest
        {
            return Err(Error::PinMismatch {
                reference: reference.to_string(),
                arch: arch.to_owned(),
                actual: artifact.digest.to_string(),
            });
        }
        Ok((entry, artifact))
    }

    /// Every entry once, with aliases collapsed away.
    pub fn entries(&self) -> Vec<&Entry> {
        let mut seen = Vec::new();
        for entry in self.entries.values() {
            if !seen
                .iter()
                .any(|held: &&Entry| held.name == entry.name && held.tag == entry.tag)
            {
                seen.push(entry);
            }
        }
        seen
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn toml_files(root: &Path) -> Result<Vec<PathBuf>> {
    let read = |path: &Path| {
        fs::read_dir(path).map_err(|source| Error::CatalogueRead {
            path: path.to_owned(),
            source,
        })
    };
    let mut found = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in read(&directory)? {
            let entry = entry.map_err(|source| Error::CatalogueRead {
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "toml")
            {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

fn read_entry(path: &Path) -> Result<Entry> {
    let text = fs::read_to_string(path).map_err(|source| Error::CatalogueRead {
        path: path.to_owned(),
        source,
    })?;
    let raw: RawEntry = basic_toml::from_str(&text).map_err(|source| Error::CatalogueParse {
        path: path.to_owned(),
        source,
    })?;
    let artifacts = raw
        .images
        .into_iter()
        .map(|image| {
            machine::check_disk(image.machine, image.disk).map_err(|error| {
                Error::CatalogueEntry {
                    path: path.to_owned(),
                    reason: error.to_string(),
                }
            })?;
            Ok(Artifact {
                arch: image.arch,
                format: image.format,
                url: image.url,
                digest: Digest::from_str(&image.digest).map_err(|error| Error::CatalogueEntry {
                    path: path.to_owned(),
                    reason: error.to_string(),
                })?,
                size: image.size,
                compression: image.compression,
                firmware: image.firmware,
                cpu: match image.cpu {
                    Some(cpu) => {
                        machine::check_cpu(&cpu).map_err(|error| Error::CatalogueEntry {
                            path: path.to_owned(),
                            reason: error.to_string(),
                        })?;
                        Some(cpu)
                    }
                    None => None,
                },
                machine: image.machine,
                disk: image.disk,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Entry {
        name: raw.name,
        tag: raw.tag,
        aliases: raw.aliases,
        description: raw.description,
        login: raw.login,
        artifacts,
        path: path.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::io::Write;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("vm-catalogue-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, relative: &str, body: &str) {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            let mut file = fs::File::create(path).unwrap();
            file.write_all(body.as_bytes()).unwrap();
        }

        fn load(&self) -> Result<Catalogue> {
            Catalogue::load(&self.0)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn an_entry_that_is_silent_describes_an_uncompressed_image() {
        let scratch = Scratch::new("plain");
        scratch.write("x/y.toml", &entry_toml("x", "y", ""));
        let catalogue = scratch.load().unwrap();
        let (_, artifact) = catalogue.resolve(&"x:y".parse().unwrap(), "amd64").unwrap();
        assert_eq!(artifact.compression, Compression::None);
        assert_eq!(artifact.format, "qcow2");
    }

    #[test]
    fn an_entry_carries_the_compression_and_format_it_states() {
        let scratch = Scratch::new("packed");
        scratch.write(
            "netbsd/10.1.toml",
            &format!(
                r#"
name = "netbsd"
tag = "10.1"
description = "test entry"
login = "none"

[[image]]
arch = "amd64"
format = "raw"
compression = "gzip"
url = "https://example.invalid/netbsd.img.gz"
digest = "sha512:{}"
"#,
                "a".repeat(128)
            ),
        );
        let catalogue = scratch.load().unwrap();
        let (_, artifact) = catalogue
            .resolve(&"netbsd:10.1".parse().unwrap(), "amd64")
            .unwrap();
        assert_eq!(artifact.compression, Compression::Gzip);
        assert_eq!(artifact.format, "raw");
    }

    fn machine_entry(settings: &str) -> String {
        format!(
            r#"
name = "puredarwin"
tag = "minimal"
description = "test entry"
login = "none"

[[image]]
arch = "amd64"
format = "raw"
url = "https://example.invalid/pd.img"
digest = "sha256:{}"
{settings}
"#,
            "a".repeat(64)
        )
    }

    #[test]
    fn an_entry_that_is_silent_asks_for_the_default_machine() {
        let scratch = Scratch::new("defaultmachine");
        scratch.write("puredarwin/minimal.toml", &machine_entry(""));
        let catalogue = scratch.load().unwrap();
        let (_, artifact) = catalogue
            .resolve(&"puredarwin:minimal".parse().unwrap(), "amd64")
            .unwrap();
        assert_eq!(artifact.firmware, Firmware::Bios);
        assert_eq!(artifact.cpu, None);
        assert_eq!(artifact.cpu(), "max");
        assert_eq!(artifact.machine, Chipset::Q35);
        assert_eq!(artifact.disk, Disk::Virtio);
    }

    #[test]
    fn an_entry_carries_the_machine_it_needs() {
        let scratch = Scratch::new("machine");
        scratch.write(
            "puredarwin/minimal.toml",
            &machine_entry("firmware = \"uefi\"\ncpu = \"Penryn,vendor=GenuineIntel,+avx\""),
        );
        let catalogue = scratch.load().unwrap();
        let (_, artifact) = catalogue
            .resolve(&"puredarwin:minimal".parse().unwrap(), "amd64")
            .unwrap();
        assert_eq!(artifact.firmware, Firmware::Uefi);
        assert_eq!(artifact.cpu(), "Penryn,vendor=GenuineIntel,+avx");
    }

    #[test]
    fn an_entry_carries_the_chipset_and_disk_controller_it_needs() {
        let scratch = Scratch::new("chipset");
        scratch.write(
            "puredarwin/minimal.toml",
            &machine_entry("machine = \"pc\"\ndisk = \"ide\""),
        );
        let catalogue = scratch.load().unwrap();
        let (_, artifact) = catalogue
            .resolve(&"puredarwin:minimal".parse().unwrap(), "amd64")
            .unwrap();
        assert_eq!(artifact.machine, Chipset::Pc);
        assert_eq!(artifact.disk, Disk::Ide);
    }

    #[test]
    fn an_entry_asking_for_a_controller_its_chipset_lacks_names_its_file() {
        let scratch = Scratch::new("mismatch");
        scratch.write("puredarwin/minimal.toml", &machine_entry("disk = \"ide\""));
        let error = scratch.load().unwrap_err();
        assert!(error.to_string().contains("minimal.toml"), "{error}");
        assert!(error.to_string().contains("no ide controller"), "{error}");
    }

    #[test]
    fn an_entry_with_an_unusable_cpu_model_names_its_file() {
        let scratch = Scratch::new("badcpu");
        scratch.write(
            "puredarwin/minimal.toml",
            &machine_entry("cpu = \"max -snapshot\""),
        );
        let error = scratch.load().unwrap_err();
        assert!(error.to_string().contains("minimal.toml"), "{error}");
        assert!(error.to_string().contains("cpu"), "{error}");
    }

    #[test]
    fn an_entry_with_unknown_firmware_is_refused() {
        let scratch = Scratch::new("badfirmware");
        scratch.write(
            "puredarwin/minimal.toml",
            &machine_entry("firmware = \"coreboot\""),
        );
        assert!(scratch.load().is_err());
    }

    fn entry_toml(name: &str, tag: &str, aliases: &str) -> String {
        format!(
            r#"
name = "{name}"
tag = "{tag}"
aliases = [{aliases}]
description = "test entry"
login = "cloud-init"

[[image]]
arch = "amd64"
format = "qcow2"
url = "https://example.invalid/{name}-{tag}.qcow2"
digest = "sha512:{}"
size = 1024
"#,
            "a".repeat(128)
        )
    }

    fn reference(text: &str) -> Reference {
        text.parse().unwrap()
    }

    #[test]
    fn a_missing_directory_loads_as_empty() {
        let catalogue = Catalogue::load(Path::new("/nonexistent/vm/catalogue")).unwrap();
        assert!(catalogue.is_empty());
    }

    #[test]
    fn entries_are_found_in_nested_directories() {
        let scratch = Scratch::new("nested");
        scratch.write("debian/trixie.toml", &entry_toml("debian", "trixie", ""));
        let catalogue = scratch.load().unwrap();
        let (entry, artifact) = catalogue
            .resolve(&reference("debian:trixie"), "amd64")
            .unwrap();
        assert_eq!(entry.description, "test entry");
        assert_eq!(artifact.size, Some(1024));
    }

    #[test]
    fn aliases_resolve_to_the_same_entry() {
        let scratch = Scratch::new("aliases");
        scratch.write(
            "debian/trixie.toml",
            &entry_toml("debian", "trixie", r#""13", "latest""#),
        );
        let catalogue = scratch.load().unwrap();
        let canonical = catalogue
            .resolve(&reference("debian:trixie"), "amd64")
            .unwrap()
            .0;
        for alias in ["debian:13", "debian:latest", "debian"] {
            let found = catalogue.resolve(&reference(alias), "amd64").unwrap().0;
            assert_eq!(
                found, canonical,
                "{alias} should resolve to the canonical entry"
            );
        }
    }

    #[test]
    fn aliases_do_not_duplicate_the_listing() {
        let scratch = Scratch::new("listing");
        scratch.write(
            "debian/trixie.toml",
            &entry_toml("debian", "trixie", r#""13", "latest""#),
        );
        scratch.write("debian/forky.toml", &entry_toml("debian", "forky", ""));
        let catalogue = scratch.load().unwrap();
        assert_eq!(catalogue.entries().len(), 2);
    }

    #[test]
    fn an_unknown_reference_names_itself() {
        let scratch = Scratch::new("unknown");
        scratch.write("debian/trixie.toml", &entry_toml("debian", "trixie", ""));
        let error = scratch
            .load()
            .unwrap()
            .resolve(&reference("ubuntu:noble"), "amd64")
            .unwrap_err();
        assert!(matches!(error, Error::UnknownImage { .. }));
        assert!(error.to_string().contains("ubuntu:noble"));
    }

    #[test]
    fn a_missing_architecture_lists_what_there_is() {
        let scratch = Scratch::new("arch");
        scratch.write("debian/trixie.toml", &entry_toml("debian", "trixie", ""));
        let error = scratch
            .load()
            .unwrap()
            .resolve(&reference("debian:trixie"), "riscv64")
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("riscv64"), "{message}");
        assert!(message.contains("amd64"), "{message}");
    }

    #[test]
    fn a_pinned_digest_must_match_the_build() {
        let scratch = Scratch::new("pinned");
        scratch.write("debian/trixie.toml", &entry_toml("debian", "trixie", ""));
        let catalogue = scratch.load().unwrap();
        let held = format!("debian:trixie@sha512:{}", "a".repeat(128));
        assert!(catalogue.resolve(&reference(&held), "amd64").is_ok());
        let other = format!("debian:trixie@sha512:{}", "b".repeat(128));
        let error = catalogue.resolve(&reference(&other), "amd64").unwrap_err();
        assert_eq!(error.kind(), "pin-mismatch");
        let message = error.to_string();
        assert!(message.contains(&"a".repeat(128)), "{message}");
        assert!(message.contains("amd64"), "{message}");
        let algorithm = format!("debian:trixie@sha256:{}", "a".repeat(64));
        assert!(catalogue.resolve(&reference(&algorithm), "amd64").is_err());
    }

    #[test]
    fn an_entry_records_the_file_it_came_from() {
        let scratch = Scratch::new("path");
        scratch.write("debian/trixie.toml", &entry_toml("debian", "trixie", ""));
        let catalogue = scratch.load().unwrap();
        let (entry, _) = catalogue
            .resolve(&reference("debian:trixie"), "amd64")
            .unwrap();
        assert_eq!(entry.path, scratch.0.join("debian/trixie.toml"));
    }

    #[test]
    fn a_tag_colliding_with_an_alias_is_rejected() {
        let scratch = Scratch::new("collision");
        scratch.write(
            "debian/trixie.toml",
            &entry_toml("debian", "trixie", r#""13""#),
        );
        scratch.write("debian/thirteen.toml", &entry_toml("debian", "13", ""));
        let error = scratch.load().unwrap_err();
        assert!(matches!(error, Error::CatalogueEntry { .. }), "{error}");
        assert!(error.to_string().contains("debian:13"));
    }

    #[test]
    fn an_entry_without_images_is_rejected() {
        let scratch = Scratch::new("imageless");
        scratch.write(
            "debian/trixie.toml",
            r#"
name = "debian"
tag = "trixie"
description = "test entry"
login = "none"
image = []
"#,
        );
        let error = scratch.load().unwrap_err();
        assert!(
            error.to_string().contains("no [[image]] sections"),
            "{error}"
        );
    }

    #[test]
    fn a_malformed_digest_names_the_file() {
        let scratch = Scratch::new("digest");
        scratch.write(
            "debian/trixie.toml",
            r#"
name = "debian"
tag = "trixie"
description = "test entry"
login = "none"

[[image]]
arch = "amd64"
format = "qcow2"
url = "https://example.invalid/x.qcow2"
digest = "sha512:tooshort"
"#,
        );
        let error = scratch.load().unwrap_err();
        assert!(error.to_string().contains("trixie.toml"), "{error}");
    }

    #[test]
    fn unparseable_toml_names_the_file() {
        let scratch = Scratch::new("broken");
        scratch.write("debian/trixie.toml", "name = \"debian\"\nthis is not toml");
        let error = scratch.load().unwrap_err();
        assert!(matches!(error, Error::CatalogueParse { .. }), "{error}");
    }

    #[test]
    fn non_toml_files_are_ignored() {
        let scratch = Scratch::new("ignored");
        scratch.write("debian/trixie.toml", &entry_toml("debian", "trixie", ""));
        scratch.write("debian/README.md", "not an entry");
        assert_eq!(scratch.load().unwrap().entries().len(), 1);
    }

    #[test]
    fn seedable_is_decided_by_the_login_field() {
        assert!(Login::CloudInit.is_seedable());
        assert!(!Login::None.is_seedable());
    }
}

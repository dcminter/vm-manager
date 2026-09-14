use crate::compression::Compression;
use crate::error::{Error, Result};
use crate::machine::{self, Chipset, Disk, Firmware};
use crate::reference::{Digest, Reference};
use crate::value::toml_string;
use serde::{Deserialize, Serialize};
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

impl FromStr for Login {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        match text {
            "cloud-init" => Ok(Self::CloudInit),
            "none" => Ok(Self::None),
            _ => Err(Error::MachineSetting {
                setting: "login",
                value: text.to_owned(),
                expected: "'cloud-init' or 'none'",
            }),
        }
    }
}

impl Login {
    pub const ALL: [Self; 2] = [Self::CloudInit, Self::None];

    pub const fn is_seedable(self) -> bool {
        matches!(self, Self::CloudInit)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::CloudInit => "cloud-init",
            Self::None => "none",
        }
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
    /// Absent when published in `format`.
    source_format: Option<String>,
    /// The file in a published tar archive that is the image.
    archive_member: Option<String>,
    #[serde(default)]
    media: Media,
    #[serde(default)]
    firmware: Firmware,
    #[serde(alias = "cpu")]
    cpu_model: Option<String>,
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
    /// The published image's format, where a pull converts it to `format`.
    pub source_format: Option<String>,
    /// The file in a published tar archive that is the image.
    pub archive_member: Option<String>,
    pub media: Media,
    pub firmware: Firmware,
    /// The CPU model this image needs, where the default will not boot it.
    pub cpu_model: Option<String>,
    pub machine: Chipset,
    pub disk: Disk,
}

impl Artifact {
    /// The CPU model to present, falling back to the default.
    pub fn cpu_model(&self) -> &str {
        self.cpu_model
            .as_deref()
            .unwrap_or(machine::DEFAULT_CPU_MODEL)
    }
}

/// How a machine is given an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Media {
    /// The base of the machine's disk.
    #[default]
    Disk,
    /// A CD-ROM beside a blank disk.
    Cdrom,
}

impl Media {
    pub const fn is_cdrom(self) -> bool {
        matches!(self, Self::Cdrom)
    }

    pub const fn is_disk(&self) -> bool {
        matches!(self, Self::Disk)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Disk => "disk",
            Self::Cdrom => "cdrom",
        }
    }
}

/// Whether a catalogue is fetched or read where it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Remote,
    Local,
}

impl Kind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Remote => "remote",
            Self::Local => "local",
        }
    }
}

/// A catalogue directory and what it is called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub kind: Kind,
    pub directory: PathBuf,
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
    pub catalogue: String,
    pub kind: Kind,
    /// Lower catalogues whose entry of the same name this one hides.
    pub shadows: Vec<String>,
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

    /// Reads catalogues in order, later entries shadowing earlier ones.
    pub fn load_layered(sources: &[Source]) -> Result<Self> {
        let mut catalogue = Self::default();
        for source in sources {
            let layer = Self::load(&source.directory)?;
            for entry in layer.entries() {
                let mut entry = entry.clone();
                entry.catalogue.clone_from(&source.name);
                entry.kind = source.kind;
                catalogue.replace(entry);
            }
        }
        Ok(catalogue)
    }

    /// Inserts an entry, displacing any with the same names.
    fn replace(&mut self, mut entry: Entry) {
        let mut keys = vec![key(&entry.name, &entry.tag)];
        keys.extend(entry.aliases.iter().map(|alias| key(&entry.name, alias)));
        for candidate in &keys {
            if let Some(displaced) = self.entries.get(candidate) {
                for name in std::iter::once(&displaced.catalogue).chain(&displaced.shadows) {
                    if *name != entry.catalogue && !entry.shadows.contains(name) {
                        entry.shadows.push(name.clone());
                    }
                }
            }
        }
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

    /// The entry `name:tag` names, by its tag or an alias, whatever its architectures.
    pub fn find(&self, name: &str, tag: &str) -> Option<&Entry> {
        self.entries.get(&key(name, tag))
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

/// An entry for the store catalogue, as `vm clone` and `vm import` write one.
#[derive(Debug, Clone, Copy)]
pub struct NewEntry<'a> {
    pub name: &'a str,
    pub tag: &'a str,
    pub description: &'a str,
    pub login: Login,
    pub artifact: &'a Artifact,
}

impl NewEntry<'_> {
    /// The entry as TOML, leaving out what defaults.
    pub fn to_toml(&self) -> String {
        use std::fmt::Write as _;
        let artifact = self.artifact;
        let mut body = format!(
            "name = {}\ntag = {}\ndescription = {}\nlogin = \"{}\"\n\n[[image]]\narch = {}\nformat = {}\n",
            toml_string(self.name),
            toml_string(self.tag),
            toml_string(self.description),
            self.login.name(),
            toml_string(&artifact.arch),
            toml_string(&artifact.format),
        );
        if let Some(url) = &artifact.url {
            let _ = writeln!(body, "url = {}", toml_string(url));
        }
        let _ = writeln!(body, "digest = \"{}\"", artifact.digest);
        if let Some(size) = artifact.size {
            let _ = writeln!(body, "size = {size}");
        }
        if !artifact.compression.is_none() {
            let _ = writeln!(body, "compression = \"{}\"", artifact.compression.name());
        }
        if let Some(format) = &artifact.source_format {
            let _ = writeln!(body, "source_format = {}", toml_string(format));
        }
        if let Some(member) = &artifact.archive_member {
            let _ = writeln!(body, "archive_member = {}", toml_string(member));
        }
        if artifact.media.is_cdrom() {
            let _ = writeln!(body, "media = \"{}\"", artifact.media.name());
        }
        if !artifact.firmware.is_default() {
            let _ = writeln!(body, "firmware = \"{}\"", artifact.firmware.name());
        }
        if !artifact.machine.is_default() {
            let _ = writeln!(body, "machine = \"{}\"", artifact.machine.name());
        }
        if !artifact.disk.is_default() {
            let _ = writeln!(body, "disk = \"{}\"", artifact.disk.name());
        }
        if let Some(cpu) = &artifact.cpu_model {
            let _ = writeln!(body, "cpu_model = {}", toml_string(cpu));
        }
        body
    }

    /// Writes the entry as `NAME/TAG.toml` under `root`, replacing any there.
    pub fn write(&self, root: &Path) -> Result<PathBuf> {
        let directory = root.join(self.name);
        fs::create_dir_all(&directory).map_err(|source| Error::Store {
            path: directory.clone(),
            action: "create",
            source,
        })?;
        let path = directory.join(format!("{}.toml", self.tag));
        let staging = directory.join(format!("{}.toml.new", self.tag));
        fs::write(&staging, self.to_toml()).map_err(|source| Error::Store {
            path: staging.clone(),
            action: "write",
            source,
        })?;
        fs::rename(&staging, &path).map_err(|source| Error::Store {
            path: path.clone(),
            action: "write",
            source,
        })?;
        Ok(path)
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

/// Refuses formats a pull could not store or a machine could not use.
fn check_media(image: &RawImage) -> std::result::Result<(), &'static str> {
    if image.source_format.is_some() && image.format != "qcow2" {
        return Err("an image with a source_format needs format = \"qcow2\"");
    }
    if image.media.is_cdrom() && (image.format != "raw" || image.source_format.is_some()) {
        return Err("a cdrom image needs format = \"raw\" and no source_format");
    }
    if image
        .archive_member
        .as_deref()
        .is_some_and(|member| member.is_empty() || member.chars().any(char::is_control))
    {
        return Err("an archive_member names a file with a non-empty path of plain characters");
    }
    Ok(())
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
            check_media(&image).map_err(|reason| Error::CatalogueEntry {
                path: path.to_owned(),
                reason: reason.to_owned(),
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
                source_format: image.source_format,
                archive_member: image.archive_member,
                media: image.media,
                firmware: image.firmware,
                cpu_model: match image.cpu_model {
                    Some(cpu) => {
                        machine::check_cpu_model(&cpu).map_err(|error| Error::CatalogueEntry {
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
        catalogue: String::new(),
        kind: Kind::Remote,
        shadows: Vec::new(),
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

    fn image_entry(format: &str, settings: &str) -> String {
        format!(
            r#"
name = "x"
tag = "y"
description = "test entry"
login = "none"

[[image]]
arch = "amd64"
format = "{format}"
url = "https://example.invalid/x"
digest = "sha256:{}"
{settings}
"#,
            "a".repeat(64)
        )
    }

    fn artifact_of(body: &str) -> Result<Artifact> {
        let scratch = Scratch::new(&format!("media{}", body.len()));
        scratch.write("x/y.toml", body);
        let catalogue = scratch.load()?;
        Ok(catalogue.resolve(&reference("x:y"), "amd64")?.1.clone())
    }

    #[test]
    fn an_image_is_a_disk_published_as_stored_unless_the_entry_says_otherwise() {
        let artifact = artifact_of(&image_entry("qcow2", "")).unwrap();
        assert_eq!(artifact.media, Media::Disk);
        assert_eq!(artifact.source_format, None);
    }

    #[test]
    fn an_image_can_be_published_in_another_format_and_stored_as_qcow2() {
        let artifact = artifact_of(&image_entry("qcow2", "source_format = \"vmdk\"")).unwrap();
        assert_eq!(artifact.source_format.as_deref(), Some("vmdk"));
        let error = artifact_of(&image_entry("raw", "source_format = \"vmdk\"")).unwrap_err();
        assert!(error.to_string().contains("qcow2"), "{error}");
    }

    #[test]
    fn an_image_can_be_published_inside_an_archive() {
        let artifact = artifact_of(&image_entry(
            "qcow2",
            "compression = \"xz\"\nsource_format = \"raw\"\narchive_member = \"disk.raw\"",
        ))
        .unwrap();
        assert_eq!(artifact.archive_member.as_deref(), Some("disk.raw"));
        assert_eq!(artifact.source_format.as_deref(), Some("raw"));
        for member in ["\"\"", "\"disk\\nraw\""] {
            let error = artifact_of(&image_entry(
                "qcow2",
                &format!(
                    "archive_member = {member}\n# {}",
                    "x".repeat(member.len() + 40)
                ),
            ))
            .unwrap_err();
            assert!(error.to_string().contains("archive_member"), "{error}");
        }
    }

    #[test]
    fn a_cdrom_image_is_raw_and_published_as_it_is() {
        let artifact = artifact_of(&image_entry("raw", "media = \"cdrom\"")).unwrap();
        assert!(artifact.media.is_cdrom());
        for body in [
            image_entry("qcow2", "media = \"cdrom\""),
            image_entry("qcow2", "media = \"cdrom\"\nsource_format = \"raw\""),
        ] {
            let error = artifact_of(&body).unwrap_err();
            assert!(error.to_string().contains("cdrom"), "{error}");
        }
        assert!(artifact_of(&image_entry("raw", "media = \"tape\"")).is_err());
    }

    fn written(artifact: &Artifact, description: &str) -> (Entry, Artifact) {
        let scratch = Scratch::new(&format!("written{}", artifact.format));
        let path = NewEntry {
            name: "mine",
            tag: "1.0",
            description,
            login: Login::None,
            artifact,
        }
        .write(&scratch.0)
        .unwrap();
        assert_eq!(path, scratch.0.join("mine/1.0.toml"));
        let catalogue = scratch.load().unwrap();
        let (entry, read) = catalogue
            .resolve(&reference("mine:1.0"), &artifact.arch)
            .unwrap();
        (entry.clone(), read.clone())
    }

    fn plain_artifact() -> Artifact {
        Artifact {
            arch: "amd64".to_owned(),
            format: "qcow2".to_owned(),
            url: None,
            digest: format!("sha256:{}", "b".repeat(64)).parse().unwrap(),
            size: Some(10),
            compression: Compression::None,
            source_format: None,
            archive_member: None,
            media: Media::Disk,
            firmware: Firmware::Bios,
            cpu_model: None,
            machine: Chipset::Q35,
            disk: Disk::Virtio,
        }
    }

    #[test]
    fn a_written_entry_reads_back_as_it_was_given() {
        let fetched = Artifact {
            url: Some("https://example.invalid/a \"b\".vmdk.xz".to_owned()),
            compression: Compression::Xz,
            source_format: Some("vmdk".to_owned()),
            archive_member: Some("images/disk.vmdk".to_owned()),
            firmware: Firmware::Uefi,
            cpu_model: Some("Penryn,+avx".to_owned()),
            machine: Chipset::Pc,
            disk: Disk::Ide,
            ..plain_artifact()
        };
        let (entry, read) = written(&fetched, "Imported \"x\"\\y");
        assert_eq!(read, fetched);
        assert_eq!(entry.description, "Imported \"x\"\\y");
        assert_eq!(entry.login, Login::None);
        let disc = Artifact {
            format: "raw".to_owned(),
            media: Media::Cdrom,
            arch: "arm64".to_owned(),
            ..plain_artifact()
        };
        assert_eq!(written(&disc, "d").1, disc);
    }

    #[test]
    fn a_written_entry_leaves_out_what_defaults() {
        let text = NewEntry {
            name: "mine",
            tag: "1.0",
            description: "d",
            login: Login::CloudInit,
            artifact: &plain_artifact(),
        }
        .to_toml();
        for absent in [
            "url",
            "compression",
            "source_format",
            "media",
            "firmware",
            "machine",
            "disk =",
            "cpu",
        ] {
            assert!(!text.contains(absent), "{absent}: {text}");
        }
        assert!(text.contains("login = \"cloud-init\""), "{text}");
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
        assert_eq!(artifact.cpu_model, None);
        assert_eq!(artifact.cpu_model(), "max");
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
        assert_eq!(artifact.cpu_model(), "Penryn,vendor=GenuineIntel,+avx");
    }

    #[test]
    fn an_entry_names_its_cpu_model_either_way() {
        for key in ["cpu_model", "cpu"] {
            let scratch = Scratch::new(&format!("cpukey{key}"));
            scratch.write(
                "puredarwin/minimal.toml",
                &machine_entry(&format!("{key} = \"Penryn,+avx\"")),
            );
            let catalogue = scratch.load().unwrap();
            let (_, artifact) = catalogue
                .resolve(&"puredarwin:minimal".parse().unwrap(), "amd64")
                .unwrap();
            assert_eq!(artifact.cpu_model(), "Penryn,+avx", "{key}");
        }
    }

    #[test]
    fn a_written_entry_names_its_cpu_model_as_cpu_model() {
        let artifact = Artifact {
            cpu_model: Some("Penryn,+avx".to_owned()),
            ..plain_artifact()
        };
        let text = NewEntry {
            name: "mine",
            tag: "1.0",
            description: "d",
            login: Login::None,
            artifact: &artifact,
        }
        .to_toml();
        assert!(text.contains("cpu_model = \"Penryn,+avx\""), "{text}");
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
        assert!(error.to_string().contains("CPU model"), "{error}");
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
    fn a_login_is_read_by_its_name() {
        for login in Login::ALL {
            assert_eq!(login.name().parse::<Login>().unwrap(), login);
        }
        assert_eq!(
            "ssh".parse::<Login>().unwrap_err().kind(),
            "invalid-machine-setting"
        );
    }

    #[test]
    fn an_entry_is_found_by_its_tag_or_an_alias() {
        let scratch = Scratch::new("find");
        scratch.write(
            "debian/trixie.toml",
            &entry_toml("debian", "trixie", "\"13\""),
        );
        let catalogue = scratch.load().unwrap();
        assert_eq!(catalogue.find("debian", "trixie").unwrap().tag, "trixie");
        assert_eq!(catalogue.find("debian", "13").unwrap().tag, "trixie");
        assert!(catalogue.find("debian", "12").is_none());
        assert!(catalogue.find("ubuntu", "trixie").is_none());
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

    fn source(scratch: &Scratch, name: &str, kind: Kind) -> Source {
        Source {
            name: name.to_owned(),
            kind,
            directory: scratch.0.join(name),
        }
    }

    #[test]
    fn the_last_catalogue_to_name_an_image_wins_and_records_what_it_hides() {
        let scratch = Scratch::new("layers");
        scratch.write(
            "public/debian/trixie.toml",
            &entry_toml("debian", "trixie", r#""latest""#),
        );
        scratch.write(
            "internal/debian/trixie.toml",
            &entry_toml("debian", "trixie", ""),
        );
        scratch.write(
            "internal/alpine/edge.toml",
            &entry_toml("alpine", "edge", ""),
        );
        scratch.write(
            "team/debian/trixie.toml",
            &entry_toml("debian", "trixie", ""),
        );
        let catalogue = Catalogue::load_layered(&[
            source(&scratch, "public", Kind::Remote),
            source(&scratch, "internal", Kind::Remote),
            source(&scratch, "team", Kind::Local),
            source(&scratch, "absent", Kind::Local),
        ])
        .unwrap();
        let (entry, _) = catalogue
            .resolve(&reference("debian:trixie"), "amd64")
            .unwrap();
        assert_eq!(entry.catalogue, "team");
        assert_eq!(entry.kind, Kind::Local);
        assert_eq!(entry.shadows, ["internal", "public"]);
        assert_eq!(entry.path, scratch.0.join("team/debian/trixie.toml"));
        let (alias, _) = catalogue
            .resolve(&reference("debian:latest"), "amd64")
            .unwrap();
        assert_eq!(alias.catalogue, "public");
        let (alpine, _) = catalogue
            .resolve(&reference("alpine:edge"), "amd64")
            .unwrap();
        assert_eq!(
            (alpine.catalogue.as_str(), alpine.kind),
            ("internal", Kind::Remote)
        );
        assert!(alpine.shadows.is_empty());
    }

    #[test]
    fn a_name_used_twice_within_one_catalogue_is_still_an_error() {
        let scratch = Scratch::new("within");
        scratch.write("one/debian/a.toml", &entry_toml("debian", "trixie", ""));
        scratch.write("one/debian/b.toml", &entry_toml("debian", "trixie", ""));
        assert!(Catalogue::load_layered(&[source(&scratch, "one", Kind::Remote)]).is_err());
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

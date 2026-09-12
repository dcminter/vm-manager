//! The virtual hardware a guest is given where an image needs more than the defaults.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// The processor model presented unless an image or the user asks for another.
pub const DEFAULT_CPU: &str = "max";

/// The firmware that finds the guest's boot loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Firmware {
    #[default]
    Bios,
    Uefi,
}

impl Firmware {
    pub const fn is_default(&self) -> bool {
        matches!(self, Self::Bios)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Bios => "bios",
            Self::Uefi => "uefi",
        }
    }
}

impl FromStr for Firmware {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        match text {
            "bios" => Ok(Self::Bios),
            "uefi" => Ok(Self::Uefi),
            _ => Err(Error::MachineSetting {
                setting: "firmware",
                value: text.to_owned(),
                expected: "'bios' or 'uefi'",
            }),
        }
    }
}

/// The chipset, which decides which disk controllers exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Chipset {
    #[default]
    Q35,
    Pc,
}

impl Chipset {
    pub const fn is_default(&self) -> bool {
        matches!(self, Self::Q35)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Q35 => "q35",
            Self::Pc => "pc",
        }
    }
}

impl FromStr for Chipset {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        match text {
            "q35" => Ok(Self::Q35),
            "pc" => Ok(Self::Pc),
            _ => Err(Error::MachineSetting {
                setting: "machine",
                value: text.to_owned(),
                expected: "'q35' or 'pc'",
            }),
        }
    }
}

/// The controller the guest's disks are attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Disk {
    #[default]
    Virtio,
    Ide,
    Sata,
}

impl Disk {
    pub const fn is_default(&self) -> bool {
        matches!(self, Self::Virtio)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Virtio => "virtio",
            Self::Ide => "ide",
            Self::Sata => "sata",
        }
    }

    /// The `-drive` interface for the disk at `index` on this controller.
    pub fn interface(self, index: u8) -> String {
        match self {
            Self::Virtio => "if=virtio".to_owned(),
            // The chipset's built-in controller: IDE on pc, AHCI on q35.
            Self::Ide | Self::Sata => format!("if=ide,index={index}"),
        }
    }
}

impl FromStr for Disk {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        match text {
            "virtio" => Ok(Self::Virtio),
            "ide" => Ok(Self::Ide),
            "sata" => Ok(Self::Sata),
            _ => Err(Error::MachineSetting {
                setting: "disk",
                value: text.to_owned(),
                expected: "'virtio', 'ide' or 'sata'",
            }),
        }
    }
}

/// Refuses a disk controller the chipset does not have.
pub const fn check_disk(chipset: Chipset, disk: Disk) -> Result<()> {
    match (chipset, disk) {
        (Chipset::Q35, Disk::Ide) => Err(Error::MachineMismatch {
            chipset: chipset.name(),
            disk: disk.name(),
            instead: "use 'pc' for an IDE disk",
        }),
        (Chipset::Pc, Disk::Sata) => Err(Error::MachineMismatch {
            chipset: chipset.name(),
            disk: disk.name(),
            instead: "use 'q35' for a SATA disk",
        }),
        _ => Ok(()),
    }
}

/// Refuses a CPU model that could not be one, before it reaches the command line.
pub fn check_cpu(text: &str) -> Result<()> {
    let usable = !text.is_empty()
        && text.len() <= 256
        && text.starts_with(|first: char| first.is_ascii_alphanumeric())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b",._=+-".contains(&byte));
    if usable {
        Ok(())
    } else {
        Err(Error::MachineSetting {
            setting: "cpu",
            value: text.to_owned(),
            expected: "a QEMU CPU model such as 'max' or 'Penryn,+avx'",
        })
    }
}

/// UEFI firmware: code every machine shares, and a variable store each keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uefi {
    pub code: PathBuf,
    pub variables: PathBuf,
}

/// The firmware files and their package, where Debian installs them.
fn installed(arch: &str) -> Option<(Uefi, &'static str)> {
    match arch {
        "amd64" => Some((
            Uefi {
                code: PathBuf::from("/usr/share/OVMF/OVMF_CODE_4M.fd"),
                variables: PathBuf::from("/usr/share/OVMF/OVMF_VARS_4M.fd"),
            },
            "ovmf",
        )),
        "arm64" => Some((
            Uefi {
                code: PathBuf::from("/usr/share/AAVMF/AAVMF_CODE.fd"),
                variables: PathBuf::from("/usr/share/AAVMF/AAVMF_VARS.fd"),
            },
            "qemu-efi-aarch64",
        )),
        _ => None,
    }
}

/// Where an architecture's UEFI code is installed, whether or not it is there.
pub fn uefi_code(arch: &str) -> Option<PathBuf> {
    installed(arch).map(|(uefi, _)| uefi.code)
}

/// The firmware for a machine, copying the variable store template into `store` on first use.
pub fn prepare_uefi(arch: &str, store: &Path) -> Result<Uefi> {
    prepare_uefi_from(arch, store, installed(arch))
}

fn prepare_uefi_from(
    arch: &str,
    store: &Path,
    installed: Option<(Uefi, &'static str)>,
) -> Result<Uefi> {
    let Some((template, package)) = installed else {
        return Err(Error::NoFirmware {
            arch: arch.to_owned(),
        });
    };
    if !template.code.is_file() || !template.variables.is_file() {
        return Err(Error::MissingFirmware {
            path: template.code,
            package,
        });
    }
    if !store.is_file() {
        std::fs::copy(&template.variables, store).map_err(|source| Error::State {
            path: store.to_owned(),
            action: "copy the UEFI variable store",
            source,
        })?;
    }
    Ok(Uefi {
        code: template.code,
        variables: store.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::fs;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("vm-machine-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn firmware(&self) -> (Uefi, &'static str) {
            let code = self.0.join("CODE.fd");
            let variables = self.0.join("VARS.fd");
            fs::write(&code, b"code").unwrap();
            fs::write(&variables, b"template").unwrap();
            (Uefi { code, variables }, "ovmf")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn firmware_names_round_trip() {
        for held in [Firmware::Bios, Firmware::Uefi] {
            assert_eq!(held.name().parse::<Firmware>().unwrap(), held);
        }
    }

    #[test]
    fn an_unknown_firmware_is_refused_with_the_choices() {
        let error = "coreboot".parse::<Firmware>().unwrap_err();
        assert_eq!(error.kind(), "invalid-machine-setting");
        assert!(error.to_string().contains("'bios' or 'uefi'"), "{error}");
    }

    #[test]
    fn bios_is_the_default_and_uefi_is_not() {
        assert!(Firmware::default().is_default());
        assert!(!Firmware::Uefi.is_default());
    }

    #[test]
    fn chipset_and_disk_names_round_trip() {
        for held in [Chipset::Q35, Chipset::Pc] {
            assert_eq!(held.name().parse::<Chipset>().unwrap(), held);
        }
        for held in [Disk::Virtio, Disk::Ide, Disk::Sata] {
            assert_eq!(held.name().parse::<Disk>().unwrap(), held);
        }
    }

    #[test]
    fn unknown_chipsets_and_disks_are_refused_with_the_choices() {
        let error = "isapc".parse::<Chipset>().unwrap_err();
        assert!(error.to_string().contains("'q35' or 'pc'"), "{error}");
        let error = "scsi".parse::<Disk>().unwrap_err();
        assert!(
            error.to_string().contains("'virtio', 'ide' or 'sata'"),
            "{error}"
        );
    }

    #[test]
    fn q35_and_virtio_are_the_defaults() {
        assert!(Chipset::default().is_default());
        assert!(Disk::default().is_default());
        assert!(!Chipset::Pc.is_default());
        assert!(!Disk::Ide.is_default());
    }

    #[test]
    fn a_disk_controller_the_chipset_lacks_is_refused() {
        let error = check_disk(Chipset::Q35, Disk::Ide).unwrap_err();
        assert_eq!(error.kind(), "machine-mismatch");
        assert!(error.to_string().contains("use 'pc'"), "{error}");
        let error = check_disk(Chipset::Pc, Disk::Sata).unwrap_err();
        assert!(error.to_string().contains("use 'q35'"), "{error}");
    }

    #[test]
    fn every_chipset_takes_virtio_and_its_own_controller() {
        for (chipset, disk) in [
            (Chipset::Q35, Disk::Virtio),
            (Chipset::Q35, Disk::Sata),
            (Chipset::Pc, Disk::Virtio),
            (Chipset::Pc, Disk::Ide),
        ] {
            assert!(check_disk(chipset, disk).is_ok(), "{chipset:?} {disk:?}");
        }
    }

    #[test]
    fn disks_are_placed_on_their_controller_by_index() {
        assert_eq!(Disk::Virtio.interface(1), "if=virtio");
        assert_eq!(Disk::Ide.interface(0), "if=ide,index=0");
        assert_eq!(Disk::Sata.interface(1), "if=ide,index=1");
    }

    #[test]
    fn the_models_images_need_are_accepted() {
        for model in [
            "max",
            "host",
            "Penryn,vendor=GenuineIntel,+ssse3,+sse4.1,+sse4.2,+aes,+xsave,+avx,+popcnt",
            "Skylake-Client-v4",
            "EPYC,-svm",
        ] {
            assert!(check_cpu(model).is_ok(), "{model}");
        }
    }

    #[test]
    fn what_could_not_be_a_model_is_refused() {
        for model in [
            "",
            "-machine",
            ",max",
            "max hidden",
            "max\n",
            "a;b",
            &"a".repeat(257),
        ] {
            let error = check_cpu(model).unwrap_err();
            assert_eq!(error.kind(), "invalid-machine-setting", "{model:?}");
        }
    }

    #[test]
    fn the_variable_store_is_copied_from_the_template_on_first_use() {
        let scratch = Scratch::new("copy");
        let store = scratch.0.join("efivars.fd");
        let uefi = prepare_uefi_from("amd64", &store, Some(scratch.firmware())).unwrap();
        assert_eq!(uefi.variables, store);
        assert_eq!(fs::read(&store).unwrap(), b"template");
        assert_eq!(uefi.code, scratch.0.join("CODE.fd"));
    }

    /// The store holds the guest's boot entries, so a second start must not reset it.
    #[test]
    fn an_existing_variable_store_is_kept() {
        let scratch = Scratch::new("keep");
        let store = scratch.0.join("efivars.fd");
        fs::write(&store, b"boot entries").unwrap();
        prepare_uefi_from("amd64", &store, Some(scratch.firmware())).unwrap();
        assert_eq!(fs::read(&store).unwrap(), b"boot entries");
    }

    #[test]
    fn missing_firmware_names_its_package() {
        let scratch = Scratch::new("missing");
        let absent = Uefi {
            code: scratch.0.join("absent-code.fd"),
            variables: scratch.0.join("absent-vars.fd"),
        };
        let store = scratch.0.join("efivars.fd");
        let error = prepare_uefi_from("amd64", &store, Some((absent, "ovmf"))).unwrap_err();
        assert_eq!(error.kind(), "missing-firmware");
        assert!(error.to_string().contains("apt install ovmf"), "{error}");
        assert!(!store.exists());
    }

    #[test]
    fn an_architecture_without_known_firmware_is_refused() {
        let scratch = Scratch::new("arch");
        let error = prepare_uefi("riscv64", &scratch.0.join("efivars.fd")).unwrap_err();
        assert_eq!(error.kind(), "no-uefi-firmware");
    }

    #[test]
    fn debian_paths_are_known_for_the_architectures_it_packages() {
        assert!(installed("amd64").is_some());
        assert!(installed("arm64").is_some());
        assert!(installed("riscv64").is_none());
    }
}

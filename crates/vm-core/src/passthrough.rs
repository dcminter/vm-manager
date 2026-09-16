//! Host PCI and USB devices handed to a guest, and the host checks that precede it.

use crate::error::{Error, Result};
use crate::instance::Instance;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// A host PCI function, written `domain:bus:device.function` as `lspci -D` shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PciAddress {
    pub domain: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl fmt::Display for PciAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04x}:{:02x}:{:02x}.{:x}",
            self.domain, self.bus, self.device, self.function
        )
    }
}

/// Parses `bus:device.function`, with an optional leading `domain:`.
impl FromStr for PciAddress {
    type Err = String;

    fn from_str(text: &str) -> std::result::Result<Self, String> {
        let refuse = || {
            format!(
                "'{text}' is not a PCI address; write it as bus:device.function, such as 01:00.0"
            )
        };
        let hex = |part: &str, digits: usize| {
            (!part.is_empty()
                && part.len() <= digits
                && part.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| u16::from_str_radix(part, 16).ok())
            .flatten()
            .ok_or_else(refuse)
        };
        let (slot, function) = text.rsplit_once('.').ok_or_else(refuse)?;
        let parts: Vec<&str> = slot.split(':').collect();
        let (domain, bus, device) = match parts.as_slice() {
            [bus, device] => (0, hex(bus, 2)?, hex(device, 2)?),
            [domain, bus, device] => (hex(domain, 4)?, hex(bus, 2)?, hex(device, 2)?),
            _ => return Err(refuse()),
        };
        let function = hex(function, 1)?;
        if device > 0x1f || function > 7 {
            return Err(refuse());
        }
        Ok(Self {
            domain,
            bus: u8::try_from(bus).map_err(|_| refuse())?,
            device: u8::try_from(device).map_err(|_| refuse())?,
            function: u8::try_from(function).map_err(|_| refuse())?,
        })
    }
}

impl serde::Serialize for PciAddress {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for PciAddress {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// A host USB device, named by its IDs or by where it is plugged in.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UsbDevice {
    /// Written `vendor:product` in hex, as `lsusb` shows it.
    Product { vendor: u16, product: u16 },
    /// Written `bus-port.port...`, as the kernel names it.
    Port { bus: u8, ports: Vec<u8> },
}

/// USB allows a chain of at most seven ports below the root.
const MAX_PORT_DEPTH: usize = 7;

impl UsbDevice {
    /// The `usb-host` properties that select the device.
    fn selector(&self) -> String {
        match self {
            Self::Product { vendor, product } => {
                format!("vendorid=0x{vendor:04x},productid=0x{product:04x}")
            }
            Self::Port { bus, .. } => format!("hostbus={bus},hostport={}", self.port_path()),
        }
    }

    fn port_path(&self) -> String {
        match self {
            Self::Product { .. } => String::new(),
            Self::Port { ports, .. } => ports
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join("."),
        }
    }
}

impl fmt::Display for UsbDevice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Product { vendor, product } => write!(formatter, "{vendor:04x}:{product:04x}"),
            Self::Port { bus, .. } => write!(formatter, "{bus}-{}", self.port_path()),
        }
    }
}

impl FromStr for UsbDevice {
    type Err = String;

    fn from_str(text: &str) -> std::result::Result<Self, String> {
        let refuse = || {
            format!(
                "'{text}' is not a USB device; write it as vendor:product, such as 046d:c52b, \
                 or as bus-port, such as 1-2.3"
            )
        };
        if let Some((vendor, product)) = text.split_once(':') {
            let id = |part: &str| {
                (part.len() == 4 && part.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    .then(|| u16::from_str_radix(part, 16).ok())
                    .flatten()
                    .ok_or_else(refuse)
            };
            return Ok(Self::Product {
                vendor: id(vendor)?,
                product: id(product)?,
            });
        }
        let number = |part: &str| {
            (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| part.parse::<u8>().ok())
                .flatten()
                .filter(|value| *value > 0)
                .ok_or_else(refuse)
        };
        let (bus, path) = text.split_once('-').ok_or_else(refuse)?;
        let ports = path
            .split('.')
            .map(number)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if ports.len() > MAX_PORT_DEPTH {
            return Err(refuse());
        }
        Ok(Self::Port {
            bus: number(bus)?,
            ports,
        })
    }
}

impl serde::Serialize for UsbDevice {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for UsbDevice {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// The hypervisor arguments attaching an instance's host devices.
pub fn arguments(instance: &Instance) -> Vec<String> {
    let mut out = Vec::new();
    for address in &instance.pci {
        out.push("-device".to_owned());
        out.push(format!("vfio-pci,host={address}"));
    }
    if !instance.usb.is_empty() {
        out.push("-device".to_owned());
        out.push("qemu-xhci,id=xhci".to_owned());
    }
    for device in &instance.usb {
        out.push("-device".to_owned());
        out.push(format!("usb-host,bus=xhci.0,{}", device.selector()));
    }
    out
}

/// Where the host's device information is read from.
#[derive(Debug, Clone)]
pub struct Host {
    sys: PathBuf,
    dev: PathBuf,
    limits: PathBuf,
}

const VFIO_DRIVER: &str = "vfio-pci";
const BRIDGE_CLASS: &str = "0x0604";
const USB_HUB_CLASS: &str = "09";

impl Host {
    pub fn system() -> Self {
        Self {
            sys: PathBuf::from("/sys"),
            dev: PathBuf::from("/dev"),
            limits: PathBuf::from("/proc/self/limits"),
        }
    }

    /// A host laid out under `root`, as `root/sys`, `root/dev` and `root/proc/self/limits`.
    pub fn at(root: &Path) -> Self {
        Self {
            sys: root.join("sys"),
            dev: root.join("dev"),
            limits: root.join("proc/self/limits"),
        }
    }

    fn pci_device(&self, address: PciAddress) -> PathBuf {
        self.sys.join("bus/pci/devices").join(address.to_string())
    }

    fn usb_devices(&self) -> PathBuf {
        self.sys.join("bus/usb/devices")
    }

    /// Refuses devices the hypervisor could not open, saying what to change.
    pub fn check(&self, instance: &Instance) -> Result<()> {
        for (index, address) in instance.pci.iter().enumerate() {
            if instance.pci[..index].contains(address) {
                return Err(refused(address, "it is given twice".to_owned()));
            }
            self.check_pci(*address)?;
        }
        let mut claimed: Vec<(String, &UsbDevice)> = Vec::new();
        for device in &instance.usb {
            let found = self.find_usb(device)?;
            if let Some((_, earlier)) = claimed.iter().find(|(name, _)| *name == found.name) {
                return Err(refused(
                    device,
                    format!("it is the same device as {earlier}"),
                ));
            }
            openable(device, &found.node)?;
            claimed.push((found.name, device));
        }
        if !instance.pci.is_empty() {
            self.check_locked_memory(instance.memory)?;
        }
        Ok(())
    }

    fn check_pci(&self, address: PciAddress) -> Result<()> {
        let path = self.pci_device(address);
        if !path.is_dir() {
            return Err(refused(
                &address,
                "this host has no PCI device there; 'lspci -D' lists them".to_owned(),
            ));
        }
        match link_name(&path.join("driver")).as_deref() {
            Some(VFIO_DRIVER) => {}
            held => {
                let bound = held.map_or_else(
                    || "no driver".to_owned(),
                    |driver| format!("the {driver} driver"),
                );
                return Err(refused(
                    &address,
                    format!(
                        "it is bound to {bound}; bind it to vfio-pci, \
                         such as with 'driverctl set-override {address} vfio-pci'"
                    ),
                ));
            }
        }
        let Some(group) = link_name(&path.join("iommu_group")) else {
            return Err(refused(
                &address,
                "it is in no IOMMU group; turn the IOMMU on with intel_iommu=on \
                 or amd_iommu=on on the kernel command line"
                    .to_owned(),
            ));
        };
        self.check_group(address, &path, &group)?;
        openable(&address, &self.dev.join("vfio").join(&group))?;
        openable(&address, &self.dev.join("vfio/vfio"))
    }

    /// Every other function in the group must be free for vfio-pci to take it.
    fn check_group(&self, address: PciAddress, path: &Path, group: &str) -> Result<()> {
        let members = fs::read_dir(path.join("iommu_group/devices")).map_err(|error| {
            refused(&address, format!("its IOMMU group is unreadable: {error}"))
        })?;
        let mut names: Vec<String> = members
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| *name != address.to_string())
            .collect();
        names.sort();
        for name in names {
            let member = self.sys.join("bus/pci/devices").join(&name);
            let bridge = fs::read_to_string(member.join("class"))
                .is_ok_and(|class| class.trim().starts_with(BRIDGE_CLASS));
            match link_name(&member.join("driver")) {
                Some(driver) if driver != VFIO_DRIVER && !bridge => {
                    return Err(refused(
                        &address,
                        format!(
                            "IOMMU group {group} also holds {name}, bound to the {driver} driver; \
                             bind every device in the group to vfio-pci"
                        ),
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn find_usb(&self, device: &UsbDevice) -> Result<PluggedUsb> {
        let found = self.match_usb(device)?;
        if found.hub {
            return Err(refused(
                device,
                "it is a hub, which cannot be handed over; name a device plugged into it"
                    .to_owned(),
            ));
        }
        Ok(found)
    }

    fn match_usb(&self, device: &UsbDevice) -> Result<PluggedUsb> {
        let plugged = self.plugged_usb();
        match device {
            UsbDevice::Port { .. } => {
                let name = device.to_string();
                plugged
                    .into_iter()
                    .find(|held| held.name == name)
                    .ok_or_else(|| {
                        refused(
                            device,
                            "nothing is plugged in there; 'lsusb -t' shows the ports".to_owned(),
                        )
                    })
            }
            UsbDevice::Product { vendor, product } => {
                let mut matching: Vec<PluggedUsb> = plugged
                    .into_iter()
                    .filter(|held| held.vendor == *vendor && held.product == *product)
                    .collect();
                match matching.len() {
                    0 => Err(refused(
                        device,
                        "no such device is plugged in; 'lsusb' lists them".to_owned(),
                    )),
                    1 => Ok(matching.remove(0)),
                    _ => Err(refused(
                        device,
                        format!(
                            "{} plugged-in devices match; name one by where it is plugged in: {}",
                            matching.len(),
                            matching
                                .iter()
                                .map(|held| held.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    )),
                }
            }
        }
    }

    /// Every USB device plugged in, root hubs left out.
    fn plugged_usb(&self) -> Vec<PluggedUsb> {
        let Ok(entries) = fs::read_dir(self.usb_devices()) else {
            return Vec::new();
        };
        let mut plugged: Vec<PluggedUsb> = entries
            .filter_map(std::result::Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let UsbDevice::Port { .. } = name.parse::<UsbDevice>().ok()? else {
                    return None;
                };
                let path = entry.path();
                let read = |file: &str| {
                    fs::read_to_string(path.join(file))
                        .ok()
                        .map(|text| text.trim().to_owned())
                };
                let hex = |file: &str| u16::from_str_radix(&read(file)?, 16).ok();
                let decimal = |file: &str| read(file)?.parse::<u16>().ok();
                let node = self.dev.join(format!(
                    "bus/usb/{:03}/{:03}",
                    decimal("busnum")?,
                    decimal("devnum")?
                ));
                let label = [read("manufacturer"), read("product")]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                Some(PluggedUsb {
                    vendor: hex("idVendor")?,
                    product: hex("idProduct")?,
                    name,
                    node,
                    label,
                    hub: read("bDeviceClass").as_deref() == Some(USB_HUB_CLASS),
                })
            })
            .collect();
        plugged.sort_by(|left, right| left.name.cmp(&right.name));
        plugged
    }

    /// The guest's memory is pinned for the device's DMA, so the lock limit must cover it.
    fn check_locked_memory(&self, memory: u64) -> Result<()> {
        let Some(bytes) = fs::read_to_string(&self.limits)
            .ok()
            .and_then(|text| locked_memory_limit(&text))
        else {
            return Ok(());
        };
        if bytes < memory.saturating_mul(1 << 20) {
            return Err(Error::LockedMemory {
                needed: memory,
                limit: bytes >> 20,
            });
        }
        Ok(())
    }

    /// PCI functions bound to vfio-pci, each with its vendor and device IDs.
    pub fn pci_candidates(&self) -> Vec<(String, String)> {
        fs::read_dir(self.sys.join("bus/pci/devices"))
            .map(|entries| {
                let mut found: Vec<(String, String)> = entries
                    .filter_map(std::result::Result::ok)
                    .filter(|entry| {
                        link_name(&entry.path().join("driver")).as_deref() == Some(VFIO_DRIVER)
                    })
                    .map(|entry| {
                        let path = entry.path();
                        let id = |file: &str| {
                            fs::read_to_string(path.join(file))
                                .map(|text| text.trim().trim_start_matches("0x").to_owned())
                                .unwrap_or_default()
                        };
                        (
                            entry.file_name().to_string_lossy().into_owned(),
                            format!("{}:{}", id("vendor"), id("device")),
                        )
                    })
                    .collect();
                found.sort();
                found
            })
            .unwrap_or_default()
    }

    /// Every plugged-in USB device by its IDs, each with its name and where it is plugged in.
    pub fn usb_candidates(&self) -> Vec<(String, String)> {
        self.plugged_usb()
            .into_iter()
            .filter(|held| !held.hub)
            .map(|held| {
                (
                    UsbDevice::Product {
                        vendor: held.vendor,
                        product: held.product,
                    }
                    .to_string(),
                    format!("{} at {}", held.label, held.name).trim().to_owned(),
                )
            })
            .collect()
    }
}

struct PluggedUsb {
    /// The kernel's name for it, such as `1-2.3`.
    name: String,
    vendor: u16,
    product: u16,
    node: PathBuf,
    label: String,
    hub: bool,
}

fn refused(device: &dyn fmt::Display, reason: String) -> Error {
    Error::Passthrough {
        device: device.to_string(),
        reason,
    }
}

/// The last component of a symbolic link's target.
fn link_name(link: &Path) -> Option<String> {
    fs::read_link(link)
        .ok()?
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// Opens a device node as the hypervisor will, to find out whether it can.
fn openable(device: &dyn fmt::Display, node: &Path) -> Result<()> {
    match fs::OpenOptions::new().read(true).write(true).open(node) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Err(refused(
            device,
            format!(
                "{} is not open to this user; a udev rule can grant it",
                node.display()
            ),
        )),
        Err(error) => Err(refused(device, format!("{}: {error}", node.display()))),
    }
}

/// The soft limit on locked memory in bytes, with no limit as the largest number.
fn locked_memory_limit(limits: &str) -> Option<u64> {
    let line = limits
        .lines()
        .find(|line| line.starts_with("Max locked memory"))?;
    let soft = line
        .trim_start_matches("Max locked memory")
        .split_whitespace()
        .next()?;
    if soft == "unlimited" {
        Some(u64::MAX)
    } else {
        soft.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn pci_addresses_are_read_with_or_without_a_domain() {
        let short: PciAddress = "01:00.0".parse().unwrap();
        assert_eq!(short.to_string(), "0000:01:00.0");
        let long: PciAddress = "000A:FF:1f.7".parse().unwrap();
        assert_eq!(
            long,
            PciAddress {
                domain: 0xa,
                bus: 0xff,
                device: 0x1f,
                function: 7
            }
        );
        assert_eq!(long.to_string(), "000a:ff:1f.7");
    }

    #[test]
    fn malformed_pci_addresses_are_refused() {
        for text in [
            "",
            "01:00",
            "0100.0",
            "01:20.0",
            "01:00.8",
            "100:00.0",
            "00000:01:00.0",
            "0:0:0:00.0",
            "01:00.",
            ":00.0",
            "0g:00.0",
            "01:00.0.0",
            "+1:00.0",
            "10de:1b80",
        ] {
            let error = text.parse::<PciAddress>().unwrap_err();
            assert!(error.contains("not a PCI address"), "{text}: {error}");
        }
    }

    #[test]
    fn usb_devices_are_read_by_ids_or_by_port() {
        let ids: UsbDevice = "046D:c52b".parse().unwrap();
        assert_eq!(
            ids,
            UsbDevice::Product {
                vendor: 0x046d,
                product: 0xc52b
            }
        );
        assert_eq!(ids.to_string(), "046d:c52b");
        let port: UsbDevice = "3-1.4.2".parse().unwrap();
        assert_eq!(
            port,
            UsbDevice::Port {
                bus: 3,
                ports: vec![1, 4, 2]
            }
        );
        assert_eq!(port.to_string(), "3-1.4.2");
        assert_eq!("1-2".parse::<UsbDevice>().unwrap().to_string(), "1-2");
    }

    #[test]
    fn malformed_usb_devices_are_refused() {
        for text in [
            "",
            "46d:c52b",
            "046d:c52b0",
            "046d:",
            "046x:c52b",
            "usb1",
            "1",
            "1-",
            "-2",
            "0-2",
            "1-0",
            "1-2..3",
            "1-2.3.",
            "1-256",
            "1-1.1.1.1.1.1.1.1",
            "1-2:1.0",
            "a-2",
        ] {
            let error = text.parse::<UsbDevice>().unwrap_err();
            assert!(error.contains("not a USB device"), "{text}: {error}");
        }
    }

    #[test]
    fn devices_survive_the_instance_record() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Record {
            pci: Vec<PciAddress>,
            usb: Vec<UsbDevice>,
        }
        let record = Record {
            pci: vec!["01:00.0".parse().unwrap()],
            usb: vec!["046d:c52b".parse().unwrap(), "1-2.3".parse().unwrap()],
        };
        let text = basic_toml::to_string(&record).unwrap();
        assert_eq!(
            text,
            "pci = [\"0000:01:00.0\"]\nusb = [\"046d:c52b\", \"1-2.3\"]\n"
        );
        assert_eq!(basic_toml::from_str::<Record>(&text).unwrap(), record);
        assert!(basic_toml::from_str::<Record>("pci = [\"1:2:3\"]\nusb = []\n").is_err());
    }

    fn instance(pci: &[&str], usb: &[&str]) -> Instance {
        Instance {
            name: "gpu".to_owned(),
            image: "debian:trixie".to_owned(),
            digest: "sha512:abc".to_owned(),
            arch: "amd64".to_owned(),
            created: 1_700_000_000,
            memory: 1024,
            cpus: 2,
            firmware: crate::machine::Firmware::Bios,
            cpu_model: "max".to_owned(),
            machine: crate::machine::Chipset::Q35,
            disk: crate::machine::Disk::Virtio,
            user: "vm".to_owned(),
            seeded: true,
            monitor: PathBuf::new(),
            ssh_port: None,
            pid: None,
            started: None,
            generation: 0,
            ssh_config: false,
            auto_remove: false,
            media: crate::catalogue::Media::Disk,
            cdrom: None,
            password: None,
            pci: pci.iter().map(|text| text.parse().unwrap()).collect(),
            usb: usb.iter().map(|text| text.parse().unwrap()).collect(),
            ports: Vec::new(),
            shares: Vec::new(),
        }
    }

    #[test]
    fn devices_become_hypervisor_arguments() {
        assert!(arguments(&instance(&[], &[])).is_empty());
        assert_eq!(
            arguments(&instance(&["01:00.0", "01:00.1"], &["046d:c52b", "1-2.3"])),
            [
                "-device",
                "vfio-pci,host=0000:01:00.0",
                "-device",
                "vfio-pci,host=0000:01:00.1",
                "-device",
                "qemu-xhci,id=xhci",
                "-device",
                "usb-host,bus=xhci.0,vendorid=0x046d,productid=0xc52b",
                "-device",
                "usb-host,bus=xhci.0,hostbus=1,hostport=2.3",
            ]
        );
        assert_eq!(
            arguments(&instance(&["01:00.0"], &[])),
            ["-device", "vfio-pci,host=0000:01:00.0"]
        );
    }

    /// A fake host whose sysfs, device nodes and limits are files under a scratch directory.
    struct FakeHost {
        root: PathBuf,
    }

    impl Drop for FakeHost {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    impl FakeHost {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("vm-passthrough-{}-{serial}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            let host = Self { root };
            host.write("dev/vfio/vfio", "");
            host.limit("unlimited");
            host
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.join(relative)
        }

        fn write(&self, relative: &str, text: &str) {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn link(&self, relative: &str, target: &str) {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::create_dir_all(self.path(target)).unwrap();
            symlink(self.path(target), path).unwrap();
        }

        fn limit(&self, soft: &str) {
            self.write(
                "proc/self/limits",
                &format!(
                    "Limit                     Soft Limit           Hard Limit           Units\n\
                     Max processes             63449                63449                processes\n\
                     Max locked memory         {soft:<20} unlimited            bytes\n"
                ),
            );
        }

        /// A PCI function, bound to `driver` if given, in IOMMU group `group` if given.
        fn pci(&self, address: &str, class: &str, driver: Option<&str>, group: Option<&str>) {
            let device = format!("sys/bus/pci/devices/{address}");
            self.write(&format!("{device}/class"), &format!("{class}\n"));
            self.write(&format!("{device}/vendor"), "0x10de\n");
            self.write(&format!("{device}/device"), "0x1b80\n");
            if let Some(driver) = driver {
                self.link(
                    &format!("{device}/driver"),
                    &format!("sys/bus/pci/drivers/{driver}"),
                );
            }
            if let Some(group) = group {
                let members = format!("sys/kernel/iommu_groups/{group}");
                self.link(&format!("{device}/iommu_group"), &members);
                self.link(&format!("{members}/devices/{address}"), &device);
                self.write(&format!("dev/vfio/{group}"), "");
            }
        }

        fn usb(&self, name: &str, ids: (&str, &str), numbers: (u16, u16)) {
            let device = format!("sys/bus/usb/devices/{name}");
            self.write(&format!("{device}/idVendor"), &format!("{}\n", ids.0));
            self.write(&format!("{device}/idProduct"), &format!("{}\n", ids.1));
            self.write(&format!("{device}/busnum"), &format!("{}\n", numbers.0));
            self.write(&format!("{device}/devnum"), &format!("{}\n", numbers.1));
            self.write(&format!("{device}/manufacturer"), "Logitech\n");
            self.write(&format!("{device}/product"), "Receiver\n");
            self.write(
                &format!("dev/bus/usb/{:03}/{:03}", numbers.0, numbers.1),
                "",
            );
        }

        fn host(&self) -> Host {
            Host::at(&self.root)
        }

        fn refusal(&self, held: &Instance) -> String {
            self.host().check(held).unwrap_err().to_string()
        }
    }

    const GPU: &str = "0x030000";

    #[test]
    fn a_function_bound_to_vfio_in_its_own_group_passes() {
        let fake = FakeHost::new();
        fake.pci("0000:01:00.0", GPU, Some("vfio-pci"), Some("7"));
        fake.host().check(&instance(&["01:00.0"], &[])).unwrap();
    }

    #[test]
    fn an_absent_function_is_refused() {
        let fake = FakeHost::new();
        let message = fake.refusal(&instance(&["02:00.0"], &[]));
        assert_eq!(
            message,
            "cannot pass 0000:02:00.0 through: this host has no PCI device there; \
             'lspci -D' lists them"
        );
    }

    #[test]
    fn a_function_bound_elsewhere_is_refused_with_the_command_to_rebind_it() {
        let fake = FakeHost::new();
        fake.pci("0000:01:00.0", GPU, Some("nvidia"), Some("7"));
        let message = fake.refusal(&instance(&["01:00.0"], &[]));
        assert!(message.contains("bound to the nvidia driver"), "{message}");
        assert!(
            message.contains("driverctl set-override 0000:01:00.0 vfio-pci"),
            "{message}"
        );
        let fake = FakeHost::new();
        fake.pci("0000:01:00.0", GPU, None, Some("7"));
        let message = fake.refusal(&instance(&["01:00.0"], &[]));
        assert!(message.contains("bound to no driver"), "{message}");
    }

    #[test]
    fn a_function_outside_any_group_means_the_iommu_is_off() {
        let fake = FakeHost::new();
        fake.pci("0000:01:00.0", GPU, Some("vfio-pci"), None);
        let message = fake.refusal(&instance(&["01:00.0"], &[]));
        assert!(message.contains("intel_iommu=on"), "{message}");
    }

    #[test]
    fn a_group_member_held_by_another_driver_is_refused_but_bridges_and_unbound_ones_are_not() {
        let fake = FakeHost::new();
        fake.pci("0000:01:00.0", GPU, Some("vfio-pci"), Some("7"));
        fake.pci("0000:00:01.0", "0x060400", Some("pcieport"), Some("7"));
        fake.pci("0000:01:00.2", "0x0c0330", None, Some("7"));
        fake.pci("0000:01:00.1", "0x040300", Some("vfio-pci"), Some("7"));
        fake.host().check(&instance(&["01:00.0"], &[])).unwrap();
        fake.pci("0000:01:00.3", "0x0c8000", Some("nvidia-gpu"), Some("7"));
        let message = fake.refusal(&instance(&["01:00.0"], &[]));
        assert_eq!(
            message,
            "cannot pass 0000:01:00.0 through: IOMMU group 7 also holds 0000:01:00.3, \
             bound to the nvidia-gpu driver; bind every device in the group to vfio-pci"
        );
    }

    #[test]
    fn missing_vfio_nodes_are_refused() {
        let fake = FakeHost::new();
        fake.pci("0000:01:00.0", GPU, Some("vfio-pci"), Some("7"));
        fs::remove_file(fake.path("dev/vfio/7")).unwrap();
        let message = fake.refusal(&instance(&["01:00.0"], &[]));
        assert!(message.contains("dev/vfio/7"), "{message}");
        fake.write("dev/vfio/7", "");
        fs::remove_file(fake.path("dev/vfio/vfio")).unwrap();
        let message = fake.refusal(&instance(&["01:00.0"], &[]));
        assert!(message.contains("dev/vfio/vfio"), "{message}");
    }

    #[test]
    fn an_unwritable_node_is_refused_with_a_hint() {
        use std::os::unix::fs::PermissionsExt as _;
        let fake = FakeHost::new();
        fake.pci("0000:01:00.0", GPU, Some("vfio-pci"), Some("7"));
        let node = fake.path("dev/vfio/7");
        fs::set_permissions(&node, fs::Permissions::from_mode(0o400)).unwrap();
        // Root opens it regardless.
        if fs::OpenOptions::new().write(true).open(&node).is_ok() {
            return;
        }
        let message = fake.refusal(&instance(&["01:00.0"], &[]));
        assert!(
            message.ends_with("dev/vfio/7 is not open to this user; a udev rule can grant it"),
            "{message}"
        );
    }

    #[test]
    fn a_function_given_twice_is_refused() {
        let fake = FakeHost::new();
        fake.pci("0000:01:00.0", GPU, Some("vfio-pci"), Some("7"));
        let message = fake.refusal(&instance(&["01:00.0", "0000:01:00.0"], &[]));
        assert_eq!(
            message,
            "cannot pass 0000:01:00.0 through: it is given twice"
        );
    }

    #[test]
    fn the_locked_memory_limit_must_cover_the_guest() {
        let fake = FakeHost::new();
        fake.pci("0000:01:00.0", GPU, Some("vfio-pci"), Some("7"));
        fake.limit("8388608");
        let message = fake.refusal(&instance(&["01:00.0"], &[]));
        assert!(message.contains("1024 MiB"), "{message}");
        assert!(message.contains("8 MiB"), "{message}");
        assert!(matches!(
            fake.host().check(&instance(&["01:00.0"], &[])),
            Err(Error::LockedMemory {
                needed: 1024,
                limit: 8
            })
        ));
        fake.limit(&(1024_u64 << 20).to_string());
        fake.host().check(&instance(&["01:00.0"], &[])).unwrap();
        // USB devices pin nothing.
        fake.limit("8388608");
        fake.usb("1-2", ("046d", "c52b"), (1, 5));
        fake.host().check(&instance(&[], &["046d:c52b"])).unwrap();
    }

    #[test]
    fn the_limits_file_is_read() {
        let text = "Limit Soft Hard Units\nMax locked memory         65536                65536                bytes\n";
        assert_eq!(locked_memory_limit(text), Some(65536));
        assert_eq!(
            locked_memory_limit("Max locked memory unlimited unlimited bytes"),
            Some(u64::MAX)
        );
        assert_eq!(locked_memory_limit("Max processes 1 1 processes"), None);
        assert_eq!(locked_memory_limit("Max locked memory lots"), None);
    }

    #[test]
    fn usb_devices_are_found_by_ids_or_by_port() {
        let fake = FakeHost::new();
        fake.usb("1-2", ("046d", "c52b"), (1, 5));
        fake.usb("3-1.4", ("0bda", "8153"), (3, 9));
        fake.write("sys/bus/usb/devices/1-2:1.0/bInterfaceClass", "03\n");
        fake.write("sys/bus/usb/devices/usb1/idVendor", "1d6b\n");
        fake.host()
            .check(&instance(&[], &["046d:c52b", "3-1.4"]))
            .unwrap();
    }

    #[test]
    fn absent_usb_devices_are_refused() {
        let fake = FakeHost::new();
        fake.usb("1-2", ("046d", "c52b"), (1, 5));
        assert_eq!(
            fake.refusal(&instance(&[], &["046d:c52c"])),
            "cannot pass 046d:c52c through: no such device is plugged in; 'lsusb' lists them"
        );
        assert_eq!(
            fake.refusal(&instance(&[], &["1-3"])),
            "cannot pass 1-3 through: nothing is plugged in there; 'lsusb -t' shows the ports"
        );
    }

    #[test]
    fn ids_matching_several_devices_are_refused_with_their_ports() {
        let fake = FakeHost::new();
        fake.usb("1-2", ("046d", "c52b"), (1, 5));
        fake.usb("2-1", ("046d", "c52b"), (2, 3));
        assert_eq!(
            fake.refusal(&instance(&[], &["046d:c52b"])),
            "cannot pass 046d:c52b through: 2 plugged-in devices match; \
             name one by where it is plugged in: 1-2, 2-1"
        );
        fake.host().check(&instance(&[], &["2-1"])).unwrap();
    }

    #[test]
    fn one_usb_device_named_twice_is_refused() {
        let fake = FakeHost::new();
        fake.usb("1-2", ("046d", "c52b"), (1, 5));
        assert_eq!(
            fake.refusal(&instance(&[], &["046d:c52b", "1-2"])),
            "cannot pass 1-2 through: it is the same device as 046d:c52b"
        );
    }

    #[test]
    fn a_hub_is_refused_and_not_offered() {
        let fake = FakeHost::new();
        fake.usb("3-2", ("05e3", "0610"), (3, 2));
        fake.write("sys/bus/usb/devices/3-2/bDeviceClass", "09\n");
        fake.usb("3-2.1", ("046d", "c52b"), (3, 4));
        fake.write("sys/bus/usb/devices/3-2.1/bDeviceClass", "00\n");
        for named in ["05e3:0610", "3-2"] {
            assert_eq!(
                fake.refusal(&instance(&[], &[named])),
                format!(
                    "cannot pass {named} through: it is a hub, which cannot be handed over; \
                     name a device plugged into it"
                )
            );
        }
        fake.host().check(&instance(&[], &["3-2.1"])).unwrap();
        let offered: Vec<String> = fake
            .host()
            .usb_candidates()
            .into_iter()
            .map(|(value, _)| value)
            .collect();
        assert_eq!(offered, ["046d:c52b"]);
    }

    #[test]
    fn a_usb_device_without_its_node_is_refused() {
        let fake = FakeHost::new();
        fake.usb("1-2", ("046d", "c52b"), (1, 5));
        fs::remove_file(fake.path("dev/bus/usb/001/005")).unwrap();
        let message = fake.refusal(&instance(&[], &["1-2"]));
        assert!(message.contains("dev/bus/usb/001/005"), "{message}");
    }

    #[test]
    fn no_devices_need_no_host() {
        let host = Host::at(Path::new("/nonexistent"));
        host.check(&instance(&[], &[])).unwrap();
    }

    #[test]
    fn candidates_are_vfio_functions_and_plugged_usb_devices() {
        let fake = FakeHost::new();
        fake.pci("0000:01:00.0", GPU, Some("vfio-pci"), Some("7"));
        fake.pci("0000:00:02.0", GPU, Some("amdgpu"), Some("3"));
        fake.usb("3-1.4", ("0bda", "8153"), (3, 9));
        let (pci, usb) = (fake.host().pci_candidates(), fake.host().usb_candidates());
        assert_eq!(pci, [("0000:01:00.0".to_owned(), "10de:1b80".to_owned())]);
        assert_eq!(
            usb,
            [(
                "0bda:8153".to_owned(),
                "Logitech Receiver at 3-1.4".to_owned()
            )]
        );
        let nowhere = Host::at(Path::new("/nonexistent"));
        assert!(nowhere.pci_candidates().is_empty());
        assert!(nowhere.usb_candidates().is_empty());
    }
}

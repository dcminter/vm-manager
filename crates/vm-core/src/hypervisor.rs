//! Launching the hypervisor behind a trait, so the lifecycle is testable without booting.

use crate::error::{Error, Result};
use crate::instance::{Directory, Instance};
use crate::machine::{self, Firmware};
use crate::process::Handle;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The QEMU binary for an architecture.
pub fn binary_for(arch: &str) -> &'static str {
    match arch {
        "arm64" => "qemu-system-aarch64",
        "riscv64" => "qemu-system-riscv64",
        "ppc64el" => "qemu-system-ppc64",
        _ => "qemu-system-x86_64",
    }
}

/// A fully resolved program to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: String,
    pub arguments: Vec<String>,
    /// The process's own output, not the guest console.
    pub log: PathBuf,
}

pub trait Supervisor {
    /// Starts the process and returns a handle that outlives this command.
    fn start(&self, launch: &Launch) -> Result<Handle>;
}

/// Launches a detached process in its own process group, so terminal interrupts miss it.
#[derive(Debug, Clone, Copy)]
pub struct Detached;

impl Supervisor for Detached {
    fn start(&self, launch: &Launch) -> Result<Handle> {
        use std::os::unix::process::CommandExt as _;
        let log = File::create(&launch.log).map_err(|source| Error::State {
            path: launch.log.clone(),
            action: "open the hypervisor log",
            source,
        })?;
        let errors = log.try_clone().map_err(|source| Error::State {
            path: launch.log.clone(),
            action: "open the hypervisor log",
            source,
        })?;
        let child = Command::new(&launch.program)
            .args(&launch.arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(errors))
            .process_group(0)
            .spawn()
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    missing(&launch.program)
                } else {
                    Error::Launch {
                        program: launch.program.clone(),
                        source,
                    }
                }
            })?;
        // The start time distinguishes this process from a later reuse of its pid.
        Handle::of(child.id()).ok_or_else(|| Error::Launch {
            program: launch.program.clone(),
            source: std::io::Error::other("the hypervisor exited before it could be recorded"),
        })
    }
}

fn missing(program: &str) -> Error {
    // The program may be a full path.
    let program = Path::new(program)
        .file_name()
        .map_or(program, |name| name.to_str().unwrap_or(program));
    match program {
        "virtiofsd" => Error::MissingTool {
            binary: "virtiofsd",
            package: "virtiofsd",
            operation: "sharing a directory with a virtual machine",
        },
        held if held.starts_with("qemu-system") => Error::MissingTool {
            binary: "qemu-system-x86_64",
            package: "qemu-system-x86",
            operation: "starting a virtual machine",
        },
        _ => Error::MissingTool {
            binary: "qemu-img",
            package: "qemu-utils",
            operation: "starting a virtual machine",
        },
    }
}

/// The hypervisor's arguments for an instance.
pub fn arguments(instance: &Instance, directory: &Directory) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |argument: &str| out.push(argument.to_owned());

    // virtiofs needs shared memory from the first boot.
    push("-object");
    push(&format!(
        "memory-backend-memfd,id=mem,size={}M,share=on",
        instance.memory
    ));
    push("-machine");
    push(&format!(
        "{},memory-backend=mem,accel=kvm:tcg",
        instance.machine.name()
    ));
    // The default is `max` rather than `host`, which only exists under KVM.
    push("-cpu");
    push(&instance.cpu);
    if instance.firmware == Firmware::Uefi {
        if let Some(code) = machine::uefi_code(&instance.arch) {
            push("-drive");
            push(&format!(
                "if=pflash,format=raw,unit=0,readonly=on,file={}",
                code.display()
            ));
        }
        push("-drive");
        push(&format!(
            "if=pflash,format=raw,unit=1,file={}",
            directory.firmware_variables().display()
        ));
    }
    push("-m");
    push(&instance.memory.to_string());
    push("-smp");
    push(&instance.cpus.to_string());
    push("-drive");
    push(&format!(
        "file={},{},format=qcow2",
        directory.overlay().display(),
        instance.disk.interface(0)
    ));
    if instance.seeded {
        push("-drive");
        // Writable, because FreeBSD's nuageinit ignores a seed it cannot mount read-write.
        push(&format!(
            "file={},{},format=raw",
            directory.seed().display(),
            instance.disk.interface(1)
        ));
    }
    push("-netdev");
    push(&network(instance));
    push("-device");
    push("virtio-net-pci,netdev=net0");
    // Early boot otherwise stalls waiting for entropy.
    push("-device");
    push("virtio-rng-pci");
    // Each share is a socket its `virtiofsd` already listens on.
    for (index, share) in instance.shares.iter().enumerate() {
        push("-chardev");
        push(&format!(
            "socket,id=vfs{index},path={}",
            directory.share_socket(index).display()
        ));
        push("-device");
        push(&format!(
            "vhost-user-fs-pci,chardev=vfs{index},tag={}",
            share.tag
        ));
    }
    push("-display");
    push("none");
    // QMP cannot add a VNC server later.
    push("-vnc");
    push(&format!("unix:{}", directory.screen_socket().display()));
    // Logged with append so a machine started again keeps the evidence of the last boot.
    push("-chardev");
    push(&format!(
        "socket,id=console,path={},server=on,wait=off,logfile={},logappend=on",
        directory.console_socket().display(),
        directory.console().display()
    ));
    push("-serial");
    push("chardev:console");
    push("-qmp");
    push(&format!(
        "unix:{},server,nowait",
        directory.monitor().display()
    ));
    out
}

/// User-mode networking with port forwards.
fn network(instance: &Instance) -> String {
    use std::fmt::Write as _;
    let mut netdev =
        instance
            .ports
            .iter()
            .fold(String::from("user,id=net0"), |mut netdev, port| {
                let _ = write!(
                    netdev,
                    ",hostfwd=tcp:127.0.0.1:{}-:{}",
                    port.host, port.guest
                );
                netdev
            });
    // The forward `vm ssh` uses, kept out of the published list.
    if let Some(port) = instance.ssh_port {
        let _ = write!(netdev, ",hostfwd=tcp:127.0.0.1:{port}-:22");
    }
    netdev
}

/// Where Debian and Ubuntu install `virtiofsd` outside `PATH`.
const VIRTIOFSD_DIRECTORIES: [&str; 3] = ["/usr/libexec", "/usr/lib/qemu", "/usr/lib/virtiofsd"];

/// The `virtiofsd` to run, preferring `PATH`.
pub fn virtiofsd() -> String {
    let path = std::env::var("PATH").unwrap_or_default();
    locate("virtiofsd", &path, &VIRTIOFSD_DIRECTORIES, &|at| {
        at.is_file()
    })
}

/// The search itself, with the filesystem passed in so it can be tested.
fn locate(name: &str, path: &str, extras: &[&str], exists: &dyn Fn(&Path) -> bool) -> String {
    let found = std::env::split_paths(path)
        .chain(extras.iter().map(PathBuf::from))
        .map(|directory| directory.join(name))
        .find(|candidate| exists(candidate));
    // The bare name fails to spawn with an error naming the package.
    found.map_or_else(|| name.to_owned(), |at| at.display().to_string())
}

/// What to run to serve one share, unsandboxed because the sandbox needs `CAP_SYS_ADMIN`.
pub fn share_launch(source: &Path, socket: &Path, log: PathBuf) -> Launch {
    Launch {
        program: virtiofsd(),
        arguments: vec![
            format!("--socket-path={}", socket.display()),
            "--shared-dir".to_owned(),
            source.display().to_string(),
            "--sandbox".to_owned(),
            "none".to_owned(),
        ],
        log,
    }
}

/// Grows a disk; `qemu-img` refuses to shrink one.
pub fn resize_overlay(overlay: &Path, size: &str) -> Result<()> {
    let output = Command::new("qemu-img")
        .arg("resize")
        .arg("-q")
        .arg("-f")
        .arg("qcow2")
        .arg(overlay)
        .arg(size)
        .output()
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::MissingTool {
                    binary: "qemu-img",
                    package: "qemu-utils",
                    operation: "resizing a virtual machine's disk",
                }
            } else {
                Error::Launch {
                    program: "qemu-img".to_owned(),
                    source,
                }
            }
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::Overlay {
            path: overlay.to_owned(),
            reason: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

/// Creates the instance's qcow2 disk over an image in the store.
pub fn create_overlay(
    image: &Path,
    overlay: &Path,
    backing: &str,
    size: Option<&str>,
) -> Result<()> {
    let mut command = Command::new("qemu-img");
    command
        .arg("create")
        .arg("-q")
        .arg("-f")
        .arg("qcow2")
        .arg("-F")
        .arg(backing)
        .arg("-b")
        .arg(image)
        .arg(overlay);
    if let Some(size) = size {
        command.arg(size);
    }
    let output = command.output().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            Error::MissingTool {
                binary: "qemu-img",
                package: "qemu-utils",
                operation: "creating a virtual machine's disk",
            }
        } else {
            Error::Launch {
                program: "qemu-img".to_owned(),
                source,
            }
        }
    })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::Overlay {
            path: overlay.to_owned(),
            reason: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::instance::{Instances, Port, Share};
    use std::fs;
    use std::sync::Mutex;

    /// A supervisor that starts nothing and remembers everything.
    #[derive(Debug, Default)]
    struct Recording {
        launches: Mutex<Vec<Launch>>,
    }

    impl Supervisor for Recording {
        fn start(&self, launch: &Launch) -> Result<Handle> {
            self.launches.lock().unwrap().push(launch.clone());
            Ok(Handle { pid: 1, started: 2 })
        }
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("vm-hypervisor-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn directory(&self, name: &str) -> Directory {
            Instances::at(self.0.join("instances"), self.0.join("run"))
                .create(name)
                .unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn instance(name: &str) -> Instance {
        Instance {
            name: name.to_owned(),
            image: "debian:trixie".to_owned(),
            digest: "sha512:abc".to_owned(),
            arch: "amd64".to_owned(),
            created: 1_700_000_000,
            memory: 2048,
            cpus: 2,
            firmware: crate::machine::Firmware::Bios,
            cpu: "max".to_owned(),
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
            password: None,
            ports: Vec::new(),
            shares: Vec::new(),
        }
    }

    fn pair(arguments: &[String], flag: &str) -> Option<String> {
        arguments
            .iter()
            .position(|held| held == flag)
            .and_then(|at| arguments.get(at + 1))
            .cloned()
    }

    #[test]
    fn the_architecture_chooses_the_binary() {
        assert_eq!(binary_for("amd64"), "qemu-system-x86_64");
        assert_eq!(binary_for("arm64"), "qemu-system-aarch64");
        assert_eq!(binary_for("riscv64"), "qemu-system-riscv64");
    }

    #[test]
    fn the_memory_and_processor_counts_are_passed_through() {
        let scratch = Scratch::new("counts");
        let mut held = instance("one");
        held.memory = 4096;
        held.cpus = 8;
        let arguments = arguments(&held, &scratch.directory("one"));
        assert_eq!(pair(&arguments, "-m"), Some("4096".to_owned()));
        assert_eq!(pair(&arguments, "-smp"), Some("8".to_owned()));
    }

    #[test]
    fn shared_memory_is_configured_whether_or_not_anything_is_shared() {
        let scratch = Scratch::new("memfd");
        let arguments = arguments(&instance("one"), &scratch.directory("one"));
        assert_eq!(
            pair(&arguments, "-object"),
            Some("memory-backend-memfd,id=mem,size=2048M,share=on".to_owned())
        );
        assert!(
            pair(&arguments, "-machine")
                .unwrap()
                .contains("memory-backend=mem")
        );
    }

    /// The default model has to exist under emulation too, which `host` does not.
    #[test]
    fn the_accelerator_falls_back_and_the_processor_survives_it() {
        let scratch = Scratch::new("accel");
        let arguments = arguments(&instance("one"), &scratch.directory("one"));
        assert!(
            pair(&arguments, "-machine")
                .unwrap()
                .contains("accel=kvm:tcg")
        );
        assert_eq!(pair(&arguments, "-cpu"), Some("max".to_owned()));
    }

    fn drives(arguments: &[String]) -> Vec<String> {
        arguments
            .iter()
            .enumerate()
            .filter(|(at, held)| *held == "-drive" && arguments.len() > at + 1)
            .filter_map(|(at, _)| arguments.get(at + 1).cloned())
            .collect()
    }

    #[test]
    fn the_recorded_processor_model_is_the_one_presented() {
        let scratch = Scratch::new("cpumodel");
        let mut held = instance("one");
        held.cpu = "Penryn,vendor=GenuineIntel,+avx".to_owned();
        let arguments = arguments(&held, &scratch.directory("one"));
        assert_eq!(
            pair(&arguments, "-cpu"),
            Some("Penryn,vendor=GenuineIntel,+avx".to_owned())
        );
    }

    #[test]
    fn a_pc_machine_with_ide_disks_puts_both_disks_on_the_ide_controller() {
        let scratch = Scratch::new("ide");
        let directory = scratch.directory("one");
        let mut held = instance("one");
        held.machine = crate::machine::Chipset::Pc;
        held.disk = crate::machine::Disk::Ide;
        let arguments = arguments(&held, &directory);
        assert!(
            pair(&arguments, "-machine").unwrap().starts_with("pc,"),
            "{arguments:?}"
        );
        let disks = drives(&arguments);
        let overlay = disks
            .iter()
            .find(|drive| drive.contains(&directory.overlay().display().to_string()))
            .unwrap();
        assert!(overlay.contains("if=ide,index=0"), "{overlay}");
        let seed = disks
            .iter()
            .find(|drive| drive.contains("seed.img"))
            .unwrap();
        assert!(seed.contains("if=ide,index=1"), "{seed}");
    }

    #[test]
    fn the_default_machine_is_q35_with_virtio_disks() {
        let scratch = Scratch::new("q35virtio");
        let directory = scratch.directory("one");
        let arguments = arguments(&instance("one"), &directory);
        assert!(pair(&arguments, "-machine").unwrap().starts_with("q35,"));
        assert!(
            drives(&arguments)
                .iter()
                .all(|drive| drive.contains("pflash") || drive.contains("if=virtio"))
        );
    }

    #[test]
    fn a_bios_machine_is_given_no_flash() {
        let scratch = Scratch::new("bios");
        let arguments = arguments(&instance("one"), &scratch.directory("one"));
        assert!(
            !drives(&arguments)
                .iter()
                .any(|drive| drive.contains("pflash"))
        );
    }

    #[test]
    fn a_uefi_machine_is_given_shared_code_and_its_own_variables() {
        let scratch = Scratch::new("uefi");
        let directory = scratch.directory("one");
        let mut held = instance("one");
        held.firmware = Firmware::Uefi;
        let flash: Vec<String> = drives(&arguments(&held, &directory))
            .into_iter()
            .filter(|drive| drive.contains("if=pflash"))
            .collect();
        assert_eq!(flash.len(), 2, "{flash:?}");
        assert!(flash[0].contains("unit=0"), "{}", flash[0]);
        assert!(flash[0].contains("readonly=on"), "{}", flash[0]);
        assert!(flash[0].contains("OVMF_CODE_4M.fd"), "{}", flash[0]);
        assert!(flash[1].contains("unit=1"), "{}", flash[1]);
        assert!(!flash[1].contains("readonly"), "{}", flash[1]);
        assert!(
            flash[1].contains(&directory.firmware_variables().display().to_string()),
            "{}",
            flash[1]
        );
    }

    #[test]
    fn the_overlay_is_the_disk_and_the_image_is_not_named() {
        let scratch = Scratch::new("disk");
        let directory = scratch.directory("one");
        let arguments = arguments(&instance("one"), &directory);
        let drives: Vec<String> = arguments
            .iter()
            .enumerate()
            .filter(|(at, held)| *held == "-drive" && arguments.len() > at + 1)
            .filter_map(|(at, _)| arguments.get(at + 1).cloned())
            .collect();
        assert!(drives[0].contains(&directory.overlay().display().to_string()));
    }

    #[test]
    fn a_seeded_instance_is_given_its_seed_as_a_raw_volume() {
        let scratch = Scratch::new("seeded");
        let directory = scratch.directory("one");
        let arguments = arguments(&instance("one"), &directory);
        let seed = arguments
            .iter()
            .find(|held| held.contains("seed.img"))
            .unwrap();
        assert!(seed.contains("format=raw"), "{seed}");
        assert!(!seed.contains("readonly"), "{seed}");
    }

    #[test]
    fn an_unseedable_instance_is_given_no_seed() {
        let scratch = Scratch::new("unseeded");
        let mut held = instance("one");
        held.seeded = false;
        let arguments = arguments(&held, &scratch.directory("one"));
        assert!(!arguments.iter().any(|held| held.contains("seed.img")));
    }

    #[test]
    fn published_ports_become_forwards_on_the_loopback_address() {
        let scratch = Scratch::new("ports");
        let mut held = instance("one");
        held.ports.push(Port {
            host: 2222,
            guest: 22,
        });
        held.ports.push(Port {
            host: 8080,
            guest: 80,
        });
        let netdev = pair(&arguments(&held, &scratch.directory("one")), "-netdev").unwrap();
        assert!(
            netdev.contains("hostfwd=tcp:127.0.0.1:2222-:22"),
            "{netdev}"
        );
        assert!(
            netdev.contains("hostfwd=tcp:127.0.0.1:8080-:80"),
            "{netdev}"
        );
    }

    #[test]
    fn the_ssh_port_is_forwarded_without_being_published() {
        let scratch = Scratch::new("sshport");
        let mut held = instance("one");
        held.ssh_port = Some(2222);
        let netdev = pair(&arguments(&held, &scratch.directory("one")), "-netdev").unwrap();
        assert_eq!(netdev, "user,id=net0,hostfwd=tcp:127.0.0.1:2222-:22");
    }

    #[test]
    fn an_instance_with_no_ports_forwards_nothing() {
        let scratch = Scratch::new("noports");
        let netdev = pair(
            &arguments(&instance("one"), &scratch.directory("one")),
            "-netdev",
        )
        .unwrap();
        assert_eq!(netdev, "user,id=net0");
    }

    #[test]
    fn the_monitor_and_the_console_are_where_the_directory_says() {
        let scratch = Scratch::new("paths");
        let directory = scratch.directory("one");
        let arguments = arguments(&instance("one"), &directory);
        assert_eq!(
            pair(&arguments, "-qmp"),
            Some(format!(
                "unix:{},server,nowait",
                directory.monitor().display()
            ))
        );
        assert_eq!(
            pair(&arguments, "-serial"),
            Some("chardev:console".to_owned())
        );
        assert_eq!(
            pair(&arguments, "-chardev"),
            Some(format!(
                "socket,id=console,path={},server=on,wait=off,logfile={},logappend=on",
                directory.console_socket().display(),
                directory.console().display()
            ))
        );
    }

    /// Without `wait=off` the guest would not boot until someone attached.
    #[test]
    fn the_console_waits_for_nobody_and_keeps_its_log() {
        let scratch = Scratch::new("consolewait");
        let arguments = arguments(&instance("one"), &scratch.directory("one"));
        let console = pair(&arguments, "-chardev").unwrap();
        assert!(console.contains("wait=off"), "{console}");
        assert!(console.contains("logappend=on"), "{console}");
    }

    #[test]
    fn every_machine_has_a_screen_on_a_private_socket() {
        let scratch = Scratch::new("screen");
        let directory = scratch.directory("one");
        let arguments = arguments(&instance("one"), &directory);
        assert_eq!(
            pair(&arguments, "-vnc"),
            Some(format!("unix:{}", directory.screen_socket().display()))
        );
        assert_eq!(pair(&arguments, "-display"), Some("none".to_owned()));
    }

    fn share(tag: &str, target: &str) -> Share {
        Share {
            tag: tag.to_owned(),
            source: PathBuf::from("/home/x/work"),
            target: target.to_owned(),
            pid: None,
            started: None,
        }
    }

    #[test]
    fn a_share_becomes_a_socket_and_a_tagged_device() {
        let scratch = Scratch::new("share");
        let directory = scratch.directory("one");
        let mut held = instance("one");
        held.shares.push(share("work", "/mnt/work"));
        let arguments = arguments(&held, &directory);
        assert_eq!(
            pair(&arguments, "-chardev"),
            Some(format!(
                "socket,id=vfs0,path={}",
                directory.share_socket(0).display()
            ))
        );
        assert!(
            arguments
                .iter()
                .any(|held| held == "vhost-user-fs-pci,chardev=vfs0,tag=work"),
            "{arguments:?}"
        );
    }

    #[test]
    fn several_shares_are_numbered_apart() {
        let scratch = Scratch::new("shares");
        let mut held = instance("one");
        held.shares.push(share("work", "/mnt/work"));
        held.shares.push(share("src", "/mnt/src"));
        let arguments = arguments(&held, &scratch.directory("one"));
        let devices: Vec<&String> = arguments
            .iter()
            .filter(|held| held.starts_with("vhost-user-fs-pci"))
            .collect();
        assert_eq!(devices.len(), 2);
        assert!(devices[0].contains("chardev=vfs0,tag=work"), "{devices:?}");
        assert!(devices[1].contains("chardev=vfs1,tag=src"), "{devices:?}");
    }

    #[test]
    fn an_instance_with_no_shares_has_no_share_devices() {
        let scratch = Scratch::new("noshares");
        let arguments = arguments(&instance("one"), &scratch.directory("one"));
        assert!(!arguments.iter().any(|held| held.contains("vhost-user-fs")));
        // The console has its own chardev.
        assert!(!arguments.iter().any(|held| held.contains("vfs")));
    }

    #[test]
    fn a_share_is_served_from_the_directory_it_names() {
        let launch = share_launch(
            Path::new("/home/x/work"),
            Path::new("/run/user/1000/vm/abc.fs0"),
            PathBuf::from("/dev/null"),
        );
        assert_eq!(Path::new(&launch.program).file_name().unwrap(), "virtiofsd");
        assert!(
            launch
                .arguments
                .contains(&"--socket-path=/run/user/1000/vm/abc.fs0".to_owned()),
            "{:?}",
            launch.arguments
        );
        assert!(launch.arguments.contains(&"/home/x/work".to_owned()));
    }

    #[test]
    fn a_share_server_that_is_not_installed_names_its_package() {
        let scratch = Scratch::new("novirtiofsd");
        let launch = Launch {
            program: scratch.0.join("nowhere/virtiofsd").display().to_string(),
            arguments: Vec::new(),
            log: scratch.0.join("log"),
        };
        let error = Detached.start(&launch).unwrap_err();
        assert_eq!(error.kind(), "missing-tool");
        assert!(
            error.to_string().contains("apt install virtiofsd"),
            "{error}"
        );
    }

    #[test]
    fn a_tool_off_the_path_is_found_where_the_distribution_puts_it() {
        let found = locate("virtiofsd", "/usr/bin:/bin", &["/usr/libexec"], &|at| {
            at == Path::new("/usr/libexec/virtiofsd")
        });
        assert_eq!(found, "/usr/libexec/virtiofsd");
    }

    /// PATH is searched first, so a build of one's own is still the one run.
    #[test]
    fn the_path_is_preferred_to_the_places_the_distribution_uses() {
        let found = locate("virtiofsd", "/opt/mine/bin", &["/usr/libexec"], &|_| true);
        assert_eq!(found, "/opt/mine/bin/virtiofsd");
    }

    #[test]
    fn a_tool_that_is_nowhere_is_left_as_a_name() {
        assert_eq!(
            locate("virtiofsd", "/usr/bin", &["/usr/libexec"], &|_| false),
            "virtiofsd"
        );
    }

    /// The search order is PATH, then each extra directory in turn.
    #[test]
    fn the_distribution_directories_are_tried_in_order() {
        let found = locate("virtiofsd", "", &["/a", "/b"], &|at| {
            at.starts_with("/a") || at.starts_with("/b")
        });
        assert_eq!(found, "/a/virtiofsd");
    }

    #[test]
    fn nothing_is_displayed_and_the_machine_is_not_a_window() {
        let scratch = Scratch::new("display");
        let arguments = arguments(&instance("one"), &scratch.directory("one"));
        assert_eq!(pair(&arguments, "-display"), Some("none".to_owned()));
    }

    #[test]
    fn a_recorded_launch_is_the_one_that_was_asked_for() {
        let supervisor = Recording::default();
        let launch = Launch {
            program: "qemu-system-x86_64".to_owned(),
            arguments: vec!["-m".to_owned(), "512".to_owned()],
            log: PathBuf::from("/dev/null"),
        };
        let handle = supervisor.start(&launch).unwrap();
        assert_eq!(handle.pid, 1);
        assert_eq!(supervisor.launches.lock().unwrap().as_slice(), [launch]);
    }

    #[test]
    fn a_hypervisor_that_is_not_installed_names_its_package() {
        let scratch = Scratch::new("absent");
        let error = Detached
            .start(&Launch {
                program: "qemu-system-nothing-like-this".to_owned(),
                arguments: Vec::new(),
                log: scratch.0.join("log"),
            })
            .unwrap_err();
        assert_eq!(error.kind(), "missing-tool");
        assert!(error.to_string().contains("apt install"), "{error}");
    }

    #[test]
    fn a_log_that_cannot_be_opened_is_reported() {
        let error = Detached
            .start(&Launch {
                program: "true".to_owned(),
                arguments: Vec::new(),
                log: PathBuf::from("/nonexistent/directory/log"),
            })
            .unwrap_err();
        assert_eq!(error.kind(), "state-unusable");
    }

    #[test]
    fn a_started_process_is_identified_and_its_output_captured() {
        let scratch = Scratch::new("start");
        let log = scratch.0.join("log");
        let handle = Detached
            .start(&Launch {
                program: "sh".to_owned(),
                arguments: vec!["-c".to_owned(), "echo spoken; echo aside >&2".to_owned()],
                log: log.clone(),
            })
            .unwrap();
        assert!(handle.pid > 0);
        for _ in 0..100 {
            if fs::read_to_string(&log).is_ok_and(|held| held.contains("aside")) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let held = fs::read_to_string(&log).unwrap();
        assert!(held.contains("spoken"), "{held}");
        assert!(held.contains("aside"), "{held}");
    }

    #[test]
    fn a_disk_grows_to_the_size_it_is_given() {
        let scratch = Scratch::new("resize");
        let overlay = scratch.0.join("disk.qcow2");
        let made = Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2"])
            .arg(&overlay)
            .arg("64M")
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return; // qemu-img is not installed.
        }
        resize_overlay(&overlay, "128M").unwrap();
        let info = Command::new("qemu-img")
            .args(["info", "--output=json"])
            .arg(&overlay)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&info.stdout);
        assert!(text.contains("134217728"), "{text}");
    }

    /// Shrinking would leave the guest's filesystem past the end of its disk.
    #[test]
    fn a_disk_is_not_shrunk_by_asking_for_less() {
        let scratch = Scratch::new("shrink");
        let overlay = scratch.0.join("disk.qcow2");
        let made = Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2"])
            .arg(&overlay)
            .arg("128M")
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return;
        }
        let error = resize_overlay(&overlay, "64M").unwrap_err();
        assert_eq!(error.kind(), "overlay-failed");
        assert!(error.to_string().contains("shrink"), "{error}");
    }

    #[test]
    fn an_overlay_is_created_over_its_image() {
        let scratch = Scratch::new("overlay");
        let image = scratch.0.join("image.qcow2");
        let made = Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2"])
            .arg(&image)
            .arg("64M")
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return; // qemu-img is not installed.
        }
        let overlay = scratch.0.join("overlay.qcow2");
        create_overlay(&image, &overlay, "qcow2", "128M".into()).unwrap();
        assert!(overlay.is_file());
        let info = Command::new("qemu-img")
            .args(["info", "--output=json"])
            .arg(&overlay)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&info.stdout);
        assert!(text.contains("backing-filename"), "{text}");
    }

    #[test]
    fn an_overlay_over_a_raw_image_records_the_backing_format() {
        let scratch = Scratch::new("rawoverlay");
        let image = scratch.0.join("image.raw");
        let made = Command::new("qemu-img")
            .args(["create", "-q", "-f", "raw"])
            .arg(&image)
            .arg("64M")
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return; // qemu-img is not installed.
        }
        let overlay = scratch.0.join("overlay.qcow2");
        create_overlay(&image, &overlay, "raw", None).unwrap();
        let info = Command::new("qemu-img")
            .args(["info", "--output=json"])
            .arg(&overlay)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&info.stdout);
        assert!(
            text.contains(r#""backing-filename-format": "raw""#),
            "{text}"
        );
        assert!(
            String::from_utf8_lossy(&info.stderr).is_empty(),
            "{}",
            String::from_utf8_lossy(&info.stderr)
        );
    }

    #[test]
    fn an_overlay_over_an_image_that_is_not_there_is_refused() {
        let scratch = Scratch::new("nobacking");
        let outcome = create_overlay(
            &scratch.0.join("absent.qcow2"),
            &scratch.0.join("overlay.qcow2"),
            "qcow2",
            None,
        );
        match outcome {
            Err(error) => assert!(
                matches!(error.kind(), "overlay-failed" | "missing-tool"),
                "{error}"
            ),
            Ok(()) => panic!("an overlay over nothing should not have been created"),
        }
    }
}

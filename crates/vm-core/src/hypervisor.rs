//! Launching the hypervisor, and the seam behind which that happens.
//!
//! Supervision is a trait for two reasons: systemd user units are a later
//! alternative to launching processes directly, and a fake implementation is
//! what makes the rest of the lifecycle testable without booting anything.
//! The trait is narrow because QMP is the control channel under every
//! implementation, so only starting differs.

use crate::error::{Error, Result};
use crate::instance::{Directory, Instance};
use crate::process::Handle;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The QEMU binary for an architecture. Only the host's own is useful without
/// emulation, but the name is per-architecture either way.
pub fn binary_for(arch: &str) -> &'static str {
    match arch {
        "arm64" => "qemu-system-aarch64",
        "riscv64" => "qemu-system-riscv64",
        "ppc64el" => "qemu-system-ppc64",
        _ => "qemu-system-x86_64",
    }
}

/// What to start, fully resolved. Nothing here is interpreted further.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: String,
    pub arguments: Vec<String>,
    /// Where the process's own output goes. Not the guest's console, which
    /// QEMU writes itself.
    pub log: PathBuf,
}

pub trait Supervisor {
    /// Starts the process and returns a handle that outlives this command.
    fn start(&self, launch: &Launch) -> Result<Handle>;
}

/// Launches the hypervisor as a detached process.
///
/// It is put in a process group of its own, so that a terminal interrupt after
/// `vm run` returns does not reach a machine the user has already been told is
/// running. Nothing of this process's own stdio is inherited.
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
        // Read while it is certainly alive: the start time is what tells this
        // process apart from whoever inherits its pid later.
        Handle::of(child.id()).ok_or_else(|| Error::Launch {
            program: launch.program.clone(),
            source: std::io::Error::other("the hypervisor exited before it could be recorded"),
        })
    }
}

fn missing(program: &str) -> Error {
    Error::MissingTool {
        binary: if program.starts_with("qemu-system") {
            "qemu-system-x86_64"
        } else {
            "qemu-img"
        },
        package: if program.starts_with("qemu-system") {
            "qemu-system-x86"
        } else {
            "qemu-utils"
        },
        operation: "starting a virtual machine",
    }
}

/// Builds the hypervisor's arguments for an instance.
///
/// Separated from starting it so that the command line can be asserted
/// without a hypervisor present, which is most of what there is to get wrong.
pub fn arguments(instance: &Instance, directory: &Directory) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |argument: &str| out.push(argument.to_owned());

    // Shared memory is a condition of virtiofs working at all, and the backend
    // has to be in place from the first boot rather than added when a share
    // is, so every instance is built this way whether it shares anything now
    // or not.
    push("-object");
    push(&format!(
        "memory-backend-memfd,id=mem,size={}M,share=on",
        instance.memory
    ));
    push("-machine");
    push("q35,memory-backend=mem,accel=kvm:tcg");
    // `max` rather than `host`, which only exists under KVM: the accelerator
    // above falls back to emulation, and the CPU model must survive that.
    push("-cpu");
    push("max");
    push("-m");
    push(&instance.memory.to_string());
    push("-smp");
    push(&instance.cpus.to_string());
    push("-drive");
    push(&format!(
        "file={},if=virtio,format=qcow2",
        directory.overlay().display()
    ));
    if instance.seeded {
        push("-drive");
        push(&format!(
            "file={},if=virtio,format=raw,readonly=on",
            directory.seed().display()
        ));
    }
    push("-netdev");
    push(&network(instance));
    push("-device");
    push("virtio-net-pci,netdev=net0");
    // Without this the guest waits on entropy during early boot, which under
    // emulation is long enough to look like a hang.
    push("-device");
    push("virtio-rng-pci");
    push("-display");
    push("none");
    push("-serial");
    push(&format!("file:{}", directory.console().display()));
    push("-qmp");
    push(&format!(
        "unix:{},server,nowait",
        directory.monitor().display()
    ));
    out
}

/// User-mode networking: no privilege, no bridge, and port forwarding that is
/// the direct analogue of publishing a container's port.
fn network(instance: &Instance) -> String {
    use std::fmt::Write as _;
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
        })
}

/// Creates the instance's writable disk over an image in the store. The image
/// itself is never written to: it backs every instance built from it.
pub fn create_overlay(image: &Path, overlay: &Path, size: Option<&str>) -> Result<()> {
    let mut command = Command::new("qemu-img");
    command
        .arg("create")
        .arg("-q")
        .arg("-f")
        .arg("qcow2")
        .arg("-F")
        .arg("qcow2")
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
    use crate::instance::{Instances, Port};
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
            user: "vm".to_owned(),
            seeded: true,
            monitor: PathBuf::new(),
            pid: None,
            started: None,
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

    /// virtiofs needs shared memory, and it cannot be introduced later without
    /// restarting the machine, so it is there from the first boot.
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

    /// The accelerator falls back rather than failing, so the processor model
    /// has to be one that exists under emulation too.
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
    fn a_seeded_instance_is_given_its_seed_read_only() {
        let scratch = Scratch::new("seeded");
        let directory = scratch.directory("one");
        let arguments = arguments(&instance("one"), &directory);
        let seed = arguments
            .iter()
            .find(|held| held.contains("seed.img"))
            .unwrap();
        assert!(seed.contains("readonly=on"), "{seed}");
        assert!(seed.contains("format=raw"), "{seed}");
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
            Some(format!("file:{}", directory.console().display()))
        );
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

    /// The detached process is real, so this checks the parts that matter:
    /// that it starts, that it is identified, and that its output lands in the
    /// log rather than on the terminal.
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
    fn an_overlay_is_created_over_its_image() {
        let scratch = Scratch::new("overlay");
        let image = scratch.0.join("image.qcow2");
        let made = Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2"])
            .arg(&image)
            .arg("64M")
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return; // qemu-img is not installed here; the CLI reports that itself.
        }
        let overlay = scratch.0.join("overlay.qcow2");
        create_overlay(&image, &overlay, Some("128M")).unwrap();
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
    fn an_overlay_over_an_image_that_is_not_there_is_refused() {
        let scratch = Scratch::new("nobacking");
        let outcome = create_overlay(
            &scratch.0.join("absent.qcow2"),
            &scratch.0.join("overlay.qcow2"),
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

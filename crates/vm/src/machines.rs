//! The instance commands: starting a machine, listing them, stopping one and
//! removing it.
//!
//! Everything here is the composition; the pieces it composes — the state
//! directory, the seed, the monitor, the supervisor — carry their own tests.

use crate::progress;
use crate::reports;
use crate::style::Style;
use std::path::Path;
use std::time::{Duration, Instant};
use vm_core::catalogue::Catalogue;
use vm_core::error::{Error, Result};
use vm_core::hypervisor::Supervisor as _;
use vm_core::instance::{Directory, Instance, Instances, Port};
use vm_core::store::{Pulled, Store};
use vm_core::value::Value;
use vm_core::{host_architecture, hypervisor, instance, keys, process, qmp, seed};

/// How long to wait for a machine to open its monitor before concluding that
/// it did not start. Under emulation a machine is slow to boot but quick to
/// answer, because the monitor is open before the guest runs at all.
const MONITOR_WAIT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Pull {
    /// Fetch the image even if it is already held.
    Always,
    /// Fetch it only if it is not held. The default.
    Missing,
    /// Never fetch; a missing image is an error.
    Never,
}

/// What `vm run` was asked for, before any of it is resolved.
pub struct Request {
    pub reference: String,
    pub name: Option<String>,
    pub memory: u64,
    pub cpus: u32,
    pub ports: Vec<Port>,
    pub user: String,
    pub disk_size: Option<String>,
    pub pull: Option<Pull>,
}

pub fn run(
    catalogue: &Catalogue,
    store: &Store,
    request: &Request,
    style: Style,
    text: bool,
) -> Result<reports::Run> {
    let reference: vm_core::Reference = request.reference.parse()?;
    let (entry, artifact) = catalogue.resolve(&reference, host_architecture())?;
    let pull = match request.pull {
        Some(held) => held,
        None if vm_core::config::Config::load()?.auto_pull => Pull::Missing,
        None => Pull::Never,
    };
    let held = store.contains(&artifact.digest);
    match (pull, held) {
        (Pull::Never, false) => {
            return Err(Error::UnheldImage {
                reference: request.reference.clone(),
            });
        }
        (Pull::Always, _) | (Pull::Missing, false) => {
            let mut bar = progress::Bar::new("  ", text);
            if text {
                eprintln!("Fetching {}", style.name(&artifact.url));
            }
            let outcome = store.pull(artifact, &vm_core::store::http_agent(), &mut |update| {
                bar.update(update);
            });
            bar.clear();
            let _: Pulled = outcome?;
            store.record(entry, artifact)?;
        }
        _ => {}
    }

    let instances = Instances::discover()?;
    let name = request.name.clone().unwrap_or_else(|| {
        let taken = instances.names().unwrap_or_default();
        instance::suggest_name(&entry.name, &|candidate| {
            taken.iter().any(|held| held == candidate)
        })
    });
    let directory = instances.create(&name)?;
    // Everything after this point owns a claimed name, so a failure has to
    // give it back rather than leave a directory nothing can start.
    match build(store, &directory, entry, artifact, request, &name) {
        Ok(report) => Ok(report),
        Err(error) => {
            let _ = directory.remove();
            Err(error)
        }
    }
}

fn build(
    store: &Store,
    directory: &Directory,
    entry: &vm_core::catalogue::Entry,
    artifact: &vm_core::catalogue::Artifact,
    request: &Request,
    name: &str,
) -> Result<reports::Run> {
    let mut held = Instance {
        name: name.to_owned(),
        image: request.reference.clone(),
        digest: artifact.digest.to_string(),
        arch: artifact.arch.clone(),
        created: instance::now(),
        memory: request.memory,
        cpus: request.cpus,
        user: request.user.clone(),
        seeded: entry.login.is_seedable(),
        monitor: directory.monitor().to_owned(),
        ssh_port: None,
        pid: None,
        started: None,
        ports: request.ports.clone(),
        shares: Vec::new(),
    };

    if held.seeded {
        // A published forward to the guest's SSH port is the one to use; only
        // allocate when the user has not already arranged one.
        held.ssh_port = Some(match held.ports.iter().find(|port| port.guest == 22) {
            Some(port) => port.host,
            None => free_port()?,
        });
    }

    hypervisor::create_overlay(
        &store.path_for(&artifact.digest),
        &directory.overlay(),
        request.disk_size.as_deref(),
    )?;

    let public = keys::generate(&directory.key(), &format!("vm-{name}"))?;
    if held.seeded {
        instance::seed_for(&held, &public).write(&directory.seed())?;
    }
    directory.write(&held)?;

    let accelerated = launch(directory, &mut held)?;
    Ok(report(
        &held,
        directory,
        accelerated,
        reports::RunStatus::Created,
    ))
}

/// Starts the hypervisor and waits for it to answer.
///
/// A machine that dies on its arguments dies within a moment of starting, so
/// waiting for its monitor is what separates "started" from "reported as
/// started". What it says about acceleration comes free with the wait.
fn launch(directory: &Directory, held: &mut Instance) -> Result<Option<bool>> {
    prepare_runtime(directory.monitor())?;
    let handle = vm_core::hypervisor::Detached.start(&hypervisor::Launch {
        program: hypervisor::binary_for(&held.arch).to_owned(),
        arguments: hypervisor::arguments(held, directory),
        log: directory.log(),
    })?;
    held.pid = Some(handle.pid);
    held.started = Some(handle.started);
    directory.write(held)?;

    match await_monitor(directory.monitor(), &handle) {
        Ok(accelerated) => Ok(accelerated),
        Err(error) => {
            let _ = process::signal(&handle, process::Signal::Kill);
            held.forget_process();
            let _ = directory.write(held);
            Err(refusal(directory, error))
        }
    }
}

fn report(
    held: &Instance,
    directory: &Directory,
    accelerated: Option<bool>,
    status: reports::RunStatus,
) -> reports::Run {
    reports::Run {
        name: held.name.clone(),
        image: held.image.clone(),
        arch: held.arch.clone(),
        memory: held.memory,
        cpus: held.cpus,
        ports: held.ports.clone(),
        ssh_port: held.ssh_port,
        user: held.user.clone(),
        seeded: held.seeded,
        pid: held.pid.unwrap_or_default(),
        accelerated,
        console: directory.console().display().to_string(),
        status,
    }
}

/// Boots an instance that exists but is not running.
pub fn start(name: &str) -> Result<reports::Run> {
    let instances = Instances::discover()?;
    let directory = instances.open(name)?;
    let mut held = directory.read()?;
    if held.is_running() {
        return Ok(report(
            &held,
            &directory,
            None,
            reports::RunStatus::AlreadyRunning,
        ));
    }
    held.forget_process();
    // The port it used last time may belong to something else by now, and a
    // forward that cannot bind would take the whole machine down with it.
    if held.seeded && !held.ssh_port.is_some_and(port_is_free) {
        held.ssh_port = Some(free_port()?);
    }
    let accelerated = launch(&directory, &mut held)?;
    Ok(report(
        &held,
        &directory,
        accelerated,
        reports::RunStatus::Restarted,
    ))
}

/// Replaces this process with `ssh`, so that the terminal, the signals and the
/// exit status are the guest's rather than filtered through ours.
pub fn connect(name: &str, command: &[String]) -> Result<std::convert::Infallible> {
    let instances = Instances::discover()?;
    let directory = instances.open(name)?;
    let held = directory.read()?;
    let arguments = vm_core::access::ssh(&held, &directory, command)?;
    Err(exec("ssh", &arguments))
}

/// Copies between here and a guest, the same way.
pub fn copy(from: &str, to: &str) -> Result<std::convert::Infallible> {
    use vm_core::access::Location;
    let (from, to) = (Location::parse(from), Location::parse(to));
    let name = match (from.instance(), to.instance()) {
        (Some(_), Some(_)) => {
            return Err(Error::CopyBetweenGuests);
        }
        (Some(name), None) | (None, Some(name)) => name.clone(),
        (None, None) => return Err(Error::CopyWithoutGuest),
    };
    let instances = Instances::discover()?;
    let directory = instances.open(&name)?;
    let held = directory.read()?;
    let arguments = vm_core::access::scp(&held, &directory, &from, &to)?;
    Err(exec("scp", &arguments))
}

fn exec(program: &str, arguments: &[String]) -> Error {
    use std::os::unix::process::CommandExt as _;
    // This returns only on failure; on success the process is gone.
    let source = std::process::Command::new(program).args(arguments).exec();
    if source.kind() == std::io::ErrorKind::NotFound {
        Error::MissingTool {
            binary: "ssh",
            package: "openssh-client",
            operation: "reaching a virtual machine",
        }
    } else {
        Error::Launch {
            program: program.to_owned(),
            source,
        }
    }
}

/// A free port on the loopback address, found by asking the kernel for one.
/// There is a moment between letting it go and the hypervisor taking it; if
/// something else wins that race the hypervisor says so and the run is
/// reported as failed rather than as started.
fn free_port() -> Result<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|source| Error::Launch {
            program: "the port allocator".to_owned(),
            source,
        })
}

fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// A hypervisor that refuses its arguments says why on its own output and
/// then exits, so its log is the diagnosis and repeating it is the whole job.
fn refusal(directory: &Directory, error: Error) -> Error {
    let complaint = std::fs::read_to_string(directory.log())
        .unwrap_or_default()
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .map(ToOwned::to_owned);
    complaint.map_or(error, |reason| Error::Launch {
        program: "the hypervisor".to_owned(),
        source: std::io::Error::other(reason),
    })
}

fn prepare_runtime(monitor: &Path) -> Result<()> {
    if let Some(parent) = monitor.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::State {
            path: parent.to_owned(),
            action: "create the runtime directory",
            source,
        })?;
    }
    // A socket left behind by a machine that is gone would refuse the new one.
    let _ = std::fs::remove_file(monitor);
    Ok(())
}

/// Waits for the machine to answer, and asks it whether it is accelerated
/// while it has its attention.
fn await_monitor(monitor: &Path, handle: &process::Handle) -> Result<Option<bool>> {
    let deadline = Instant::now() + MONITOR_WAIT;
    loop {
        if !handle.is_running() {
            return Err(Error::Launch {
                program: "the hypervisor".to_owned(),
                source: std::io::Error::other("it exited immediately"),
            });
        }
        if monitor.exists() {
            if let Ok(mut client) = qmp::connect(monitor) {
                return Ok(acceleration(&mut client));
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::Launch {
                program: "the hypervisor".to_owned(),
                source: std::io::Error::other("it did not open its monitor"),
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether the machine got KVM. It falls back to emulation rather than
/// failing, and the difference is the difference between seconds and minutes,
/// so it is worth saying which happened.
fn acceleration(client: &mut qmp::Connection) -> Option<bool> {
    let reply = client.execute("query-kvm", None).ok()?;
    match reply.get("enabled") {
        Some(Value::Bool(enabled)) => Some(*enabled),
        _ => None,
    }
}

/// Lists instances, forgetting any process that is no longer there.
pub fn list(all: bool) -> Result<reports::Machines> {
    let instances = Instances::discover()?;
    let mut rows = Vec::new();
    for name in instances.names()? {
        let Ok(directory) = instances.open(&name) else {
            continue;
        };
        let Ok(mut held) = directory.read() else {
            rows.push(reports::MachineRow::damaged(&name));
            continue;
        };
        let running = held.is_running();
        if !running && held.handle().is_some() {
            held.forget_process();
            let _ = directory.write(&held);
        }
        if running || all {
            rows.push(reports::MachineRow::of(&held, running));
        }
    }
    Ok(reports::Machines { rows, all })
}

/// Asks the guest to shut down, then insists.
pub fn stop(name: &str, timeout: Duration, force: bool, text: bool) -> Result<reports::Stopped> {
    let instances = Instances::discover()?;
    let directory = instances.open(name)?;
    let mut held = directory.read()?;
    let Some(handle) = held.handle().filter(process::Handle::is_running) else {
        held.forget_process();
        let _ = directory.write(&held);
        return Ok(reports::Stopped {
            name: name.to_owned(),
            outcome: reports::StopOutcome::AlreadyStopped,
            waited: 0,
        });
    };

    let mut outcome = reports::StopOutcome::PoweredDown;
    if force {
        outcome = reports::StopOutcome::Killed;
        if let Ok(mut client) = qmp::connect(&held.monitor) {
            let _ = client.quit();
        }
    } else if let Ok(mut client) = qmp::connect(&held.monitor) {
        client.powerdown()?;
    } else {
        // No monitor to ask through, so the polite route is not available.
        outcome = reports::StopOutcome::Unreachable;
        process::signal(&handle, process::Signal::Terminate)?;
    }

    if text && outcome == reports::StopOutcome::PoweredDown {
        eprintln!(
            "Waiting for {name} to shut down, up to {}s",
            timeout.as_secs()
        );
    }

    if !wait_for_exit(&handle, timeout) {
        // The guest ignored the power button, which is what a machine early in
        // its boot does: the handler is not loaded yet.
        if outcome == reports::StopOutcome::PoweredDown {
            outcome = reports::StopOutcome::Unresponsive;
        }
        process::signal(&handle, process::Signal::Kill)?;
        if !wait_for_exit(&handle, Duration::from_secs(5)) {
            return Err(Error::Signal {
                pid: handle.pid,
                source: std::io::Error::other("the hypervisor did not exit"),
            });
        }
    }

    held.forget_process();
    directory.write(&held)?;
    let _ = std::fs::remove_file(&held.monitor);
    Ok(reports::Stopped {
        name: name.to_owned(),
        outcome,
        waited: timeout.as_secs(),
    })
}

fn wait_for_exit(handle: &process::Handle, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !handle.is_running() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !handle.is_running()
}

/// Removes an instance and everything it owns.
pub fn remove(name: &str, force: bool) -> Result<reports::Removed> {
    let instances = Instances::discover()?;
    let directory = instances.open(name)?;
    let held = directory.read().ok();
    if held.as_ref().is_some_and(Instance::is_running) {
        if !force {
            return Err(Error::InstanceRunning {
                name: name.to_owned(),
            });
        }
        stop(name, Duration::from_secs(10), true, false)?;
    }
    directory.remove()?;
    Ok(reports::Removed {
        name: name.to_owned(),
    })
}

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

/// The seed's own rules apply to the user name, so they are checked here
/// rather than at the point where the machine is about to boot.
pub fn parse_user(text: &str) -> std::result::Result<String, String> {
    let trial = seed::Seed {
        instance_id: "check".to_owned(),
        hostname: "check".to_owned(),
        user: text.to_owned(),
        authorized_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKq7YQ== check".to_owned(),
        mounts: Vec::new(),
    };
    trial
        .image()
        .map(|_| text.to_owned())
        .map_err(|_| format!("'{text}' is not a usable user name"))
}

#[cfg(test)]
mod tests {
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

    /// A machine too small to boot is a slow way to find out that a number was
    /// meant to be gibibytes.
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
    fn a_user_name_the_guest_would_refuse_is_refused_here() {
        assert_eq!(parse_user("vm"), Ok("vm".to_owned()));
        assert!(parse_user("Capitals").is_err());
        assert!(parse_user("has space").is_err());
        assert!(parse_user("").is_err());
    }
}

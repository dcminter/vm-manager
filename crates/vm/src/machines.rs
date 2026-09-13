//! Instance commands.

use crate::progress;
use crate::reports;
use crate::style::Style;
use std::path::Path;
use std::time::{Duration, Instant};
use vm_core::catalogue::Catalogue;
use vm_core::error::{Error, Result};
use vm_core::hypervisor::Supervisor as _;
use vm_core::instance::{Directory, Instance, Instances, Port, Share};
use vm_core::machine::{self, Chipset, Disk, Firmware};
use vm_core::store::{Pulled, Store};
use vm_core::value::Value;
use vm_core::{host_architecture, hypervisor, instance, keys, process, qmp, seed};

/// How long a machine has to open its monitor before it counts as failed.
const MONITOR_WAIT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Pull {
    /// Fetch the image even if it is already held.
    Always,
    /// Fetch it only if it is not held (default).
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
    pub shares: Vec<Share>,
    pub disk_size: Option<String>,
    pub pull: Option<Pull>,
    pub firmware: Option<Firmware>,
    pub cpu: Option<String>,
    pub machine: Option<Chipset>,
    pub disk: Option<Disk>,
    /// A `$6$` hash for console login.
    pub password: Option<String>,
    /// Whether to add an SSH config entry; absent defers to the config file.
    pub ssh_config: Option<bool>,
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
    machine::check_disk(
        request.machine.unwrap_or(artifact.machine),
        request.disk.unwrap_or(artifact.disk),
    )?;
    if request.password.is_some() && !entry.login.is_seedable() {
        return Err(Error::PasswordUnseeded {
            name: request.reference.clone(),
        });
    }
    if request.ssh_config == Some(true) && !entry.login.is_seedable() {
        return Err(Error::NoGuestAccess {
            name: request.reference.clone(),
        });
    }
    let config = vm_core::config::Config::load()?;
    let ssh_config = request
        .ssh_config
        .unwrap_or_else(|| config.add_ssh_config && entry.login.is_seedable());
    let pull = match request.pull {
        Some(held) => held,
        None if config.auto_pull => Pull::Missing,
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
                if let Some(url) = &artifact.url {
                    eprintln!("Fetching {}", style.name(url));
                }
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
    // Refused before a name is claimed.
    if ssh_config {
        vm_core::ssh_config::check_name(&user_ssh_config()?, &name)?;
    }
    let directory = instances.create(&name)?;
    // From here a failure must release the claimed name.
    match build(
        store, &directory, entry, artifact, request, &instances, ssh_config,
    ) {
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
    instances: &Instances,
    ssh_config: bool,
) -> Result<reports::Run> {
    let name = directory.name();
    let mut held = Instance {
        name: name.to_owned(),
        image: request.reference.clone(),
        digest: artifact.digest.to_string(),
        arch: artifact.arch.clone(),
        created: instance::now(),
        memory: request.memory,
        cpus: request.cpus,
        firmware: request.firmware.unwrap_or(artifact.firmware),
        cpu: request
            .cpu
            .clone()
            .unwrap_or_else(|| artifact.cpu().to_owned()),
        machine: request.machine.unwrap_or(artifact.machine),
        disk: request.disk.unwrap_or(artifact.disk),
        user: request.user.clone(),
        seeded: entry.login.is_seedable(),
        monitor: directory.monitor().to_owned(),
        ssh_port: None,
        pid: None,
        started: None,
        generation: 0,
        ssh_config,
        password: request.password.clone(),
        ports: request.ports.clone(),
        shares: request.shares.clone(),
    };
    distinguish(&mut held.shares);

    if held.seeded {
        // Prefer a published forward to port 22 over allocating one.
        held.ssh_port = Some(match held.ports.iter().find(|port| port.guest == 22) {
            Some(port) => port.host,
            None => free_port()?,
        });
    }

    hypervisor::create_overlay(
        &store.path_for(&artifact.digest),
        &directory.overlay(),
        &artifact.format,
        request.disk_size.as_deref(),
    )?;

    let public = keys::generate(&directory.key(), &format!("vm-{name}"))?;
    if held.seeded {
        instance::seed_for(&held, &public).write(&directory.seed())?;
    }
    directory.write(&held)?;
    let included = publish(directory, &held, instances.root())?;

    let accelerated = launch(directory, &mut held)?;
    Ok(report(
        &held,
        directory,
        accelerated,
        reports::RunStatus::Created,
        false,
        included,
    ))
}

/// Starts the hypervisor and waits for its monitor, reporting whether KVM is in use.
fn launch(directory: &Directory, held: &mut Instance) -> Result<Option<bool>> {
    prepare_runtime(directory.monitor())?;
    if held.firmware == Firmware::Uefi {
        machine::prepare_uefi(&held.arch, &directory.firmware_variables())?;
    }
    // Sockets left behind by a machine that is gone would refuse the new one.
    directory.clear_runtime();
    // The hypervisor connects to these sockets, so they must be listening first.
    if let Err(error) = serve_shares(directory, held) {
        reap_shares(held);
        return Err(error);
    }
    let handle = match vm_core::hypervisor::Detached.start(&hypervisor::Launch {
        program: hypervisor::binary_for(&held.arch).to_owned(),
        arguments: hypervisor::arguments(held, directory),
        log: directory.log(),
    }) {
        Ok(handle) => handle,
        Err(error) => {
            reap_shares(held);
            let _ = directory.write(held);
            return Err(error);
        }
    };
    held.pid = Some(handle.pid);
    held.started = Some(handle.started);
    directory.write(held)?;

    match await_monitor(directory.monitor(), &handle) {
        Ok(accelerated) => Ok(accelerated),
        Err(error) => {
            let _ = process::signal(&handle, process::Signal::Kill);
            reap_shares(held);
            held.forget_process();
            let _ = directory.write(held);
            Err(refusal(directory, error))
        }
    }
}

/// Starts one `virtiofsd` per share and records it.
fn serve_shares(directory: &Directory, held: &mut Instance) -> Result<()> {
    for index in 0..held.shares.len() {
        let socket = directory.share_socket(index);
        let launch = hypervisor::share_launch(
            &held.shares[index].source,
            &socket,
            directory.share_log(index),
        );
        let handle = vm_core::hypervisor::Detached.start(&launch)?;
        held.shares[index].pid = Some(handle.pid);
        held.shares[index].started = Some(handle.started);
        // The hypervisor refuses a socket that does not exist yet.
        await_socket(&socket, &handle)?;
    }
    Ok(())
}

fn await_socket(socket: &Path, handle: &process::Handle) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if socket.exists() {
            return Ok(());
        }
        if !handle.is_running() {
            return Err(Error::Launch {
                program: "virtiofsd".to_owned(),
                source: std::io::Error::other("it exited before it began serving"),
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(Error::Launch {
        program: "virtiofsd".to_owned(),
        source: std::io::Error::other("it did not open its socket"),
    })
}

/// Ends every share server an instance owns.
fn reap_shares(held: &mut Instance) {
    for share in &mut held.shares {
        if let Some(handle) = share.handle() {
            let _ = process::signal(&handle, process::Signal::Terminate);
        }
        share.forget_process();
    }
}

/// Writes the SSH config entry, returning the user's config path if it gained the `Include` line.
fn publish(
    directory: &Directory,
    held: &Instance,
    instances: &Path,
) -> Result<Option<std::path::PathBuf>> {
    if !held.ssh_config {
        return Ok(None);
    }
    vm_core::ssh_config::write(held, directory)?;
    let user = user_ssh_config()?;
    Ok(vm_core::ssh_config::ensure_include(&user, instances)?.then_some(user))
}

fn user_ssh_config() -> Result<std::path::PathBuf> {
    vm_core::paths::ssh_config().ok_or(Error::NoHome)
}

fn report(
    held: &Instance,
    directory: &Directory,
    accelerated: Option<bool>,
    status: reports::RunStatus,
    firmware_changed: bool,
    included: Option<std::path::PathBuf>,
) -> reports::Run {
    reports::Run {
        ssh_config: held.ssh_config,
        ssh_config_changed: included.map(|path| path.display().to_string()),
        firmware: held.firmware,
        cpu: held.cpu.clone(),
        machine: held.machine,
        disk: held.disk,
        firmware_changed,
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
        screen: directory.screen_socket().display().to_string(),
        status,
    }
}

/// Boots an instance that exists but is not running.
pub fn start(name: &str, changes: &Changes) -> Result<reports::Run> {
    let instances = Instances::discover()?;
    let directory = instances.open(name)?;
    let mut held = directory.read()?;
    if held.is_running() {
        // Settings take effect at start, so a running machine cannot change.
        if changes.any() {
            return Err(Error::ChangeWhileRunning {
                name: name.to_owned(),
            });
        }
        return Ok(report(
            &held,
            &directory,
            None,
            reports::RunStatus::AlreadyRunning,
            false,
            None,
        ));
    }
    held.forget_process();
    let firmware = held.firmware;
    if changes.ssh_config == Some(true) && !held.ssh_config {
        vm_core::ssh_config::check_name(&user_ssh_config()?, name)?;
    }
    apply(&directory, &mut held, changes)?;
    // The last port used may now be taken.
    if held.seeded && !held.ssh_port.is_some_and(port_is_free) {
        held.ssh_port = Some(free_port()?);
    }
    // The port may have moved, so the entry is written afresh.
    let included = publish(&directory, &held, instances.root())?;
    let accelerated = launch(&directory, &mut held)?;
    Ok(report(
        &held,
        &directory,
        accelerated,
        reports::RunStatus::Restarted,
        held.firmware != firmware,
        included,
    ))
}

/// What `vm start` was asked to change; `None` leaves a setting as it was.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Changes {
    pub memory: Option<u64>,
    pub cpus: Option<u32>,
    pub ports: Option<Vec<Port>>,
    pub shares: Option<Vec<Share>>,
    pub user: Option<String>,
    pub disk_size: Option<String>,
    pub firmware: Option<Firmware>,
    pub cpu: Option<String>,
    pub machine: Option<Chipset>,
    pub disk: Option<Disk>,
    /// A `$6$` hash, or `*` to take the password away.
    pub password: Option<String>,
    /// Whether plain `ssh` reaches the machine by name.
    pub ssh_config: Option<bool>,
}

impl Changes {
    pub const fn any(&self) -> bool {
        self.memory.is_some()
            || self.password.is_some()
            || self.cpus.is_some()
            || self.firmware.is_some()
            || self.cpu.is_some()
            || self.machine.is_some()
            || self.disk.is_some()
            || self.ports.is_some()
            || self.shares.is_some()
            || self.user.is_some()
            || self.disk_size.is_some()
            || self.ssh_config.is_some()
    }
}

/// Folds the changes into the record, rewriting the seed where the guest reads them.
fn apply(directory: &Directory, held: &mut Instance, changes: &Changes) -> Result<()> {
    if !changes.any() {
        return Ok(());
    }
    machine::check_disk(
        changes.machine.unwrap_or(held.machine),
        changes.disk.unwrap_or(held.disk),
    )?;
    if let Some(memory) = changes.memory {
        held.memory = memory;
    }
    if let Some(cpus) = changes.cpus {
        held.cpus = cpus;
    }
    // The variable store is kept so a return to UEFI finds its boot entries.
    if let Some(firmware) = changes.firmware {
        held.firmware = firmware;
    }
    if let Some(cpu) = &changes.cpu {
        held.cpu.clone_from(cpu);
    }
    if let Some(chipset) = changes.machine {
        held.machine = chipset;
    }
    if let Some(disk) = changes.disk {
        held.disk = disk;
    }
    let mut rewrite = false;
    let mut renew = false;
    if let Some(ports) = changes.ports.clone() {
        held.ports = ports;
        // Choose again so a published SSH forward and the one `vm ssh` uses agree.
        if held.seeded {
            held.ssh_port = held
                .ports
                .iter()
                .find(|port| port.guest == 22)
                .map(|p| p.host);
        }
    }
    if let Some(shares) = changes.shares.clone() {
        held.shares = shares;
        distinguish(&mut held.shares);
        // Shares are mounted from the seed on every boot.
        rewrite = true;
    }
    if let Some(user) = changes.user.clone() {
        // cloud-init creates accounts only on a new instance id.
        renew |= user != held.user;
        held.user = user;
    }
    if let Some(password) = &changes.password {
        if !held.seeded {
            return Err(Error::PasswordUnseeded {
                name: held.name.clone(),
            });
        }
        // cloud-init applies chpasswd only on a new instance id.
        renew = true;
        held.password = Some(password.clone());
        directory.restrict()?;
    }
    if let Some(wanted) = changes.ssh_config {
        if wanted && !held.seeded {
            return Err(Error::NoGuestAccess {
                name: held.name.clone(),
            });
        }
        held.ssh_config = wanted;
        if !wanted {
            vm_core::ssh_config::remove(directory)?;
        }
    }
    if let Some(size) = &changes.disk_size {
        hypervisor::resize_overlay(&directory.overlay(), size)?;
    }
    if (rewrite || renew) && held.seeded {
        if renew {
            held.generation = held.generation.saturating_add(1);
            // A new instance id regenerates host keys.
            let _ = std::fs::remove_file(directory.known_hosts());
        }
        let public = keys::read_public(&directory.key())?;
        instance::seed_for(held, &public).write(&directory.seed())?;
    }
    directory.write(held)
}

/// Replaces this process with `ssh`.
pub fn connect(name: &str, command: &[String]) -> Result<std::convert::Infallible> {
    let instances = Instances::discover()?;
    let directory = instances.open(name)?;
    let held = directory.read()?;
    answering(&held)?;
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
    answering(&held)?;
    let arguments = vm_core::access::scp(&held, &directory, &from, &to)?;
    Err(exec("scp", &arguments))
}

fn exec(program: &str, arguments: &[String]) -> Error {
    use std::os::unix::process::CommandExt as _;
    // Returns only on failure.
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

/// A free loopback port, chosen by the kernel.
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

/// Adds the hypervisor's log to the error it exited with.
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
    Ok(())
}

/// Waits for the monitor, then asks whether the machine is accelerated.
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

/// Whether the machine got KVM rather than emulation.
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
        if !running && (held.handle().is_some() || held.shares.iter().any(|s| s.pid.is_some())) {
            held.forget_process();
            reap_shares(&mut held);
            let _ = directory.write(&held);
        }
        if running || all {
            let state = if running {
                reports::State::Live(doing(&held))
            } else {
                reports::State::Stopped
            };
            let disk = vm_core::disk::usage(&directory.overlay());
            rows.push(reports::MachineRow::of(&held, state, disk));
        }
    }
    Ok(reports::Machines { rows, all })
}

/// How often a followed listing refreshes.
const REFRESH: Duration = Duration::from_secs(1);

/// Lists instances repeatedly, clearing the screen between listings on a terminal.
pub fn watch(all: bool, format: crate::output::Format, style: Style) -> Result<()> {
    use std::io::{IsTerminal as _, Write as _};
    let clearing = format.is_text() && std::io::stdout().is_terminal();
    loop {
        let report = list(all)?;
        if clearing {
            // Home before clearing, so the cursor starts at the top.
            let _ = write!(std::io::stdout(), "\u{1b}[H\u{1b}[2J");
        }
        if format == crate::output::Format::Yaml {
            // Separates YAML documents.
            let _ = writeln!(std::io::stdout(), "---");
        }
        // A closed output, as with `| head`, ends the stream.
        if crate::output::emit(&report, format, style).is_err() {
            return Ok(());
        }
        std::thread::sleep(REFRESH);
    }
}

/// Refuses a paused machine, which would otherwise accept a connection and hang.
fn answering(held: &Instance) -> Result<()> {
    if held.is_running() && doing(held) == "paused" {
        return Err(Error::InstancePaused {
            name: held.name.clone(),
        });
    }
    Ok(())
}

/// How long a listing waits for a machine's status.
const STATUS_WAIT: Duration = Duration::from_secs(2);

/// What a running machine reports it is doing.
fn doing(held: &Instance) -> String {
    qmp::connect_with_timeout(&held.monitor, STATUS_WAIT)
        .and_then(|mut client| client.status())
        .unwrap_or_else(|_| "running".to_owned())
}

/// Stops the guest's processors.
pub fn pause(name: &str) -> Result<reports::Switched> {
    switch(name, true)
}

/// Lets it carry on from the instruction it stopped at.
pub fn resume(name: &str) -> Result<reports::Switched> {
    switch(name, false)
}

fn switch(name: &str, pausing: bool) -> Result<reports::Switched> {
    let instances = Instances::discover()?;
    let directory = instances.open(name)?;
    let held = directory.read()?;
    if !held.is_running() {
        return Err(Error::InstanceStopped {
            name: name.to_owned(),
        });
    }
    let mut client = qmp::connect(&held.monitor)?;
    let before = client.status()?;
    // Asking for what it is already doing is the outcome that was wanted.
    if before == if pausing { "paused" } else { "running" } {
        return Ok(reports::Switched {
            name: name.to_owned(),
            state: before,
            changed: false,
        });
    }
    if pausing {
        client.pause()?;
    } else {
        client.resume()?;
    }
    Ok(reports::Switched {
        name: name.to_owned(),
        state: client.status()?,
        changed: true,
    })
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
        // A paused guest cannot act on the power button.
        let _ = client.resume();
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
        // A guest early in its boot ignores the power button.
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
    reap_shares(&mut held);
    directory.write(&held)?;
    directory.clear_runtime();
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

/// How consistent a cloned disk is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consistency {
    /// The machine was not running.
    Stopped,
    /// The guest was paused first, so no write was half-done.
    Paused,
    /// Taken from a running guest, so unflushed writes are missing.
    Running,
}

/// Clones a machine's disk into the store, pausing a running guest unless forced.
pub fn clone(name: &str, target: &str, force: bool, text: bool) -> Result<reports::Cloned> {
    let reference: vm_core::Reference = target.parse()?;
    let instances = Instances::discover()?;
    let directory = instances.open(name)?;
    let held = directory.read()?;
    let store = Store::discover()?;
    let local = vm_core::paths::local_catalogue_directory().ok_or(Error::NoImageStore)?;

    let running = held.is_running();
    let consistency = match (running, force) {
        (false, _) => Consistency::Stopped,
        (true, false) => Consistency::Paused,
        (true, true) => Consistency::Running,
    };

    // Both passes are named so a restarting bar does not read as a stall.
    let mut bar = progress::Bar::new("", text);
    let cloned = if consistency == Consistency::Paused {
        let mut client = qmp::connect(&held.monitor)?;
        // A machine the user paused stays paused.
        let was_running = client.status()? == "running";
        client.pause()?;
        // Resume whether or not the clone succeeds.
        let outcome = vm_core::clone::image(
            &store,
            &local,
            &held,
            &directory.overlay(),
            &reference,
            true,
            Some(&mut |stage, percent| {
                bar.naming(&format!("  {:<10}", vm_core::clone::Stage::label(stage)));
                bar.portion(percent);
            }),
        );
        bar.clear();
        if was_running {
            client.resume()?;
        }
        outcome?
    } else {
        let outcome = vm_core::clone::image(
            &store,
            &local,
            &held,
            &directory.overlay(),
            &reference,
            running,
            Some(&mut |stage, percent| {
                bar.naming(&format!("  {:<10}", vm_core::clone::Stage::label(stage)));
                bar.portion(percent);
            }),
        );
        bar.clear();
        outcome?
    };

    Ok(reports::Cloned {
        source: name.to_owned(),
        name: cloned.name,
        tag: cloned.tag,
        arch: cloned.arch,
        digest: cloned.digest.to_string(),
        size: cloned.size,
        consistency,
    })
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

/// Removes an image name from the store, and its file with the last name for it.
pub fn remove_image(
    catalogue: &Catalogue,
    store: &Store,
    reference: &str,
    force: bool,
) -> Result<reports::Untagged> {
    let parsed: vm_core::Reference = reference.parse()?;
    let (entry, artifact) = catalogue.resolve(&parsed, host_architecture())?;
    // A clone's entry was written here, so it is removed with the image.
    let local = artifact.url.is_none();
    if !store.contains(&artifact.digest) && !local {
        // Nothing to remove, and the name would remain.
        return Err(Error::UnheldImage {
            reference: reference.to_owned(),
        });
    }
    let digest = artifact.digest.to_string();
    let users = holders(&digest)?;
    if !users.is_empty() && !force {
        return Err(Error::ImageInUse {
            reference: reference.to_owned(),
            instances: users,
        });
    }
    let kept_by = stranded(catalogue, entry, artifact);
    let size = if kept_by.is_empty() {
        store.discard(&artifact.digest)?
    } else {
        0
    };
    store.forget(&entry.name, &entry.tag, &artifact.arch);
    if local && let Some(root) = vm_core::paths::local_catalogue_directory() {
        let directory = root.join(&entry.name);
        let _ = std::fs::remove_file(directory.join(format!("{}.toml", entry.tag)));
        let _ = std::fs::remove_dir(&directory);
    }
    Ok(reports::Untagged {
        name: entry.name.clone(),
        tag: entry.tag.clone(),
        arch: artifact.arch.clone(),
        digest,
        size,
        forgotten: local,
        broke: users,
        kept_by,
    })
}

/// Other names for the same file that could not be fetched again.
fn stranded(
    catalogue: &Catalogue,
    entry: &vm_core::catalogue::Entry,
    artifact: &vm_core::catalogue::Artifact,
) -> Vec<String> {
    catalogue
        .entries()
        .into_iter()
        .filter(|other| other.name != entry.name || other.tag != entry.tag)
        .filter(|other| {
            other
                .artifacts
                .iter()
                .any(|held| held.digest == artifact.digest && held.url.is_none())
        })
        .map(|other| format!("{}:{}", other.name, other.tag))
        .collect()
}

/// The machines whose disk is backed by a given image.
fn holders(digest: &str) -> Result<Vec<String>> {
    let instances = Instances::discover()?;
    let mut names = Vec::new();
    for name in instances.names()? {
        let Ok(directory) = instances.open(&name) else {
            continue;
        };
        if directory.read().is_ok_and(|held| held.digest == digest) {
            names.push(name);
        }
    }
    Ok(names)
}

/// The guest's console, as far as it has been written.
pub fn logs(name: &str, lines: Option<usize>) -> Result<reports::Console> {
    let instances = Instances::discover()?;
    let directory = instances.open(name)?;
    let console = directory.console();
    let text = match std::fs::read(&console) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        // A machine that has never started has no console.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(Error::State {
                path: console,
                action: "read the console log",
                source,
            });
        }
    };
    Ok(reports::Console {
        name: name.to_owned(),
        lines: tail(&text, lines),
    })
}

/// The last few lines, or all of them when no number was asked for.
fn tail(text: &str, lines: Option<usize>) -> Vec<String> {
    let all: Vec<&str> = text.lines().collect();
    let from = lines.map_or(0, |wanted| all.len().saturating_sub(wanted));
    all[from..].iter().map(|line| (*line).to_owned()).collect()
}

/// Writes the console as it grows, starting with what is already there, until the machine stops.
pub fn follow(name: &str, from: Option<usize>) -> Result<()> {
    use std::io::{Read as _, Write as _};
    let instances = Instances::discover()?;
    let directory = instances.open(name)?;
    let console = directory.console();
    let mut out = std::io::stdout().lock();
    let mut file = match std::fs::File::open(&console) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(source) => {
            return Err(Error::State {
                path: console,
                action: "read the console log",
                source,
            });
        }
    };
    // Reading to the end leaves the handle where the next write lands.
    let mut buffer = Vec::new();
    if file.read_to_end(&mut buffer).is_err() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&buffer);
    for line in tail(&text, from) {
        if writeln!(out, "{line}").is_err() {
            return Ok(());
        }
    }
    loop {
        let running = directory.read().is_ok_and(|held| held.is_running());
        buffer.clear();
        if file.read_to_end(&mut buffer).is_err() {
            return Ok(());
        }
        if buffer.is_empty() {
            // Nothing new, and if nothing is running there will be no more.
            if !running {
                let _ = out.flush();
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        if out.write_all(&buffer).is_err() || out.flush().is_err() {
            return Ok(());
        }
    }
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

/// Parses `/host/path:/guest/path`, tagging the share with the host directory's name.
pub fn parse_share(text: &str) -> std::result::Result<Share, String> {
    let (source, target) = text
        .rsplit_once(':')
        .ok_or_else(|| format!("'{text}' is not a share; write it as host:guest"))?;
    if source.is_empty() || target.is_empty() {
        return Err(format!("'{text}' is not a share; write it as host:guest"));
    }
    if !target.starts_with('/') {
        return Err(format!("'{target}' is not an absolute path in the guest"));
    }
    let source = std::path::PathBuf::from(source);
    let source = source
        .canonicalize()
        .map_err(|error| format!("{}: {error}", source.display()))?;
    if !source.is_dir() {
        return Err(format!("{} is not a directory", source.display()));
    }
    Ok(Share {
        tag: tag_for(&source),
        source,
        target: target.to_owned(),
        pid: None,
        started: None,
    })
}

/// A virtiofs tag of at most 36 bytes, in characters the seed allows.
fn tag_for(source: &Path) -> String {
    let stem: String = source
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .take(30)
        .collect();
    if stem.is_empty() {
        "share".to_owned()
    } else {
        stem
    }
}

/// Makes every tag its own, since two directories may share a name.
fn distinguish(shares: &mut [Share]) {
    for index in 1..shares.len() {
        let (earlier, rest) = shares.split_at_mut(index);
        let share = &mut rest[0];
        if earlier.iter().any(|held| held.tag == share.tag) {
            share.tag = format!("{}{index}", share.tag);
        }
    }
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

pub fn parse_firmware(text: &str) -> std::result::Result<Firmware, String> {
    text.parse().map_err(|error: Error| error.to_string())
}

pub fn parse_machine(text: &str) -> std::result::Result<Chipset, String> {
    text.parse().map_err(|error: Error| error.to_string())
}

pub fn parse_disk(text: &str) -> std::result::Result<Disk, String> {
    text.parse().map_err(|error: Error| error.to_string())
}

pub fn parse_cpu(text: &str) -> std::result::Result<String, String> {
    machine::check_cpu(text)
        .map(|()| text.to_owned())
        .map_err(|error| error.to_string())
}

pub fn parse_user(text: &str) -> std::result::Result<String, String> {
    let trial = seed::Seed {
        instance_id: "check".to_owned(),
        hostname: "check".to_owned(),
        user: text.to_owned(),
        authorized_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKq7YQ== check".to_owned(),
        mounts: Vec::new(),
        password: None,
    };
    trial
        .image()
        .map(|_| text.to_owned())
        .map_err(|_| format!("'{text}' is not a usable user name"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    use vm_core::instance::Instances;
    use vm_core::seed;

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("vm-machines-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        /// A machine with a key, as `vm run` leaves one.
        fn machine(&self, name: &str) -> (Directory, Instance) {
            let instances = Instances::at(self.0.join("instances"), self.0.join("run"));
            let directory = instances.create(name).unwrap();
            let public = keys::generate(&directory.key(), "vm-test").unwrap();
            let held = Instance {
                name: name.to_owned(),
                image: "debian:trixie".to_owned(),
                digest: "sha512:abc".to_owned(),
                arch: "amd64".to_owned(),
                created: 1_700_000_000,
                memory: 2048,
                cpus: 2,
                firmware: vm_core::machine::Firmware::Bios,
                cpu: "max".to_owned(),
                machine: vm_core::machine::Chipset::Q35,
                disk: vm_core::machine::Disk::Virtio,
                user: "vm".to_owned(),
                seeded: true,
                monitor: directory.monitor().to_owned(),
                ssh_port: Some(2222),
                pid: None,
                started: None,
                generation: 0,
                ssh_config: false,
                password: None,
                ports: Vec::new(),
                shares: Vec::new(),
            };
            instance::seed_for(&held, &public)
                .write(&directory.seed())
                .unwrap();
            directory.write(&held).unwrap();
            std::fs::write(
                directory.known_hosts(),
                b"[127.0.0.1]:2222 ssh-ed25519 AAAA\n",
            )
            .unwrap();
            (directory, held)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn share_at(target: &str) -> Share {
        Share {
            tag: "data".to_owned(),
            source: std::path::PathBuf::from("/tmp"),
            target: target.to_owned(),
            pid: None,
            started: None,
        }
    }

    #[test]
    fn nothing_asked_for_is_nothing_changed() {
        let scratch = Scratch::new("nochange");
        let (directory, mut held) = scratch.machine("one");
        let before = held.clone();
        apply(&directory, &mut held, &Changes::default()).unwrap();
        assert_eq!(held, before);
    }

    #[test]
    fn the_size_of_a_machine_is_what_was_asked_for() {
        let scratch = Scratch::new("size");
        let (directory, mut held) = scratch.machine("one");
        let changes = Changes {
            memory: Some(4096),
            cpus: Some(8),
            ..Changes::default()
        };
        apply(&directory, &mut held, &changes).unwrap();
        assert_eq!((held.memory, held.cpus), (4096, 8));
        // The change is persisted.
        assert_eq!(directory.read().unwrap().memory, 4096);
    }

    #[test]
    fn publishing_the_guests_ssh_port_takes_over_the_one_it_had() {
        let scratch = Scratch::new("ssh");
        let (directory, mut held) = scratch.machine("one");
        let changes = Changes {
            ports: Some(vec![Port {
                host: 2022,
                guest: 22,
            }]),
            ..Changes::default()
        };
        apply(&directory, &mut held, &changes).unwrap();
        assert_eq!(held.ssh_port, Some(2022));
    }

    #[test]
    fn clearing_the_forwards_leaves_nothing_forwarded() {
        let scratch = Scratch::new("noports");
        let (directory, mut held) = scratch.machine("one");
        held.ports.push(Port {
            host: 8080,
            guest: 80,
        });
        let changes = Changes {
            ports: Some(Vec::new()),
            ..Changes::default()
        };
        apply(&directory, &mut held, &changes).unwrap();
        assert!(held.ports.is_empty());
    }

    #[test]
    fn shares_given_again_replace_the_ones_it_had() {
        let scratch = Scratch::new("shares");
        let (directory, mut held) = scratch.machine("one");
        held.shares.push(share_at("/mnt/old"));
        let changes = Changes {
            shares: Some(vec![share_at("/mnt/new"), share_at("/mnt/other")]),
            ..Changes::default()
        };
        apply(&directory, &mut held, &changes).unwrap();
        assert_eq!(held.shares.len(), 2);
        assert_eq!(held.shares[0].target, "/mnt/new");
        // Two directories named the same still need distinct tags.
        assert_ne!(held.shares[0].tag, held.shares[1].tag);
    }

    #[test]
    fn changing_the_shares_does_not_make_the_guest_a_new_machine() {
        let scratch = Scratch::new("samehost");
        let (directory, mut held) = scratch.machine("one");
        let changes = Changes {
            shares: Some(vec![share_at("/mnt/new")]),
            ..Changes::default()
        };
        apply(&directory, &mut held, &changes).unwrap();
        assert_eq!(held.generation, 0);
        assert!(
            directory.known_hosts().exists(),
            "the remembered host key was thrown away for nothing"
        );
        let seed = std::fs::read(directory.seed()).unwrap();
        assert!(
            String::from_utf8_lossy(&seed).contains("/mnt/new"),
            "the seed was not rewritten"
        );
    }

    #[test]
    fn changing_the_account_makes_the_guest_a_new_machine() {
        let scratch = Scratch::new("newuser");
        let (directory, mut held) = scratch.machine("one");
        let changes = Changes {
            user: Some("pilot".to_owned()),
            ..Changes::default()
        };
        apply(&directory, &mut held, &changes).unwrap();
        assert_eq!(held.generation, 1);
        assert!(
            !directory.known_hosts().exists(),
            "the old host key would look like an impostor"
        );
    }

    const HASH: &str = "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1";

    /// cloud-init applies a password once per instance id, so a new one needs a new id.
    #[test]
    fn a_new_password_is_recorded_seeded_and_acted_on() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = Scratch::new("password");
        let (directory, mut held) = scratch.machine("one");
        let changes = Changes {
            password: Some(HASH.to_owned()),
            ..Changes::default()
        };
        apply(&directory, &mut held, &changes).unwrap();
        assert_eq!(held.generation, 1);
        assert_eq!(directory.read().unwrap().password.as_deref(), Some(HASH));
        let seed = String::from_utf8_lossy(&std::fs::read(directory.seed()).unwrap()).into_owned();
        assert!(seed.contains("chpasswd:"), "{seed}");
        assert!(seed.contains(HASH), "{seed}");
        let mode = std::fs::metadata(directory.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    }

    #[test]
    fn taking_the_password_away_seeds_the_disabled_marker() {
        let scratch = Scratch::new("nopassword");
        let (directory, mut held) = scratch.machine("one");
        held.password = Some(HASH.to_owned());
        let changes = Changes {
            password: Some("*".to_owned()),
            ..Changes::default()
        };
        apply(&directory, &mut held, &changes).unwrap();
        assert_eq!(held.password.as_deref(), Some("*"));
        let seed = String::from_utf8_lossy(&std::fs::read(directory.seed()).unwrap()).into_owned();
        assert!(!seed.contains(HASH), "{seed}");
        assert!(seed.contains("password: \"*\""), "{seed}");
    }

    #[test]
    fn a_password_for_a_machine_with_no_seed_is_refused_and_nothing_changes() {
        let scratch = Scratch::new("unseededpassword");
        let (directory, mut held) = scratch.machine("one");
        held.seeded = false;
        directory.write(&held).unwrap();
        let changes = Changes {
            memory: Some(8192),
            password: Some(HASH.to_owned()),
            ..Changes::default()
        };
        let error = apply(&directory, &mut held, &changes).unwrap_err();
        assert_eq!(error.kind(), "password-needs-seed");
        let stored = directory.read().unwrap();
        assert_eq!(stored.password, None);
        assert_eq!(stored.memory, 2048);
    }

    #[test]
    fn an_ssh_configuration_entry_is_asked_for_and_taken_away() {
        let scratch = Scratch::new("sshconfig");
        let (directory, mut held) = scratch.machine("one");
        let before = held.generation;
        let on = Changes {
            ssh_config: Some(true),
            ..Changes::default()
        };
        apply(&directory, &mut held, &on).unwrap();
        assert!(directory.read().unwrap().ssh_config);
        // The guest reads nothing of this, so it is not told it is new.
        assert_eq!(held.generation, before);
        vm_core::ssh_config::write(&held, &directory).unwrap();
        assert!(directory.ssh_config().exists());
        let off = Changes {
            ssh_config: Some(false),
            ..Changes::default()
        };
        apply(&directory, &mut held, &off).unwrap();
        assert!(!directory.read().unwrap().ssh_config);
        assert!(!directory.ssh_config().exists());
    }

    #[test]
    fn an_ssh_configuration_entry_for_a_machine_with_no_seed_is_refused() {
        let scratch = Scratch::new("unseededsshconfig");
        let (directory, mut held) = scratch.machine("one");
        held.seeded = false;
        directory.write(&held).unwrap();
        let changes = Changes {
            ssh_config: Some(true),
            ..Changes::default()
        };
        let error = apply(&directory, &mut held, &changes).unwrap_err();
        assert_eq!(error.kind(), "no-guest-access");
        assert!(!directory.read().unwrap().ssh_config);
        // Taking away what is not there is not an error.
        let changes = Changes {
            ssh_config: Some(false),
            ..Changes::default()
        };
        apply(&directory, &mut held, &changes).unwrap();
    }

    #[test]
    fn an_ssh_configuration_change_counts_as_a_change() {
        let changes = Changes {
            ssh_config: Some(false),
            ..Changes::default()
        };
        assert!(changes.any());
    }

    /// Firmware and processor are the hypervisor's, so the guest is not told it is new.
    #[test]
    fn a_machine_change_is_recorded_without_renewing_the_guest() {
        let scratch = Scratch::new("machinechange");
        let (directory, mut held) = scratch.machine("one");
        std::fs::write(directory.firmware_variables(), b"boot entries").unwrap();
        let changes = Changes {
            firmware: Some(Firmware::Bios),
            cpu: Some("Penryn,+avx".to_owned()),
            ..Changes::default()
        };
        held.firmware = Firmware::Uefi;
        apply(&directory, &mut held, &changes).unwrap();
        let stored = directory.read().unwrap();
        assert_eq!(stored.firmware, Firmware::Bios);
        assert_eq!(stored.cpu, "Penryn,+avx");
        assert_eq!(stored.generation, 0);
        assert_eq!(
            std::fs::read(directory.firmware_variables()).unwrap(),
            b"boot entries",
            "a return to UEFI would lose its boot entries"
        );
    }

    #[test]
    fn a_chipset_and_disk_change_is_recorded() {
        let scratch = Scratch::new("chipsetchange");
        let (directory, mut held) = scratch.machine("one");
        let changes = Changes {
            machine: Some(Chipset::Pc),
            disk: Some(Disk::Ide),
            ..Changes::default()
        };
        apply(&directory, &mut held, &changes).unwrap();
        let stored = directory.read().unwrap();
        assert_eq!((stored.machine, stored.disk), (Chipset::Pc, Disk::Ide));
        assert_eq!(stored.generation, 0);
    }

    #[test]
    fn a_disk_controller_the_resulting_chipset_lacks_is_refused_and_nothing_changes() {
        let scratch = Scratch::new("chipsetmismatch");
        let (directory, mut held) = scratch.machine("one");
        let changes = Changes {
            memory: Some(8192),
            disk: Some(Disk::Ide),
            ..Changes::default()
        };
        let error = apply(&directory, &mut held, &changes).unwrap_err();
        assert_eq!(error.kind(), "machine-mismatch");
        let stored = directory.read().unwrap();
        assert_eq!((stored.disk, stored.memory), (Disk::Virtio, 2048));
    }

    #[test]
    fn changing_the_chipset_and_disk_together_is_checked_as_a_pair() {
        let scratch = Scratch::new("chipsetboth");
        let (directory, mut held) = scratch.machine("one");
        held.machine = Chipset::Pc;
        held.disk = Disk::Ide;
        let changes = Changes {
            machine: Some(Chipset::Q35),
            disk: Some(Disk::Sata),
            ..Changes::default()
        };
        apply(&directory, &mut held, &changes).unwrap();
        assert_eq!((held.machine, held.disk), (Chipset::Q35, Disk::Sata));
    }

    #[test]
    fn the_account_it_already_has_is_not_a_change() {
        let scratch = Scratch::new("sameuser");
        let (directory, mut held) = scratch.machine("one");
        let changes = Changes {
            user: Some("vm".to_owned()),
            ..Changes::default()
        };
        apply(&directory, &mut held, &changes).unwrap();
        assert_eq!(held.generation, 0);
        assert!(directory.known_hosts().exists());
    }

    #[test]
    fn a_machine_that_takes_no_seed_is_changed_without_one() {
        let scratch = Scratch::new("unseeded");
        let (directory, mut held) = scratch.machine("one");
        held.seeded = false;
        let _ = std::fs::remove_file(directory.seed());
        let changes = Changes {
            shares: Some(vec![share_at("/mnt/new")]),
            ..Changes::default()
        };
        apply(&directory, &mut held, &changes).unwrap();
        assert!(!directory.seed().exists());
    }

    #[test]
    fn a_console_is_shown_whole_or_by_its_last_lines() {
        let text = "one\ntwo\nthree\n";
        assert_eq!(tail(text, None), ["one", "two", "three"]);
        assert_eq!(tail(text, Some(2)), ["two", "three"]);
        assert_eq!(tail(text, Some(9)), ["one", "two", "three"]);
        assert!(tail("", Some(5)).is_empty());
        assert_eq!(tail(text, Some(0)).len(), 0);
    }

    #[test]
    fn a_console_that_was_never_written_is_empty() {
        let seed = seed::Seed {
            instance_id: "x".to_owned(),
            hostname: "x".to_owned(),
            user: "vm".to_owned(),
            authorized_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKq7YQ== vm".to_owned(),
            mounts: Vec::new(),
            password: None,
        };
        assert!(seed.image().is_ok());
        assert!(tail("", None).is_empty());
    }

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
    fn a_share_takes_its_tag_from_the_host_directory() {
        let share = parse_share(&format!("{}:/mnt/tmp", std::env::temp_dir().display())).unwrap();
        assert_eq!(share.target, "/mnt/tmp");
        assert_eq!(share.tag, "tmp");
    }

    #[test]
    fn a_share_that_is_not_one_is_refused() {
        assert!(parse_share("/tmp").is_err());
        assert!(parse_share("/tmp:").is_err());
        assert!(parse_share(":/mnt").is_err());
        assert!(
            parse_share("/tmp:mnt").is_err(),
            "the guest path must be absolute"
        );
        assert!(parse_share("/nonexistent/directory:/mnt").is_err());
    }

    #[test]
    fn a_share_of_something_that_is_not_a_directory_is_refused() {
        let mut path = std::env::temp_dir();
        path.push(format!("vm-share-{}.txt", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        let outcome = parse_share(&format!("{}:/mnt/x", path.display()));
        let _ = std::fs::remove_file(&path);
        assert!(outcome.is_err());
    }

    fn share(tag: &str) -> Share {
        Share {
            tag: tag.to_owned(),
            source: std::path::PathBuf::from("/home/x"),
            target: "/mnt/x".to_owned(),
            pid: None,
            started: None,
        }
    }

    #[test]
    fn tags_that_would_collide_are_made_distinct() {
        let mut shares = vec![share("work"), share("work"), share("src"), share("work")];
        distinguish(&mut shares);
        let tags: Vec<&str> = shares.iter().map(|held| held.tag.as_str()).collect();
        assert_eq!(tags, ["work", "work1", "src", "work3"]);
        let mut sorted = tags.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), tags.len(), "every tag must be its own");
    }

    #[test]
    fn a_single_share_keeps_its_plain_tag() {
        let mut shares = vec![share("work")];
        distinguish(&mut shares);
        assert_eq!(shares[0].tag, "work");
    }

    #[test]
    fn a_user_name_the_guest_would_refuse_is_refused_here() {
        assert_eq!(parse_user("vm"), Ok("vm".to_owned()));
        assert!(parse_user("Capitals").is_err());
        assert!(parse_user("has space").is_err());
        assert!(parse_user("").is_err());
    }
}

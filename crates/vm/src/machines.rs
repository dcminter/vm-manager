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
use vm_core::instance::{Directory, Instance, Instances, Port, Share};
use vm_core::machine::{self, Chipset, Disk, Firmware};
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
    pub shares: Vec<Share>,
    pub disk_size: Option<String>,
    pub pull: Option<Pull>,
    pub firmware: Option<Firmware>,
    pub cpu: Option<String>,
    pub machine: Option<Chipset>,
    pub disk: Option<Disk>,
    /// A `$6$` hash for console login.
    pub password: Option<String>,
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
        password: request.password.clone(),
        ports: request.ports.clone(),
        shares: request.shares.clone(),
    };
    distinguish(&mut held.shares);

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
        &artifact.format,
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
        false,
    ))
}

/// Starts the hypervisor and waits for it to answer.
///
/// A machine that dies on its arguments dies within a moment of starting, so
/// waiting for its monitor is what separates "started" from "reported as
/// started". What it says about acceleration comes free with the wait.
fn launch(directory: &Directory, held: &mut Instance) -> Result<Option<bool>> {
    prepare_runtime(directory.monitor())?;
    if held.firmware == Firmware::Uefi {
        machine::prepare_uefi(&held.arch, &directory.firmware_variables())?;
    }
    // Sockets left behind by a machine that is gone would refuse the new one.
    directory.clear_runtime();
    // The hypervisor connects to these sockets, so something has to be
    // listening on them before it starts.
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
        // The hypervisor refuses a socket that is not there yet, and the
        // server takes a moment to create it.
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

/// Ends every share server an instance owns. A server left behind holds its
/// socket and its share open against a machine that is no longer there.
fn reap_shares(held: &mut Instance) {
    for share in &mut held.shares {
        if let Some(handle) = share.handle() {
            let _ = process::signal(&handle, process::Signal::Terminate);
        }
        share.forget_process();
    }
}

fn report(
    held: &Instance,
    directory: &Directory,
    accelerated: Option<bool>,
    status: reports::RunStatus,
    firmware_changed: bool,
) -> reports::Run {
    reports::Run {
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
        // Every one of these is read when the hypervisor starts, so a machine
        // that is already running would report a change it had not made.
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
        ));
    }
    held.forget_process();
    let firmware = held.firmware;
    apply(&directory, &mut held, changes)?;
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
        held.firmware != firmware,
    ))
}

/// What `vm start` was asked to change. Absent means "as it was", which is not
/// the same as an empty list: clearing the ports is asked for explicitly.
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
    }
}

/// Folds the changes into the record before it is started again.
///
/// The seed is rewritten only for what the guest reads from it. Memory,
/// processors and forwards are the hypervisor's business and the guest never
/// learns of them, but a share it should mount and an account it should create
/// are cloud-init's, and it acts on those once per instance id.
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
    // The variable store is kept across a switch to BIOS so a return to UEFI finds its boot entries.
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
    // Two different things: the seed's contents changing, and the guest being
    // told it is a machine it has not seen before.
    let mut rewrite = false;
    let mut renew = false;
    if let Some(ports) = changes.ports.clone() {
        held.ports = ports;
        // The forward to SSH was chosen against the old list; choosing again
        // is what keeps a published forward and the one `vm ssh` uses the same.
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
        // Shares are mounted from the seed on every boot, so a new list is
        // acted on without the guest having to think itself new.
        rewrite = true;
    }
    if let Some(user) = changes.user.clone() {
        // An account is made once, and only for a machine cloud-init has not
        // met. The one it already has is left where it is.
        renew |= user != held.user;
        held.user = user;
    }
    if let Some(password) = &changes.password {
        if !held.seeded {
            return Err(Error::PasswordUnseeded {
                name: held.name.clone(),
            });
        }
        // cloud-init applies chpasswd once per instance id, so the guest must think itself new.
        renew = true;
        held.password = Some(password.clone());
        directory.restrict()?;
    }
    if let Some(size) = &changes.disk_size {
        hypervisor::resize_overlay(&directory.overlay(), size)?;
    }
    if (rewrite || renew) && held.seeded {
        if renew {
            held.generation = held.generation.saturating_add(1);
            // A machine cloud-init thinks is new gets fresh host keys, so the
            // remembered one would look like an impostor.
            let _ = std::fs::remove_file(directory.known_hosts());
        }
        let public = keys::read_public(&directory.key())?;
        instance::seed_for(held, &public).write(&directory.seed())?;
    }
    directory.write(held)
}

/// Replaces this process with `ssh`, so that the terminal, the signals and the
/// exit status are the guest's rather than filtered through ours.
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

/// How often a followed listing is taken again. A machine's memory and disk
/// move slowly enough that a second is already generous.
const REFRESH: Duration = Duration::from_secs(1);

/// Lists instances over and over until the reader has had enough.
///
/// On a terminal each pass clears the screen, so what is there stays in one
/// place and can be read as a display. Redirected, and in the document
/// formats, one listing simply follows another, because a reader that is
/// piping this somewhere wants all of them rather than the latest.
pub fn watch(all: bool, format: crate::output::Format, style: Style) -> Result<()> {
    use std::io::{IsTerminal as _, Write as _};
    let clearing = format.is_text() && std::io::stdout().is_terminal();
    loop {
        let report = list(all)?;
        if clearing {
            // Home, then clear: clearing first leaves the cursor wherever the
            // last listing left it.
            let _ = write!(std::io::stdout(), "\u{1b}[H\u{1b}[2J");
        }
        if format == crate::output::Format::Yaml {
            // Without it, one listing's items run on from the last one's and
            // the stream reads as a single ever-growing list. JSON needs no
            // equivalent: its values are self-delimiting.
            let _ = writeln!(std::io::stdout(), "---");
        }
        // A sink that has gone is the end of the stream rather than a failure
        // to report: `vm ps --follow | head` ends this way by design.
        if crate::output::emit(&report, format, style).is_err() {
            return Ok(());
        }
        std::thread::sleep(REFRESH);
    }
}

/// Refuses a machine that cannot answer.
///
/// The forward to a paused guest is still bound, so connecting to one succeeds
/// and then waits for a guest that will never reply. Saying so costs a round
/// trip on a local socket, which is worth it not to hang.
fn answering(held: &Instance) -> Result<()> {
    if held.is_running() && doing(held) == "paused" {
        return Err(Error::InstancePaused {
            name: held.name.clone(),
        });
    }
    Ok(())
}

/// How long to wait for a machine to say what it is doing. A listing should
/// not stall on one wedged machine, and the record already says it is running.
const STATUS_WAIT: Duration = Duration::from_secs(2);

/// What a running machine says it is doing.
///
/// Only the machine knows whether it is paused, and a machine that cannot
/// answer is reported as what looking for its process already established.
fn doing(held: &Instance) -> String {
    qmp::connect_with_timeout(&held.monitor, STATUS_WAIT)
        .and_then(|mut client| client.status())
        .unwrap_or_else(|_| "running".to_owned())
}

/// Stops the guest's processors without the guest knowing.
///
/// The machine, its memory and everything it holds open stay where they are;
/// nothing of it is written anywhere, so this outlives neither a host reboot
/// nor `vm kill`.
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
        // A paused guest cannot act on the power button, and waiting for it to
        // would only end in the same kill by a longer road.
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

/// How consistent a cloned disk is, which depends on what could be done
/// about the guest at the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consistency {
    /// The machine was not running. Nothing was in flight.
    Stopped,
    /// The guest was paused first, so no write was half-done.
    Paused,
    /// Taken from under a running guest. Whatever was in its page cache and
    /// not yet on disk is simply absent.
    Running,
}

/// Clones a machine's disk into the store as a new image.
///
/// A running guest is paused for the duration unless the user insists
/// otherwise, because a disk taken from under one is crash-consistent at best
/// and there is no guest agent here to freeze its filesystems properly.
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

    // A clone reads the whole backing chain and then reads the result back to
    // hash it, either of which on a large image is long enough that a silent
    // tool looks like a wedged one. Both passes are named, because a bar that
    // reaches the end and starts again otherwise reads as a stall.
    let mut bar = progress::Bar::new("", text);
    let cloned = if consistency == Consistency::Paused {
        let mut client = qmp::connect(&held.monitor)?;
        // A machine the user paused is left paused; only one paused here is
        // let go again.
        let was_running = client.status()? == "running";
        client.pause()?;
        // The guest stays paused until this returns, so resuming has to happen
        // on the way out whether the clone worked or not.
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

/// Removes an image from the store.
///
/// What is removed is the name. A reference from the fetched catalogue survives
/// and simply shows as unheld again; one made here by `vm clone` has nowhere
/// to be fetched from, so its entry goes with it rather than naming an image
/// nothing could ever produce.
///
/// The bytes go with the last name for them. Images are addressed by digest, so
/// two clones of an unchanged disk are one file under two names, and removing
/// the file out from under the other one would strand an image that cannot be
/// fetched back.
pub fn remove_image(
    catalogue: &Catalogue,
    store: &Store,
    reference: &str,
    force: bool,
) -> Result<reports::Untagged> {
    let parsed: vm_core::Reference = reference.parse()?;
    let (entry, artifact) = catalogue.resolve(&parsed, host_architecture())?;
    // An entry with nowhere to fetch from was written here, so it is ours to
    // take away; one the catalogue provides would come back on the next update.
    let local = artifact.url.is_none();
    if !store.contains(&artifact.digest) && !local {
        // There is nothing to remove and the name will still be there
        // afterwards, so this would do nothing at all.
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

/// Other names for the same bytes that could not get them back.
///
/// Only an image made here is at risk: one the catalogue provides is listed as
/// unfetched and pulled again, which is the point of removing it. The machines
/// built on an image are a separate question, answered by `holders`, and one
/// the user can overrule; this one they cannot, because nothing could undo it.
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
        // A machine that has never started has no console, which is not a
        // failure to report; there is simply nothing to show.
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

/// Writes the console out as it is written, until the machine stops.
///
/// The whole file comes first, so following a machine that has already booted
/// shows what it said on the way. Text only: a document cannot be emitted a
/// line at a time and still be a document.
pub fn follow(name: &str, from: Option<usize>) -> Result<()> {
    use std::io::{Read as _, Write as _};
    let instances = Instances::discover()?;
    let directory = instances.open(name)?;
    let console = directory.console();
    let mut out = std::io::stdout().lock();
    let mut file = match std::fs::File::open(&console) {
        Ok(file) => file,
        // Nothing has been written yet, and waiting for a file that may never
        // appear is worse than saying so.
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
    // The whole file first, so following a machine that has already booted
    // shows what it said on the way. Reading it leaves the handle where the
    // next write will land, so nothing between the two is missed.
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

/// Turns `/host/path:/guest/path` into a share, with a tag taken from the
/// host directory's own name so that the guest's fstab reads as something.
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

/// A tag the guest will accept: at most 36 bytes by virtiofs convention, and
/// only the characters the seed's own rules allow.
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

/// The seed's own rules apply to the user name, so they are checked here
/// rather than at the point where the machine is about to boot.
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
        // And it survives, because the next start reads the record.
        assert_eq!(directory.read().unwrap().memory, 4096);
    }

    /// A published forward to the guest's SSH port is the one `vm ssh` uses,
    /// so replacing the forwards has to be able to change it.
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

    /// Clearing the forwards leaves the machine reachable: `vm start` picks a
    /// port for it, as it does for a machine that published nothing.
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

    /// Shares are mounted from the seed on every boot, so the guest acts on a
    /// new list without being told it is a machine it has never seen.
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

    /// An account is created once, and only for a machine cloud-init has not
    /// met, so a new one means a new instance id and fresh host keys with it.
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

    /// Checked against what the machine will be once the change is made, not against either half alone.
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

    /// An image that takes no seed has nothing to rewrite, and asking for a
    /// share on one must not try.
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

    /// A machine that has never started has no console, which is nothing to
    /// show rather than something to complain about.
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

    /// A path is easier to get wrong than to get right, and a share that
    /// silently does not appear in the guest is a bad way to learn that.
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

    /// Two directories can have the same name, and two shares cannot have the
    /// same tag: the guest would mount one of them twice.
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

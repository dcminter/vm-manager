use std::fmt;
use std::path::PathBuf;

#[derive(Debug)]
pub enum Error {
    Reference {
        input: String,
        reason: &'static str,
    },
    CatalogueRead {
        path: PathBuf,
        source: std::io::Error,
    },
    CatalogueParse {
        path: PathBuf,
        source: basic_toml::Error,
    },
    Config {
        path: PathBuf,
        reason: String,
    },
    UnknownCatalogue {
        name: String,
        available: Vec<String>,
    },
    UpdateFailed {
        failed: Vec<String>,
    },
    CatalogueEntry {
        path: PathBuf,
        reason: String,
    },
    UnknownImage {
        reference: String,
    },
    UnsupportedArchitecture {
        reference: String,
        wanted: String,
        available: Vec<String>,
    },
    MissingTool {
        binary: &'static str,
        package: &'static str,
        operation: &'static str,
    },
    NoImageStore,
    Store {
        path: PathBuf,
        action: &'static str,
        source: std::io::Error,
    },
    Download {
        url: String,
        source: Box<ureq::Error>,
    },
    HttpStatus {
        url: String,
        status: u16,
    },
    PinMismatch {
        reference: String,
        arch: String,
        actual: String,
    },
    DigestMismatch {
        url: String,
        expected: String,
        actual: String,
    },
    MalformedArchive {
        url: String,
        reason: String,
    },
    MalformedPointer {
        url: String,
        reason: String,
    },
    EmptyCatalogue {
        url: String,
        path: String,
    },
    SeedRefused {
        reason: String,
    },
    SeedWrite {
        path: PathBuf,
        source: std::io::Error,
    },
    NoStateDirectory,
    State {
        path: PathBuf,
        action: &'static str,
        source: std::io::Error,
    },
    InstanceExists {
        name: String,
    },
    UnknownInstance {
        name: String,
    },
    InstanceName {
        name: String,
        reason: &'static str,
    },
    InstanceRecord {
        path: PathBuf,
        reason: String,
    },
    Launch {
        program: String,
        source: std::io::Error,
    },
    Overlay {
        path: PathBuf,
        reason: String,
    },
    KeyGeneration {
        path: PathBuf,
        reason: String,
    },
    NotFetchable,
    ImageExists {
        reference: String,
        /// The entry file naming it under another tag, which `--force` does not replace.
        alias_in: Option<PathBuf>,
    },
    ImportMismatch {
        source: String,
        expected: String,
        actual: String,
    },
    UnreadableSource {
        name: String,
        error: std::io::Error,
    },
    Password {
        reason: &'static str,
    },
    PasswordUnseeded {
        name: String,
    },
    MachineSetting {
        setting: &'static str,
        value: String,
        expected: &'static str,
    },
    MachineMismatch {
        chipset: &'static str,
        disk: &'static str,
        instead: &'static str,
    },
    MissingFirmware {
        path: PathBuf,
        package: &'static str,
    },
    NoFirmware {
        arch: String,
    },
    Decompress {
        scheme: &'static str,
        reason: String,
    },
    Archive {
        reason: String,
    },
    Clone {
        reason: String,
    },
    Convert {
        reason: String,
    },
    NotExportable {
        reference: String,
    },
    SuffixMismatch {
        path: PathBuf,
        suffix: String,
        exported: String,
        expected: String,
    },
    Compress {
        scheme: &'static str,
        reason: String,
    },
    OutputExists {
        path: PathBuf,
    },
    DamagedImage {
        path: PathBuf,
        expected: String,
    },
    Export {
        path: PathBuf,
        source: std::io::Error,
    },
    UnheldImage {
        reference: String,
    },
    ImageInUse {
        reference: String,
        instances: Vec<String>,
    },
    ChangeWhileRunning {
        name: String,
    },
    FollowNeedsText,
    ConsoleNeedsText,
    NoConsole {
        name: String,
    },
    NoScreen {
        name: String,
    },
    InstanceRunning {
        name: String,
    },
    InstanceStopped {
        name: String,
    },
    InstancePaused {
        name: String,
    },
    NoCdrom {
        name: String,
    },
    NoGuestAccess {
        name: String,
    },
    SshHostTaken {
        name: String,
        path: PathBuf,
    },
    NoHome,
    NoConfigDirectory,
    UnusableDefaultUser {
        name: String,
        current: bool,
    },
    NoCurrentUser {
        reason: String,
    },
    CopyBetweenGuests,
    CopyWithoutGuest,
    Signal {
        pid: u32,
        source: std::io::Error,
    },
    QmpConnect {
        path: PathBuf,
        source: std::io::Error,
    },
    QmpIo {
        source: std::io::Error,
    },
    /// The monitor closed the connection, as it does after `quit` or a guest power-down.
    QmpClosed,
    QmpTimeout {
        seconds: u64,
    },
    QmpProtocol {
        reason: String,
    },
    QmpCommand {
        command: String,
        class: String,
        description: String,
    },
}

impl Error {
    /// A stable identifier for structured output.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Reference { .. } => "invalid-reference",
            Self::CatalogueRead { .. } => "catalogue-unreadable",
            Self::CatalogueParse { .. } => "catalogue-unparseable",
            Self::Config { .. } => "config-invalid",
            Self::UnknownCatalogue { .. } => "unknown-catalogue",
            Self::UpdateFailed { .. } => "update-failed",
            Self::CatalogueEntry { .. } => "catalogue-entry-invalid",
            Self::UnknownImage { .. } => "unknown-image",
            Self::UnsupportedArchitecture { .. } => "unsupported-architecture",
            Self::MissingTool { .. } => "missing-tool",
            Self::NoImageStore => "no-image-store",
            Self::Store { .. } => "store-unwritable",
            Self::Download { .. } => "download-failed",
            Self::HttpStatus { .. } => "http-error",
            Self::PinMismatch { .. } => "pin-mismatch",
            Self::DigestMismatch { .. } | Self::ImportMismatch { .. } => "digest-mismatch",
            Self::MalformedArchive { .. } => "malformed-archive",
            Self::MalformedPointer { .. } => "malformed-pointer",
            Self::EmptyCatalogue { .. } => "empty-catalogue",
            Self::SeedRefused { .. } => "seed-invalid",
            Self::SeedWrite { .. } => "seed-unwritable",
            Self::NoStateDirectory => "no-state-directory",
            Self::State { .. } => "state-unusable",
            Self::InstanceExists { .. } => "instance-exists",
            Self::UnknownInstance { .. } => "unknown-instance",
            Self::InstanceName { .. } => "invalid-instance-name",
            Self::InstanceRecord { .. } => "instance-damaged",
            Self::NotFetchable => "image-not-fetchable",
            Self::ImageExists { .. } => "image-exists",
            Self::UnreadableSource { .. } => "unreadable-source",

            Self::Decompress { .. } => "decompression-failed",
            Self::Archive { .. } => "archive-unreadable",
            Self::MachineSetting { .. } => "invalid-machine-setting",
            Self::Password { .. } => "unusable-password",
            Self::PasswordUnseeded { .. } => "password-needs-seed",
            Self::MachineMismatch { .. } => "machine-mismatch",
            Self::MissingFirmware { .. } => "missing-firmware",
            Self::NoFirmware { .. } => "no-uefi-firmware",
            Self::Clone { .. } => "clone-failed",
            Self::Convert { .. } => "conversion-failed",
            Self::UnheldImage { .. } | Self::NotExportable { .. } => "image-not-held",
            Self::SuffixMismatch { .. } => "suffix-mismatch",
            Self::Compress { .. } => "compression-failed",
            Self::OutputExists { .. } => "output-exists",
            Self::DamagedImage { .. } => "image-damaged",
            Self::Export { .. } => "export-failed",
            Self::ImageInUse { .. } => "image-in-use",
            Self::ChangeWhileRunning { .. } => "change-while-running",
            Self::FollowNeedsText => "follow-needs-text",
            Self::ConsoleNeedsText => "console-needs-text",
            Self::NoConsole { .. } => "no-console",
            Self::NoScreen { .. } => "no-screen",
            Self::InstanceRunning { .. } => "instance-running",
            Self::InstanceStopped { .. } => "instance-stopped",
            Self::InstancePaused { .. } => "instance-paused",
            Self::NoCdrom { .. } => "no-cdrom",
            Self::NoGuestAccess { .. } => "no-guest-access",
            Self::SshHostTaken { .. } => "ssh-host-taken",
            Self::NoHome => "no-home",
            Self::NoConfigDirectory => "no-config-directory",
            Self::UnusableDefaultUser { .. } => "unusable-default-user",
            Self::NoCurrentUser { .. } => "no-current-user",
            Self::CopyBetweenGuests => "copy-between-guests",
            Self::CopyWithoutGuest => "copy-without-guest",
            Self::Launch { .. } => "launch-failed",
            Self::Overlay { .. } => "overlay-failed",
            Self::KeyGeneration { .. } => "key-generation-failed",
            Self::Signal { .. } => "signal-failed",
            Self::QmpConnect { .. } => "qmp-unreachable",
            Self::QmpIo { .. } => "qmp-io-error",
            Self::QmpClosed => "qmp-closed",
            Self::QmpTimeout { .. } => "qmp-timeout",
            Self::QmpProtocol { .. } => "qmp-protocol-error",
            Self::QmpCommand { .. } => "qmp-command-failed",
        }
    }
}

impl fmt::Display for Error {
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per variant; splitting it would only hide the list"
    )]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reference { input, reason } => {
                write!(f, "'{input}' is not a valid image reference: {reason}")
            }
            Self::CatalogueRead { path, source } => {
                write!(f, "cannot read catalogue at {}: {source}", path.display())
            }
            Self::Config { path, reason } => {
                write!(f, "{}: {reason}", path.display())
            }
            Self::UnknownCatalogue { name, available } if available.is_empty() => {
                write!(f, "no catalogue named '{name}'; none is configured")
            }
            Self::UnknownCatalogue { name, available } => write!(
                f,
                "no catalogue named '{name}'; available: {}",
                available.join(", ")
            ),
            Self::UpdateFailed { failed } => {
                write!(f, "could not update {}", failed.join(", "))
            }
            Self::CatalogueParse { path, source } => {
                write!(
                    f,
                    "cannot parse catalogue entry {}: {source}",
                    path.display()
                )
            }
            Self::CatalogueEntry { path, reason } => {
                write!(f, "catalogue entry {} is invalid: {reason}", path.display())
            }
            Self::UnknownImage { reference } => {
                write!(
                    f,
                    "no image '{reference}' in the catalogue; try 'vm update'"
                )
            }
            Self::UnsupportedArchitecture {
                reference,
                wanted,
                available,
            } => {
                write!(
                    f,
                    "'{reference}' has no {wanted} build; available: {}",
                    available.join(", ")
                )
            }
            Self::NoImageStore => write!(
                f,
                "cannot locate the image store: neither HOME nor XDG_DATA_HOME is set"
            ),
            Self::Store {
                path,
                action,
                source,
            } => {
                write!(f, "cannot {action} {}: {source}", path.display())
            }
            Self::Download { url, source } => write!(f, "cannot fetch {url}: {source}"),
            Self::HttpStatus { url, status } => {
                write!(f, "cannot fetch {url}: server returned HTTP {status}")
            }
            Self::PinMismatch {
                reference,
                arch,
                actual,
            } => write!(
                f,
                "the catalogue has no build of '{reference}' with that digest; its {arch} build is {actual}"
            ),
            Self::DigestMismatch {
                url,
                expected,
                actual,
            } => write!(
                f,
                "{url} does not match the catalogue digest.\n  expected {expected}\n  \
                 received {actual}\nThe image was not kept; run 'vm update' in case the \
                 catalogue is stale."
            ),
            Self::MalformedArchive { url, reason } => {
                write!(f, "the archive at {url} is not readable: {reason}")
            }
            Self::MalformedPointer { url, reason } => {
                write!(f, "{url} is not a catalogue pointer: {reason}")
            }
            Self::EmptyCatalogue { url, path } => write!(
                f,
                "the archive at {url} holds no catalogue entries under '{path}'"
            ),
            Self::SeedRefused { reason } => {
                write!(f, "cannot build the cloud-init seed: {reason}")
            }
            Self::SeedWrite { path, source } => {
                write!(f, "cannot write the seed to {}: {source}", path.display())
            }
            Self::NoStateDirectory => write!(
                f,
                "cannot locate the state directory: neither HOME nor XDG_STATE_HOME is set"
            ),
            Self::State {
                path,
                action,
                source,
            } => write!(f, "cannot {action} at {}: {source}", path.display()),
            Self::InstanceExists { name } => {
                write!(f, "an instance named '{name}' already exists")
            }
            Self::UnknownInstance { name } => {
                write!(f, "no instance named '{name}'; try 'vm ps --all'")
            }
            Self::InstanceName { name, reason } => {
                write!(f, "'{name}' is not a usable instance name: {reason}")
            }
            Self::InstanceRecord { path, reason } => write!(
                f,
                "the instance record at {} is damaged: {reason}",
                path.display()
            ),
            Self::Clone { reason } => {
                write!(f, "cannot clone the machine's disk: {reason}")
            }
            Self::Convert { reason } => write!(f, "cannot convert the image: {reason}"),
            Self::Password { reason } => write!(f, "cannot use that password: {reason}"),
            Self::PasswordUnseeded { name } => write!(
                f,
                "'{name}' takes no cloud-init seed, so there is no account of ours to give a password"
            ),
            Self::MachineSetting {
                setting,
                value,
                expected,
            } => write!(
                f,
                "'{value}' is not a usable {setting}; expected {expected}"
            ),
            Self::MachineMismatch {
                chipset,
                disk,
                instead,
            } => write!(
                f,
                "the '{chipset}' machine has no {disk} controller; {instead}"
            ),
            Self::MissingFirmware { path, package } => write!(
                f,
                "UEFI firmware is not installed at {}; install it with 'apt install {package}'",
                path.display()
            ),
            Self::NoFirmware { arch } => {
                write!(f, "there is no known UEFI firmware for {arch} guests")
            }
            Self::Decompress { scheme, reason } => {
                write!(f, "cannot expand the {scheme} image: {reason}")
            }
            Self::Archive { reason } => {
                write!(f, "cannot take the image from its archive: {reason}")
            }
            Self::ImageExists {
                reference,
                alias_in: None,
            } => write!(
                f,
                "'{reference}' is already in the store catalogue; --force replaces it"
            ),
            Self::ImageExists {
                reference,
                alias_in: Some(path),
            } => write!(
                f,
                "'{reference}' is an alias in {}; remove that entry first",
                path.display()
            ),
            Self::ImportMismatch {
                source,
                expected,
                actual,
            } => write!(
                f,
                "{source} does not match the digest given.\n  expected {expected}\n  \
                 received {actual}\nThe image was not kept."
            ),
            Self::UnreadableSource { name, error } => write!(f, "cannot read {name}: {error}"),
            Self::NotFetchable => write!(
                f,
                "this image was made here by 'vm clone' or 'vm import'; there is nowhere to fetch it from"
            ),
            Self::NotExportable { reference } => write!(
                f,
                "'{reference}' is not in the local store; run 'vm pull {reference}' first"
            ),
            Self::SuffixMismatch {
                path,
                suffix,
                exported,
                expected,
            } => write!(
                f,
                "{} ends in .{suffix}, but the export is {exported}; \
                 name it with .{expected}, which is added to a name without a suffix",
                path.display()
            ),
            Self::Compress { scheme, reason } => {
                write!(f, "cannot compress the image with {scheme}: {reason}")
            }
            Self::OutputExists { path } => {
                write!(f, "{} already exists; --force replaces it", path.display())
            }
            Self::DamagedImage { path, expected } => write!(
                f,
                "the stored image at {} does not match its digest {expected}, \
                 so nothing was exported",
                path.display()
            ),
            Self::Export { path, source } => {
                write!(f, "cannot export to {}: {source}", path.display())
            }
            Self::UnheldImage { reference } => write!(
                f,
                "'{reference}' is not in the local store and pulling is disabled; \
                 run 'vm pull {reference}' first"
            ),
            Self::ImageInUse {
                reference,
                instances,
            } => write!(
                f,
                "'{reference}' is in use by {}; \
                 remove them first, or use --force to break them",
                instances.join(", ")
            ),
            Self::ChangeWhileRunning { name } => write!(
                f,
                "'{name}' is running, and these settings are read when a machine starts; \
                 stop it first"
            ),
            Self::ConsoleNeedsText => write!(
                f,
                "a console is a conversation, which no document format can hold; use text output"
            ),
            Self::NoConsole { name } => write!(
                f,
                "'{name}' was started without a console to attach to; \
                 restart it with 'vm stop {name}' and 'vm start {name}'"
            ),
            Self::NoScreen { name } => write!(
                f,
                "'{name}' was started without a screen to show; \
                 restart it with 'vm stop {name}' and 'vm start {name}'"
            ),
            Self::FollowNeedsText => write!(
                f,
                "a console cannot be followed in a document format; \
                 drop --follow, or read it as text"
            ),
            Self::InstanceRunning { name } => {
                write!(f, "'{name}' is running; stop it first, or use --force")
            }
            Self::InstanceStopped { name } => {
                write!(
                    f,
                    "'{name}' is not running; start it with 'vm start {name}'"
                )
            }
            Self::InstancePaused { name } => write!(
                f,
                "'{name}' is paused, so nothing in it is answering; \
                 let it carry on with 'vm resume {name}'"
            ),
            Self::NoCdrom { name } => write!(f, "{name} has no CD-ROM in its drive"),
            Self::NoGuestAccess { name } => write!(
                f,
                "'{name}' has no account of ours to connect to: its image takes no \
                 cloud-init seed, so no key was installed"
            ),
            Self::SshHostTaken { name, path } => write!(
                f,
                "{} already has a Host entry for '{name}', which the machine's entry would take over; \
                 choose another --name, or leave out --add-ssh-config",
                path.display()
            ),
            Self::NoConfigDirectory => write!(
                f,
                "neither XDG_CONFIG_HOME nor HOME is set, so there is nowhere for a config file"
            ),
            Self::NoHome => write!(
                f,
                "HOME is not set, so there is no SSH configuration to add to"
            ),
            Self::UnusableDefaultUser {
                name,
                current: true,
            } => write!(
                f,
                "'{name}', the name of the user running vm, is not a usable guest account name; \
                 give --user, or set default_user in config.toml"
            ),
            Self::UnusableDefaultUser {
                name,
                current: false,
            } => write!(
                f,
                "'{name}', the default_user in config.toml, is not a usable user name"
            ),
            Self::NoCurrentUser { reason } => write!(
                f,
                "could not find the name of the user running vm ({reason}); give --user"
            ),
            Self::CopyBetweenGuests => write!(
                f,
                "a copy runs between this machine and one guest; copy it here first"
            ),
            Self::CopyWithoutGuest => write!(
                f,
                "neither side names an instance; write one of them as 'name:path'"
            ),
            Self::Launch { program, source } => {
                write!(f, "cannot start {program}: {source}")
            }
            Self::Overlay { path, reason } => {
                write!(f, "cannot create the disk at {}: {reason}", path.display())
            }
            Self::KeyGeneration { path, reason } => write!(
                f,
                "cannot create the instance key at {}: {reason}",
                path.display()
            ),
            Self::Signal { pid, source } => {
                write!(f, "cannot signal process {pid}: {source}")
            }
            Self::QmpConnect { path, source } => write!(
                f,
                "cannot reach the monitor socket at {}: {source}",
                path.display()
            ),
            Self::QmpIo { source } => write!(f, "the monitor connection failed: {source}"),
            Self::QmpClosed => write!(f, "the monitor connection ended"),
            Self::QmpTimeout { seconds } => {
                write!(f, "the monitor did not answer within {seconds} seconds")
            }
            Self::QmpProtocol { reason } => {
                write!(f, "the monitor said something unexpected: {reason}")
            }
            Self::QmpCommand {
                command,
                class,
                description,
            } => write!(
                f,
                "the monitor refused '{command}' ({class}): {description}"
            ),
            Self::MissingTool {
                binary,
                package,
                operation,
            } => {
                write!(
                    f,
                    "{operation} needs the '{binary}' command, which is not installed; \
                     install it with 'apt install {package}'"
                )
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CatalogueRead { source, .. }
            | Self::Store { source, .. }
            | Self::Export { source, .. }
            | Self::SeedWrite { source, .. }
            | Self::State { source, .. }
            | Self::Launch { source, .. }
            | Self::Signal { source, .. }
            | Self::QmpConnect { source, .. }
            | Self::QmpIo { source, .. } => Some(source),
            Self::CatalogueParse { source, .. } => Some(source),
            Self::Download { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

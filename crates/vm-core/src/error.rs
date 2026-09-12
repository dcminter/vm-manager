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
    DigestMismatch {
        url: String,
        expected: String,
        actual: String,
    },
    MalformedArchive {
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
    Commit {
        reason: String,
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
    InstanceRunning {
        name: String,
    },
    InstanceStopped {
        name: String,
    },
    InstancePaused {
        name: String,
    },
    NoGuestAccess {
        name: String,
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
    /// The far end went away. Expected after `quit`, and after a guest powers
    /// itself down, so it is not a fault by itself.
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
    /// A stable slug for structured output. These names are an interface:
    /// scripts match on them, so they outlive any rewording of the message.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Reference { .. } => "invalid-reference",
            Self::CatalogueRead { .. } => "catalogue-unreadable",
            Self::CatalogueParse { .. } => "catalogue-unparseable",
            Self::CatalogueEntry { .. } => "catalogue-entry-invalid",
            Self::UnknownImage { .. } => "unknown-image",
            Self::UnsupportedArchitecture { .. } => "unsupported-architecture",
            Self::MissingTool { .. } => "missing-tool",
            Self::NoImageStore => "no-image-store",
            Self::Store { .. } => "store-unwritable",
            Self::Download { .. } => "download-failed",
            Self::HttpStatus { .. } => "http-error",
            Self::DigestMismatch { .. } => "digest-mismatch",
            Self::MalformedArchive { .. } => "malformed-archive",
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
            Self::Commit { .. } => "commit-failed",
            Self::UnheldImage { .. } => "image-not-held",
            Self::ImageInUse { .. } => "image-in-use",
            Self::ChangeWhileRunning { .. } => "change-while-running",
            Self::FollowNeedsText => "follow-needs-text",
            Self::InstanceRunning { .. } => "instance-running",
            Self::InstanceStopped { .. } => "instance-stopped",
            Self::InstancePaused { .. } => "instance-paused",
            Self::NoGuestAccess { .. } => "no-guest-access",
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
            Self::Commit { reason } => {
                write!(f, "cannot commit the machine's disk: {reason}")
            }
            Self::NotFetchable => write!(
                f,
                "this image was made here by 'vm commit'; there is nowhere to fetch it from"
            ),
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
                "'{reference}' is the disk behind {}; \
                 remove them first, or use --force to break them",
                instances.join(", ")
            ),
            Self::ChangeWhileRunning { name } => write!(
                f,
                "'{name}' is running, and these settings are read when a machine starts; \
                 stop it first"
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
            Self::NoGuestAccess { name } => write!(
                f,
                "'{name}' has no account of ours to connect to: its image takes no \
                 cloud-init seed, so no key was installed"
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

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
        }
    }
}

impl fmt::Display for Error {
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
            Self::MissingTool {
                binary,
                package,
                operation,
            } => {
                write!(
                    f,
                    "{operation} needs the '{binary}' command, which is not on PATH; \
                     install it with 'apt install {package}'"
                )
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CatalogueRead { source, .. } | Self::Store { source, .. } => Some(source),
            Self::CatalogueParse { source, .. } => Some(source),
            Self::Download { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub mod catalogue;
pub mod config;
pub mod digest;
pub mod error;
pub mod fat;
pub mod hypervisor;
pub mod instance;
pub mod keys;
pub mod paths;
pub mod process;
pub mod qmp;
pub mod reference;
pub mod seed;
pub mod store;
pub mod tar;
pub mod update;
pub mod value;

pub use error::{Error, Result};
pub use reference::Reference;

/// The catalogue's name for the architecture this build runs on.
pub const fn host_architecture() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "amd64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "riscv64") {
        "riscv64"
    } else if cfg!(target_arch = "powerpc64") {
        "ppc64el"
    } else {
        "unknown"
    }
}

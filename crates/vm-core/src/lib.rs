pub mod access;
pub mod catalogue;
pub mod clone;
pub mod compression;
pub mod config;
pub mod configuration;
pub mod conversion;
pub mod crypt;
pub mod digest;
pub mod disk;
pub mod error;
pub mod export;
pub mod fat;
pub mod hypervisor;
pub mod images;
pub mod import;
pub mod inert;
pub mod instance;
pub mod keys;
pub mod machine;
pub mod machines;
pub mod paths;
pub mod process;
pub mod prune;
pub mod qmp;
pub mod reference;
pub mod reports;
pub mod screen;
pub mod seed;
pub mod settings;
pub mod ssh_config;
pub mod store;
pub mod tar;
#[cfg(test)]
mod testing;
pub mod update;
pub mod value;

pub use error::{Error, Result};
pub use reference::Reference;

/// The catalogue's names for the architectures a machine can have.
pub const ARCHITECTURES: [&str; 4] = ["amd64", "arm64", "riscv64", "ppc64el"];

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

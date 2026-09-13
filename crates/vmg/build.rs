//! Compiles the resources and the settings schema into the build directory.

use std::path::{Path, PathBuf};

const SCHEMA: &str = "com.paperstack.VmManager.gschema.xml";

fn main() {
    glib_build_tools::compile_resources(&["data"], "data/vmg.gresource.xml", "vmg.gresource");
    compile_schema();
}

/// An installed schema takes precedence, so the copy here serves uninstalled runs.
fn compile_schema() {
    println!("cargo:rerun-if-changed=data/{SCHEMA}");
    let Some(out_dir) = std::env::var_os("OUT_DIR").map(PathBuf::from) else {
        panic!("cargo did not set OUT_DIR");
    };
    let directory = out_dir.join("schemas");
    if let Err(error) = std::fs::create_dir_all(&directory) {
        panic!("could not create {}: {error}", directory.display());
    }
    let source = Path::new("data").join(SCHEMA);
    if let Err(error) = std::fs::copy(&source, directory.join(SCHEMA)) {
        panic!("could not stage {}: {error}", source.display());
    }
    let outcome = std::process::Command::new("glib-compile-schemas")
        .arg(&directory)
        .status();
    match outcome {
        Ok(status) if status.success() => {}
        Ok(status) => panic!("glib-compile-schemas failed: {status}"),
        Err(error) => {
            panic!("could not run glib-compile-schemas ({error}); apt install libglib2.0-dev")
        }
    }
    println!("cargo:rustc-env=VMG_SCHEMA_DIR={}", directory.display());
}

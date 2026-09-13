//! A machine's screen: shown in a VNC viewer, or saved as a picture.

use crate::reports;
use std::path::{Path, PathBuf};
use vm_core::error::{Error, Result};
use vm_core::instance::{Directory, Instance, Instances};
use vm_core::qmp;
use vm_core::value::Value;

/// The VNC viewer, which connects to a Unix socket directly.
const VIEWER: &str = "xtigervncviewer";

fn running(name: &str) -> Result<(Directory, Instance)> {
    let instances = Instances::discover()?;
    let directory = instances.open(name)?;
    let held = directory.read()?;
    if !held.is_running() {
        return Err(Error::InstanceStopped {
            name: name.to_owned(),
        });
    }
    Ok((directory, held))
}

/// Opens a viewer on the screen, or says where it is when a viewer cannot be opened here.
pub fn show(name: &str, launch: bool) -> Result<Option<reports::Screen>> {
    let (directory, _) = running(name)?;
    let socket = directory.screen_socket();
    if !socket.exists() {
        return Err(Error::NoScreen {
            name: name.to_owned(),
        });
    }
    let report = reports::Screen {
        name: name.to_owned(),
        socket: socket.display().to_string(),
        host: host_name(),
    };
    if !launch || !has_display(&|variable| std::env::var_os(variable)) {
        return Ok(Some(report));
    }
    let status = std::process::Command::new(VIEWER)
        .arg(&socket)
        .status()
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::MissingTool {
                    binary: VIEWER,
                    package: "tigervnc-viewer",
                    operation: "showing a machine's screen",
                }
            } else {
                Error::Launch {
                    program: VIEWER.to_owned(),
                    source,
                }
            }
        })?;
    if status.success() {
        Ok(None)
    } else {
        Err(Error::Launch {
            program: VIEWER.to_owned(),
            source: std::io::Error::other(format!("it exited with {status}")),
        })
    }
}

/// Whether a graphical session is there to open a window in.
fn has_display(variable: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|name| variable(name).is_some_and(|value| !value.is_empty()))
}

fn host_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map_or_else(|_| "this-host".to_owned(), |name| name.trim().to_owned())
}

/// Saves what the machine's screen shows as a PNG.
pub fn capture(name: &str, file: Option<&Path>) -> Result<reports::Screenshot> {
    let (_, held) = running(name)?;
    let path = destination(name, file)?;
    let mut client = qmp::connect(&held.monitor)?;
    client.execute(
        "screendump",
        Some(Value::map([
            ("filename", Value::string(path.display().to_string())),
            ("format", Value::string("png")),
        ])),
    )?;
    let bytes = std::fs::read(&path).map_err(|source| Error::State {
        path: path.clone(),
        action: "read the screenshot",
        source,
    })?;
    let (width, height) = dimensions(&bytes).unwrap_or_default();
    Ok(reports::Screenshot {
        name: name.to_owned(),
        path: path.display().to_string(),
        size: bytes.len() as u64,
        width,
        height,
    })
}

/// An absolute path, since the hypervisor has its own working directory.
fn destination(name: &str, file: Option<&Path>) -> Result<PathBuf> {
    let wanted = file.map_or_else(|| PathBuf::from(format!("{name}.png")), Path::to_path_buf);
    if wanted.is_absolute() {
        return Ok(wanted);
    }
    std::env::current_dir()
        .map(|here| here.join(&wanted))
        .map_err(|source| Error::State {
            path: wanted,
            action: "find the current directory",
            source,
        })
}

/// Width and height from a PNG's header chunk.
fn dimensions(png: &[u8]) -> Option<(u32, u32)> {
    if png.get(..8)? != b"\x89PNG\r\n\x1a\n" || png.get(12..16)? != b"IHDR" {
        return None;
    }
    let number = |at: usize| png.get(at..at + 4)?.try_into().ok().map(u32::from_be_bytes);
    Some((number(16)?, number(20)?))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_screenshot_is_named_after_its_machine_by_default() {
        let path = destination("pd", None).unwrap();
        assert!(path.is_absolute(), "{}", path.display());
        assert!(path.ends_with("pd.png"), "{}", path.display());
    }

    #[test]
    fn a_relative_file_is_made_absolute_against_this_directory() {
        let path = destination("pd", Some(Path::new("shots/boot.png"))).unwrap();
        assert_eq!(
            path,
            std::env::current_dir().unwrap().join("shots/boot.png")
        );
    }

    #[test]
    fn an_absolute_file_is_used_as_given() {
        let path = destination("pd", Some(Path::new("/tmp/boot.png"))).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/boot.png"));
    }

    #[test]
    fn dimensions_are_read_from_the_header() {
        let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        png.extend(1280u32.to_be_bytes());
        png.extend(800u32.to_be_bytes());
        png.extend([8, 2, 0, 0, 0]);
        assert_eq!(dimensions(&png), Some((1280, 800)));
    }

    #[test]
    fn what_is_not_a_png_has_no_dimensions() {
        assert_eq!(dimensions(b""), None);
        assert_eq!(dimensions(b"P6\n720 400\n255\n"), None);
        assert_eq!(
            dimensions(b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00"),
            None
        );
    }

    #[test]
    fn a_display_is_found_from_either_variable_and_not_from_an_empty_one() {
        let only = |wanted: &'static str, value: &'static str| {
            move |name: &str| (name == wanted).then(|| std::ffi::OsString::from(value))
        };
        assert!(has_display(&only("DISPLAY", ":0")));
        assert!(has_display(&only("WAYLAND_DISPLAY", "wayland-0")));
        assert!(!has_display(&only("DISPLAY", "")));
        assert!(!has_display(&|_| None));
    }
}

//! Instance commands that need a terminal.

use crate::style::Style;
use std::time::Duration;
use vm_core::error::{Error, Result};
use vm_core::inert::{Colour, Filter};
use vm_core::machines::{self, Tail};

/// How often a followed listing refreshes.
const REFRESH: Duration = Duration::from_secs(1);

/// Lists instances repeatedly, clearing the screen between listings on a terminal.
pub fn watch(all: bool, format: crate::output::Format, style: Style) -> Result<()> {
    use std::io::{IsTerminal as _, Write as _};
    let clearing = format.is_text() && std::io::stdout().is_terminal();
    loop {
        let report = machines::list(all)?;
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

/// Replaces this process with `ssh`.
pub fn connect(name: &str, command: &[String]) -> Result<std::convert::Infallible> {
    let arguments = machines::ssh_arguments(name, command)?;
    Err(exec("ssh", &arguments))
}

/// Replaces this process with `ssh` running a command, once the guest accepts its key.
pub fn execute(
    name: &str,
    request: &vm_core::access::Exec,
    wait: Duration,
) -> Result<std::convert::Infallible> {
    let arguments = machines::exec_arguments(name, request, wait)?;
    Err(exec("ssh", &arguments))
}

/// Copies between here and a guest, the same way.
pub fn copy(from: &str, to: &str) -> Result<std::convert::Infallible> {
    let arguments = machines::scp_arguments(from, to)?;
    Err(exec("scp", &arguments))
}

/// Replaces this process with the graphical front end, preferring the one installed beside vm.
pub fn gui() -> Error {
    use std::os::unix::process::CommandExt as _;
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|directory| directory.join("vmg")))
        .filter(|path| path.is_file());
    let program = beside.unwrap_or_else(|| std::path::PathBuf::from("vmg"));
    let source = std::process::Command::new(&program).exec();
    if source.kind() == std::io::ErrorKind::NotFound {
        Error::MissingTool {
            binary: "vmg",
            package: "vm-manager-gui",
            operation: "opening the graphical front end",
        }
    } else {
        Error::Launch {
            program: "vmg".to_owned(),
            source,
        }
    }
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

/// Writes the console as it grows, starting with what is already there, until the machine stops.
pub fn follow(name: &str, from: Option<usize>, style: Style) -> Result<()> {
    use std::io::Write as _;
    let Some(mut tail) = Tail::open(name)? else {
        return Ok(());
    };
    let mut out = std::io::stdout().lock();
    let colour = if style.is_coloured() {
        Colour::Keep
    } else {
        Colour::Drop
    };
    let mut filter = Filter::new(colour);
    let Ok(buffer) = tail.read() else {
        return Ok(());
    };
    let kept = filter.apply(&buffer);
    let text = String::from_utf8_lossy(&kept);
    for line in last_lines(&text, from) {
        if writeln!(out, "{line}").is_err() {
            return Ok(());
        }
    }
    loop {
        let running = tail.running();
        let Ok(buffer) = tail.read() else {
            return Ok(());
        };
        if buffer.is_empty() {
            // Nothing new, and if nothing is running there will be no more.
            if !running {
                let _ = out.flush();
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        if out.write_all(&filter.apply(&buffer)).is_err() || out.flush().is_err() {
            return Ok(());
        }
    }
}

/// The last few lines, or all of them when no number was asked for.
fn last_lines(text: &str, lines: Option<usize>) -> Vec<&str> {
    let all: Vec<&str> = text.lines().collect();
    let from = lines.map_or(0, |wanted| all.len().saturating_sub(wanted));
    all[from..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_followed_console_starts_from_its_last_lines() {
        let text = "one\ntwo\nthree\n";
        assert_eq!(last_lines(text, None), ["one", "two", "three"]);
        assert_eq!(last_lines(text, Some(2)), ["two", "three"]);
        assert!(last_lines("", Some(5)).is_empty());
    }
}

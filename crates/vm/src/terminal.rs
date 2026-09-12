//! Terminal modes for reading a password and for attaching to a console.

use rustix::termios::{self, LocalModes, OptionalActions, Termios};
use std::io::{IsTerminal as _, Write as _};
use vm_core::error::{Error, Result};

/// A changed terminal mode, put back when this is dropped.
pub struct Mode {
    original: Termios,
}

impl Mode {
    /// Turns off echo, for a password.
    pub fn quiet() -> Result<Self> {
        Self::change(|held| held.local_modes.remove(LocalModes::ECHO))
    }

    /// Hands every keystroke through unprocessed, for a console.
    pub fn raw() -> Result<Self> {
        Self::change(Termios::make_raw)
    }

    fn change(adjust: impl FnOnce(&mut Termios)) -> Result<Self> {
        let stdin = std::io::stdin();
        let original = termios::tcgetattr(&stdin).map_err(terminal_error)?;
        let mut changed = original.clone();
        adjust(&mut changed);
        termios::tcsetattr(&stdin, OptionalActions::Now, &changed).map_err(terminal_error)?;
        Ok(Self { original })
    }
}

impl Drop for Mode {
    fn drop(&mut self) {
        let _ = termios::tcsetattr(std::io::stdin(), OptionalActions::Now, &self.original);
    }
}

fn terminal_error(source: rustix::io::Errno) -> Error {
    Error::State {
        path: "/dev/stdin".into(),
        action: "change the terminal mode",
        source: source.into(),
    }
}

/// Reads a password: asked for twice on a terminal, or taken as one line of standard input.
pub fn password() -> Result<String> {
    if !std::io::stdin().is_terminal() {
        return read_line(&mut std::io::stdin().lock());
    }
    let first = ask("Password: ")?;
    let second = ask("Password again: ")?;
    if first == second {
        Ok(first)
    } else {
        Err(Error::Password {
            reason: "the two entries differ",
        })
    }
}

fn ask(prompt: &str) -> Result<String> {
    let mut errors = std::io::stderr();
    let _ = write!(errors, "{prompt}");
    let _ = errors.flush();
    let line = {
        let _quiet = Mode::quiet()?;
        read_line(&mut std::io::stdin().lock())
    };
    let _ = writeln!(errors);
    line
}

fn read_line(input: &mut impl std::io::BufRead) -> Result<String> {
    let mut line = String::new();
    input.read_line(&mut line).map_err(|source| Error::State {
        path: "/dev/stdin".into(),
        action: "read a password",
        source,
    })?;
    let trimmed = line.strip_suffix('\n').unwrap_or(&line);
    Ok(trimmed.strip_suffix('\r').unwrap_or(trimmed).to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_piped_password_loses_only_its_line_ending() {
        assert_eq!(
            read_line(&mut &b"s3cret pass\n"[..]).unwrap(),
            "s3cret pass"
        );
        assert_eq!(read_line(&mut &b"windows\r\n"[..]).unwrap(), "windows");
        assert_eq!(read_line(&mut &b"no ending"[..]).unwrap(), "no ending");
        assert_eq!(read_line(&mut &b"first\nsecond\n"[..]).unwrap(), "first");
    }

    #[test]
    fn nothing_piped_is_an_empty_password() {
        assert_eq!(read_line(&mut &b""[..]).unwrap(), "");
    }
}

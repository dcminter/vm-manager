//! Attaching the terminal to a machine's serial console.

use crate::terminal::Mode;
use std::io::{IsTerminal as _, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use vm_core::error::{Error, Result};
use vm_core::instance::Instances;

/// Ctrl-], which telnet and virsh also use, and which nothing typed at a shell needs.
pub const DETACH: u8 = 0x1d;

/// How often a wait for keystrokes looks up to see whether the guest has gone.
const GLANCE: Duration = Duration::from_millis(200);

/// Why an attachment ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Detached,
    InputClosed,
    GuestClosed,
}

/// Connects this terminal to the console of a running machine until detached.
pub fn attach(name: &str) -> Result<()> {
    let instances = Instances::discover()?;
    let directory = instances.open(name)?;
    let held = directory.read()?;
    if !held.is_running() {
        return Err(Error::InstanceStopped {
            name: name.to_owned(),
        });
    }
    let socket = directory.console_socket();
    let stream = UnixStream::connect(&socket).map_err(|source| match source.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => Error::NoConsole {
            name: name.to_owned(),
        },
        _ => Error::State {
            path: socket.clone(),
            action: "connect to the console",
            source,
        },
    })?;
    let failed = |source| Error::State {
        path: socket.clone(),
        action: "use the console",
        source,
    };
    let interactive = std::io::stdin().is_terminal();
    if interactive {
        eprintln!("Attached to {name}'s console; press Ctrl-] to detach.");
    }

    let closed = Arc::new(AtomicBool::new(false));
    let mut from_guest = stream.try_clone().map_err(failed)?;
    let shown = {
        let closed = Arc::clone(&closed);
        std::thread::spawn(move || show(&mut from_guest, &mut std::io::stdout(), &closed))
    };
    let mode = if interactive {
        Some(Mode::raw()?)
    } else {
        None
    };
    let ended = pump(
        &mut Keyboard,
        &mut wait_for_keys,
        &mut &stream,
        interactive.then_some(DETACH),
        &closed,
    );
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = shown.join();
    drop(mode);
    match ended.map_err(failed)? {
        Ended::Detached if interactive => eprintln!("\r\nDetached from {name}'s console."),
        Ended::GuestClosed if interactive => eprintln!("\r\nThe console closed."),
        _ => {}
    }
    Ok(())
}

/// Standard input read straight from its descriptor, because a buffered reader hides keystrokes from `poll`.
struct Keyboard;

impl Read for Keyboard {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        rustix::io::read(std::io::stdin(), buffer).map_err(std::io::Error::from)
    }
}

/// Whether a keystroke is waiting, giving up after a glance so the caller can check on the guest.
fn wait_for_keys() -> std::io::Result<bool> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let stdin = std::io::stdin();
    let mut descriptors = [PollFd::new(&stdin, PollFlags::IN)];
    let glance = Timespec {
        tv_sec: 0,
        tv_nsec: i64::from(GLANCE.subsec_nanos()),
    };
    match poll(&mut descriptors, Some(&glance)) {
        Ok(ready) => Ok(ready > 0),
        Err(rustix::io::Errno::INTR) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Copies input to the guest until the detach key, the end of input, or the guest going away.
fn pump(
    input: &mut dyn Read,
    ready: &mut dyn FnMut() -> std::io::Result<bool>,
    guest: &mut dyn Write,
    detach: Option<u8>,
    closed: &AtomicBool,
) -> std::io::Result<Ended> {
    let mut buffer = [0u8; 1024];
    loop {
        if closed.load(Ordering::Relaxed) {
            return Ok(Ended::GuestClosed);
        }
        if !ready()? {
            continue;
        }
        let count = input.read(&mut buffer)?;
        if count == 0 {
            return Ok(Ended::InputClosed);
        }
        let chunk = &buffer[..count];
        let (sent, ending) = detach
            .and_then(|key| chunk.iter().position(|byte| *byte == key))
            .map_or((chunk, None), |at| (&chunk[..at], Some(Ended::Detached)));
        match guest.write_all(sent).and_then(|()| guest.flush()) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {
                return Ok(Ended::GuestClosed);
            }
            Err(error) => return Err(error),
        }
        if let Some(ending) = ending {
            return Ok(ending);
        }
    }
}

/// Copies what the guest says to the terminal, and marks the console closed when it stops.
fn show(guest: &mut dyn Read, out: &mut dyn Write, closed: &AtomicBool) {
    let mut buffer = [0u8; 4096];
    loop {
        match guest.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                if out
                    .write_all(&buffer[..count])
                    .and_then(|()| out.flush())
                    .is_err()
                {
                    break;
                }
            }
        }
    }
    closed.store(true, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[expect(clippy::unnecessary_wraps, reason = "the signature pump waits with")]
    fn always() -> std::io::Result<bool> {
        Ok(true)
    }

    #[test]
    fn keystrokes_before_the_detach_key_reach_the_guest_and_the_rest_do_not() {
        let (mut ours, mut theirs) = UnixStream::pair().unwrap();
        let closed = AtomicBool::new(false);
        let mut input = &b"ls -l\r\x1dnever sent"[..];
        let ended = pump(&mut input, &mut always, &mut ours, Some(DETACH), &closed).unwrap();
        assert_eq!(ended, Ended::Detached);
        drop(ours);
        let mut received = Vec::new();
        theirs.read_to_end(&mut received).unwrap();
        assert_eq!(received, b"ls -l\r");
    }

    /// Piped input has no detach key; the byte is the guest's like any other.
    #[test]
    fn without_a_detach_key_every_byte_is_sent_until_input_ends() {
        let (mut ours, mut theirs) = UnixStream::pair().unwrap();
        let closed = AtomicBool::new(false);
        let mut input = &b"a\x1db"[..];
        let ended = pump(&mut input, &mut always, &mut ours, None, &closed).unwrap();
        assert_eq!(ended, Ended::InputClosed);
        drop(ours);
        let mut received = Vec::new();
        theirs.read_to_end(&mut received).unwrap();
        assert_eq!(received, b"a\x1db");
    }

    /// A machine that stops while nobody is typing must not leave the terminal waiting for a key.
    #[test]
    fn a_closed_guest_ends_the_wait_for_keys() {
        let (mut ours, _theirs) = UnixStream::pair().unwrap();
        let closed = AtomicBool::new(false);
        let mut glances = 0;
        let mut idle = || {
            glances += 1;
            if glances == 3 {
                closed.store(true, Ordering::Relaxed);
            }
            Ok(false)
        };
        let mut input = &b"unread"[..];
        let ended = pump(&mut input, &mut idle, &mut ours, Some(DETACH), &closed).unwrap();
        assert_eq!(ended, Ended::GuestClosed);
    }

    #[test]
    fn a_guest_that_has_gone_is_reported_rather_than_failed() {
        let (mut ours, theirs) = UnixStream::pair().unwrap();
        drop(theirs);
        let closed = AtomicBool::new(false);
        let mut input = &b"hello"[..];
        let ended = pump(&mut input, &mut always, &mut ours, Some(DETACH), &closed).unwrap();
        assert_eq!(ended, Ended::GuestClosed);
    }

    #[test]
    fn what_the_guest_says_is_shown_until_it_stops_saying_it() {
        let (mut ours, mut theirs) = UnixStream::pair().unwrap();
        theirs.write_all(b"login: ").unwrap();
        drop(theirs);
        let closed = AtomicBool::new(false);
        let mut out = Vec::new();
        show(&mut ours, &mut out, &closed);
        assert_eq!(out, b"login: ");
        assert!(closed.load(Ordering::Relaxed));
    }
}

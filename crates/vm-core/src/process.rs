//! Liveness for processes this tool did not keep as children.
//!
//! A detached hypervisor is reparented to init as soon as the command that
//! launched it exits, so there is no child to wait on and nothing but the pid
//! recorded on disk. A pid alone is not enough: they are reused, and a stale
//! record naming a pid that now belongs to someone else's process would report
//! an instance as running and then act on a stranger. The process start time
//! from `/proc` disambiguates, because a reused pid cannot have started when
//! the original did.

use crate::error::{Error, Result};
use std::fs;
use std::process::Command;

/// Identifies a process beyond the life of its pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handle {
    pub pid: u32,
    /// Field 22 of `/proc/<pid>/stat`, in clock ticks since boot. Its units do
    /// not matter; only that it differs between one process and the next.
    pub started: u64,
}

impl Handle {
    /// Reads the start time of a pid that is expected to be running.
    pub fn of(pid: u32) -> Option<Self> {
        start_time(&read_stat(pid)?).map(|started| Self { pid, started })
    }

    /// Whether this exact process is still running. A pid that has been
    /// reused answers no, which is the point, and so does one that has exited
    /// but not yet been reaped.
    pub fn is_running(&self) -> bool {
        read_stat(self.pid)
            .is_some_and(|stat| start_time(&stat) == Some(self.started) && !is_dead(&stat))
    }
}

fn read_stat(pid: u32) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/stat")).ok()
}

/// The second field of the line is the executable name in parentheses, and it
/// may hold both spaces and parentheses of its own, so the fields after it are
/// found from the last closing parenthesis rather than by splitting the line.
fn start_time(stat: &str) -> Option<u64> {
    // Field 3 is the first of these, so field 22 is the twentieth.
    fields(stat)?.split_whitespace().nth(19)?.parse().ok()
}

fn fields(stat: &str) -> Option<&str> {
    Some(&stat[stat.rfind(')')? + 1..])
}

/// A process that has exited but whose parent has not yet collected it still
/// has an entry under `/proc` and still answers with its original start time.
/// It is not running, and treating it as running would wait forever.
fn is_dead(stat: &str) -> bool {
    matches!(
        fields(stat).and_then(|rest| rest.split_whitespace().next()),
        Some("Z" | "X")
    )
}

/// Signals a process. Used only where the monitor has already failed to
/// answer: every ordinary stop goes through QMP, which tells the guest what is
/// happening rather than taking it away.
pub fn signal(handle: &Handle, signal: Signal) -> Result<()> {
    if !handle.is_running() {
        return Ok(());
    }
    let status = Command::new("kill")
        .arg(signal.flag())
        .arg(handle.pid.to_string())
        .status()
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::MissingTool {
                    binary: "kill",
                    package: "util-linux",
                    operation: "signalling a virtual machine",
                }
            } else {
                Error::Signal {
                    pid: handle.pid,
                    source,
                }
            }
        })?;
    // A process that has exited between the check and the signal is the
    // outcome that was wanted, so a refusal is only reported if it is still
    // there afterwards.
    if status.success() || !handle.is_running() {
        Ok(())
    } else {
        Err(Error::Signal {
            pid: handle.pid,
            source: std::io::Error::other(format!("kill exited with {status}")),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Ask. A hypervisor treats this as a request to quit.
    Terminate,
    /// Take. Nothing in the guest is told.
    Kill,
}

impl Signal {
    const fn flag(self) -> &'static str {
        match self {
            Self::Terminate => "-TERM",
            Self::Kill => "-KILL",
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::process::{Child, Stdio};

    /// A process of our own to ask questions about. `sleep` is in coreutils,
    /// which is Essential, so it is present wherever the tests run.
    fn sleeper() -> Child {
        Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn wait_until_gone(handle: &Handle) -> bool {
        for _ in 0..100 {
            if !handle.is_running() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn this_process_is_running() {
        let handle = Handle::of(std::process::id()).unwrap();
        assert!(handle.is_running());
    }

    #[test]
    fn a_pid_that_does_not_exist_has_no_handle() {
        // Chosen above the usual pid ceiling so that it names nothing.
        assert!(Handle::of(4_194_303).is_none());
    }

    #[test]
    fn a_process_that_has_exited_is_not_running() {
        let mut child = sleeper();
        let handle = Handle::of(child.id()).unwrap();
        assert!(handle.is_running());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!handle.is_running());
    }

    /// The whole reason the start time is recorded: a record naming a pid that
    /// has since been reused must not report the new process as the old one.
    #[test]
    fn a_handle_with_the_wrong_start_time_is_not_running() {
        let mut child = sleeper();
        let mut handle = Handle::of(child.id()).unwrap();
        handle.started += 1;
        assert!(!handle.is_running());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn terminating_a_process_ends_it() {
        let mut child = sleeper();
        let handle = Handle::of(child.id()).unwrap();
        signal(&handle, Signal::Terminate).unwrap();
        assert!(wait_until_gone(&handle));
        child.wait().unwrap();
    }

    #[test]
    fn killing_a_process_ends_it() {
        let mut child = sleeper();
        let handle = Handle::of(child.id()).unwrap();
        signal(&handle, Signal::Kill).unwrap();
        assert!(wait_until_gone(&handle));
        child.wait().unwrap();
    }

    #[test]
    fn signalling_something_already_gone_is_not_a_failure() {
        let mut child = sleeper();
        let handle = Handle::of(child.id()).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(signal(&handle, Signal::Kill).is_ok());
    }

    #[test]
    fn a_name_holding_spaces_and_brackets_does_not_confuse_the_reader() {
        let stat = "1234 (od d) ee) S 1 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 \
                    9876543 0 0 0 0";
        assert_eq!(start_time(stat), Some(9_876_543));
    }

    /// A child of this process that has exited is a zombie until it is waited
    /// on, and a zombie is not something to keep waiting for.
    #[test]
    fn a_process_that_has_exited_but_not_been_reaped_is_not_running() {
        let mut child = sleeper();
        let handle = Handle::of(child.id()).unwrap();
        child.kill().unwrap();
        assert!(wait_until_gone(&handle), "a zombie was reported as running");
        child.wait().unwrap();
    }

    #[test]
    fn a_stat_line_that_is_not_one_yields_nothing() {
        assert_eq!(start_time("nonsense"), None);
        assert_eq!(start_time("1234 (sleep) S 1 2 3"), None);
    }
}

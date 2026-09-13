//! Liveness of detached processes, identified by pid and start time.

use crate::error::{Error, Result};
use std::fs;
use std::process::Command;

/// Identifies a process beyond the life of its pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handle {
    pub pid: u32,
    /// Field 22 of `/proc/<pid>/stat`, which tells a reused pid apart.
    pub started: u64,
}

impl Handle {
    /// Reads the start time of a pid that is expected to be running.
    pub fn of(pid: u32) -> Option<Self> {
        start_time(&read_stat(pid)?).map(|started| Self { pid, started })
    }

    /// Whether this exact process has any thread still running.
    pub fn is_running(&self) -> bool {
        read_stat(self.pid).is_some_and(|stat| {
            start_time(&stat) == Some(self.started) && alive(&stat, thread_stats(self.pid))
        })
    }
}

/// Whether the first thread, or failing that any thread, has yet to exit.
fn alive(leader: &str, threads: impl IntoIterator<Item = String>) -> bool {
    !is_dead(leader) || threads.into_iter().any(|task| !is_dead(&task))
}

fn read_stat(pid: u32) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/stat")).ok()
}

/// The stat line of each of a process's threads, skipping any that end while being read.
fn thread_stats(pid: u32) -> impl Iterator<Item = String> {
    fs::read_dir(format!("/proc/{pid}/task"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|task| fs::read_to_string(task.path().join("stat")).ok())
}

/// Field 22, counted from the last `)` because the name may contain spaces and parentheses.
fn start_time(stat: &str) -> Option<u64> {
    // Field 3 is the first of these, so field 22 is the twentieth.
    fields(stat)?.split_whitespace().nth(19)?.parse().ok()
}

fn fields(stat: &str) -> Option<&str> {
    Some(&stat[stat.rfind(')')? + 1..])
}

/// Whether the stat line is of a zombie or dead task.
fn is_dead(stat: &str) -> bool {
    matches!(
        fields(stat).and_then(|rest| rest.split_whitespace().next()),
        Some("Z" | "X")
    )
}

/// Signals a process.
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
    // A process that exited meanwhile is the outcome wanted.
    if status.success() || !handle.is_running() {
        Ok(())
    } else {
        Err(Error::Signal {
            pid: handle.pid,
            source: std::io::Error::other(format!("kill exited with {status}")),
        })
    }
}

/// Resident memory in bytes.
pub fn resident(pid: u32) -> Option<u64> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kibibytes: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kibibytes.checked_mul(1024)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// A request to quit.
    Terminate,
    /// Immediate termination.
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

    /// A child process to inspect.
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

    #[test]
    fn a_process_that_has_exited_but_not_been_reaped_is_not_running() {
        let mut child = sleeper();
        let handle = Handle::of(child.id()).unwrap();
        child.kill().unwrap();
        assert!(wait_until_gone(&handle), "a zombie was reported as running");
        child.wait().unwrap();
    }

    #[test]
    fn a_process_is_running_while_any_of_its_threads_is() {
        let leader = "42 (qemu) Z 1 42 42 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 555 0 0";
        let exiting = "43 (worker) R 1 42 42 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 556 0 0";
        let running = "42 (qemu) S 1 42 42 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 555 0 0";
        assert!(alive(leader, [leader, exiting].map(str::to_owned)));
        assert!(!alive(leader, [leader.to_owned()]));
        assert!(!alive(leader, []));
        assert!(alive(running, []));
    }

    #[test]
    fn every_thread_of_a_process_is_read() {
        let (sender, receiver) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::spawn(move || receiver.recv());
        let threads: Vec<String> = thread_stats(std::process::id()).collect();
        sender.send(()).unwrap();
        worker.join().unwrap().unwrap();
        assert!(threads.len() > 1, "{threads:?}");
        let leader = format!("{} (", std::process::id());
        assert!(
            threads.iter().any(|task| task.starts_with(&leader)),
            "{threads:?}"
        );
    }

    #[test]
    fn a_pid_that_is_not_there_has_no_threads() {
        assert_eq!(thread_stats(4_194_303).count(), 0);
    }

    #[test]
    fn a_stat_line_that_is_not_one_yields_nothing() {
        assert_eq!(start_time("nonsense"), None);
        assert_eq!(start_time("1234 (sleep) S 1 2 3"), None);
    }

    #[test]
    fn a_running_process_reports_what_it_is_holding() {
        let held = resident(std::process::id()).unwrap();
        assert!(held >= 4096, "{held}");
    }

    #[test]
    fn a_pid_that_is_not_there_holds_nothing() {
        let mut child = sleeper();
        let pid = child.id();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(resident(pid), None);
    }
}

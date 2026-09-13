//! Terminal tabs: a console on a machine's serial socket, a shell, a copy, or its log.

use std::cell::{Cell, RefCell};
use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use vm_core::error::{Error, Result};
use vm_core::instance::Instances;
use vm_core::machines::Tail;
use vte::prelude::*;

/// How much of a console log a console tab shows before attaching.
const CONSOLE_HISTORY: usize = 64 << 10;
/// How many lines a log tab shows when not showing the whole log.
pub const TAIL_LINES: usize = 200;
const POLL: Duration = Duration::from_millis(200);

/// A terminal with whatever drives it, released when the tab closes.
pub struct Tab {
    pub root: gtk::Box,
    pub terminal: vte::Terminal,
    closer: Rc<dyn Fn()>,
}

impl Tab {
    pub fn close(&self) {
        (self.closer)();
    }
}

fn terminal() -> (gtk::Box, vte::Terminal) {
    let terminal = vte::Terminal::builder()
        .scrollback_lines(20_000)
        .vexpand(true)
        .hexpand(true)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&terminal)
        .vexpand(true)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("terminal-tab");
    root.append(&scroller);
    (root, terminal)
}

/// Runs a program in the terminal, as a shell or a copy does.
pub fn spawn(program: &str, arguments: &[String]) -> Tab {
    let (root, terminal) = terminal();
    let named = program.to_owned();
    let mut argv: Vec<&str> = vec![program];
    argv.extend(arguments.iter().map(String::as_str));
    let child: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
    let started = child.clone();
    terminal.spawn_async(
        vte::PtyFlags::DEFAULT,
        None,
        &argv,
        &[],
        glib::SpawnFlags::SEARCH_PATH,
        || {},
        -1,
        gtk::gio::Cancellable::NONE,
        glib::clone!(
            #[weak]
            terminal,
            move |outcome| match outcome {
                Ok(pid) => started.set(u32::try_from(pid.0).ok()),
                Err(error) => {
                    terminal.feed(format!("\r\ncould not start {named}: {error}\r\n").as_bytes());
                }
            }
        ),
    );
    let exited = child.clone();
    terminal.connect_child_exited(move |terminal, status| {
        exited.set(None);
        terminal.feed(format!("\r\n[exited with {status}]\r\n").as_bytes());
    });
    Tab {
        root,
        terminal,
        closer: Rc::new(move || {
            if let Some(handle) = child.take().and_then(vm_core::process::Handle::of) {
                let _ = vm_core::process::signal(&handle, vm_core::process::Signal::Terminate);
            }
        }),
    }
}

/// Attaches to a running machine's serial console.
pub fn console(name: &str) -> Result<Tab> {
    let instances = Instances::discover()?;
    let directory = instances.open(name)?;
    let held = directory.read()?;
    if !held.is_running() {
        return Err(Error::InstanceStopped {
            name: name.to_owned(),
        });
    }
    let socket = directory.console_socket();
    let stream = UnixStream::connect(&socket).map_err(|source| Error::State {
        path: socket.clone(),
        action: "connect to the console",
        source,
    })?;
    let (root, terminal) = terminal();
    if let Ok(log) = std::fs::read(directory.console()) {
        let from = log.len().saturating_sub(CONSOLE_HISTORY);
        terminal.feed(&log[from..]);
    }
    let writer = stream.try_clone().map_err(|source| Error::State {
        path: socket.clone(),
        action: "open the console for writing",
        source,
    })?;
    terminal.connect_commit(move |_, text, _| {
        let _ = (&writer).write_all(text.as_bytes());
    });
    let (sender, receiver) = async_channel::unbounded::<Vec<u8>>();
    let mut reader = stream.try_clone().map_err(|source| Error::State {
        path: socket,
        action: "open the console for reading",
        source,
    })?;
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    if sender.send_blocking(buffer[..count].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    glib::spawn_future_local(glib::clone!(
        #[weak]
        terminal,
        async move {
            while let Ok(bytes) = receiver.recv().await {
                terminal.feed(&bytes);
            }
            terminal.feed(b"\r\n[console closed]\r\n");
        }
    ));
    Ok(Tab {
        root,
        terminal,
        closer: Rc::new(move || {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }),
    })
}

/// Shows a machine's console log as it grows.
pub fn logs(name: &str) -> Tab {
    let (root, terminal) = terminal();
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bar.set_margin_bottom(4);
    let tail = gtk::ToggleButton::with_label(&format!("Last {TAIL_LINES} lines"));
    let whole = gtk::ToggleButton::with_label("Whole log");
    whole.set_group(Some(&tail));
    tail.set_active(true);
    let linked = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    linked.add_css_class("linked");
    linked.append(&tail);
    linked.append(&whole);
    bar.append(&linked);
    root.prepend(&bar);
    let state: Rc<RefCell<Option<Tail>>> = Rc::new(RefCell::new(None));
    let closed = Rc::new(Cell::new(false));
    let show_whole = Rc::new(Cell::new(false));
    let name = name.to_owned();
    let restart = {
        let (state, terminal, show_whole, name) = (
            state.clone(),
            terminal.clone(),
            show_whole.clone(),
            name.clone(),
        );
        Rc::new(move || {
            terminal.reset(true, true);
            state.replace(None);
            match Tail::open(&name) {
                Ok(Some(mut tail)) => {
                    let bytes = tail.read().unwrap_or_default();
                    terminal.feed(if show_whole.get() {
                        &bytes
                    } else {
                        last_lines(&bytes, TAIL_LINES)
                    });
                    state.replace(Some(tail));
                }
                Ok(None) => terminal.feed(b"[no console output yet]\r\n"),
                Err(error) => terminal.feed(format!("[{error}]\r\n").as_bytes()),
            }
        })
    };
    restart();
    let again = restart;
    whole.connect_toggled(glib::clone!(
        #[weak]
        show_whole,
        move |button| {
            show_whole.set(button.is_active());
            again();
        }
    ));
    glib::timeout_add_local(
        POLL,
        glib::clone!(
            #[weak]
            terminal,
            #[strong]
            state,
            #[strong]
            closed,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                if closed.get() {
                    return glib::ControlFlow::Break;
                }
                let mut held = state.borrow_mut();
                match held.as_mut() {
                    Some(tail) => {
                        if let Ok(bytes) = tail.read()
                            && !bytes.is_empty()
                        {
                            terminal.feed(&bytes);
                        }
                    }
                    None => {
                        if let Ok(Some(mut tail)) = Tail::open(&name) {
                            terminal.reset(true, true);
                            terminal.feed(&tail.read().unwrap_or_default());
                            *held = Some(tail);
                        }
                    }
                }
                glib::ControlFlow::Continue
            }
        ),
    );
    Tab {
        root,
        terminal,
        closer: Rc::new(move || closed.set(true)),
    }
}

/// The last `count` lines of some bytes.
pub fn last_lines(bytes: &[u8], count: usize) -> &[u8] {
    let mut seen = 0;
    for (index, byte) in bytes.iter().enumerate().rev() {
        if *byte != b'\n' || index + 1 == bytes.len() {
            continue;
        }
        seen += 1;
        if seen == count {
            return &bytes[index + 1..];
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_lines_are_cut_at_a_line_boundary() {
        let text = b"one\ntwo\nthree\nfour\n";
        assert_eq!(last_lines(text, 2), b"three\nfour\n");
        assert_eq!(last_lines(text, 9), text);
        assert_eq!(last_lines(b"partial", 1), b"partial");
        assert_eq!(last_lines(b"a\nb", 1), b"b");
    }
}

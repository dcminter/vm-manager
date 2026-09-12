//! A client for QEMU's machine protocol.
//!
//! One newline-terminated JSON document per message, over a Unix socket. This
//! is the control channel for every instance operation that does not need the
//! guest's cooperation: powering down, pausing, querying, taking a screenshot.
//!
//! The transport is a type parameter so that the protocol can be tested
//! against a recorded conversation rather than a running hypervisor.

use crate::error::{Error, Result};
use crate::value::{Value, from_json, to_json_line};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// A wedged hypervisor must not wedge the tool with it.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// What a `sockaddr_un` holds, less its terminator.
pub const MAX_SOCKET_PATH: usize = 107;

/// A message the guest sent unprompted, such as `SHUTDOWN` or `RESET`.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub name: String,
    pub body: Value,
}

pub struct Client<R: BufRead, W: Write> {
    reader: R,
    writer: W,
    events: Vec<Event>,
    version: String,
    timeout: Duration,
}

impl<R: BufRead, W: Write> std::fmt::Debug for Client<R, W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("version", &self.version)
            .field("events", &self.events.len())
            .finish_non_exhaustive()
    }
}

/// What [`connect`] returns: the protocol over a Unix socket.
pub type Connection = Client<BufReader<UnixStream>, UnixStream>;

/// Opens a monitor socket and completes the handshake.
pub fn connect(path: &Path) -> Result<Connection> {
    connect_with_timeout(path, TIMEOUT)
}

pub fn connect_with_timeout(path: &Path, timeout: Duration) -> Result<Connection> {
    let fail = |source| Error::QmpConnect {
        path: path.to_owned(),
        source,
    };
    // A Unix socket path is bounded by the kernel, not by the filesystem, and
    // a state directory under a long home can cross it. QEMU refuses the same
    // path at launch, so say so here rather than reporting it as missing.
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(fail(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("the path is longer than the {MAX_SOCKET_PATH} bytes a Unix socket allows"),
        )));
    }
    let stream = UnixStream::connect(path).map_err(fail)?;
    stream.set_read_timeout(Some(timeout)).map_err(fail)?;
    stream.set_write_timeout(Some(timeout)).map_err(fail)?;
    let writer = stream.try_clone().map_err(fail)?;
    Client::with_timeout(BufReader::new(stream), writer, timeout)
}

impl<R: BufRead, W: Write> Client<R, W> {
    /// Reads the greeting and leaves capability negotiation behind, so that a
    /// caller never has to remember that commands are refused before it.
    pub fn new(reader: R, writer: W) -> Result<Self> {
        Self::with_timeout(reader, writer, TIMEOUT)
    }

    /// As [`Client::new`], told what patience the transport was given, so that
    /// a timeout can say how long it waited.
    pub fn with_timeout(reader: R, writer: W, timeout: Duration) -> Result<Self> {
        let mut client = Self {
            reader,
            writer,
            events: Vec::new(),
            version: String::new(),
            timeout,
        };
        let greeting = client.read_message()?;
        let Some(hello) = greeting.get("QMP") else {
            return Err(Error::QmpProtocol {
                reason: "the greeting is not a QMP greeting".to_owned(),
            });
        };
        client.version = hello
            .get("version")
            .and_then(|version| version.get("qemu"))
            .map_or_else(String::new, describe_version);
        client.execute("qmp_capabilities", None)?;
        Ok(client)
    }

    /// The QEMU version the far end reported, as `major.minor.micro`.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Runs a command and returns what it returned. Events that arrive while
    /// waiting are kept rather than discarded; see [`Client::take_events`].
    pub fn execute(&mut self, command: &str, arguments: Option<Value>) -> Result<Value> {
        let mut fields = vec![("execute".to_owned(), Value::string(command))];
        if let Some(arguments) = arguments {
            fields.push(("arguments".to_owned(), arguments));
        }
        self.writer
            .write_all(to_json_line(&Value::Map(fields)).as_bytes())
            .and_then(|()| self.writer.flush())
            .map_err(|source| self.classify(source))?;
        loop {
            let message = self.read_message()?;
            if let Some(error) = message.get("error") {
                return Err(Error::QmpCommand {
                    command: command.to_owned(),
                    class: field(error, "class"),
                    description: field(error, "desc"),
                });
            }
            if let Some(returned) = message.get("return") {
                return Ok(returned.clone());
            }
            if message.get("event").is_some() {
                self.remember(&message);
                continue;
            }
            return Err(Error::QmpProtocol {
                reason: "a message is neither a reply nor an event".to_owned(),
            });
        }
    }

    /// Reads whatever events have arrived without waiting for more. Used by
    /// the lifecycle to notice that a guest shut itself down.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// ACPI power button. The guest may ignore it, so the caller needs its own
    /// patience and its own fallback.
    pub fn powerdown(&mut self) -> Result<()> {
        self.execute("system_powerdown", None).map(|_| ())
    }

    /// Ends the hypervisor process without telling the guest.
    pub fn quit(&mut self) -> Result<()> {
        match self.execute("quit", None) {
            // A reply is success, and so is the socket closing under the
            // command: that is what the command does.
            Ok(_) | Err(Error::QmpClosed) => Ok(()),
            Err(other) => Err(other),
        }
    }

    pub fn pause(&mut self) -> Result<()> {
        self.execute("stop", None).map(|_| ())
    }

    pub fn resume(&mut self) -> Result<()> {
        self.execute("cont", None).map(|_| ())
    }

    /// The run state, as QEMU names it: `running`, `paused`, `prelaunch` and
    /// the rest. Passed through rather than mapped, so a state this tool has
    /// never heard of still reaches the user.
    pub fn status(&mut self) -> Result<String> {
        let reply = self.execute("query-status", None)?;
        reply
            .get("status")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| Error::QmpProtocol {
                reason: "query-status returned no status".to_owned(),
            })
    }

    /// A connection ending and a connection timing out are both outcomes the
    /// lifecycle has to act on, so neither is left inside a generic I/O fault.
    /// A guest that powers itself down leaves by this route.
    fn classify(&self, source: std::io::Error) -> Error {
        Self::classify_with(source, self.timeout)
    }

    fn classify_with(source: std::io::Error, timeout: Duration) -> Error {
        use std::io::ErrorKind::{
            BrokenPipe, ConnectionAborted, ConnectionReset, NotConnected, TimedOut, UnexpectedEof,
            WouldBlock,
        };
        match source.kind() {
            BrokenPipe | ConnectionReset | ConnectionAborted | NotConnected | UnexpectedEof => {
                Error::QmpClosed
            }
            WouldBlock | TimedOut => Error::QmpTimeout {
                seconds: timeout.as_secs(),
            },
            _ => Error::QmpIo { source },
        }
    }

    fn remember(&mut self, message: &Value) {
        if let Some(name) = message.get("event").and_then(Value::as_str) {
            self.events.push(Event {
                name: name.to_owned(),
                body: message.clone(),
            });
        }
    }

    fn read_message(&mut self) -> Result<Value> {
        let mut line = String::new();
        loop {
            line.clear();
            let read = self
                .reader
                .read_line(&mut line)
                .map_err(|source| Self::classify_with(source, self.timeout))?;
            if read == 0 {
                return Err(Error::QmpClosed);
            }
            if !line.trim().is_empty() {
                break;
            }
        }
        from_json(&line).map_err(|invalid| Error::QmpProtocol {
            reason: format!("{invalid}"),
        })
    }
}

fn field(error: &Value, key: &str) -> String {
    error
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("unspecified")
        .to_owned()
}

fn describe_version(qemu: &Value) -> String {
    let part = |key: &str| {
        qemu.get(key)
            .and_then(Value::as_integer)
            .map_or_else(|| "?".to_owned(), |held| held.to_string())
    };
    format!("{}.{}.{}", part("major"), part("minor"), part("micro"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::io::Cursor;

    const GREETING: &str = r#"{"QMP": {"version": {"qemu": {"micro": 2, "minor": 1, "major": 9}, "package": ""}, "capabilities": ["oob"]}}"#;

    /// A recorded conversation. The written side is kept so that what the
    /// client said can be asserted as well as what it understood.
    struct Wire {
        written: Vec<u8>,
    }

    impl Write for Wire {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn dialogue(lines: &[&str]) -> Cursor<Vec<u8>> {
        let mut text = String::from(GREETING);
        text.push('\n');
        text.push_str(r#"{"return": {}}"#);
        text.push('\n');
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        Cursor::new(text.into_bytes())
    }

    fn client(lines: &[&str]) -> Client<Cursor<Vec<u8>>, Wire> {
        Client::new(
            dialogue(lines),
            Wire {
                written: Vec::new(),
            },
        )
        .unwrap()
    }

    fn said(client: &Client<Cursor<Vec<u8>>, Wire>) -> String {
        String::from_utf8(client.writer.written.clone()).unwrap()
    }

    #[test]
    fn the_handshake_negotiates_capabilities_before_anything_else() {
        let client = client(&[]);
        assert_eq!(said(&client), "{\"execute\":\"qmp_capabilities\"}\n");
    }

    #[test]
    fn the_greeting_yields_the_version() {
        assert_eq!(client(&[]).version(), "9.1.2");
    }

    #[test]
    fn a_greeting_that_is_not_one_is_refused() {
        let wire = Cursor::new(b"{\"return\": {}}\n".to_vec());
        let error = Client::new(
            wire,
            Wire {
                written: Vec::new(),
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), "qmp-protocol-error");
    }

    #[test]
    fn a_closed_socket_during_the_handshake_is_refused() {
        let wire = Cursor::new(Vec::new());
        let error = Client::new(
            wire,
            Wire {
                written: Vec::new(),
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), "qmp-closed");
    }

    #[test]
    fn a_command_is_written_as_one_line() {
        let mut client = client(&[r#"{"return": {}}"#]);
        client.powerdown().unwrap();
        let said = said(&client);
        assert_eq!(said.lines().count(), 2);
        assert!(
            said.ends_with("{\"execute\":\"system_powerdown\"}\n"),
            "{said}"
        );
    }

    #[test]
    fn arguments_travel_with_the_command() {
        let mut client = client(&[r#"{"return": {}}"#]);
        client
            .execute(
                "screendump",
                Some(Value::map([("filename", Value::string("/tmp/a.ppm"))])),
            )
            .unwrap();
        assert!(
            said(&client).contains(
                "{\"execute\":\"screendump\",\"arguments\":{\"filename\":\"/tmp/a.ppm\"}}"
            ),
            "{}",
            said(&client)
        );
    }

    #[test]
    fn a_reply_is_returned_as_it_arrived() {
        let mut client = client(&[r#"{"return": {"status": "running", "running": true}}"#]);
        let reply = client.execute("query-status", None).unwrap();
        assert_eq!(reply.get("running"), Some(&Value::Bool(true)));
    }

    #[test]
    fn the_status_is_passed_through_rather_than_interpreted() {
        let mut client = client(&[r#"{"return": {"status": "guest-panicked"}}"#]);
        assert_eq!(client.status().unwrap(), "guest-panicked");
    }

    #[test]
    fn a_status_reply_without_a_status_is_a_protocol_fault() {
        let mut client = client(&[r#"{"return": {}}"#]);
        assert_eq!(client.status().unwrap_err().kind(), "qmp-protocol-error");
    }

    #[test]
    fn a_refusal_names_the_command_and_the_reason() {
        let mut client =
            client(&[r#"{"error": {"class": "GenericError", "desc": "Invalid parameter 'x'"}}"#]);
        let error = client.execute("screendump", None).unwrap_err();
        assert_eq!(error.kind(), "qmp-command-failed");
        let text = error.to_string();
        assert!(text.contains("screendump"), "{text}");
        assert!(text.contains("GenericError"), "{text}");
        assert!(text.contains("Invalid parameter"), "{text}");
    }

    #[test]
    fn an_error_missing_its_fields_still_reports() {
        let mut client = client(&[r#"{"error": {}}"#]);
        let error = client.execute("quit", None).unwrap_err();
        assert!(error.to_string().contains("unspecified"), "{error}");
    }

    #[test]
    fn events_arriving_before_a_reply_do_not_hide_it() {
        let mut client = client(&[
            r#"{"event": "RESET", "timestamp": {"seconds": 1, "microseconds": 2}}"#,
            r#"{"event": "STOP", "timestamp": {"seconds": 1, "microseconds": 3}}"#,
            r#"{"return": {"status": "paused"}}"#,
        ]);
        assert_eq!(client.status().unwrap(), "paused");
        let events = client.take_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].name, "RESET");
        assert_eq!(events[1].name, "STOP");
    }

    #[test]
    fn events_are_taken_once() {
        let mut client = client(&[r#"{"event": "SHUTDOWN"}"#, r#"{"return": {}}"#]);
        client.powerdown().unwrap();
        assert_eq!(client.take_events().len(), 1);
        assert!(client.take_events().is_empty());
    }

    #[test]
    fn a_blank_line_is_not_a_message() {
        let mut client = client(&["", "   ", r#"{"return": {}}"#]);
        assert!(client.powerdown().is_ok());
    }

    #[test]
    fn a_message_that_is_not_json_is_a_protocol_fault() {
        let mut client = client(&["this is not json"]);
        assert_eq!(client.powerdown().unwrap_err().kind(), "qmp-protocol-error");
    }

    #[test]
    fn a_message_that_is_neither_reply_nor_event_is_a_protocol_fault() {
        let mut client = client(&[r#"{"something": 1}"#]);
        assert_eq!(client.powerdown().unwrap_err().kind(), "qmp-protocol-error");
    }

    /// A guest that powers itself down ends the connection, so this is an
    /// outcome the lifecycle acts on rather than an I/O fault to report.
    #[test]
    fn a_socket_that_closes_while_waiting_is_reported_as_closed() {
        let mut client = client(&[]);
        assert_eq!(client.status().unwrap_err().kind(), "qmp-closed");
    }

    /// A writer that carries the handshake and then fails the way a socket
    /// does when the process at the far end has gone.
    struct Broken {
        kind: std::io::ErrorKind,
        allowed: usize,
    }

    impl Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.allowed > 0 {
                self.allowed -= 1;
                return Ok(bytes.len());
            }
            Err(std::io::Error::new(self.kind, "as the far end went away"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn broken(kind: std::io::ErrorKind) -> Client<Cursor<Vec<u8>>, Broken> {
        Client::new(dialogue(&[]), Broken { kind, allowed: 1 }).unwrap()
    }

    #[test]
    fn a_pipe_that_breaks_under_a_command_is_a_closed_connection() {
        for kind in [
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::UnexpectedEof,
        ] {
            let mut client = broken(kind);
            assert_eq!(
                client.powerdown().unwrap_err().kind(),
                "qmp-closed",
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_transport_that_times_out_says_how_long_it_waited() {
        for kind in [std::io::ErrorKind::WouldBlock, std::io::ErrorKind::TimedOut] {
            let mut client = broken(kind);
            let error = client.powerdown().unwrap_err();
            assert_eq!(error.kind(), "qmp-timeout", "{kind:?}");
            assert!(
                error.to_string().contains(&TIMEOUT.as_secs().to_string()),
                "{error}"
            );
        }
    }

    #[test]
    fn any_other_fault_is_still_reported_as_one() {
        let mut client = broken(std::io::ErrorKind::PermissionDenied);
        assert_eq!(client.powerdown().unwrap_err().kind(), "qmp-io-error");
    }

    #[test]
    fn a_socket_path_too_long_for_the_kernel_says_so() {
        let path = std::path::PathBuf::from(format!("/tmp/{}.sock", "x".repeat(MAX_SOCKET_PATH)));
        let error = connect(&path).unwrap_err();
        assert_eq!(error.kind(), "qmp-unreachable");
        assert!(error.to_string().contains("longer than"), "{error}");
    }

    /// `quit` takes the far end down, so the socket closing under it is the
    /// command succeeding rather than failing.
    #[test]
    fn quit_treats_the_connection_ending_as_success() {
        let mut client = client(&[]);
        assert!(client.quit().is_ok());
    }

    #[test]
    fn quit_still_reports_a_refusal() {
        let mut client = client(&[r#"{"error": {"class": "CommandNotFound", "desc": "no"}}"#]);
        assert_eq!(client.quit().unwrap_err().kind(), "qmp-command-failed");
    }

    #[test]
    fn pausing_and_resuming_use_the_names_qemu_uses() {
        let mut client = client(&[r#"{"return": {}}"#, r#"{"return": {}}"#]);
        client.pause().unwrap();
        client.resume().unwrap();
        let said = said(&client);
        assert!(said.contains("\"stop\""), "{said}");
        assert!(said.contains("\"cont\""), "{said}");
    }

    #[test]
    fn an_absent_socket_is_reported_as_unreachable() {
        let error = connect(Path::new("/nonexistent/vm/monitor.sock")).unwrap_err();
        assert_eq!(error.kind(), "qmp-unreachable");
    }
}

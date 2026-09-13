//! Helpers shared by tests.

#![allow(clippy::unwrap_used)]

use std::io::{BufRead as _, Write as _};
use std::path::Path;

/// Answers one request with `body`, returning the URL to ask.
pub fn serve(body: Vec<u8>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
            line.clear();
        }
        let mut stream = stream;
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(&body);
    });
    format!("http://{address}/image")
}

/// Makes an image with `qemu-img`, saying whether it could.
pub fn create_image(path: &Path, format: &str, size: &str) -> bool {
    std::process::Command::new("qemu-img")
        .args(["create", "-q", "-f", format])
        .arg(path)
        .arg(size)
        .status()
        .is_ok_and(|status| status.success())
}

/// Compresses with the real tool.
pub fn pack(tool: &str, plain: &[u8]) -> Vec<u8> {
    let mut child = std::process::Command::new(tool)
        .arg("-c")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(plain).unwrap();
    let finished = child.wait_with_output().unwrap();
    assert!(finished.status.success(), "{tool} would not pack");
    finished.stdout
}

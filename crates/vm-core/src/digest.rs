use crate::reference::{Algorithm, Digest};
use sha2::{Digest as _, Sha256, Sha512};
use std::io::{self, Read, Write};

/// Hashes everything written through it, so a download is verified in the same
/// pass that writes it to disk.
pub struct Hashing<W> {
    inner: W,
    state: State,
    written: u64,
}

enum State {
    Sha256(Box<Sha256>),
    Sha512(Box<Sha512>),
}

impl State {
    fn new(algorithm: Algorithm) -> Self {
        match algorithm {
            Algorithm::Sha256 => Self::Sha256(Box::new(Sha256::new())),
            Algorithm::Sha512 => Self::Sha512(Box::new(Sha512::new())),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha256(state) => state.update(bytes),
            Self::Sha512(state) => state.update(bytes),
        }
    }

    fn finish(self) -> String {
        fn hex(bytes: &[u8]) -> String {
            bytes.iter().fold(String::new(), |mut text, byte| {
                use std::fmt::Write as _;
                let _ = write!(text, "{byte:02x}");
                text
            })
        }
        match self {
            Self::Sha256(state) => hex(&state.finalize()),
            Self::Sha512(state) => hex(&state.finalize()),
        }
    }
}

impl<W: Write> Hashing<W> {
    pub fn new(inner: W, algorithm: Algorithm) -> Self {
        Self {
            inner,
            state: State::new(algorithm),
            written: 0,
        }
    }

    pub const fn written(&self) -> u64 {
        self.written
    }

    /// The hex digest of everything written so far.
    pub fn finish(self) -> (W, String) {
        (self.inner, self.state.finish())
    }
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(buffer)?;
        self.state.update(&buffer[..count]);
        self.written += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Copies `source` into `sink`, hashing as it goes and reporting progress.
pub fn copy_hashing<R: Read, W: Write>(
    source: &mut R,
    sink: W,
    algorithm: Algorithm,
    progress: &mut dyn FnMut(u64),
) -> io::Result<(W, String)> {
    let mut hashing = Hashing::new(sink, algorithm);
    let mut buffer = vec![0u8; 128 * 1024];
    loop {
        let count = match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hashing.write_all(&buffer[..count])?;
        progress(hashing.written());
    }
    hashing.flush()?;
    Ok(hashing.finish())
}

pub fn matches(expected: &Digest, actual_hex: &str) -> bool {
    expected.hex() == actual_hex
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::str::FromStr;

    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const ABC_SHA512: &str = "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
                              2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f";

    fn hash(bytes: &[u8], algorithm: Algorithm) -> String {
        let mut source = bytes;
        let mut ignored = |_| {};
        let (_, hex) = copy_hashing(&mut source, Vec::new(), algorithm, &mut ignored).unwrap();
        hex
    }

    #[test]
    fn known_vectors_are_reproduced() {
        assert_eq!(hash(b"", Algorithm::Sha256), EMPTY_SHA256);
        assert_eq!(hash(b"abc", Algorithm::Sha256), ABC_SHA256);
        assert_eq!(hash(b"abc", Algorithm::Sha512), ABC_SHA512.replace(' ', ""));
    }

    #[test]
    fn the_payload_reaches_the_sink_intact() {
        let mut source = &b"hello world"[..];
        let mut ignored = |_| {};
        let (sink, _) =
            copy_hashing(&mut source, Vec::new(), Algorithm::Sha256, &mut ignored).unwrap();
        assert_eq!(sink, b"hello world");
    }

    #[test]
    fn progress_is_reported_and_ends_at_the_total() {
        let payload = vec![7u8; 300 * 1024];
        let mut source = payload.as_slice();
        let mut seen = Vec::new();
        let mut record = |count| seen.push(count);
        copy_hashing(&mut source, Vec::new(), Algorithm::Sha256, &mut record).unwrap();
        assert!(
            seen.len() > 1,
            "a large payload should report more than once"
        );
        assert_eq!(seen.last().copied(), Some(payload.len() as u64));
        assert!(
            seen.windows(2).all(|pair| pair[0] < pair[1]),
            "progress must increase"
        );
    }

    #[test]
    fn a_digest_matches_only_its_own_payload() {
        let expected = Digest::from_str(&format!("sha256:{ABC_SHA256}")).unwrap();
        assert!(matches(&expected, &hash(b"abc", Algorithm::Sha256)));
        assert!(!matches(&expected, &hash(b"abd", Algorithm::Sha256)));
    }

    #[test]
    fn hashing_counts_what_it_wrote() {
        let mut hashing = Hashing::new(Vec::new(), Algorithm::Sha256);
        hashing.write_all(b"12345").unwrap();
        assert_eq!(hashing.written(), 5);
    }
}

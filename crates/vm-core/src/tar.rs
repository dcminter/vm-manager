use std::io::{self, Read};

const BLOCK: usize = 512;
const NAME: std::ops::Range<usize> = 0..100;
const SIZE: std::ops::Range<usize> = 124..136;
const TYPEFLAG: usize = 156;
const PREFIX: std::ops::Range<usize> = 345..500;

/// A regular file lifted out of an archive. Nothing else is represented,
/// because nothing else is ever extracted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub path: String,
    pub contents: Vec<u8>,
}

#[derive(Debug)]
pub enum Malformed {
    Truncated,
    BadHeader,
    BadSize,
    TooLarge { path: String, size: u64 },
}

impl std::fmt::Display for Malformed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "the archive ends mid-entry"),
            Self::BadHeader => write!(f, "an entry header is not a tar header"),
            Self::BadSize => write!(f, "an entry declares an unreadable size"),
            Self::TooLarge { path, size } => {
                write!(
                    f,
                    "entry '{path}' is {size} bytes, beyond the permitted size"
                )
            }
        }
    }
}

/// Reads the regular files from a tar stream, ignoring directories, symlinks,
/// devices and extension headers. `limit` caps any single file.
pub fn read(source: &mut impl Read, limit: u64) -> io::Result<Result<Vec<File>, Malformed>> {
    let mut files = Vec::new();
    let mut header = [0u8; BLOCK];
    loop {
        match read_exact_or_end(source, &mut header)? {
            Filled::End => return Ok(Ok(files)),
            Filled::Partial => return Ok(Err(Malformed::Truncated)),
            Filled::Whole => {}
        }
        if header.iter().all(|byte| *byte == 0) {
            return Ok(Ok(files));
        }
        if !looks_like_a_header(&header) {
            return Ok(Err(Malformed::BadHeader));
        }
        let Some(size) = octal(&header[SIZE]) else {
            return Ok(Err(Malformed::BadSize));
        };
        let path = entry_path(&header);
        let regular = matches!(header[TYPEFLAG], b'0' | 0);
        if regular && size > limit {
            return Ok(Err(Malformed::TooLarge { path, size }));
        }
        let padded = size.div_ceil(BLOCK as u64) * BLOCK as u64;
        if regular {
            let mut contents = vec![0u8; usize::try_from(size).unwrap_or(0)];
            if source.read_exact(&mut contents).is_err() {
                return Ok(Err(Malformed::Truncated));
            }
            skip(source, padded - size)?;
            files.push(File { path, contents });
        } else {
            skip(source, padded)?;
        }
    }
}

enum Filled {
    Whole,
    Partial,
    End,
}

fn read_exact_or_end(source: &mut impl Read, buffer: &mut [u8]) -> io::Result<Filled> {
    let mut filled = 0;
    while filled < buffer.len() {
        match source.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(match filled {
        0 => Filled::End,
        count if count == buffer.len() => Filled::Whole,
        _ => Filled::Partial,
    })
}

fn skip(source: &mut impl Read, mut count: u64) -> io::Result<()> {
    let mut scratch = [0u8; BLOCK];
    while count > 0 {
        let wanted = usize::try_from(count.min(BLOCK as u64)).unwrap_or(BLOCK);
        match read_exact_or_end(source, &mut scratch[..wanted])? {
            Filled::Whole => count -= wanted as u64,
            Filled::Partial | Filled::End => return Ok(()),
        }
    }
    Ok(())
}

/// Both the POSIX and the older GNU magic are accepted; anything else is not a
/// tar stream we should be reading.
fn looks_like_a_header(header: &[u8; BLOCK]) -> bool {
    let magic = &header[257..263];
    magic == b"ustar\0" || magic == b"ustar " || magic == b"\0\0\0\0\0\0"
}

fn entry_path(header: &[u8; BLOCK]) -> String {
    let name = trimmed(&header[NAME]);
    let prefix = trimmed(&header[PREFIX]);
    if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    }
}

fn trimmed(field: &[u8]) -> String {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).trim().to_owned()
}

fn octal(field: &[u8]) -> Option<u64> {
    let text = trimmed(field);
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(&text, 8).ok()
}

/// A path is safe to join onto a directory only when it stays inside it.
pub fn is_safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path
            .split('/')
            .any(|part| part == ".." || part == "." || part.is_empty())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// Builds a ustar entry so the tests do not depend on an external tar.
    fn entry(path: &str, typeflag: u8, contents: &[u8]) -> Vec<u8> {
        let mut header = [0u8; BLOCK];
        let name = path.as_bytes();
        header[..name.len()].copy_from_slice(name);
        let size = format!("{:011o}\0", contents.len());
        header[SIZE.start..SIZE.start + size.len()].copy_from_slice(size.as_bytes());
        header[TYPEFLAG] = typeflag;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let mut block = header.to_vec();
        block.extend_from_slice(contents);
        let padding = (BLOCK - contents.len() % BLOCK) % BLOCK;
        block.extend(std::iter::repeat_n(0u8, padding));
        block
    }

    fn archive(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes: Vec<u8> = parts.iter().flatten().copied().collect();
        bytes.extend(std::iter::repeat_n(0u8, BLOCK * 2));
        bytes
    }

    fn read_all(bytes: &[u8]) -> Vec<File> {
        let mut source = bytes;
        read(&mut source, 1 << 20).unwrap().unwrap()
    }

    #[test]
    fn a_regular_file_is_read_with_its_contents() {
        let bytes = archive(&[entry("a.toml", b'0', b"name = \"x\"")]);
        let files = read_all(&bytes);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "a.toml");
        assert_eq!(files[0].contents, b"name = \"x\"");
    }

    #[test]
    fn several_files_are_read_in_order() {
        let bytes = archive(&[
            entry("a.toml", b'0', b"first"),
            entry("b.toml", b'0', b"second"),
        ]);
        let files = read_all(&bytes);
        assert_eq!(files.len(), 2);
        assert_eq!(files[1].contents, b"second");
    }

    #[test]
    fn a_file_spanning_several_blocks_is_read_whole() {
        let payload = vec![b'x'; BLOCK * 3 + 17];
        let bytes = archive(&[entry("big.toml", b'0', &payload)]);
        assert_eq!(read_all(&bytes)[0].contents, payload);
    }

    #[test]
    fn an_empty_file_is_read_as_empty() {
        let bytes = archive(&[entry("empty.toml", b'0', b"")]);
        let files = read_all(&bytes);
        assert_eq!(files.len(), 1);
        assert!(files[0].contents.is_empty());
    }

    #[test]
    fn a_typeflag_of_zero_is_also_a_regular_file() {
        let bytes = archive(&[entry("old.toml", 0, b"payload")]);
        assert_eq!(read_all(&bytes).len(), 1);
    }

    #[test]
    fn directories_symlinks_and_devices_are_skipped() {
        let bytes = archive(&[
            entry("dir/", b'5', b""),
            entry("link", b'2', b""),
            entry("dev", b'3', b""),
            entry("real.toml", b'0', b"kept"),
        ]);
        let files = read_all(&bytes);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "real.toml");
    }

    #[test]
    fn an_extension_header_is_skipped_along_with_its_payload() {
        let bytes = archive(&[
            entry("pax_global_header", b'g', b"52 comment=abcdef\n"),
            entry("real.toml", b'0', b"kept"),
        ]);
        let files = read_all(&bytes);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].contents, b"kept");
    }

    #[test]
    fn a_prefixed_name_is_rejoined() {
        let mut block = entry("trixie.toml", b'0', b"x");
        let prefix = b"catalogue/debian";
        block[PREFIX.start..PREFIX.start + prefix.len()].copy_from_slice(prefix);
        let bytes = archive(&[block]);
        assert_eq!(read_all(&bytes)[0].path, "catalogue/debian/trixie.toml");
    }

    #[test]
    fn the_archive_ends_at_the_zero_blocks() {
        let mut bytes = archive(&[entry("a.toml", b'0', b"x")]);
        bytes.extend_from_slice(&entry("ignored.toml", b'0', b"y"));
        assert_eq!(read_all(&bytes).len(), 1);
    }

    #[test]
    fn a_stream_that_is_not_a_tar_is_refused() {
        let bytes = vec![b'z'; BLOCK * 2];
        let mut source = bytes.as_slice();
        let outcome = read(&mut source, 1 << 20).unwrap();
        assert!(matches!(outcome, Err(Malformed::BadHeader)), "{outcome:?}");
    }

    #[test]
    fn a_stream_shorter_than_one_header_is_refused() {
        let mut source = &b"too short to be a header"[..];
        let outcome = read(&mut source, 1 << 20).unwrap();
        assert!(matches!(outcome, Err(Malformed::Truncated)), "{outcome:?}");
    }

    #[test]
    fn a_truncated_entry_is_refused() {
        let mut bytes = entry("a.toml", b'0', &vec![b'x'; BLOCK * 2]);
        bytes.truncate(BLOCK + 10);
        let mut source = bytes.as_slice();
        let outcome = read(&mut source, 1 << 20).unwrap();
        assert!(matches!(outcome, Err(Malformed::Truncated)), "{outcome:?}");
    }

    #[test]
    fn a_file_beyond_the_limit_is_refused_before_it_is_read() {
        let bytes = archive(&[entry("big.toml", b'0', &vec![b'x'; 4096])]);
        let mut source = bytes.as_slice();
        let outcome = read(&mut source, 1024).unwrap();
        match outcome {
            Err(Malformed::TooLarge { path, size }) => {
                assert_eq!(path, "big.toml");
                assert_eq!(size, 4096);
            }
            other => panic!("expected a size refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_stream_yields_nothing() {
        let mut source = &b""[..];
        assert!(read(&mut source, 1 << 20).unwrap().unwrap().is_empty());
    }

    #[test]
    fn ordinary_relative_paths_are_safe() {
        assert!(is_safe_path("catalogue/debian/trixie.toml"));
        assert!(is_safe_path("a.toml"));
    }

    #[test]
    fn escaping_paths_are_not_safe() {
        for path in [
            "",
            "/etc/passwd",
            "../outside.toml",
            "catalogue/../../outside.toml",
            "catalogue/./trixie.toml",
            "catalogue//trixie.toml",
            "catalogue\\windows.toml",
        ] {
            assert!(!is_safe_path(path), "{path} should be refused");
        }
    }
}

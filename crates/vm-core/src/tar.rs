use std::io::{self, Read, Seek, Write};

const BLOCK: usize = 512;
const NAME: std::ops::Range<usize> = 0..100;
const SIZE: std::ops::Range<usize> = 124..136;
const CHECKSUM: std::ops::Range<usize> = 148..156;
const TYPEFLAG: usize = 156;
const MAGIC: std::ops::Range<usize> = 257..263;
const PREFIX: std::ops::Range<usize> = 345..500;
/// The longest name or extended header an archive may give an entry.
const MAX_METADATA: u64 = 1 << 20;
/// How much of an extracted file is read at once, and skipped when it is all zeros.
const CHUNK: usize = 1 << 16;

/// A regular file from an archive.
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
    Sparse { path: String },
    Missing { member: String },
    Several { paths: Vec<String> },
    Empty,
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
            Self::Sparse { path } => {
                write!(
                    f,
                    "entry '{path}' has a sparse map that is not supported or does not fit the file"
                )
            }
            Self::Missing { member } => write!(f, "the archive holds no file '{member}'"),
            Self::Several { paths } => write!(
                f,
                "the archive holds several files ({}), so which is the image is unclear",
                paths.join(", ")
            ),
            Self::Empty => write!(f, "the archive holds no file"),
        }
    }
}

/// Which file of an archive to extract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wanted<'a> {
    Named(&'a str),
    /// The only regular file, refusing an archive with more than one.
    Only,
}

/// Whether a block is a tar header: ustar magic and a correct checksum.
pub fn is_archive(head: &[u8]) -> bool {
    let Ok(header) = <&[u8; BLOCK]>::try_from(head.get(..BLOCK).unwrap_or_default()) else {
        return false;
    };
    let magic = &header[MAGIC];
    (magic == b"ustar\0" || magic == b"ustar ") && checksum_matches(header)
}

fn checksum_matches(header: &[u8; BLOCK]) -> bool {
    let Some(stated) = octal(&header[CHECKSUM]) else {
        return false;
    };
    let sum: u64 = header
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            if CHECKSUM.contains(&index) {
                u64::from(b' ')
            } else {
                u64::from(*byte)
            }
        })
        .sum();
    sum == stated
}

/// Extracts one file into `destination`, reading the stream to its end so no writer is left blocked.
pub fn extract(
    source: &mut impl Read,
    wanted: Wanted<'_>,
    destination: &mut std::fs::File,
) -> io::Result<Result<String, Malformed>> {
    let outcome = extract_entries(source, wanted, destination);
    io::copy(source, &mut io::sink())?;
    outcome
}

fn extract_entries(
    source: &mut impl Read,
    wanted: Wanted<'_>,
    destination: &mut std::fs::File,
) -> io::Result<Result<String, Malformed>> {
    let mut extracted: Option<String> = None;
    let mut others = Vec::new();
    loop {
        let entry = match next_entry(source)? {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(malformed) => return Ok(Err(malformed)),
        };
        let padded = entry.size.div_ceil(BLOCK as u64) * BLOCK as u64;
        if !entry.regular {
            skip(source, padded)?;
            continue;
        }
        let matched = extracted.is_none()
            && match wanted {
                Wanted::Named(member) => entry.path.trim_start_matches("./") == member,
                Wanted::Only => true,
            };
        if !matched {
            skip(source, padded)?;
            others.push(entry.path);
            continue;
        }
        let consumed = match write_entry(source, &entry, destination)? {
            Ok(consumed) => consumed,
            Err(malformed) => return Ok(Err(malformed)),
        };
        skip(source, padded - consumed)?;
        extracted = Some(entry.path);
    }
    Ok(match (wanted, extracted) {
        (Wanted::Only, Some(path)) if !others.is_empty() => Err(Malformed::Several {
            paths: std::iter::once(path).chain(others).collect(),
        }),
        (_, Some(path)) => Ok(path),
        (Wanted::Named(member), None) => Err(Malformed::Missing {
            member: member.to_owned(),
        }),
        (Wanted::Only, None) => Err(Malformed::Empty),
    })
}

/// Offsets in a file and the lengths of data stored for them.
type Regions = Vec<(u64, u64)>;

/// How an entry's data maps onto the file it holds.
enum Layout {
    Whole,
    /// Old GNU sparse: these regions of a file of this length, in order.
    Regions {
        regions: Regions,
        length: u64,
    },
    /// pax sparse 1.0: the region map leads the data.
    MappedData {
        length: u64,
    },
    Unsupported,
}

/// An entry's header, with any GNU long name or pax extended header applied.
struct Entry {
    path: String,
    /// The bytes the entry occupies in the archive, before padding.
    size: u64,
    regular: bool,
    layout: Layout,
}

fn next_entry(source: &mut impl Read) -> io::Result<Result<Option<Entry>, Malformed>> {
    let mut long_name: Option<String> = None;
    let mut extended: Option<Extended> = None;
    let mut header = [0u8; BLOCK];
    loop {
        match read_exact_or_end(source, &mut header)? {
            Filled::End => return Ok(Ok(None)),
            Filled::Partial => return Ok(Err(Malformed::Truncated)),
            Filled::Whole => {}
        }
        if header.iter().all(|byte| *byte == 0) {
            return Ok(Ok(None));
        }
        if !looks_like_a_header(&header) || !checksum_matches(&header) {
            return Ok(Err(Malformed::BadHeader));
        }
        let Some(size) = size_field(&header[SIZE]) else {
            return Ok(Err(Malformed::BadSize));
        };
        let padded = size.div_ceil(BLOCK as u64) * BLOCK as u64;
        match header[TYPEFLAG] {
            b'L' | b'x' => {
                if size > MAX_METADATA {
                    return Ok(Err(Malformed::TooLarge {
                        path: entry_path(&header),
                        size,
                    }));
                }
                let mut contents = vec![0u8; usize::try_from(size).unwrap_or(0)];
                if source.read_exact(&mut contents).is_err() {
                    return Ok(Err(Malformed::Truncated));
                }
                skip(source, padded - size)?;
                if header[TYPEFLAG] == b'L' {
                    long_name = Some(trimmed(&contents));
                } else {
                    extended = Some(Extended::parse(&contents));
                }
            }
            b'g' => skip(source, padded)?,
            flag => {
                let extended = extended.unwrap_or_default();
                let layout = if flag == b'S' {
                    match old_gnu_regions(source, &header)? {
                        Ok(layout) => layout,
                        Err(malformed) => return Ok(Err(malformed)),
                    }
                } else {
                    extended.layout()
                };
                return Ok(Ok(Some(Entry {
                    path: extended
                        .sparse_name
                        .or(extended.path)
                        .or(long_name)
                        .unwrap_or_else(|| entry_path(&header)),
                    size: extended.size.unwrap_or(size),
                    regular: matches!(flag, b'0' | b'7' | 0 | b'S'),
                    layout,
                })));
            }
        }
    }
}

/// Reads an old GNU sparse map: four regions in the header, then extension blocks of 21.
fn old_gnu_regions(
    source: &mut impl Read,
    header: &[u8; BLOCK],
) -> io::Result<Result<Layout, Malformed>> {
    const REGIONS: usize = 386;
    const EXTENDED: usize = 482;
    const LENGTH: std::ops::Range<usize> = 483..495;
    let Some(length) = size_field(&header[LENGTH]) else {
        return Ok(Err(Malformed::BadSize));
    };
    let mut regions = Vec::new();
    let mut block = header.to_vec();
    let (mut start, mut count, mut flag) = (REGIONS, 4, EXTENDED);
    loop {
        for index in 0..count {
            let at = start + index * 24;
            let (offset, bytes) = (&block[at..at + 12], &block[at + 12..at + 24]);
            if offset.iter().all(|byte| *byte == 0) {
                break;
            }
            match (size_field(offset), size_field(bytes)) {
                (Some(offset), Some(bytes)) => regions.push((offset, bytes)),
                _ => return Ok(Err(Malformed::BadSize)),
            }
        }
        if block[flag] == 0 {
            return Ok(Ok(Layout::Regions { regions, length }));
        }
        let mut extension = [0u8; BLOCK];
        if !matches!(read_exact_or_end(source, &mut extension)?, Filled::Whole) {
            return Ok(Err(Malformed::Truncated));
        }
        block = extension.to_vec();
        (start, count, flag) = (0, 21, 504);
    }
}

/// Writes an entry's data to `destination`, returning the archive bytes it used.
fn write_entry(
    source: &mut impl Read,
    entry: &Entry,
    destination: &mut std::fs::File,
) -> io::Result<Result<u64, Malformed>> {
    let sparse = || Malformed::Sparse {
        path: entry.path.clone(),
    };
    let (regions, length, map_bytes) = match &entry.layout {
        Layout::Whole => (vec![(0, entry.size)], entry.size, 0),
        Layout::Regions { regions, length } => (regions.clone(), *length, 0),
        Layout::MappedData { length } => match read_map(source, entry.size)? {
            Ok((regions, map_bytes)) => (regions, *length, map_bytes),
            Err(malformed) => return Ok(Err(malformed)),
        },
        Layout::Unsupported => return Ok(Err(sparse())),
    };
    let stored = regions
        .iter()
        .try_fold(map_bytes, |total: u64, (_, bytes)| {
            total.checked_add(*bytes)
        });
    if stored.is_none_or(|stored| stored > entry.size)
        || regions
            .iter()
            .any(|(offset, bytes)| offset.checked_add(*bytes).is_none_or(|end| end > length))
    {
        return Ok(Err(sparse()));
    }
    for (offset, bytes) in &regions {
        destination.seek(io::SeekFrom::Start(*offset))?;
        if let Err(malformed) = copy_skipping_zeros(source, *bytes, destination)? {
            return Ok(Err(malformed));
        }
    }
    destination.set_len(length)?;
    Ok(Ok(stored.unwrap_or(entry.size)))
}

/// Reads the decimal region map that leads pax sparse 1.0 data, padded to a block.
fn read_map(source: &mut impl Read, size: u64) -> io::Result<Result<(Regions, u64), Malformed>> {
    let mut consumed = 0u64;
    let mut block = [0u8; BLOCK];
    let mut text = Vec::new();
    let mut numbers = Vec::new();
    let mut wanted: Option<usize> = None;
    while wanted.is_none_or(|count| numbers.len() < 1 + count * 2) {
        if consumed + BLOCK as u64 > size || consumed > MAX_METADATA {
            return Ok(Err(Malformed::BadSize));
        }
        if !matches!(read_exact_or_end(source, &mut block)?, Filled::Whole) {
            return Ok(Err(Malformed::Truncated));
        }
        consumed += BLOCK as u64;
        for byte in block {
            if byte != b'\n' {
                text.push(byte);
                continue;
            }
            let Some(number) = std::str::from_utf8(&text)
                .ok()
                .and_then(|digits| digits.parse::<u64>().ok())
            else {
                return Ok(Err(Malformed::BadSize));
            };
            text.clear();
            if wanted.is_none() {
                wanted = usize::try_from(number).ok();
            }
            numbers.push(number);
            if wanted.is_some_and(|count| numbers.len() == 1 + count * 2) {
                break;
            }
        }
    }
    let regions = numbers[1..]
        .chunks(2)
        .map(|pair| (pair[0], pair[1]))
        .collect();
    Ok(Ok((regions, consumed)))
}

/// What a pax extended header says about the entry after it.
#[derive(Default)]
struct Extended {
    path: Option<String>,
    size: Option<u64>,
    sparse_name: Option<String>,
    sparse_length: Option<u64>,
    sparse_version: Option<(String, String)>,
    sparse: bool,
}

impl Extended {
    /// Reads records of the form `LENGTH KEY=VALUE\n`.
    fn parse(contents: &[u8]) -> Self {
        let mut extended = Self::default();
        let mut major = None;
        let mut minor = None;
        let mut rest = contents;
        while let Some(space) = rest.iter().position(|byte| *byte == b' ') {
            let Some(length) = std::str::from_utf8(&rest[..space])
                .ok()
                .and_then(|text| text.parse::<usize>().ok())
                .filter(|length| *length > space + 1 && *length <= rest.len())
            else {
                break;
            };
            let record = &rest[space + 1..length];
            let record = record.strip_suffix(b"\n").unwrap_or(record);
            if let Some(equals) = record.iter().position(|byte| *byte == b'=') {
                let key = &record[..equals];
                let value = String::from_utf8_lossy(&record[equals + 1..]).into_owned();
                match key {
                    b"path" => extended.path = Some(value),
                    b"size" => extended.size = value.parse().ok(),
                    b"GNU.sparse.name" => extended.sparse_name = Some(value),
                    b"GNU.sparse.realsize" => extended.sparse_length = value.parse().ok(),
                    b"GNU.sparse.major" => major = Some(value),
                    b"GNU.sparse.minor" => minor = Some(value),
                    key if key.starts_with(b"GNU.sparse") => extended.sparse = true,
                    _ => {}
                }
            }
            rest = &rest[length..];
        }
        extended.sparse |= major.is_some() || minor.is_some();
        extended.sparse_version = major.zip(minor);
        extended
    }

    fn layout(&self) -> Layout {
        match (&self.sparse_version, self.sparse_length) {
            (Some((major, minor)), Some(length)) if major == "1" && minor == "0" => {
                Layout::MappedData { length }
            }
            _ if self.sparse => Layout::Unsupported,
            _ => Layout::Whole,
        }
    }
}

/// Copies `size` bytes at the file's position, seeking over whole chunks of zeros.
fn copy_skipping_zeros(
    source: &mut impl Read,
    size: u64,
    destination: &mut std::fs::File,
) -> io::Result<Result<(), Malformed>> {
    let mut buffer = vec![0u8; CHUNK];
    let mut remaining = size;
    while remaining > 0 {
        let wanted = usize::try_from(remaining.min(CHUNK as u64)).unwrap_or(CHUNK);
        let chunk = &mut buffer[..wanted];
        if !matches!(read_exact_or_end(source, chunk)?, Filled::Whole) {
            return Ok(Err(Malformed::Truncated));
        }
        if chunk.iter().all(|byte| *byte == 0) {
            destination.seek(io::SeekFrom::Current(i64::try_from(wanted).unwrap_or(0)))?;
        } else {
            destination.write_all(chunk)?;
        }
        remaining -= wanted as u64;
    }
    Ok(Ok(()))
}

/// Reads the regular files from a tar stream, each capped at `limit` bytes.
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
        let Some(size) = size_field(&header[SIZE]) else {
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

/// Whether the block carries POSIX or GNU tar magic.
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

/// A size in octal, or in GNU base-256 where the top bit of the first byte is set.
fn size_field(field: &[u8]) -> Option<u64> {
    match field.first() {
        Some(first) if first & 0x80 != 0 => {
            let mut value = u64::from(first & 0x7f);
            for byte in &field[1..] {
                value = value.checked_mul(256)?.checked_add(u64::from(*byte))?;
            }
            Some(value)
        }
        _ => octal(field),
    }
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
        sealed(&mut header);
        let mut block = header.to_vec();
        block.extend_from_slice(contents);
        let padding = (BLOCK - contents.len() % BLOCK) % BLOCK;
        block.extend(std::iter::repeat_n(0u8, padding));
        block
    }

    /// Writes the header's checksum.
    fn sealed(header: &mut [u8; BLOCK]) {
        header[CHECKSUM].copy_from_slice(b"        ");
        let sum: u64 = header.iter().map(|byte| u64::from(*byte)).sum();
        let text = format!("{sum:06o}\0 ");
        header[CHECKSUM].copy_from_slice(text.as_bytes());
    }

    fn scratch_file(label: &str) -> (std::path::PathBuf, std::fs::File) {
        let path = std::env::temp_dir().join(format!("vm-tar-{label}-{}", std::process::id()));
        let file = std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        (path, file)
    }

    fn extracted(
        bytes: &[u8],
        wanted: Wanted<'_>,
        label: &str,
    ) -> (Result<String, Malformed>, Vec<u8>) {
        let (path, mut file) = scratch_file(label);
        let mut source = bytes;
        let outcome = extract(&mut source, wanted, &mut file).unwrap();
        assert!(source.is_empty(), "the stream was not read to its end");
        let contents = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        (outcome, contents)
    }

    #[test]
    fn a_named_file_is_extracted_from_among_others() {
        let bytes = archive(&[
            entry("dir/", b'5', b""),
            entry("readme", b'0', b"not this"),
            entry("disk.raw", b'0', b"the disk"),
            entry("after", b'0', b"nor this"),
        ]);
        let (outcome, contents) = extracted(&bytes, Wanted::Named("disk.raw"), "named");
        assert_eq!(outcome.unwrap(), "disk.raw");
        assert_eq!(contents, b"the disk");
    }

    #[test]
    fn a_name_with_a_leading_dot_slash_still_matches() {
        let bytes = archive(&[entry("./disk.raw", b'0', b"the disk")]);
        let (outcome, contents) = extracted(&bytes, Wanted::Named("disk.raw"), "dotslash");
        assert_eq!(outcome.unwrap(), "./disk.raw");
        assert_eq!(contents, b"the disk");
    }

    #[test]
    fn a_missing_member_is_refused_by_name() {
        let bytes = archive(&[entry("other.raw", b'0', b"x")]);
        let (outcome, _) = extracted(&bytes, Wanted::Named("disk.raw"), "missing");
        match outcome {
            Err(Malformed::Missing { member }) => assert_eq!(member, "disk.raw"),
            other => panic!("expected a missing member, got {other:?}"),
        }
    }

    #[test]
    fn the_only_file_is_extracted_without_being_named() {
        let bytes = archive(&[
            entry("dir/", b'5', b""),
            entry("dir/image.qcow2", b'0', b"img"),
        ]);
        let (outcome, contents) = extracted(&bytes, Wanted::Only, "only");
        assert_eq!(outcome.unwrap(), "dir/image.qcow2");
        assert_eq!(contents, b"img");
    }

    #[test]
    fn an_archive_of_several_files_names_them_when_none_was_named() {
        let bytes = archive(&[entry("a.vmdk", b'0', b"a"), entry("b.ovf", b'0', b"b")]);
        let (outcome, _) = extracted(&bytes, Wanted::Only, "several");
        match outcome {
            Err(Malformed::Several { paths }) => assert_eq!(paths, ["a.vmdk", "b.ovf"]),
            other => panic!("expected several files, got {other:?}"),
        }
        let empty = archive(&[entry("dir/", b'5', b"")]);
        assert!(matches!(
            extracted(&empty, Wanted::Only, "empty").0,
            Err(Malformed::Empty)
        ));
    }

    #[test]
    fn zeros_are_skipped_rather_than_written_and_the_length_is_kept() {
        use std::os::unix::fs::MetadataExt as _;
        let mut payload = vec![0u8; CHUNK * 64];
        payload[CHUNK * 10] = 7;
        payload.extend_from_slice(&[0u8; 100]);
        let bytes = archive(&[entry("disk.raw", b'0', &payload)]);
        let (path, mut file) = scratch_file("sparse");
        let mut source = bytes.as_slice();
        extract(&mut source, Wanted::Only, &mut file)
            .unwrap()
            .unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        let contents = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(contents, payload);
        assert_eq!(metadata.len(), payload.len() as u64);
        assert!(
            metadata.blocks() * 512 < payload.len() as u64 / 4,
            "{} blocks allocated for {} bytes",
            metadata.blocks(),
            payload.len()
        );
    }

    #[test]
    fn a_gnu_long_name_names_the_entry_after_it() {
        let long = format!("{}/disk.raw", "d".repeat(120));
        let bytes = archive(&[
            entry("././@LongLink", b'L', format!("{long}\0").as_bytes()),
            entry("truncated-name", b'0', b"the disk"),
        ]);
        let (outcome, contents) = extracted(&bytes, Wanted::Named(&long), "longname");
        assert_eq!(outcome.unwrap(), long);
        assert_eq!(contents, b"the disk");
    }

    #[test]
    fn a_pax_header_gives_the_path_and_size() {
        let records = "17 path=disk.raw\n10 size=4\n";
        let mut member = entry("short", b'0', b"disk");
        let mut header = [0u8; BLOCK];
        header.copy_from_slice(&member[..BLOCK]);
        header[SIZE].copy_from_slice(b"00000000000\0");
        sealed(&mut header);
        member[..BLOCK].copy_from_slice(&header);
        let bytes = archive(&[
            entry("PaxHeaders/disk.raw", b'x', records.as_bytes()),
            member,
        ]);
        let (outcome, contents) = extracted(&bytes, Wanted::Named("disk.raw"), "pax");
        assert_eq!(outcome.unwrap(), "disk.raw");
        assert_eq!(contents, b"disk");
    }

    #[test]
    fn an_older_pax_sparse_entry_is_refused() {
        let records = "26 GNU.sparse.map=0,1,4,1\n";
        let bytes = archive(&[
            entry("PaxHeaders/disk.raw", b'x', records.as_bytes()),
            entry("disk.raw", b'0', b"ab"),
        ]);
        assert!(matches!(
            extracted(&bytes, Wanted::Only, "paxsparse01").0,
            Err(Malformed::Sparse { .. })
        ));
    }

    /// An old GNU sparse header with its regions, and extension blocks for the rest.
    fn old_gnu_sparse(path: &str, regions: &[(u64, &[u8])], length: u64) -> Vec<u8> {
        let data: Vec<u8> = regions
            .iter()
            .flat_map(|(_, bytes)| bytes.to_vec())
            .collect();
        let mut header = [0u8; BLOCK];
        header[..path.len()].copy_from_slice(path.as_bytes());
        header[SIZE].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
        header[TYPEFLAG] = b'S';
        header[MAGIC].copy_from_slice(b"ustar ");
        header[263..265].copy_from_slice(b" \0");
        let field = |offset: u64| format!("{offset:011o}\0").into_bytes();
        let mut extensions: Vec<[u8; BLOCK]> = Vec::new();
        for (index, (offset, bytes)) in regions.iter().enumerate() {
            let pair = [field(*offset), field(bytes.len() as u64)].concat();
            if index < 4 {
                let at = 386 + index * 24;
                header[at..at + 24].copy_from_slice(&pair);
            } else {
                let slot = (index - 4) % 21;
                if slot == 0 {
                    extensions.push([0u8; BLOCK]);
                }
                let block = extensions.last_mut().unwrap();
                block[slot * 24..slot * 24 + 24].copy_from_slice(&pair);
            }
        }
        header[482] = u8::from(!extensions.is_empty());
        let blocks = extensions.len();
        for (index, block) in extensions.iter_mut().enumerate() {
            block[504] = u8::from(index + 1 < blocks);
        }
        header[483..495].copy_from_slice(&field(length));
        sealed(&mut header);
        let mut bytes = header.to_vec();
        for block in extensions {
            bytes.extend_from_slice(&block);
        }
        bytes.extend_from_slice(&data);
        bytes.extend(std::iter::repeat_n(
            0u8,
            (BLOCK - data.len() % BLOCK) % BLOCK,
        ));
        bytes
    }

    #[test]
    fn an_old_gnu_sparse_entry_is_laid_out_at_its_offsets() {
        let regions: Vec<(u64, Vec<u8>)> = (0..30u64)
            .map(|index| (index * 10_000, format!("region {index}").into_bytes()))
            .collect();
        let borrowed: Vec<(u64, &[u8])> = regions
            .iter()
            .map(|(offset, bytes)| (*offset, bytes.as_slice()))
            .collect();
        let length = 400_000;
        let bytes = archive(&[
            old_gnu_sparse("disk.raw", &borrowed, length),
            entry("after", b'0', b"after"),
        ]);
        let (outcome, contents) = extracted(&bytes, Wanted::Named("disk.raw"), "oldgnu");
        assert_eq!(outcome.unwrap(), "disk.raw");
        let mut expected = vec![0u8; usize::try_from(length).unwrap()];
        for (offset, data) in &regions {
            let at = usize::try_from(*offset).unwrap();
            expected[at..at + data.len()].copy_from_slice(data);
        }
        assert_eq!(contents, expected);
    }

    #[test]
    fn a_sparse_region_beyond_the_files_end_is_refused() {
        let bytes = archive(&[old_gnu_sparse("disk.raw", &[(1000, b"late")], 100)]);
        assert!(matches!(
            extracted(&bytes, Wanted::Only, "beyond").0,
            Err(Malformed::Sparse { .. })
        ));
    }

    #[test]
    fn sparse_archives_written_by_gnu_tar_are_extracted() {
        let directory = std::env::temp_dir().join(format!("vm-tar-sparse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("disk.raw");
        let mut expected = vec![0u8; 8 << 20];
        expected[5] = 1;
        expected[3 << 20] = 2;
        expected[(8 << 20) - 1] = 3;
        {
            let mut file = std::fs::File::create(&path).unwrap();
            for (offset, byte) in [(5u64, 1u8), (3 << 20, 2), ((8 << 20) - 1, 3)] {
                file.seek(io::SeekFrom::Start(offset)).unwrap();
                file.write_all(&[byte]).unwrap();
            }
        }
        for format in ["gnu", "posix"] {
            let Ok(archived) = std::process::Command::new("tar")
                .arg(format!("--format={format}"))
                .arg("--sparse")
                .arg("-C")
                .arg(&directory)
                .args(["-cf", "-", "disk.raw"])
                .output()
            else {
                return;
            };
            assert!(archived.status.success(), "{format}");
            let (outcome, contents) = extracted(
                &archived.stdout,
                Wanted::Named("disk.raw"),
                &format!("sparse{format}"),
            );
            assert_eq!(outcome.unwrap(), "disk.raw", "{format}");
            assert!(contents == expected, "{format} contents differ");
        }
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_size_beyond_eight_gibibytes_is_read_in_base_256() {
        let mut field = [0u8; 12];
        field[0] = 0x80;
        field[7..].copy_from_slice(&[0x06, 0x40, 0x00, 0x00, 0x00]);
        assert_eq!(size_field(&field), Some(26_843_545_600));
        assert_eq!(size_field(b"00000000017\0"), Some(15));
        assert_eq!(size_field(&[0xff; 12]), None);
    }

    #[test]
    fn a_truncated_or_corrupt_member_is_refused_and_the_rest_is_read() {
        let mut bytes = entry("disk.raw", b'0', &vec![1u8; BLOCK * 4]);
        bytes.truncate(BLOCK * 2);
        assert!(matches!(
            extracted(&bytes, Wanted::Only, "truncated").0,
            Err(Malformed::Truncated)
        ));
        let mut corrupt = archive(&[entry("disk.raw", b'0', b"x")]);
        corrupt[0] = b'X';
        assert!(matches!(
            extracted(&corrupt, Wanted::Only, "checksum").0,
            Err(Malformed::BadHeader)
        ));
    }

    #[test]
    fn an_archive_is_told_apart_from_an_image() {
        let bytes = archive(&[entry("disk.raw", b'0', b"x")]);
        assert!(is_archive(&bytes));
        assert!(!is_archive(&bytes[..BLOCK - 1]));
        let mut unsealed = bytes;
        unsealed[CHECKSUM].copy_from_slice(b"0000000\0");
        assert!(!is_archive(&unsealed));
        assert!(!is_archive(&[0u8; BLOCK]));
        let mut boot = vec![0u8; BLOCK];
        boot[510] = 0x55;
        boot[511] = 0xaa;
        assert!(!is_archive(&boot));
    }

    #[test]
    fn archives_written_by_gnu_tar_are_extracted() {
        let directory = std::env::temp_dir().join(format!("vm-tar-gnu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let long = "l".repeat(150);
        std::fs::create_dir_all(directory.join(&long)).unwrap();
        std::fs::write(directory.join(&long).join("disk.raw"), b"gnu disk").unwrap();
        for format in ["gnu", "pax", "ustar", "v7"] {
            let archived = std::process::Command::new("tar")
                .arg(format!("--format={format}"))
                .arg("-C")
                .arg(&directory)
                .args(["-cf", "-"])
                .arg(format!("{long}/disk.raw"))
                .output();
            let Ok(archived) = archived else {
                return;
            };
            if !archived.status.success() {
                // ustar and v7 cannot hold a name this long.
                continue;
            }
            let member = format!("{long}/disk.raw");
            let (outcome, contents) = extracted(
                &archived.stdout,
                Wanted::Named(&member),
                &format!("gnu{format}"),
            );
            assert_eq!(outcome.unwrap(), member, "{format}");
            assert_eq!(contents, b"gnu disk", "{format}");
        }
        let _ = std::fs::remove_dir_all(&directory);
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

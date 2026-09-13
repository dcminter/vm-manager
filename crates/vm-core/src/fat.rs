//! A deterministic FAT16 image builder for cloud-init seeds.

const SECTOR: usize = 512;
const SECTORS_PER_CLUSTER: usize = 1;
const RESERVED_SECTORS: usize = 1;
const FAT_COPIES: usize = 2;
const ROOT_ENTRIES: usize = 512;
const ENTRY: usize = 32;

/// Aligns the table to a sector and stays within FAT16's cluster range.
const CLUSTERS: usize = 8190;
const FAT_SECTORS: usize = (CLUSTERS + 2) * 2 / SECTOR;
const ROOT_SECTORS: usize = ROOT_ENTRIES * ENTRY / SECTOR;

const FIRST_FAT_SECTOR: usize = RESERVED_SECTORS;
const ROOT_SECTOR: usize = FIRST_FAT_SECTOR + FAT_COPIES * FAT_SECTORS;
const DATA_SECTOR: usize = ROOT_SECTOR + ROOT_SECTORS;
const TOTAL_SECTORS: usize = DATA_SECTOR + CLUSTERS * SECTORS_PER_CLUSTER;
const CLUSTER_BYTES: usize = SECTOR * SECTORS_PER_CLUSTER;

/// 1980-01-01, the earliest FAT date, fixed for reproducibility.
const DATE: u16 = (1 << 5) | 1;
const TIME: u16 = 0;
const VOLUME_ID: u32 = 0x00CD_1DA7;

const _: () = assert!(
    (CLUSTERS + 2) * 2 == FAT_SECTORS * SECTOR,
    "the table must land on a sector boundary"
);
const _: () = assert!(
    CLUSTERS >= 4085 && CLUSTERS <= 65524,
    "outside this range the volume is read as FAT12 or FAT32"
);

/// The offsets of the thirteen name characters in a long-filename entry.
const SLOTS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];

/// A file to place in the root directory.
pub struct File<'a> {
    pub name: &'a str,
    pub contents: &'a [u8],
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The name cannot be written to a FAT directory.
    Name { name: String, reason: &'static str },
    /// The files need more space than the fixed geometry provides.
    TooLarge { wanted: u64, available: u64 },
    /// The root directory has no room for the entries the names require.
    TooManyEntries { wanted: usize, available: usize },
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Name { name, reason } => write!(f, "the name '{name}' {reason}"),
            Self::TooLarge { wanted, available } => write!(
                f,
                "the contents need {wanted} bytes but the image holds {available}"
            ),
            Self::TooManyEntries { wanted, available } => write!(
                f,
                "the names need {wanted} directory entries but the root holds {available}"
            ),
        }
    }
}

/// Builds a FAT16 image of `files`, with `label` in both the boot sector and the root directory.
pub fn image(label: &str, files: &[File<'_>]) -> Result<Vec<u8>, Refused> {
    let label = volume_label(label)?;
    let entries = directory(&label, files)?;
    let mut fat = vec![0u8; FAT_SECTORS * SECTOR];
    put16(&mut fat, 0, 0xFFF8);
    put16(&mut fat, 2, 0xFFFF);

    let mut data = vec![0u8; CLUSTERS * CLUSTER_BYTES];
    let mut next = 2usize;
    for file in files {
        let needed = file.contents.len().div_ceil(CLUSTER_BYTES);
        if next + needed > CLUSTERS + 2 {
            return Err(Refused::TooLarge {
                wanted: total_bytes(files),
                available: capacity(),
            });
        }
        for index in 0..needed {
            let cluster = next + index;
            let link = if index + 1 == needed {
                0xFFFF
            } else {
                u16::try_from(cluster + 1).unwrap_or(0xFFFF)
            };
            put16(&mut fat, cluster * 2, link);
            let from = index * CLUSTER_BYTES;
            let to = (from + CLUSTER_BYTES).min(file.contents.len());
            let at = (cluster - 2) * CLUSTER_BYTES;
            data[at..at + (to - from)].copy_from_slice(&file.contents[from..to]);
        }
        next += needed;
    }

    let mut root = vec![0u8; ROOT_SECTORS * SECTOR];
    root[..entries.len()].copy_from_slice(&entries);

    let mut out = Vec::with_capacity(TOTAL_SECTORS * SECTOR);
    out.extend_from_slice(&boot_sector(&label));
    for _ in 0..FAT_COPIES {
        out.extend_from_slice(&fat);
    }
    out.extend_from_slice(&root);
    out.extend_from_slice(&data);
    debug_assert_eq!(out.len(), TOTAL_SECTORS * SECTOR);
    Ok(out)
}

/// The size of every image this module produces.
pub const fn image_size() -> usize {
    TOTAL_SECTORS * SECTOR
}

/// The space available to file contents.
pub const fn capacity() -> u64 {
    (CLUSTERS * CLUSTER_BYTES) as u64
}

fn total_bytes(files: &[File<'_>]) -> u64 {
    files
        .iter()
        .map(|file| u64::try_from(file.contents.len()).unwrap_or(u64::MAX))
        .sum::<u64>()
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "every field written here is a compile-time constant within range"
)]
fn boot_sector(label: &[u8; 11]) -> [u8; SECTOR] {
    let mut sector = [0u8; SECTOR];
    // The volume is never booted.
    sector[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
    sector[3..11].copy_from_slice(b"MSWIN4.1");
    put16(&mut sector, 11, SECTOR as u16);
    sector[13] = SECTORS_PER_CLUSTER as u8;
    put16(&mut sector, 14, RESERVED_SECTORS as u16);
    sector[16] = FAT_COPIES as u8;
    put16(&mut sector, 17, ROOT_ENTRIES as u16);
    put16(&mut sector, 19, TOTAL_SECTORS as u16);
    sector[21] = 0xF8;
    put16(&mut sector, 22, FAT_SECTORS as u16);
    put16(&mut sector, 24, 63);
    put16(&mut sector, 26, 255);
    sector[36] = 0x80;
    sector[38] = 0x29;
    put32(&mut sector, 39, VOLUME_ID);
    sector[43..54].copy_from_slice(label);
    sector[54..62].copy_from_slice(b"FAT16   ");
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn directory(label: &[u8; 11], files: &[File<'_>]) -> Result<Vec<u8>, Refused> {
    let mut out = Vec::new();
    let mut entry = [0u8; ENTRY];
    entry[..11].copy_from_slice(label);
    entry[11] = 0x08;
    put16(&mut entry, 16, DATE);
    put16(&mut entry, 22, TIME);
    put16(&mut entry, 24, DATE);
    out.extend_from_slice(&entry);

    let mut taken: Vec<[u8; 11]> = Vec::new();
    let mut cluster = 2usize;
    for file in files {
        let short = short_name(file.name, &taken)?;
        taken.push(short);
        for long in long_entries(file.name, short)? {
            out.extend_from_slice(&long);
        }
        let used = file.contents.len().div_ceil(CLUSTER_BYTES);
        let first = if used == 0 { 0 } else { cluster };
        cluster += used;
        out.extend_from_slice(&short_entry(short, first, file.contents.len()));
    }

    if out.len() > ROOT_ENTRIES * ENTRY {
        return Err(Refused::TooManyEntries {
            wanted: out.len() / ENTRY,
            available: ROOT_ENTRIES,
        });
    }
    Ok(out)
}

fn short_entry(short: [u8; 11], cluster: usize, size: usize) -> [u8; ENTRY] {
    let mut entry = [0u8; ENTRY];
    entry[..11].copy_from_slice(&short);
    entry[11] = 0x20;
    put16(&mut entry, 16, DATE);
    put16(&mut entry, 18, DATE);
    put16(&mut entry, 22, TIME);
    put16(&mut entry, 24, DATE);
    put16(&mut entry, 26, u16::try_from(cluster).unwrap_or(0));
    put32(&mut entry, 28, u32::try_from(size).unwrap_or(u32::MAX));
    entry
}

/// Long-filename entries, last fragment first as the specification requires.
fn long_entries(name: &str, short: [u8; 11]) -> Result<Vec<[u8; ENTRY]>, Refused> {
    let units: Vec<u16> = name.encode_utf16().collect();
    if units.len() > 255 {
        return Err(Refused::Name {
            name: name.to_owned(),
            reason: "is longer than a directory entry can hold",
        });
    }
    let sum = checksum(&short);
    let count = units.len().div_ceil(13);
    let mut out = Vec::with_capacity(count);
    for index in (0..count).rev() {
        let mut entry = [0u8; ENTRY];
        entry[0] = u8::try_from(index + 1).unwrap_or(1);
        if index + 1 == count {
            entry[0] |= 0x40;
        }
        entry[11] = 0x0F;
        entry[13] = sum;
        for (slot, at) in SLOTS.into_iter().enumerate() {
            let position = index * 13 + slot;
            let unit = match position.cmp(&units.len()) {
                std::cmp::Ordering::Less => units[position],
                std::cmp::Ordering::Equal => 0x0000,
                std::cmp::Ordering::Greater => 0xFFFF,
            };
            put16(&mut entry, at, unit);
        }
        out.push(entry);
    }
    Ok(out)
}

/// The checksum linking long entries to their 8.3 entry.
fn checksum(short: &[u8; 11]) -> u8 {
    short
        .iter()
        .fold(0u8, |sum, byte| sum.rotate_right(1).wrapping_add(*byte))
}

fn volume_label(label: &str) -> Result<[u8; 11], Refused> {
    if label.is_empty() || label.len() > 11 || !label.bytes().all(is_short_name_byte) {
        return Err(Refused::Name {
            name: label.to_owned(),
            reason: "is not a usable volume label",
        });
    }
    let mut out = [b' '; 11];
    for (at, byte) in label.bytes().enumerate() {
        out[at] = byte.to_ascii_uppercase();
    }
    Ok(out)
}

/// An unused 8.3 name derived from the long name.
fn short_name(name: &str, taken: &[[u8; 11]]) -> Result<[u8; 11], Refused> {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return Err(Refused::Name {
            name: name.to_owned(),
            reason: "is not a usable filename",
        });
    }
    let (stem, extension) = match name.rfind('.') {
        Some(at) if at > 0 => (&name[..at], &name[at + 1..]),
        _ => (name, ""),
    };
    let stem = sanitised(stem);
    let extension = sanitised(extension);
    for index in 1..=999u32 {
        let suffix = format!("~{index}");
        let keep = 8usize.saturating_sub(suffix.len());
        let base = format!("{}{suffix}", &stem[..stem.len().min(keep)]);
        let mut out = [b' '; 11];
        for (at, byte) in base.bytes().take(8).enumerate() {
            out[at] = byte;
        }
        for (at, byte) in extension.bytes().take(3).enumerate() {
            out[8 + at] = byte;
        }
        if !taken.contains(&out) {
            return Ok(out);
        }
    }
    Err(Refused::Name {
        name: name.to_owned(),
        reason: "collides with too many others",
    })
}

fn sanitised(text: &str) -> String {
    text.chars()
        .map(|character| {
            let upper = character.to_ascii_uppercase();
            if upper.is_ascii() && is_short_name_byte(upper as u8) {
                upper
            } else {
                '_'
            }
        })
        .collect()
}

fn is_short_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"$%'-_@~`!(){}^#&".contains(&byte)
}

fn put16(buffer: &mut [u8], at: usize, value: u16) {
    buffer[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(buffer: &mut [u8], at: usize, value: u32) {
    buffer[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn built(files: &[File<'_>]) -> Vec<u8> {
        image("cidata", files).unwrap()
    }

    fn root_entry(bytes: &[u8], index: usize) -> &[u8] {
        let at = ROOT_SECTOR * SECTOR + index * ENTRY;
        &bytes[at..at + ENTRY]
    }

    fn cluster(bytes: &[u8], number: usize) -> &[u8] {
        let at = (DATA_SECTOR + (number - 2) * SECTORS_PER_CLUSTER) * SECTOR;
        &bytes[at..at + CLUSTER_BYTES]
    }

    fn get16(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    #[test]
    fn the_image_is_the_size_the_geometry_implies() {
        let bytes = built(&[]);
        assert_eq!(bytes.len(), image_size());
        assert_eq!(bytes.len() % SECTOR, 0);
    }

    #[test]
    fn the_boot_sector_carries_the_signature_and_the_type() {
        let bytes = built(&[]);
        assert_eq!(&bytes[510..512], &[0x55, 0xAA]);
        assert_eq!(&bytes[54..62], b"FAT16   ");
        assert_eq!(get16(&bytes, 11), u16::try_from(SECTOR).unwrap());
        assert_eq!(get16(&bytes, 19), u16::try_from(TOTAL_SECTORS).unwrap());
    }

    #[test]
    fn the_label_reaches_both_places_a_reader_might_look() {
        let bytes = built(&[]);
        assert_eq!(&bytes[43..54], b"CIDATA     ");
        let entry = root_entry(&bytes, 0);
        assert_eq!(&entry[..11], b"CIDATA     ");
        assert_eq!(entry[11], 0x08, "the first entry is the volume label");
    }

    #[test]
    fn the_label_is_uppercased() {
        assert_eq!(&volume_label("cidata").unwrap()[..], b"CIDATA     ");
    }

    #[test]
    fn an_unusable_label_is_refused() {
        for label in ["", "far too long a label", "has spaces"] {
            assert!(image(label, &[]).is_err(), "{label} should be refused");
        }
    }

    #[test]
    fn the_first_two_fat_entries_are_reserved() {
        let bytes = built(&[]);
        let fat = FIRST_FAT_SECTOR * SECTOR;
        assert_eq!(get16(&bytes, fat), 0xFFF8);
        assert_eq!(get16(&bytes, fat + 2), 0xFFFF);
    }

    #[test]
    fn both_copies_of_the_fat_are_identical() {
        let bytes = built(&[File {
            name: "user-data",
            contents: b"#cloud-config\n",
        }]);
        let first = FIRST_FAT_SECTOR * SECTOR;
        let second = first + FAT_SECTORS * SECTOR;
        assert_eq!(
            &bytes[first..first + FAT_SECTORS * SECTOR],
            &bytes[second..second + FAT_SECTORS * SECTOR]
        );
    }

    #[test]
    fn a_file_lands_in_its_cluster_with_a_terminated_chain() {
        let bytes = built(&[File {
            name: "meta-data",
            contents: b"instance-id: one\n",
        }]);
        let entry = root_entry(&bytes, 2);
        assert_eq!(get16(entry, 26), 2, "the first file takes cluster two");
        assert_eq!(
            u32::from_le_bytes([entry[28], entry[29], entry[30], entry[31]]),
            17
        );
        assert_eq!(&cluster(&bytes, 2)[..17], b"instance-id: one\n");
        let fat = FIRST_FAT_SECTOR * SECTOR;
        assert_eq!(get16(&bytes, fat + 4), 0xFFFF, "one cluster, end of chain");
    }

    #[test]
    fn a_file_spanning_clusters_is_chained_in_order() {
        let contents = vec![b'x'; CLUSTER_BYTES * 3 + 1];
        let bytes = built(&[File {
            name: "big",
            contents: &contents,
        }]);
        let fat = FIRST_FAT_SECTOR * SECTOR;
        assert_eq!(get16(&bytes, fat + 2 * 2), 3);
        assert_eq!(get16(&bytes, fat + 3 * 2), 4);
        assert_eq!(get16(&bytes, fat + 4 * 2), 5);
        assert_eq!(get16(&bytes, fat + 5 * 2), 0xFFFF);
        assert_eq!(cluster(&bytes, 5)[0], b'x');
        assert_eq!(cluster(&bytes, 5)[1], 0, "the tail cluster is zero padded");
    }

    #[test]
    fn files_are_allocated_one_after_another() {
        let bytes = built(&[
            File {
                name: "a",
                contents: &vec![b'a'; CLUSTER_BYTES + 1],
            },
            File {
                name: "b",
                contents: b"second",
            },
        ]);
        assert_eq!(&cluster(&bytes, 4)[..6], b"second");
    }

    #[test]
    fn an_empty_file_claims_no_cluster() {
        let bytes = built(&[
            File {
                name: "empty",
                contents: b"",
            },
            File {
                name: "after",
                contents: b"x",
            },
        ]);
        let empty = root_entry(&bytes, 2);
        assert_eq!(get16(empty, 26), 0);
        assert_eq!(&cluster(&bytes, 2)[..1], b"x");
    }

    #[test]
    fn a_long_name_is_written_as_long_entries_before_the_short_one() {
        let bytes = built(&[File {
            name: "user-data",
            contents: b"x",
        }]);
        let long = root_entry(&bytes, 1);
        assert_eq!(long[11], 0x0F, "long entries are marked as such");
        assert_eq!(long[0], 0x41, "one entry, and it is the last");
        let name: String = [1usize, 3, 5, 7, 9, 14, 16, 18, 20]
            .into_iter()
            .map(|at| char::from(long[at]))
            .collect();
        assert_eq!(name, "user-data");
        let short = root_entry(&bytes, 2);
        assert_eq!(&short[..11], b"USER-D~1   ");
    }

    #[test]
    fn the_long_entries_checksum_matches_the_short_name() {
        let bytes = built(&[File {
            name: "network-config",
            contents: b"x",
        }]);
        let long = root_entry(&bytes, 1);
        let short = root_entry(&bytes, 3);
        assert_eq!(short[11], 0x20, "two long entries, then the 8.3 entry");
        let mut name = [0u8; 11];
        name.copy_from_slice(&short[..11]);
        assert_eq!(long[13], checksum(&name));
    }

    #[test]
    fn the_checksum_is_the_documented_rotation() {
        let mut sum = 0u8;
        for byte in b"USER-D~1   " {
            sum = ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(*byte);
        }
        assert_eq!(checksum(b"USER-D~1   "), sum);
    }

    #[test]
    fn a_name_needing_several_long_entries_is_split_in_reverse() {
        let name = "a-very-long-filename-indeed.txt";
        let bytes = built(&[File {
            name,
            contents: b"x",
        }]);
        let count = name.len().div_ceil(13);
        assert_eq!(count, 3);
        assert_eq!(root_entry(&bytes, 1)[0], 0x40 | 3);
        assert_eq!(root_entry(&bytes, 2)[0], 2);
        assert_eq!(root_entry(&bytes, 3)[0], 1);
        assert_eq!(root_entry(&bytes, 4)[11], 0x20, "then the 8.3 entry");
    }

    #[test]
    fn an_unfilled_long_entry_is_terminated_then_padded() {
        let bytes = built(&[File {
            name: "ab",
            contents: b"x",
        }]);
        let long = root_entry(&bytes, 1);
        assert_eq!(get16(long, 1), u16::from(b'a'));
        assert_eq!(get16(long, 3), u16::from(b'b'));
        assert_eq!(get16(long, 5), 0x0000, "the terminator");
        assert_eq!(get16(long, 7), 0xFFFF, "then padding");
        assert_eq!(get16(long, 30), 0xFFFF);
    }

    #[test]
    fn an_exactly_filled_long_entry_carries_no_terminator() {
        let name = "thirteenchars";
        assert_eq!(name.len(), 13);
        let bytes = built(&[File {
            name,
            contents: b"x",
        }]);
        let long = root_entry(&bytes, 1);
        assert_eq!(get16(long, 30), u16::from(b's'));
    }

    #[test]
    fn short_names_are_numbered_apart_when_they_would_collide() {
        let bytes = built(&[
            File {
                name: "collision-one",
                contents: b"x",
            },
            File {
                name: "collision-two",
                contents: b"y",
            },
        ]);
        let first = root_entry(&bytes, 2);
        let second = root_entry(&bytes, 4);
        assert_eq!(&first[..11], b"COLLIS~1   ");
        assert_eq!(&second[..11], b"COLLIS~2   ");
    }

    #[test]
    fn an_extension_is_kept_in_the_short_name() {
        assert_eq!(&short_name("config.yaml", &[]).unwrap()[..], b"CONFIG~1YAM");
    }

    #[test]
    fn a_leading_dot_is_not_read_as_an_extension() {
        assert_eq!(&short_name(".hidden", &[]).unwrap()[..], b"_HIDDE~1   ");
    }

    #[test]
    fn characters_a_short_name_cannot_hold_become_underscores() {
        assert_eq!(&short_name("a b+c", &[]).unwrap()[..], b"A_B_C~1    ");
    }

    #[test]
    fn a_path_is_not_a_filename() {
        assert!(short_name("a/b", &[]).is_err());
        assert!(short_name("", &[]).is_err());
    }

    #[test]
    fn contents_beyond_the_capacity_are_refused() {
        let contents = vec![0u8; usize::try_from(capacity()).unwrap() + 1];
        let outcome = image(
            "cidata",
            &[File {
                name: "big",
                contents: &contents,
            }],
        );
        assert!(
            matches!(outcome, Err(Refused::TooLarge { .. })),
            "{outcome:?}"
        );
    }

    #[test]
    fn more_names_than_the_root_directory_holds_are_refused() {
        let names: Vec<String> = (0..300)
            .map(|index| format!("file-number-{index}"))
            .collect();
        let files: Vec<File<'_>> = names
            .iter()
            .map(|name| File {
                name,
                contents: b"",
            })
            .collect();
        let outcome = image("cidata", &files);
        assert!(
            matches!(outcome, Err(Refused::TooManyEntries { .. })),
            "{outcome:?}"
        );
    }

    #[test]
    fn the_same_files_always_produce_the_same_bytes() {
        let files = [
            File {
                name: "user-data",
                contents: b"#cloud-config\n",
            },
            File {
                name: "meta-data",
                contents: b"instance-id: one\n",
            },
        ];
        assert_eq!(built(&files), built(&files));
    }
}

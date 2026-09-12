//! What a machine's disk costs, which is two numbers rather than one.
//!
//! A qcow2 overlay is created at its full virtual size and fills in as the
//! guest writes, so what the guest is promised and what the host has given up
//! diverge from the first boot. The header is read directly: `qemu-img info`
//! would say the same thing at the price of a process per instance, and a
//! listing runs this once for every machine it shows.

use std::path::Path;

/// What a disk takes now and what it may grow into.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Bytes the host has actually given up.
    pub allocated: Option<u64>,
    /// Bytes the guest sees.
    pub capacity: Option<u64>,
}

/// Both figures for one disk, each absent if it could not be had.
pub fn usage(overlay: &Path) -> Usage {
    Usage {
        allocated: allocated(overlay),
        capacity: header(overlay).as_deref().and_then(capacity),
    }
}

/// What the file occupies, not what it claims to span.
///
/// A sparse file's length counts holes that cost nothing, and an overlay over
/// a 40G image is mostly hole, so the blocks are the honest figure. They are
/// counted in 512-byte units whatever the filesystem's own block size is.
fn allocated(overlay: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    let data = std::fs::metadata(overlay).ok()?;
    data.blocks().checked_mul(512)
}

/// The first 32 bytes, which is the fixed part of a qcow2 header.
fn header(overlay: &Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(overlay).ok()?;
    let mut bytes = [0_u8; 32];
    file.read_exact(&mut bytes).ok()?;
    Some(bytes.to_vec())
}

/// The virtual size a qcow2 header records, at offset 24 and big-endian.
///
/// The magic is checked because anything else read at that offset is a number
/// with no meaning, and a wrong size is worse than none.
fn capacity(header: &[u8]) -> Option<u64> {
    if header.len() < 32 || header.get(..4) != Some(b"QFI\xfb".as_slice()) {
        return None;
    }
    let field: [u8; 8] = header.get(24..32)?.try_into().ok()?;
    Some(u64::from_be_bytes(field))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("vm-disk-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn qcow2(size: u64) -> Vec<u8> {
        let mut header = vec![0_u8; 32];
        header[..4].copy_from_slice(b"QFI\xfb");
        header[24..32].copy_from_slice(&size.to_be_bytes());
        header
    }

    #[test]
    fn the_virtual_size_is_read_from_the_header() {
        assert_eq!(
            capacity(&qcow2(12 * 1024 * 1024 * 1024)),
            Some(12_884_901_888)
        );
    }

    /// Read as a number, the same offset in a raw disk is whatever the guest
    /// happened to write there.
    #[test]
    fn something_that_is_not_a_qcow2_has_no_size_to_read() {
        let mut header = qcow2(4096);
        header[0] = b'X';
        assert_eq!(capacity(&header), None);
    }

    #[test]
    fn a_header_that_was_cut_short_is_refused() {
        assert_eq!(capacity(&qcow2(4096)[..20]), None);
        assert_eq!(capacity(&[]), None);
    }

    #[test]
    fn a_disk_that_is_not_there_reports_neither_figure() {
        let scratch = Scratch::new("absent");
        assert_eq!(usage(&scratch.0.join("nothing.qcow2")), Usage::default());
    }

    /// The two figures are independent: a file that exists occupies something
    /// even when nothing can be made of its contents.
    #[test]
    fn a_file_that_is_not_a_disk_still_occupies_the_host() {
        let scratch = Scratch::new("notadisk");
        let path = scratch.0.join("notes.txt");
        std::fs::write(&path, vec![b'x'; 8192]).unwrap();
        let usage = usage(&path);
        assert_eq!(usage.capacity, None);
        assert!(
            usage.allocated.is_some_and(|held| held >= 8192),
            "{usage:?}"
        );
    }

    /// A real overlay, if `qemu-img` is here to make one: it spans gigabytes
    /// and occupies kilobytes, which is the whole reason for two figures.
    #[test]
    fn an_overlay_costs_far_less_than_it_spans() {
        let scratch = Scratch::new("overlay");
        let path = scratch.0.join("disk.qcow2");
        let made = std::process::Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2"])
            .arg(&path)
            .arg("4G")
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return;
        }
        let usage = usage(&path);
        assert_eq!(usage.capacity, Some(4 * 1024 * 1024 * 1024));
        assert!(
            usage.allocated.is_some_and(|held| held < 1024 * 1024),
            "{usage:?}"
        );
    }
}

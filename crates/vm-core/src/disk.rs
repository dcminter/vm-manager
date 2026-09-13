//! A disk's allocated size and its virtual size.

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

/// Bytes allocated to the file, excluding holes.
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

/// The backing file a qcow2 disk names, if it names one.
pub fn backing_file(overlay: &Path) -> Option<std::path::PathBuf> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    use std::os::unix::ffi::OsStringExt as _;
    let header = header(overlay)?;
    if header.get(..4) != Some(b"QFI\xfb".as_slice()) {
        return None;
    }
    let offset = u64::from_be_bytes(header.get(8..16)?.try_into().ok()?);
    let length = u32::from_be_bytes(header.get(16..20)?.try_into().ok()?);
    if offset == 0 || length == 0 || length > 1023 {
        return None;
    }
    let mut file = std::fs::File::open(overlay).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut name = vec![0_u8; usize::try_from(length).ok()?];
    file.read_exact(&mut name).ok()?;
    Some(std::ffi::OsString::from_vec(name).into())
}

/// The virtual size from a qcow2 header, or `None` without qcow2 magic.
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

    #[test]
    fn the_backing_file_is_read_from_where_the_header_points() {
        let scratch = Scratch::new("backing");
        let path = scratch.0.join("synthetic.qcow2");
        let name = b"/store/blobs/sha512/abc";
        let mut bytes = qcow2(4096);
        bytes[8..16].copy_from_slice(&64_u64.to_be_bytes());
        bytes[16..20].copy_from_slice(&u32::try_from(name.len()).unwrap().to_be_bytes());
        bytes.resize(64, 0);
        bytes.extend_from_slice(name);
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            backing_file(&path),
            Some(PathBuf::from("/store/blobs/sha512/abc"))
        );
    }

    #[test]
    fn a_disk_without_a_backing_file_names_none() {
        let scratch = Scratch::new("nobacking");
        let path = scratch.0.join("plain.qcow2");
        std::fs::write(&path, qcow2(4096)).unwrap();
        assert_eq!(backing_file(&path), None);
        std::fs::write(&path, b"not a disk at all, but long enough").unwrap();
        assert_eq!(backing_file(&path), None);
        assert_eq!(backing_file(&scratch.0.join("absent")), None);
    }

    #[test]
    fn a_real_overlay_names_its_backing_file() {
        let scratch = Scratch::new("realbacking");
        let base = scratch.0.join("base.qcow2");
        let overlay = scratch.0.join("overlay.qcow2");
        let made = std::process::Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2"])
            .arg(&base)
            .arg("16M")
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return;
        }
        let made = std::process::Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2", "-F", "qcow2", "-b"])
            .arg(&base)
            .arg(&overlay)
            .status()
            .unwrap();
        assert!(made.success());
        assert_eq!(backing_file(&overlay), Some(base));
    }
}

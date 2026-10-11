//! Small filesystem helpers: atomic replacement and stable hashing.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// FNV-1a 64-bit: a stable, dependency-free hash for cache file names.
/// Collisions are harmless because entries also store the full identity.
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Opens `path` for reading without ever blocking on special files and
/// returns at most `max` bytes. Returns Ok(None) when the file does not
/// exist. FIFOs, sockets, devices and directories are rejected using the
/// metadata of the opened descriptor itself (no stat/open race), and files
/// larger than `max` are rejected rather than truncated.
pub fn read_regular(path: &Path, max: u64) -> io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let nonblock = rustix::fs::OFlags::NONBLOCK.bits() as i32;
    let file = match OpenOptions::new().read(true).custom_flags(nonblock).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a regular file"));
    }
    if meta.len() > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "file too large"));
    }
    let mut data = Vec::with_capacity(meta.len() as usize);
    // The file may grow after fstat: never read more than max + 1 bytes.
    file.take(max + 1).read_to_end(&mut data)?;
    if data.len() as u64 > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "file too large"));
    }
    Ok(Some(data))
}

/// Like `read_regular`, but returns an open, verified regular file handle
/// (for streaming decoders) after the same non-blocking checks.
pub fn open_regular(path: &Path, max: u64) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let nonblock = rustix::fs::OFlags::NONBLOCK.bits() as i32;
    let file = OpenOptions::new().read(true).custom_flags(nonblock).open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a bounded regular file"));
    }
    Ok(file)
}

/// Creates a directory (and parents) readable only by the user.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

/// Writes `data` to `path` atomically: readers see either the old or the
/// new complete file, never a partial one. An interrupted writer leaves at
/// most a temporary file, which is ignored by readers and pruned later.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("no parent directory"))?;
    create_private_dir(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    // pid + per-process counter make the name unique across threads and
    // processes; `create_new` still refuses to reuse an existing file.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.{}.{seq}.{nanos}.tmp", std::process::id()));
    let result = (|| {
        let mut f: File = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    fn mkfifo(p: &Path) {
        rustix::fs::mkfifoat(rustix::fs::CWD, p, rustix::fs::Mode::from_raw_mode(0o600)).unwrap();
    }

    #[test]
    fn special_and_oversized_files_are_rejected_without_blocking() {
        let dir = crate::testutil::TempDir::new("fsutil-fifo");
        let fifo = dir.path().join("fifo");
        mkfifo(&fifo);
        let start = std::time::Instant::now();
        assert_eq!(read_regular(&fifo, 100).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert!(open_regular(&fifo, 100).is_err());
        assert!(start.elapsed() < std::time::Duration::from_millis(500));
        assert!(read_regular(dir.path(), 100).is_err(), "directory");
        let big = dir.path().join("big");
        fs::write(&big, vec![b'x'; 101]).unwrap();
        assert!(read_regular(&big, 100).is_err());
        assert_eq!(read_regular(&big, 101).unwrap().unwrap().len(), 101);
        assert_eq!(read_regular(&dir.path().join("missing"), 1).unwrap(), None);
        // a symlink to a FIFO is rejected too
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&fifo, &link).unwrap();
        assert!(read_regular(&link, 100).is_err());
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = crate::testutil::TempDir::new("fsutil");
        let p = dir.path().join("sub/file.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        let leftovers: Vec<_> = fs::read_dir(p.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1);
    }
}

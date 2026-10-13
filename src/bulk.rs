//! Directory listing with `getattrlistbulk(2)`.
//!
//! One `getattrlistbulk` call returns the name, type, sizes, times and
//! identity of a whole batch of entries, where the portable route costs a
//! `readdir` plus one `lstat` per entry. On APFS this is the difference
//! between a syscall per file and a syscall per few hundred files.
//!
//! Callers fall back to `std::fs::read_dir` when this returns
//! [`BulkError::Unsupported`] (some network and FUSE filesystems).

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: Box<str>,
    pub kind: EntryKind,
    pub dev: u64,
    pub inode: u64,
    pub mtime: i64,
    /// Directory on which another filesystem is mounted.
    pub mount_point: bool,
    /// For directories: the number of entries, when the listing knows it.
    /// Empty folders need not be opened, which matters inside other apps'
    /// containers where every open costs a privacy check.
    pub entry_count: Option<u32>,
    pub nlink: u32,
    /// Logical size of all forks.
    pub logical: u64,
    /// Bytes allocated on disk for all forks.
    pub allocated: u64,
}

#[derive(Debug)]
pub enum BulkError {
    /// The directory could not be opened or read (permissions, privacy).
    Io(io::Error),
    /// The filesystem does not support `getattrlistbulk`.
    Unsupported,
}

// fsobj_type_t values from <sys/vnode.h>.
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

const COMMON: u32 = libc::ATTR_CMN_RETURNED_ATTRS
    | libc::ATTR_CMN_NAME
    | libc::ATTR_CMN_DEVID
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_MODTIME
    | libc::ATTR_CMN_FILEID;
const DIR: u32 = libc::ATTR_DIR_ENTRYCOUNT | libc::ATTR_DIR_MOUNTSTATUS;
const FILE: u32 = libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_TOTALSIZE | libc::ATTR_FILE_ALLOCSIZE;

const BUFFER_SIZE: usize = 256 * 1024;

/// List `path` (not following a final symlink) with `getattrlistbulk`.
pub fn read_dir(path: &Path) -> Result<Vec<Entry>, BulkError> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| BulkError::Io(io::Error::from(io::ErrorKind::InvalidInput)))?;
    // SAFETY: plain open(2) of a NUL-terminated path; the fd is closed by
    // the guard below on every return path.
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(BulkError::Io(io::Error::last_os_error()));
    }
    struct Fd(libc::c_int);
    impl Drop for Fd {
        fn drop(&mut self) {
            // SAFETY: closes the descriptor this guard owns, exactly once.
            unsafe { libc::close(self.0) };
        }
    }
    let fd = Fd(fd);
    let counts_trusted = entry_counts_trusted(fd.0);

    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: COMMON,
        volattr: 0,
        dirattr: DIR,
        fileattr: FILE,
        forkattr: 0,
    };
    let mut buf = vec![0u8; BUFFER_SIZE];
    let mut entries = Vec::new();
    loop {
        // SAFETY: `request` and `buf` are valid for the duration of the call
        // and the kernel writes at most `buf.len()` bytes into `buf`.
        let count = unsafe {
            libc::getattrlistbulk(
                fd.0,
                (&raw mut request).cast(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                0,
            )
        };
        if count < 0 {
            let err = io::Error::last_os_error();
            return Err(match err.raw_os_error() {
                Some(libc::ENOTSUP) | Some(libc::EINVAL) if entries.is_empty() => {
                    BulkError::Unsupported
                }
                _ => BulkError::Io(err),
            });
        }
        if count == 0 {
            break;
        }
        parse_batch(&buf, count as usize, &mut entries);
    }
    if !counts_trusted {
        for entry in &mut entries {
            entry.entry_count = None;
        }
    }
    Ok(entries)
}

/// Directory entry counts are exact on writable APFS volumes. The sealed
/// system volume is read-only and its firmlink stubs (`/Users`,
/// `/Applications`, ...) count as empty although they lead into the Data
/// volume, and other filesystems may estimate.
fn entry_counts_trusted(fd: libc::c_int) -> bool {
    // SAFETY: fstatfs writes only into the zeroed struct and `fd` is open.
    unsafe {
        let mut stat: libc::statfs = std::mem::zeroed();
        if libc::fstatfs(fd, &mut stat) != 0 {
            return false;
        }
        let fstype = std::ffi::CStr::from_ptr(stat.f_fstypename.as_ptr());
        fstype.to_bytes() == b"apfs" && stat.f_flags & libc::MNT_RDONLY as u32 == 0
    }
}

/// Bounds-checked little reader over one entry's attribute record.
struct Cursor<'a> {
    record: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let bytes = self.record.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(bytes)
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_ne_bytes(b.try_into().unwrap()))
    }

    fn i32(&mut self) -> Option<i32> {
        self.take(4)
            .map(|b| i32::from_ne_bytes(b.try_into().unwrap()))
    }

    fn u64(&mut self) -> Option<u64> {
        self.take(8)
            .map(|b| u64::from_ne_bytes(b.try_into().unwrap()))
    }

    fn i64(&mut self) -> Option<i64> {
        self.take(8)
            .map(|b| i64::from_ne_bytes(b.try_into().unwrap()))
    }
}

fn parse_batch(buf: &[u8], count: usize, out: &mut Vec<Entry>) {
    let mut offset = 0;
    for _ in 0..count {
        let Some(len_bytes) = buf.get(offset..offset + 4) else {
            return;
        };
        let len = u32::from_ne_bytes(len_bytes.try_into().unwrap()) as usize;
        let Some(record) = buf.get(offset..offset + len) else {
            return;
        };
        if let Some(entry) = parse_entry(record) {
            out.push(entry);
        }
        if len == 0 {
            return;
        }
        offset += len;
    }
}

/// Attributes are packed in bit order, each 4-byte aligned, and only the
/// ones set in the returned bitmap are present.
fn parse_entry(record: &[u8]) -> Option<Entry> {
    let mut c = Cursor { record, pos: 4 };
    let common = c.u32()?;
    let _vol = c.u32()?;
    let dir = c.u32()?;
    let file = c.u32()?;
    let _fork = c.u32()?;

    let mut name: Option<Box<str>> = None;
    if common & libc::ATTR_CMN_NAME != 0 {
        let field = c.pos;
        let data_offset = c.i32()?;
        let length = c.u32()? as usize;
        let start = usize::try_from(field as i64 + i64::from(data_offset)).ok()?;
        let raw = record.get(start..start + length)?;
        let raw = raw.split(|&b| b == 0).next().unwrap_or(raw);
        name = Some(String::from_utf8_lossy(raw).into());
    }
    let dev = if common & libc::ATTR_CMN_DEVID != 0 {
        c.i32()? as u32 as u64
    } else {
        0
    };
    let objtype = if common & libc::ATTR_CMN_OBJTYPE != 0 {
        Some(c.u32()?)
    } else {
        None
    };
    let mtime = if common & libc::ATTR_CMN_MODTIME != 0 {
        let secs = c.i64()?;
        let _nanos = c.i64()?;
        secs
    } else {
        0
    };
    let inode = if common & libc::ATTR_CMN_FILEID != 0 {
        c.u64()?
    } else {
        0
    };
    let entry_count = if dir & libc::ATTR_DIR_ENTRYCOUNT != 0 {
        Some(c.u32()?)
    } else {
        None
    };
    let mount_point = if dir & libc::ATTR_DIR_MOUNTSTATUS != 0 {
        c.u32()? & libc::DIR_MNTSTATUS_MNTPOINT != 0
    } else {
        false
    };
    let nlink = if file & libc::ATTR_FILE_LINKCOUNT != 0 {
        c.u32()?
    } else {
        1
    };
    let logical = if file & libc::ATTR_FILE_TOTALSIZE != 0 {
        c.u64()?
    } else {
        0
    };
    let allocated = if file & libc::ATTR_FILE_ALLOCSIZE != 0 {
        c.u64()?
    } else {
        0
    };

    let name = name?;
    if &*name == "." || &*name == ".." {
        return None;
    }
    let kind = match objtype? {
        VREG => EntryKind::File,
        VDIR => EntryKind::Dir,
        VLNK => EntryKind::Symlink,
        _ => EntryKind::Other,
    };
    Some(Entry {
        name,
        kind,
        dev,
        inode,
        mtime,
        mount_point,
        entry_count: (kind == EntryKind::Dir).then_some(entry_count).flatten(),
        nlink,
        logical,
        allocated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn bulk_listing_matches_lstat() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("small.txt"), b"hello").unwrap();
        std::fs::write(root.join("big.bin"), vec![1u8; 300_000]).unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::os::unix::fs::symlink("small.txt", root.join("link")).unwrap();
        std::fs::hard_link(root.join("big.bin"), root.join("big-link.bin")).unwrap();
        for i in 0..2000 {
            // Enough entries to need several batches with long names.
            std::fs::write(root.join(format!("file-with-a-long-name-{i:05}.dat")), b"x").unwrap();
        }

        let mut entries = read_dir(root).expect("APFS supports getattrlistbulk");
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(entries.len(), 2005);

        for entry in &entries {
            let md = std::fs::symlink_metadata(root.join(&*entry.name)).unwrap();
            assert_eq!(entry.inode, md.ino(), "{}", entry.name);
            assert_eq!(entry.mtime, md.mtime(), "{}", entry.name);
            assert_eq!(entry.dev, md.dev() as u32 as u64, "{}", entry.name);
            match entry.kind {
                EntryKind::File => {
                    assert_eq!(entry.logical, md.len(), "{}", entry.name);
                    assert_eq!(entry.allocated, md.blocks() * 512, "{}", entry.name);
                    assert_eq!(u64::from(entry.nlink), md.nlink(), "{}", entry.name);
                }
                EntryKind::Dir => assert!(md.is_dir()),
                EntryKind::Symlink => assert!(md.file_type().is_symlink()),
                EntryKind::Other => panic!("unexpected kind for {}", entry.name),
            }
        }
        let sub = entries.iter().find(|e| &*e.name == "sub").unwrap();
        assert_eq!(sub.entry_count, Some(0));
        let big = entries.iter().find(|e| &*e.name == "big.bin").unwrap();
        assert_eq!(big.entry_count, None);
        assert_eq!(big.nlink, 2);
        assert!(!entries.iter().any(|e| e.mount_point));
    }

    #[test]
    fn firmlink_stubs_are_not_reported_empty() {
        // On the sealed system volume `/Users` is an empty stub that a
        // firmlink redirects into the Data volume.
        let entries = read_dir(Path::new("/")).unwrap();
        let users = entries.iter().find(|e| &*e.name == "Users").unwrap();
        assert_ne!(users.entry_count, Some(0));
    }

    #[test]
    fn mount_points_are_flagged() {
        // /System/Volumes/VM is its own APFS volume on every modern Mac.
        if let Ok(entries) = read_dir(Path::new("/System/Volumes"))
            && let Some(vm) = entries.iter().find(|e| &*e.name == "VM")
        {
            assert!(vm.mount_point);
        }
    }

    #[test]
    fn missing_and_non_directory_paths_are_io_errors() {
        assert!(matches!(
            read_dir(Path::new("/definitely/not/here")),
            Err(BulkError::Io(_))
        ));
        let file = tempfile::NamedTempFile::new().unwrap();
        assert!(matches!(read_dir(file.path()), Err(BulkError::Io(_))));
    }

    #[test]
    fn truncated_records_are_ignored() {
        let mut out = Vec::new();
        parse_batch(&[40, 0, 0, 0, 1, 2], 1, &mut out);
        assert!(out.is_empty());
    }
}

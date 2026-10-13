//! Mounted volumes for the welcome screen.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub struct Volume {
    pub name: String,
    pub path: PathBuf,
    pub total: u64,
    pub free: u64,
}

impl Volume {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
}

/// The startup volume first, then everything else under /Volumes.
pub fn list() -> Vec<Volume> {
    let mut boot_name = "Macintosh HD".to_owned();
    let mut others = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.starts_with("com.apple.") {
                continue;
            }
            let path = entry.path();
            match std::fs::read_link(&path) {
                // The startup volume appears as a symlink to "/".
                Ok(target) if target == Path::new("/") => boot_name = name,
                Ok(_) => {}
                Err(_) => {
                    if let Some((total, free)) = capacity(&path) {
                        others.push(Volume {
                            name,
                            path,
                            total,
                            free,
                        });
                    }
                }
            }
        }
    }
    others.sort_by(|a, b| a.name.cmp(&b.name));

    let mut out = Vec::with_capacity(others.len() + 1);
    if let Some((total, free)) = capacity(Path::new("/")) {
        out.push(Volume {
            name: boot_name,
            path: PathBuf::from("/"),
            total,
            free,
        });
    }
    out.extend(others);
    out
}

/// (total, available) bytes for the filesystem containing `path`.
pub fn capacity(path: &Path) -> Option<(u64, u64)> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs only writes into the zeroed struct we pass it and
    // reads the NUL-terminated path, which outlives the call.
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        let frsize = stat.f_frsize as u64;
        Some((stat.f_blocks as u64 * frsize, stat.f_bavail as u64 * frsize))
    }
}

/// How a volume's space breaks down beyond the files a scan can see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskAccounting {
    /// Bytes in use on the volume (for APFS: the whole container).
    pub used: u64,
    /// Bytes available.
    pub free: u64,
    /// Other volumes in the same APFS container that the scan did not
    /// enter, with the bytes each consumes.
    pub other_volumes: Vec<(String, u64)>,
}

/// Disk accounting for a scan rooted at a volume's mount point, or `None`
/// when `root` is an ordinary folder. Mounted sibling volumes are left out
/// when the scan crossed volume boundaries, since it counted them already.
pub fn accounting_for(root: &Path, crossed_volumes: bool) -> Option<DiskAccounting> {
    let mount = mount_info(root)?;
    if Path::new(&mount.on) != root {
        return None;
    }
    let (total, free) = capacity(root)?;
    let other_volumes = container_of(&mount.from)
        .map(|container| sibling_volumes(&container, crossed_volumes))
        .unwrap_or_default();
    Some(DiskAccounting {
        used: total.saturating_sub(free),
        free,
        other_volumes,
    })
}

struct MountInfo {
    on: String,
    from: String,
}

fn c_chars_to_string(chars: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn mount_info(path: &Path) -> Option<MountInfo> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statfs writes only into the zeroed struct and reads the
    // NUL-terminated path, which outlives the call.
    unsafe {
        let mut stat: libc::statfs = std::mem::zeroed();
        if libc::statfs(c_path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        Some(MountInfo {
            on: c_chars_to_string(&stat.f_mntonname),
            from: c_chars_to_string(&stat.f_mntfromname),
        })
    }
}

/// Device identifiers ("disk3s6") of every mounted filesystem.
fn mounted_devices() -> Vec<String> {
    // SAFETY: the first call sizes the buffer; the second fills at most
    // `capacity` zeroed entries and returns how many it wrote.
    unsafe {
        let count = libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT);
        if count <= 0 {
            return Vec::new();
        }
        let capacity = count as usize + 8;
        let mut buf: Vec<libc::statfs> = vec![std::mem::zeroed(); capacity];
        let bytes = (capacity * std::mem::size_of::<libc::statfs>()) as libc::c_int;
        let filled = libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT);
        if filled <= 0 {
            return Vec::new();
        }
        buf.truncate(filled as usize);
        buf.iter()
            .map(|s| c_chars_to_string(&s.f_mntfromname))
            .filter_map(|from| device_id(&from))
            .collect()
    }
}

/// "/dev/disk3s1s1" (a sealed system snapshot) -> "disk3s1".
fn device_id(from: &str) -> Option<String> {
    let rest = from.strip_prefix("/dev/disk")?;
    let mut parts = rest.split('s');
    let disk = parts.next().filter(|d| !d.is_empty())?;
    let slice = parts.next().filter(|s| !s.is_empty())?;
    Some(format!("disk{disk}s{slice}"))
}

/// "/dev/disk3s1s1" -> "disk3".
fn container_of(from: &str) -> Option<String> {
    let name = from.strip_prefix("/dev/")?;
    let disk: String = name
        .strip_prefix("disk")?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!disk.is_empty()).then(|| format!("disk{disk}"))
}

/// APFS volumes in `container` other than the System and Data roles, which
/// a scan of the startup disk already covers.
fn sibling_volumes(container: &str, crossed_volumes: bool) -> Vec<(String, u64)> {
    let Some(json) = apfs_list_json() else {
        return Vec::new();
    };
    let mounted = if crossed_volumes {
        mounted_devices()
    } else {
        Vec::new()
    };
    parse_sibling_volumes(&json, container, &mounted)
}

fn apfs_list_json() -> Option<serde_json::Value> {
    use std::process::{Command, Stdio};
    let plist = Command::new("diskutil")
        .args(["apfs", "list", "-plist"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let mut convert = Command::new("plutil")
        .args(["-convert", "json", "-o", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    {
        use std::io::Write;
        convert.stdin.take()?.write_all(&plist.stdout).ok()?;
    }
    let out = convert.wait_with_output().ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

fn parse_sibling_volumes(
    json: &serde_json::Value,
    container: &str,
    mounted: &[String],
) -> Vec<(String, u64)> {
    let Some(containers) = json.get("Containers").and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    let Some(target) = containers
        .iter()
        .find(|c| c.get("ContainerReference").and_then(|r| r.as_str()) == Some(container))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for volume in target
        .get("Volumes")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let roles: Vec<&str> = volume
            .get("Roles")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        if roles.iter().any(|r| *r == "System" || *r == "Data") {
            continue;
        }
        let device = volume
            .get("DeviceIdentifier")
            .and_then(|d| d.as_str())
            .unwrap_or("");
        if mounted.iter().any(|m| m == device) {
            continue;
        }
        let name = volume
            .get("Name")
            .and_then(|n| n.as_str())
            .unwrap_or("Volume");
        let bytes = volume
            .get("CapacityInUse")
            .and_then(|b| b.as_u64())
            .unwrap_or(0);
        if bytes > 0 {
            out.push((name.to_owned(), bytes));
        }
    }
    out.sort_by_key(|v| std::cmp::Reverse(v.1));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_and_container_names_parse() {
        assert_eq!(device_id("/dev/disk3s1s1").as_deref(), Some("disk3s1"));
        assert_eq!(device_id("/dev/disk12s4").as_deref(), Some("disk12s4"));
        assert_eq!(device_id("/dev/disk3"), None);
        assert_eq!(container_of("/dev/disk3s1s1").as_deref(), Some("disk3"));
        assert_eq!(container_of("/dev/disk12s4").as_deref(), Some("disk12"));
        assert_eq!(container_of("map auto_home"), None);
    }

    #[test]
    fn sibling_volumes_skip_system_data_and_mounted() {
        let json = serde_json::json!({
            "Containers": [
                { "ContainerReference": "disk1", "Volumes": [
                    { "Name": "Other", "Roles": [], "CapacityInUse": 5, "DeviceIdentifier": "disk1s1" }
                ]},
                { "ContainerReference": "disk3", "Volumes": [
                    { "Name": "Macintosh HD", "Roles": ["System"], "CapacityInUse": 11, "DeviceIdentifier": "disk3s1" },
                    { "Name": "Preboot", "Roles": ["Preboot"], "CapacityInUse": 15, "DeviceIdentifier": "disk3s2" },
                    { "Name": "VM", "Roles": ["VM"], "CapacityInUse": 4, "DeviceIdentifier": "disk3s6" },
                    { "Name": "Data", "Roles": ["Data"], "CapacityInUse": 450, "DeviceIdentifier": "disk3s7" }
                ]}
            ]
        });
        assert_eq!(
            parse_sibling_volumes(&json, "disk3", &[]),
            vec![("Preboot".to_owned(), 15), ("VM".to_owned(), 4)]
        );
        assert_eq!(
            parse_sibling_volumes(&json, "disk3", &["disk3s6".to_owned()]),
            vec![("Preboot".to_owned(), 15)]
        );
    }

    #[test]
    fn ordinary_folders_get_no_accounting() {
        let dir = std::env::temp_dir();
        let canonical = std::fs::canonicalize(dir).unwrap();
        assert!(accounting_for(&canonical, false).is_none());
    }

    #[test]
    fn startup_disk_accounting_covers_used_and_free() {
        let acc = accounting_for(Path::new("/"), false).expect("/ is a mount point");
        let (total, _) = capacity(Path::new("/")).unwrap();
        assert_eq!(acc.used + acc.free, total);
    }

    #[test]
    fn startup_volume_is_listed_first() {
        let volumes = list();
        assert_eq!(volumes[0].path, PathBuf::from("/"));
        assert!(volumes[0].total > 0);
        assert!(volumes[0].used() <= volumes[0].total);
    }

    #[test]
    fn capacity_fails_for_missing_paths() {
        assert!(capacity(Path::new("/definitely/not/here")).is_none());
    }
}

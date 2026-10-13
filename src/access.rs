//! Full Disk Access detection.
//!
//! macOS has no API that prompts for Full Disk Access; an app can only find
//! out whether it has it and send the user to the right Settings pane. The
//! probe reads files that only Full Disk Access unlocks.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// True when protected locations are readable.
pub fn has_full_disk_access() -> bool {
    // The system TCC database is readable only with Full Disk Access.
    match std::fs::File::open("/Library/Application Support/com.apple.TCC/TCC.db") {
        Ok(_) => return true,
        Err(err) if err.kind() == ErrorKind::PermissionDenied => return false,
        Err(_) => {}
    }
    // Fall back to a per-user protected folder.
    let Some(home) = std::env::home_dir() else {
        return false;
    };
    probe_dir(&home.join("Library/Safari")).unwrap_or(false)
}

fn probe_dir(path: &Path) -> Option<bool> {
    match std::fs::read_dir(path) {
        Ok(_) => Some(true),
        Err(err) if err.kind() == ErrorKind::NotFound => None,
        Err(_) => Some(false),
    }
}

/// Open System Settings > Privacy & Security > Full Disk Access.
pub fn open_settings() -> std::io::Result<()> {
    std::process::Command::new("open")
        .arg(SETTINGS_URL)
        .spawn()
        .map(|_| ())
}

/// The enclosing `.app` bundle, when running from one. macOS attributes a
/// bare executable's file access to the terminal that launched it instead.
pub fn app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    bundle_of(&exe)
}

fn bundle_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_is_found_from_the_executable_path() {
        let exe = Path::new("/Applications/MacDirStat.app/Contents/MacOS/MacDirStat");
        assert_eq!(
            bundle_of(exe),
            Some(PathBuf::from("/Applications/MacDirStat.app"))
        );
        assert_eq!(bundle_of(Path::new("/usr/local/bin/macdirstat")), None);
    }

    #[test]
    fn missing_probe_paths_are_inconclusive() {
        assert_eq!(probe_dir(Path::new("/definitely/not/here")), None);
    }
}

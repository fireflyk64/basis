//! Finds the load-client executable in a build directory and makes sure it can actually be run.
//!
//! On Linux the execute bit is the first thing lost when a build is produced on Windows, copied
//! off a share, or unzipped. The missing bit is repaired in place when the file is ours to
//! repair, and when it is not, the error names the file and the one-line fix instead of leaving
//! the caller to reach for sudo.

use std::path::{Path, PathBuf};

pub struct LaunchTarget;

impl LaunchTarget {
    /// Resolves `base_name` inside `directory` and returns a path that is ready to start.
    pub fn resolve(directory: &Path, base_name: &str) -> Result<PathBuf, String> {
        let path = Self::find(directory, base_name).ok_or_else(|| format!("Could not find {base_name} in {}. Build the workspace in release first.", directory.display()))?;
        Self::ensure_executable(&path)?;
        Ok(path)
    }

    /// Locates the executable without touching it, for "is a build here?" probes.
    pub fn find(directory: &Path, base_name: &str) -> Option<PathBuf> {
        if !directory.is_dir() {
            return None;
        }
        let windows = directory.join(format!("{base_name}.exe"));
        if windows.is_file() {
            return Some(windows);
        }
        let unix = directory.join(base_name);
        if unix.is_file() { Some(unix) } else { None }
    }

    /// Adds the execute bit on Unix when it is missing. No-op when the file is already runnable.
    pub fn ensure_executable(path: &Path) -> Result<(), String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::metadata(path).map_err(|e| format!("Could not read permissions on '{}': {e}", path.display()))?;
            let mode = metadata.permissions().mode();
            if mode & 0o111 != 0 {
                return Ok(());
            }
            // Mirror the read bits rather than granting a blanket 0755: a build directory that is
            // deliberately group- or user-private stays that way.
            let mut wanted = mode;
            if mode & 0o400 != 0 {
                wanted |= 0o100;
            }
            if mode & 0o040 != 0 {
                wanted |= 0o010;
            }
            if mode & 0o004 != 0 {
                wanted |= 0o001;
            }
            if wanted == mode {
                wanted |= 0o100;
            }
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(wanted)).map_err(|e| {
                format!(
                    "'{}' is not executable and the agent could not make it so ({e}). Run: chmod +x \"{}\"\nThe execute bit does not survive a Windows build, a zip, or most CI artifact downloads. Nothing here needs elevated permissions - every port is above 1024 and no system setting is changed - so do not reach for sudo: running as root leaves root-owned config files and logs that the next unprivileged run cannot rewrite.",
                    path.display(),
                    path.display()
                )
            })
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_and_repair_execute_bit() {
        let dir = std::env::temp_dir().join(format!("basis_launch_target_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(LaunchTarget::find(&dir, "nothing").is_none());
        let exe = dir.join("tool");
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o640)).unwrap();
            let resolved = LaunchTarget::resolve(&dir, "tool").unwrap();
            assert_eq!(resolved, exe);
            let mode = std::fs::metadata(&exe).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o750);
        }
        assert!(LaunchTarget::resolve(&dir, "missing").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

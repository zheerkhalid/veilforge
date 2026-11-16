//! Removing a sandbox folder again, however locked its content is.
//! For tests and `--selftest` only: it refuses anything that is not a
//! `veilforge-*` folder inside the system temp folder.

use super::{acl, safety, win};
use std::path::{Path, PathBuf};

fn is_sandbox(dir: &Path) -> bool {
    let dir = safety::canonical(dir);
    let temp = safety::canonical(&std::env::temp_dir().join("x"));
    let Some(temp) = temp.parent() else {
        return false;
    };
    let name_ok = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase().starts_with("veilforge-"))
        .unwrap_or(false);
    let parent_ok = dir
        .parent()
        .map(|p| safety::norm(&p.to_string_lossy()) == safety::norm(&temp.to_string_lossy()))
        .unwrap_or(false);
    name_ok && parent_ok
}

fn nuke(path: &Path) {
    let Ok(st) = win::stat(path) else {
        // Not even the attributes can be read: open the DACL and look again.
        if win::set_dacl(path, &acl::open_sddl()).is_ok() && win::stat(path).is_ok() {
            nuke(path);
        }
        return;
    };
    if st.is_link() {
        // Remove the link itself; never look at what it points to.
        if st.is_dir() {
            let _ = std::fs::remove_dir(win::long(path));
        } else {
            let _ = std::fs::remove_file(win::long(path));
        }
        return;
    }
    let _ = win::set_dacl(path, &acl::open_sddl());
    let _ = win::set_attributes(path, win::ATTR_NORMAL);
    if st.is_dir() {
        if let Ok(entries) = win::list(path) {
            for e in entries {
                nuke(&path.join(e.name));
            }
        }
        let _ = std::fs::remove_dir(win::long(path));
    } else {
        let _ = std::fs::remove_file(win::long(path));
    }
}

/// Resets permissions and attributes below a sandbox folder and deletes it.
/// Returns true when nothing is left. Anything that is not a `veilforge-*`
/// folder directly inside the temp folder is refused (returns false).
pub fn force_remove(dir: &Path) -> bool {
    if !is_sandbox(dir) {
        return false;
    }
    if !win::exists(dir) {
        return true;
    }
    nuke(dir);
    !win::exists(dir)
}

/// Deletes its sandbox folder when dropped (also when a test panics).
pub struct SandboxGuard(pub PathBuf);

impl SandboxGuard {
    /// A fresh `veilforge-<tag>-...` folder in the temp folder.
    pub fn new(tag: &str) -> SandboxGuard {
        SandboxGuard(crate::util::unique_temp_dir(tag))
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for SandboxGuard {
    fn drop(&mut self) {
        force_remove(&self.0);
    }
}

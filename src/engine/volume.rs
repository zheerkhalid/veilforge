//! What the engine needs to know about a volume: its root, its file system
//! and whether it stores ACLs. Tests can override the answer for a folder.

use super::win;
use std::path::Path;
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsInfo {
    /// "NTFS", "exFAT", "FAT32", ...
    pub name: String,
    /// The volume stores ACLs (FILE_PERSISTENT_ACLS).
    pub acl: bool,
    /// A network drive.
    pub remote: bool,
}

static OVERRIDES: Mutex<Vec<(String, FsInfo)>> = Mutex::new(Vec::new());

fn key(path: &Path) -> String {
    win::plain(path).trim_end_matches('\\').to_lowercase()
}

/// Test hook: pretend that everything under `prefix` lives on a volume with
/// these properties (`None` removes the override). Used to simulate FAT/exFAT.
pub fn set_override(prefix: &Path, info: Option<FsInfo>) {
    let k = key(prefix);
    let mut all = OVERRIDES.lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|(p, _)| *p != k);
    if let Some(info) = info {
        all.push((k, info));
    }
}

/// File system facts for the volume holding `path`.
pub fn info_for(path: &Path) -> FsInfo {
    let k = key(path);
    {
        let all = OVERRIDES.lock().unwrap_or_else(|e| e.into_inner());
        for (prefix, info) in all.iter() {
            if k == *prefix || k.starts_with(&format!("{prefix}\\")) {
                return info.clone();
            }
        }
    }
    let unc = win::plain(path).starts_with(r"\\");
    match root_of(path).and_then(|root| win::volume_info(&root).ok()) {
        Some((name, acl, remote)) => FsInfo {
            name,
            acl,
            remote: remote || unc,
        },
        None => FsInfo {
            name: String::new(),
            acl: true,
            remote: unc,
        },
    }
}

/// Volume root (`D:\`) of a path that may not exist yet.
pub fn root_of(path: &Path) -> Option<String> {
    win::volume_root(path).ok()
}

/// Whether two paths are on the same volume.
pub fn same_volume(a: &Path, b: &Path) -> bool {
    match (root_of(a), root_of(b)) {
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(&y),
        _ => false,
    }
}

//! The refusal rules and the pre-flight scan.
//!
//! The rules work on path text only (compared without case), against an
//! injectable set of "special folders", so they can be tested with invented
//! paths and never depend on the PC they run on.

use super::{hold, volume, win, Env, Precheck, Verdict};
use crate::model::{Item, Kind};
use std::path::{Path, PathBuf};

/// The folders the rules know about.
#[derive(Clone, Debug, Default)]
pub struct Special {
    pub windows: Option<PathBuf>,
    pub program_files: Vec<PathBuf>,
    pub program_data: Option<PathBuf>,
    /// The folder holding all profiles (`C:\Users`).
    pub users: Option<PathBuf>,
    /// The current user's profile (`C:\Users\alex`).
    pub profile: Option<PathBuf>,
    /// The folder of the VeilForge EXE.
    pub exe_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub holds: Vec<PathBuf>,
    /// Roots of cloud-synced folders (OneDrive, Dropbox, ...): a warning only.
    pub cloud: Vec<PathBuf>,
    /// Sandbox root: paths inside it are exempt from the system-folder rules
    /// (the temp folder may itself live in a refused place), never from the
    /// rules about VeilForge's own folders and already protected items.
    pub sandbox_root: Option<PathBuf>,
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

impl Special {
    /// The special folders of this PC and this environment.
    pub fn real(env: &Env) -> Special {
        let profile = env_path("USERPROFILE");
        let users = env_path("PUBLIC")
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .or_else(|| {
                profile
                    .as_ref()
                    .and_then(|p| p.parent().map(Path::to_path_buf))
            });
        let mut program_files = Vec::new();
        for name in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
            if let Some(p) = env_path(name) {
                if !program_files.contains(&p) {
                    program_files.push(p);
                }
            }
        }
        let mut cloud = Vec::new();
        for name in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
            if let Some(p) = env_path(name) {
                if !cloud.contains(&p) {
                    cloud.push(p);
                }
            }
        }
        if let Some(profile) = &profile {
            for name in [
                "Dropbox",
                "Google Drive",
                "My Drive",
                "iCloudDrive",
                "iCloud Drive",
                "Box",
            ] {
                let p = profile.join(name);
                if p.is_dir() {
                    cloud.push(p);
                }
            }
        }
        Special {
            windows: env_path("SystemRoot").or_else(|| env_path("windir")),
            program_files,
            program_data: env_path("ProgramData"),
            users,
            profile,
            exe_dir: env.exe.parent().map(Path::to_path_buf),
            data_dir: Some(env.data_dir.clone()),
            holds: hold::known(env),
            cloud,
            sandbox_root: if env.sandbox {
                env.data_dir.parent().map(Path::to_path_buf)
            } else {
                None
            },
        }
    }
}

/// Lower case, backslashes, no `\\?\` prefix, no trailing separator.
pub fn norm(path: &str) -> String {
    let mut s = path.replace('/', "\\").to_lowercase();
    if let Some(rest) = s.strip_prefix(r"\\?\unc\") {
        s = format!(r"\\{rest}");
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        s = rest.to_string();
    }
    while s.len() > 2 && s.ends_with('\\') {
        s.pop();
    }
    s
}

fn norm_p(path: &Path) -> String {
    norm(&path.to_string_lossy())
}

/// `a` is `b` or lies somewhere below it (both normalised).
fn within(a: &str, b: &str) -> bool {
    a == b || (a.len() > b.len() && a.starts_with(b) && a.as_bytes()[b.len()] == b'\\')
}

/// The two paths are the same, or one contains the other.
fn overlaps(a: &str, b: &str) -> bool {
    within(a, b) || within(b, a)
}

fn is_drive_root(n: &str) -> bool {
    let b = n.as_bytes();
    if b.len() == 2 && b[1] == b':' {
        return true;
    }
    if let Some(rest) = n.strip_prefix(r"\\") {
        // \\server or \\server\share
        return rest.split('\\').filter(|p| !p.is_empty()).count() <= 2;
    }
    false
}

/// A network path by its spelling (`\\server\share\...`).
pub fn is_unc(path: &str) -> bool {
    norm(path).starts_with(r"\\")
}

/// Whether the path lies in a known cloud-synced folder.
pub fn in_cloud_folder(sp: &Special, path: &str) -> bool {
    let n = norm(path);
    sp.cloud.iter().any(|c| within(&n, &norm_p(c)))
}

/// Why this path must not be protected, or `None` when the rules allow it.
/// `items` are the items already in the map.
pub fn refusal(sp: &Special, path: &str, items: &[Item]) -> Option<String> {
    let n = norm(path);
    if n.is_empty() {
        return Some("This is not a file or folder path.".into());
    }
    if is_drive_root(&n) {
        return Some("A whole drive cannot be protected. Choose a folder on it instead.".into());
    }

    // VeilForge's own places: never, sandbox or not.
    if let Some(d) = &sp.data_dir {
        if overlaps(&n, &norm_p(d)) {
            return Some(
                "This is where VeilForge keeps its own data. It cannot be protected.".into(),
            );
        }
    }
    for h in &sp.holds {
        if overlaps(&n, &norm_p(h)) {
            return Some(
                "This is VeilForge's holding folder (or contains it). It cannot be protected."
                    .into(),
            );
        }
    }
    for it in items {
        let o = norm(&it.original);
        if n == o {
            return Some("This item is already protected by VeilForge.".into());
        }
        if within(&n, &o) {
            return Some("This is inside a folder that VeilForge already protects.".into());
        }
        if within(&o, &n) {
            return Some(format!(
                "This folder contains \"{}\", which VeilForge already protects. Unlock that first.",
                it.name
            ));
        }
        if let Some(s) = &it.standin {
            if n == norm(s) {
                return Some(
                    "This is the shortcut VeilForge left for a protected item. It cannot be protected."
                        .into(),
                );
            }
        }
    }

    let in_sandbox = sp
        .sandbox_root
        .as_ref()
        .map(|r| within(&n, &norm_p(r)))
        .unwrap_or(false);
    if in_sandbox {
        return None;
    }

    if let Some(d) = &sp.exe_dir {
        if overlaps(&n, &norm_p(d)) {
            return Some(
                "This is the folder VeilForge runs from (or contains it). It cannot be protected."
                    .into(),
            );
        }
    }
    if let Some(w) = &sp.windows {
        if overlaps(&n, &norm_p(w)) {
            return Some(
                "This is the Windows folder (or contains it). Protecting it would stop Windows from working."
                    .into(),
            );
        }
    }
    for p in &sp.program_files {
        if overlaps(&n, &norm_p(p)) {
            return Some(
                "This is the Program Files folder (or part of it, or contains it). Protecting it would break installed programs."
                    .into(),
            );
        }
    }
    if let Some(p) = &sp.program_data {
        if overlaps(&n, &norm_p(p)) {
            return Some(
                "This is the ProgramData folder (or part of it, or contains it). Protecting it would break installed programs."
                    .into(),
            );
        }
    }
    let profile = sp.profile.as_ref().map(|p| norm_p(p));
    if let Some(u) = &sp.users {
        let u = norm_p(u);
        if within(&u, &n) {
            return Some(
                "This is the folder with all user profiles (or contains it). It cannot be protected."
                    .into(),
            );
        }
        if within(&n, &u) {
            match &profile {
                Some(p) if within(&n, p) => {}
                _ => {
                    return Some(
                        "This belongs to another user's profile. VeilForge only protects your own files."
                            .into(),
                    )
                }
            }
        }
    }
    if let Some(p) = &profile {
        if within(p, &n) {
            return Some(
                "This is your whole user profile (or contains it). Choose a folder inside it instead."
                    .into(),
            );
        }
        for sub in [
            "appdata",
            "appdata\\local",
            "appdata\\roaming",
            "appdata\\locallow",
        ] {
            if n == format!("{p}\\{sub}") {
                return Some(
                    "This is where your programs keep their settings. Protecting it would break them."
                        .into(),
                );
            }
        }
    }
    None
}

const CLOUD_REPARSE_MASK: u32 = 0xFFFF_0FFF;
const CLOUD_REPARSE_TAG: u32 = 0x9000_001A;
const ATTR_PINNED_OR_UNPINNED: u32 = 0x0018_0000;

/// Heuristic: attributes that only cloud sync providers set, or the cloud
/// files reparse tag.
fn looks_cloud_synced(path: &Path, attrs: u32) -> bool {
    if attrs
        & (win::ATTR_RECALL_ON_DATA_ACCESS
            | win::ATTR_RECALL_ON_OPEN
            | win::ATTR_OFFLINE
            | ATTR_PINNED_OR_UNPINNED)
        != 0
    {
        return true;
    }
    attrs & win::ATTR_REPARSE != 0
        && win::reparse_tag(path)
            .map(|t| t & CLOUD_REPARSE_MASK == CLOUD_REPARSE_TAG)
            .unwrap_or(false)
}

/// Makes a path absolute and resolves links and short names in its folder
/// part, so that the rules see the real location. No `\\?\` prefix.
pub fn canonical(path: &Path) -> PathBuf {
    let abs = std::path::absolute(win::plain(path)).unwrap_or_else(|_| path.to_path_buf());
    let abs = PathBuf::from(norm_keep_case(&abs.to_string_lossy()));
    match (abs.parent(), abs.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
            match std::fs::canonicalize(win::long(parent)) {
                Ok(real) => PathBuf::from(win::plain(&real)).join(name),
                Err(_) => abs,
            }
        }
        _ => abs,
    }
}

fn norm_keep_case(path: &str) -> String {
    let mut s = path.replace('/', "\\");
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s
}

struct Count {
    size: u64,
    files: u64,
    entries: u64,
    truncated: bool,
    cloud: bool,
}

fn count_tree(root: &Path, limit: u64) -> Count {
    let mut c = Count {
        size: 0,
        files: 0,
        entries: 0,
        truncated: false,
        cloud: false,
    };
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = win::list(&dir) else {
            continue;
        };
        for e in entries {
            if c.entries >= limit {
                c.truncated = true;
                return c;
            }
            c.entries += 1;
            if e.stat.attributes & (win::ATTR_RECALL_ON_DATA_ACCESS | win::ATTR_RECALL_ON_OPEN) != 0
            {
                c.cloud = true;
            }
            if e.stat.is_dir() && !e.stat.is_link() {
                stack.push(dir.join(&e.name));
            } else {
                c.files += 1;
                c.size += e.stat.size;
            }
        }
    }
    c
}

/// The full pre-flight check against the real special folders.
pub fn precheck(env: &Env, path: &Path, items: &[Item]) -> Precheck {
    precheck_with(env, &Special::real(env), path, items, super::SCAN_LIMIT)
}

/// The pre-flight check against a given set of special folders. With a
/// `scan_limit` of 0 the folder is not walked at all (rules only).
pub fn precheck_with(
    _env: &Env,
    sp: &Special,
    path: &Path,
    items: &[Item],
    scan_limit: u64,
) -> Precheck {
    let path = canonical(path);
    let text = path.to_string_lossy().into_owned();
    let mut pre = Precheck {
        path: path.clone(),
        verdict: Verdict::Ok,
        refusal: None,
        warnings: Vec::new(),
        kind: Kind::File,
        size: 0,
        files: 0,
        filesystem: String::new(),
        acl_supported: true,
        scramble_default: false,
        truncated: false,
    };
    let refuse = |mut pre: Precheck, why: String| {
        pre.verdict = Verdict::Refuse;
        pre.refusal = Some(why);
        pre
    };
    if let Some(why) = refusal(sp, &text, items) {
        if let Ok(st) = win::stat(&path) {
            pre.kind = if st.is_dir() {
                Kind::Folder
            } else {
                Kind::File
            };
        }
        return refuse(pre, why);
    }
    let st = match win::stat(&path) {
        Ok(st) => st,
        Err(_) => return refuse(pre, "This file or folder does not exist.".into()),
    };
    pre.kind = if st.is_dir() {
        Kind::Folder
    } else {
        Kind::File
    };
    if st.is_link() {
        let why = if looks_cloud_synced(&path, st.attributes) {
            "This is an online-only placeholder of a cloud-synced item. Make it available offline first."
        } else {
            "This is a link to another place, not a real file or folder. Protect the place it points to instead."
        };
        return refuse(pre, why.into());
    }

    let fs = volume::info_for(&path);
    pre.filesystem = fs.name.clone();
    pre.acl_supported = fs.acl;
    let mut cloud = in_cloud_folder(sp, &text) || looks_cloud_synced(&path, st.attributes);
    let mut entries = 1u64;
    if st.is_dir() && scan_limit > 0 {
        let c = count_tree(&path, scan_limit);
        pre.size = c.size;
        pre.files = c.files;
        pre.truncated = c.truncated;
        entries = c.entries;
        cloud |= c.cloud;
        pre.scramble_default = !c.truncated && c.entries <= super::SCRAMBLE_DEFAULT_MAX_FILES;
    } else {
        pre.size = st.size;
        pre.files = 1;
    }

    if fs.remote || is_unc(&text) {
        pre.warnings.push(
            "This is on a network drive. Protection there depends on the server, and other computers may still reach the item."
                .into(),
        );
    }
    if cloud {
        pre.warnings.push(
            "This is in a cloud-synced folder. Hiding it looks like a delete to the sync program, which may then remove it from the cloud and from your other devices. Locking it stops it from syncing."
                .into(),
        );
    }
    if entries > super::LARGE_TREE_FILES || pre.truncated {
        pre.warnings.push(format!(
            "This folder is very large (more than {} entries). Protecting and unlocking it will take a while.",
            crate::util::thousands(entries.min(super::LARGE_TREE_FILES))
        ));
    }
    if !fs.acl {
        let name = if fs.name.is_empty() {
            "This drive".to_string()
        } else {
            format!("This drive ({})", fs.name)
        };
        pre.warnings.push(format!(
            "{name} cannot store permissions. Only hiding is possible here, with reduced protection: the holding folder is hidden but not locked."
        ));
    }
    if !pre.warnings.is_empty() {
        pre.verdict = Verdict::Warn;
    }
    pre
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Extra, Mode, Saved};

    fn special() -> Special {
        Special {
            windows: Some(r"C:\Windows".into()),
            program_files: vec![r"C:\Program Files".into(), r"C:\Program Files (x86)".into()],
            program_data: Some(r"C:\ProgramData".into()),
            users: Some(r"C:\Users".into()),
            profile: Some(r"C:\Users\alex".into()),
            exe_dir: Some(r"D:\Apps\VeilForge".into()),
            data_dir: Some(r"C:\Users\alex\AppData\Roaming\VeilForge".into()),
            holds: vec![r"C:\VeilForge.sys".into(), r"D:\VeilForge.sys".into()],
            cloud: vec![r"C:\Users\alex\OneDrive".into()],
            sandbox_root: None,
        }
    }

    fn item(original: &str, standin: Option<&str>) -> Item {
        Item {
            id: "0011223344556677".into(),
            name: original.rsplit('\\').next().unwrap().into(),
            original: original.into(),
            kind: Kind::Folder,
            mode: Mode::Hide,
            ask_hidden: true,
            scrambled: false,
            group: String::new(),
            size: 0,
            files: 0,
            locked_at: 0,
            open_since: None,
            volume: "C:\\".into(),
            hold_path: None,
            standin: standin.map(str::to_string),
            reduced: false,
            saved: Saved::default(),
            children: Vec::new(),
            extra: Extra::default(),
        }
    }

    #[test]
    fn system_places_and_their_ancestors_are_refused() {
        let sp = special();
        for p in [
            r"C:\",
            r"C:",
            r"D:\",
            r"\\server\share",
            r"\\server\share\",
            r"C:\Windows",
            r"C:\WINDOWS\System32",
            r"c:\windows\system32\drivers\etc\hosts",
            r"C:\Program Files",
            r"C:\Program Files\Some App\app.exe",
            r"C:\Program Files (x86)\Other",
            r"C:\ProgramData",
            r"C:\ProgramData\Vendor\settings.ini",
            r"C:\Users",
            r"C:\Users\alex",
            r"C:\Users\alex\",
            r"\\?\C:\Users\alex",
            r"C:\Users\maria",
            r"C:\Users\maria\Documents\diary.txt",
            r"C:\Users\Public\Pictures",
            r"C:\Users\alexander\notes.txt",
            r"C:\Users\alex\AppData",
            r"C:\Users\alex\AppData\Roaming",
            r"C:\Users\alex\AppData\Local",
            r"C:\Users\alex\AppData\Roaming\VeilForge",
            r"C:\Users\alex\AppData\Roaming\VeilForge\map.vfm",
            r"D:\Apps\VeilForge",
            r"D:\Apps\VeilForge\VeilForge.exe",
            r"D:\Apps",
            r"C:\VeilForge.sys",
            r"C:\VeilForge.sys\0123456789abcdef",
            r"D:\VeilForge.sys\0123456789abcdef\inner",
        ] {
            assert!(refusal(&sp, p, &[]).is_some(), "should refuse {p}");
        }
    }

    #[test]
    fn ordinary_places_are_allowed() {
        let sp = special();
        for p in [
            r"C:\Users\alex\Documents",
            r"C:\Users\alex\Documents\taxes 2024.xlsx",
            r"C:\Users\alex\Desktop\private",
            r"C:\Users\alex\AppData\Roaming\SomeApp\profile",
            r"C:\Users\alex\OneDrive\Photos",
            r"D:\Photos",
            r"D:\Photos\holiday.jpg",
            r"D:\Windows.old.notes\readme.txt",
            r"E:\Program Files backup\x",
            r"\\server\share\folder",
            r"C:\Windowsill\photo.png",
        ] {
            let got = refusal(&sp, p, &[]);
            assert!(got.is_none(), "should allow {p}: {got:?}");
        }
    }

    #[test]
    fn protected_items_block_themselves_children_and_ancestors() {
        let sp = special();
        let items = [item(r"D:\Photos\private", Some(r"D:\Photos\private.lnk"))];
        assert!(refusal(&sp, r"D:\Photos\private", &items)
            .unwrap()
            .contains("already protected"));
        assert!(refusal(&sp, r"d:\photos\PRIVATE\2019\a.jpg", &items)
            .unwrap()
            .contains("inside a folder"));
        assert!(refusal(&sp, r"D:\Photos", &items)
            .unwrap()
            .contains("contains"));
        assert!(refusal(&sp, r"D:\Photos\private.lnk", &items).is_some());
        assert!(refusal(&sp, r"D:\Photos\private2", &items).is_none());
        assert!(refusal(&sp, r"D:\Music", &items).is_none());
    }

    #[test]
    fn sandbox_root_lifts_only_the_system_rules() {
        let mut sp = special();
        // A sandbox whose temp folder happens to be inside the Windows folder.
        sp.sandbox_root = Some(r"C:\Windows\Temp\veilforge-x".into());
        sp.data_dir = Some(r"C:\Windows\Temp\veilforge-x\data".into());
        sp.holds = vec![r"C:\Windows\Temp\veilforge-x\hold".into()];
        assert!(refusal(&sp, r"C:\Windows\Temp\veilforge-x\work\a.txt", &[]).is_none());
        assert!(refusal(&sp, r"C:\Windows\Temp\veilforge-x\data", &[]).is_some());
        assert!(refusal(&sp, r"C:\Windows\Temp\veilforge-x\hold\abc", &[]).is_some());
        assert!(refusal(&sp, r"C:\Windows\Temp\veilforge-x", &[]).is_some());
        assert!(refusal(&sp, r"C:\Windows\Temp\other", &[]).is_some());
        assert!(refusal(&sp, r"C:\Windows", &[]).is_some());
        let items = [item(r"C:\Windows\Temp\veilforge-x\work\a.txt", None)];
        assert!(refusal(&sp, r"C:\Windows\Temp\veilforge-x\work", &items).is_some());
    }

    #[test]
    fn cloud_and_network_are_recognised() {
        let sp = special();
        assert!(in_cloud_folder(&sp, r"C:\Users\alex\OneDrive\Photos\a.jpg"));
        assert!(in_cloud_folder(&sp, r"c:\users\alex\onedrive"));
        assert!(!in_cloud_folder(&sp, r"C:\Users\alex\OneDriveBackup\a.jpg"));
        assert!(is_unc(r"\\nas\media\film.mkv"));
        assert!(is_unc(r"\\?\UNC\nas\media\film.mkv"));
        assert!(!is_unc(r"D:\media\film.mkv"));
        assert!(!is_unc(r"\\?\D:\media\film.mkv"));
    }
}

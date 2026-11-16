//! The shared data model: what the map remembers about every protected item.
//!
//! This file is the contract between the protection engine and the window.
//! Fields may be added (with `#[serde(default)]`), never renamed or removed.

use serde::{Deserialize, Serialize};

/// How an item is protected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mode {
    /// Stays where it is; a deny ACL makes it unreadable, unlistable, undeletable.
    Lock,
    /// Moved (same-volume rename) into the volume's holding folder under a random name.
    Hide,
    /// Protected as `Lock` or `Hide` (see [`Item::ask_hidden`]) and a stand-in shortcut
    /// with the original name is left behind; opening it asks for the password.
    Ask,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Lock => "Locked",
            Mode::Hide => "Hidden",
            Mode::Ask => "Ask on open",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    File,
    Folder,
}

/// What the engine finds when it looks at an item right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum State {
    /// Protected, and everything is where the map says.
    Locked,
    /// Temporarily unlocked: back at its original place until it is locked again.
    Open,
    /// The protected item is not where the map says (holding folder or original place).
    Missing,
    /// The item is there but its protection was changed from outside
    /// (ACL edited, stand-in removed, holding folder attributes changed).
    Tampered,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Locked => "Locked",
            State::Open => "Temporarily open",
            State::Missing => "Missing",
            State::Tampered => "Tampered",
        }
    }
}

/// Everything needed to put an item back exactly as it was.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    /// Owner + group + DACL of the item itself, as an SDDL string.
    pub sddl: String,
    /// `FILE_ATTRIBUTE_*` bits.
    pub attributes: u32,
    /// FILETIME values (100 ns ticks since 1601).
    pub created: u64,
    pub accessed: u64,
    pub modified: u64,
}

/// One renamed child inside a hidden folder ("also scramble names inside").
/// `dir` is the path of the containing directory relative to the item root, written
/// with the *scrambled* names of its ancestors (so the table can be undone bottom-up).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildRename {
    pub dir: String,
    pub original: String,
    pub scrambled: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// 16 lowercase hex characters, random. Used by `--open <id>`.
    pub id: String,
    /// Original file or folder name (last path component).
    pub name: String,
    /// Original full path, without a `\\?\` prefix.
    pub original: String,
    pub kind: Kind,
    pub mode: Mode,
    /// For `Mode::Ask`: true = hidden underneath (default), false = locked in place.
    #[serde(default)]
    pub ask_hidden: bool,
    /// Children were renamed to random names (hidden folders only).
    #[serde(default)]
    pub scrambled: bool,
    #[serde(default)]
    pub group: String,
    /// Total bytes and number of files (1 for a file) when it was protected.
    pub size: u64,
    pub files: u64,
    /// Unix seconds.
    pub locked_at: i64,
    /// Unix seconds when it was temporarily opened; `None` while protected.
    #[serde(default)]
    pub open_since: Option<i64>,
    /// Volume root the item lives on, for example `D:\`.
    pub volume: String,
    /// Full path of the item inside the holding folder while hidden.
    #[serde(default)]
    pub hold_path: Option<String>,
    /// Full path of the stand-in shortcut (`Mode::Ask`).
    #[serde(default)]
    pub standin: Option<String>,
    /// True when the volume has no ACLs (FAT/exFAT): hide-only, reduced protection.
    #[serde(default)]
    pub reduced: bool,
    pub saved: Saved,
    #[serde(default)]
    pub children: Vec<ChildRename>,
    /// Engine bookkeeping (recovery copy, saved state of children). The window
    /// never needs to look inside; build it with `Extra::default()`.
    #[serde(default)]
    pub extra: Extra,
}

/// Timestamps of one sub-folder (renaming its children touches them), restored last.
/// `path` is relative to the item root, written with the original names.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirTimes {
    pub path: String,
    pub created: u64,
    pub accessed: u64,
    pub modified: u64,
}

/// The saved DACL (SDDL) of one child of a locked folder that differs from the
/// most common one. `path` is relative to the item root.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildAcl {
    pub path: String,
    pub sddl: String,
}

/// The attribute bits of one file inside a folder item that Windows would not
/// leave alone (renaming a file or changing its permissions switches its
/// "archive" bit on). `path` is relative to the item root, original names.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildAttributes {
    pub path: String,
    pub attributes: u32,
}

/// What the engine remembers about an item beyond the fields the window shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extra {
    /// Full path of the item's encrypted recovery copy in the holding folder.
    #[serde(default)]
    pub recovery: Option<String>,
    /// Locked folders: the DACL most files / most sub-folders inside had.
    #[serde(default)]
    pub acl_file: String,
    #[serde(default)]
    pub acl_dir: String,
    /// Locked folders: children whose DACL differs from the two above.
    #[serde(default)]
    pub child_acls: Vec<ChildAcl>,
    /// Timestamps of every sub-folder of a folder item.
    #[serde(default)]
    pub dir_times: Vec<DirTimes>,
    /// Files inside whose attribute bits have to be put back afterwards.
    #[serde(default)]
    pub child_attributes: Vec<ChildAttributes>,
    /// Children that were left untouched because they are links (reparse points).
    #[serde(default)]
    pub skipped_links: u64,
}

impl Item {
    /// True when the item is moved into the holding folder while protected.
    pub fn hides(&self) -> bool {
        matches!(self.mode, Mode::Hide) || (matches!(self.mode, Mode::Ask) && self.ask_hidden)
    }
}

/// The decrypted content of the map file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Map {
    pub version: u32,
    pub created: i64,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub items: Vec<Item>,
}

/// Argon2id cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl KdfParams {
    /// 256 MiB, 3 passes, 4 lanes (the same default as KeepForge).
    pub const DEFAULT: KdfParams = KdfParams {
        m_kib: 256 * 1024,
        t: 3,
        p: 4,
    };
    /// Cheap parameters for unit tests and the self-test only.
    pub const TEST: KdfParams = KdfParams {
        m_kib: 1024,
        t: 1,
        p: 1,
    };
}

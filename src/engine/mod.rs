//! The protection engine: the map, the holding folders, ACLs and the journal.
//!
//! Nothing here encrypts, copies or rewrites user files. An item is protected
//! with a deny ACL (`acl`), a same-volume rename into the volume's holding
//! folder (`hold`), optionally a reversible renaming of its children (`scan`),
//! and a stand-in shortcut. The only sensitive data is the map of all items,
//! sealed with the master password (`crypto`, `store`); every item also has a
//! sealed recovery copy of its own map entry in its holding folder.
//!
//! Every operation is journaled (sealed records in the data folder) and built
//! from steps that can each be repeated or undone, so that after a crash the
//! item ends up either fully protected and in the map, or exactly as it was.

mod acl;
pub mod cleanup;
mod crypto;
pub mod hold;
pub mod safety;
mod scan;
mod store;
pub mod volume;
mod win;

#[cfg(test)]
mod tests;

use crate::model::{Extra, Item, KdfParams, Kind, Map, Mode, Saved, State};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The master password does not open the map.
    WrongPassword,
    /// No map exists yet in the data folder.
    NoVault,
    /// The map file (and its backups) cannot be read.
    Corrupt(String),
    /// A safety rule refuses this path. The text is ready to show.
    Refused(String),
    /// A file is open in another program.
    InUse(String),
    /// The operation needs administrator rights for this item.
    NeedsElevation(String),
    /// Would need a copy across volumes, which VeilForge never does silently.
    CrossVolume(String),
    /// The file system cannot do this (for example ACLs on FAT/exFAT).
    Unsupported(String),
    /// No item with that id, or the path does not exist.
    NotFound(String),
    /// Another VeilForge process holds the map lock for too long.
    Busy(String),
    Io(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::WrongPassword => write!(f, "The master password is not correct."),
            Error::NoVault => write!(f, "No master password has been set yet."),
            Error::Corrupt(s)
            | Error::Refused(s)
            | Error::InUse(s)
            | Error::NeedsElevation(s)
            | Error::CrossVolume(s)
            | Error::Unsupported(s)
            | Error::NotFound(s)
            | Error::Busy(s)
            | Error::Io(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Where the engine keeps its things. Tests and `--selftest` use [`Env::sandbox`]
/// so that nothing outside one temp folder is touched.
#[derive(Clone, Debug)]
pub struct Env {
    /// Folder of the encrypted map, its backups, the journal and the lock file.
    pub data_dir: PathBuf,
    /// When set, holding folders are created under this folder instead of
    /// `<volume>\VeilForge.sys` (tests only).
    pub hold_root: Option<PathBuf>,
    /// Argon2id cost for newly written maps.
    pub kdf: KdfParams,
    /// The program a stand-in shortcut points to (normally the running EXE).
    pub exe: PathBuf,
    /// Test hook: abort the running operation (as if the process died) right
    /// after the journal step with this name has been written and performed.
    pub crash_after: Option<String>,
    /// Sandboxes skip the "is this a system/profile folder" allowance for the
    /// temp folder only; the refusal rules themselves are never disabled.
    pub sandbox: bool,
}

impl Env {
    /// The real environment: `%APPDATA%\VeilForge` (or next to the EXE when
    /// `portable.flag` exists) and per-volume holding folders.
    ///
    /// Two environment variables redirect it, for end-to-end tests of the
    /// built program: `VEILFORGE_DATA` (the data folder, see
    /// [`crate::settings::data_dir`]) and `VEILFORGE_HOLD` (one holding folder
    /// instead of `<volume>\VeilForge.sys`). Neither weakens anything: the
    /// master password and every refusal rule apply as usual.
    pub fn real() -> Env {
        let exe = std::env::current_exe().unwrap_or_default();
        Env {
            data_dir: crate::settings::data_dir(),
            hold_root: std::env::var_os("VEILFORGE_HOLD")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            kdf: KdfParams::DEFAULT,
            exe,
            crash_after: None,
            sandbox: false,
        }
    }

    /// Everything under `dir` (which the caller created inside the temp folder).
    pub fn sandbox(dir: &Path) -> Env {
        Env {
            data_dir: dir.join("data"),
            hold_root: Some(dir.join("hold")),
            kdf: KdfParams::TEST,
            exe: std::env::current_exe().unwrap_or_default(),
            crash_after: None,
            sandbox: true,
        }
    }
}

/// Options chosen in the "Protect" dialog.
#[derive(Clone, Debug)]
pub struct ProtectOptions {
    pub mode: Mode,
    /// `Mode::Ask` only: hide underneath (true, default) or lock in place.
    pub ask_hidden: bool,
    /// Hidden folders only: also rename every child to a random name.
    pub scramble: bool,
    pub group: String,
}

impl Default for ProtectOptions {
    fn default() -> Self {
        ProtectOptions {
            mode: Mode::Hide,
            ask_hidden: true,
            scramble: true,
            group: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    /// Allowed, but the user should read `Precheck::warnings` first.
    Warn,
    /// Not allowed; `Precheck::refusal` says why.
    Refuse,
}

/// What the engine knows about a path before protecting it.
#[derive(Clone, Debug)]
pub struct Precheck {
    pub path: PathBuf,
    pub verdict: Verdict,
    pub refusal: Option<String>,
    /// Plain sentences: network share, cloud-synced folder, very large tree,
    /// FAT/exFAT reduced protection, already protected, ...
    pub warnings: Vec<String>,
    pub kind: Kind,
    pub size: u64,
    pub files: u64,
    /// File system name of the volume ("NTFS", "exFAT", ...).
    pub filesystem: String,
    /// False on volumes without ACLs: only `Mode::Hide` is offered.
    pub acl_supported: bool,
    /// Whether "also scramble names inside" should start ticked (folders under
    /// [`SCRAMBLE_DEFAULT_MAX_FILES`] entries).
    pub scramble_default: bool,
    /// The scan stopped counting at [`SCAN_LIMIT`] entries.
    pub truncated: bool,
}

/// Folders with at most this many entries get scrambling by default.
pub const SCRAMBLE_DEFAULT_MAX_FILES: u64 = 20_000;
/// Trees with more entries than this get a "very large" warning.
pub const LARGE_TREE_FILES: u64 = 100_000;
/// The precheck stops counting here.
pub const SCAN_LIMIT: u64 = 2_000_000;

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    pub what: String,
}

/// One line of the health check.
#[derive(Clone, Debug)]
pub struct Health {
    pub id: String,
    pub name: String,
    pub state: State,
    /// What is wrong, in plain words; empty when fine.
    pub problem: String,
    /// Whether [`Vault::repair`] can fix it without the user's help.
    pub repairable: bool,
}
/// An unlocked map. Every mutating call takes the cross-process lock, reloads
/// the map if another VeilForge process changed it, journals each step and
/// saves atomically.
pub struct Vault {
    env: Env,
    items: Vec<Item>,
    groups: Vec<String>,
    created: i64,
    keys: crypto::Keys,
    /// Nonce of the map file this handle last read or wrote (changes on every save).
    stamp: [u8; 24],
    /// Set by the `crash_after` test hook: this handle then behaves like a dead process.
    crashed: AtomicBool,
    from_backup: bool,
    notes: Vec<String>,
}

/// Every journal step name, as accepted by [`Env::crash_after`]. The part
/// before the dot is the operation (`protect`, `open` = open_temporarily,
/// `relock`, `release`). Not every step occurs for every mode; a step named
/// `...-part` stops in the middle of a batch of renames.
pub const JOURNAL_STEPS: &[&str] = &[
    "protect.begin",
    "protect.hold-folder",
    "protect.move",
    "protect.scramble-part",
    "protect.scramble",
    "protect.attributes",
    "protect.acl-children",
    "protect.acl",
    "protect.recovery",
    "protect.standin",
    "protect.save",
    "open.begin",
    "open.standin",
    "open.unscramble-part",
    "open.unscramble",
    "open.move",
    "open.acl",
    "open.acl-children",
    "open.attributes",
    "open.times",
    "open.recovery",
    "open.save",
    "relock.begin",
    "relock.hold-folder",
    "relock.move",
    "relock.scramble-part",
    "relock.scramble",
    "relock.attributes",
    "relock.acl-children",
    "relock.acl",
    "relock.recovery",
    "relock.standin",
    "relock.save",
    "release.begin",
    "release.standin",
    "release.unscramble-part",
    "release.unscramble",
    "release.move",
    "release.acl",
    "release.acl-children",
    "release.attributes",
    "release.times",
    "release.recovery",
    "release.save",
];

/// One sealed journal record.
#[derive(Serialize, Deserialize)]
enum Record {
    /// Written before anything is touched: the operation and the complete item
    /// it works on (for `protect` / `relock`: the item as it will be).
    Begin { op: String, plan: Box<Item> },
    /// Written before each physical step.
    Step(String),
}

/// Whether steps are journaled (a normal operation) or not (recovery).
struct Ctx {
    prefix: Option<&'static str>,
}

const QUIET: Ctx = Ctx { prefix: None };

macro_rules! step {
    ($self:ident, $ctx:expr, $name:expr, $body:expr) => {{
        $self.step_begin($ctx, $name)?;
        $body?;
        $self.step_end($ctx, $name)?;
    }};
}

fn io_text(what: &str, e: std::io::Error) -> Error {
    Error::Io(format!("{what}: {e}"))
}

fn say(progress: &mut dyn FnMut(Progress), done: u64, total: u64, what: &str) {
    progress(Progress {
        done,
        total,
        what: what.to_string(),
    });
}

fn map_path(env: &Env) -> PathBuf {
    env.data_dir.join(store::MAP_FILE)
}

fn stamp_of(file: &[u8]) -> [u8; 24] {
    let mut s = [0u8; 24];
    if file.len() >= 80 {
        s.copy_from_slice(&file[56..80]);
    }
    s
}

fn move_error(it: &Item, e: std::io::Error) -> Error {
    let name = &it.name;
    match e.raw_os_error() {
        Some(win::ERROR_SHARING_VIOLATION) | Some(win::ERROR_LOCK_VIOLATION) => Error::InUse(
            format!("\"{name}\" is open in another program. Close it and try again."),
        ),
        Some(win::ERROR_ACCESS_DENIED) if it.kind == Kind::Folder => Error::InUse(format!(
            "Windows does not let \"{name}\" be moved right now. A file inside is probably open in another program. Close it and try again."
        )),
        Some(win::ERROR_ACCESS_DENIED) => Error::NeedsElevation(format!(
            "Windows does not let this account move \"{name}\". Administrator rights are needed for this item."
        )),
        Some(win::ERROR_NOT_SAME_DEVICE) => Error::CrossVolume(format!(
            "\"{name}\" would have to be copied to another drive. VeilForge never does that."
        )),
        Some(80) | Some(183) => Error::Refused(format!(
            "Something else already has the name \"{name}\" there. Move or rename it, then try again."
        )),
        _ => Error::Io(format!("\"{name}\" could not be moved: {e}")),
    }
}

impl Vault {
    /// Whether a map file exists in `env.data_dir`.
    pub fn exists(env: &Env) -> bool {
        let path = map_path(env);
        path.exists() || (1..=store::BACKUPS).any(|n| store::backup_path(&path, n).exists())
    }

    /// First run: creates an empty map sealed with `password`.
    pub fn create(env: Env, password: &str) -> Result<Vault> {
        let keys = crypto::Keys::fresh(password, &env.kdf).map_err(Error::Io)?;
        Vault::create_with(env, keys)
    }

    fn create_with(env: Env, keys: crypto::Keys) -> Result<Vault> {
        let _lock =
            store::MapLock::acquire(&env.data_dir, store::LOCK_WAIT).map_err(Error::Busy)?;
        if Vault::exists(&env) {
            return Err(Error::Refused(
                "A master password has already been set here.".into(),
            ));
        }
        let _ = store::journal_clear(&env.data_dir);
        let mut vault = Vault {
            env,
            items: Vec::new(),
            groups: Vec::new(),
            created: crate::util::now(),
            keys,
            stamp: [0u8; 24],
            crashed: AtomicBool::new(false),
            from_backup: false,
            notes: Vec::new(),
        };
        vault.save(Vec::new())?;
        Ok(vault)
    }

    /// Opens the map (falling back to the rolling backups if the newest file
    /// is damaged) and finishes any interrupted operation found in the journal.
    pub fn open(env: Env, password: &str) -> Result<Vault> {
        if !Vault::exists(&env) {
            return Err(Error::NoVault);
        }
        let _lock =
            store::MapLock::acquire(&env.data_dir, store::LOCK_WAIT).map_err(Error::Busy)?;
        let path = map_path(&env);
        let mut candidates = vec![path.clone()];
        candidates.extend((1..=store::BACKUPS).map(|n| store::backup_path(&path, n)));
        let mut derived: Vec<crypto::Keys> = Vec::new();
        let mut wrong_password = false;
        let mut found: Option<(usize, usize, Map, [u8; 24])> = None;
        for (n, candidate) in candidates.iter().enumerate() {
            let Ok(bytes) = std::fs::read(candidate) else {
                continue;
            };
            let Some(header) = crypto::Header::parse(crypto::MAGIC_MAP, &bytes) else {
                continue;
            };
            let at = match derived
                .iter()
                .position(|k| k.salt == header.salt && k.kdf == header.kdf)
            {
                Some(at) => at,
                None => {
                    derived.push(
                        crypto::Keys::derive(password, &header.kdf, &header.salt)
                            .map_err(Error::Io)?,
                    );
                    derived.len() - 1
                }
            };
            match derived[at].open(crypto::MAGIC_MAP, &bytes) {
                Ok(plain) => {
                    if let Ok(map) = serde_json::from_slice::<Map>(&plain) {
                        found = Some((n, at, map, stamp_of(&bytes)));
                        break;
                    }
                }
                Err(crypto::OpenError::WrongKey) => wrong_password = true,
                Err(_) => {}
            }
        }
        let Some((n, at, map, stamp)) = found else {
            return Err(if wrong_password {
                Error::WrongPassword
            } else {
                Error::Corrupt(
                    "The map of protected items is damaged and no backup of it can be read. Your files are untouched; use \"Rebuild from recovery copies\"."
                        .into(),
                )
            });
        };
        let mut vault = Vault {
            env,
            items: map.items,
            groups: map.groups,
            created: map.created,
            keys: derived.swap_remove(at),
            stamp,
            crashed: AtomicBool::new(false),
            from_backup: n > 0,
            notes: Vec::new(),
        };
        if vault.from_backup {
            // Keep the damaged file for inspection and make the backup current.
            let mut aside = path.as_os_str().to_os_string();
            aside.push(".damaged");
            let _ = std::fs::remove_file(&aside);
            let _ = std::fs::rename(&path, &aside);
            vault.save(vault.items.clone())?;
            vault
                .notes
                .push("The newest map file was damaged; a backup was used instead.".to_string());
        }
        let mut notes = vault.recover_locked();
        vault.notes.append(&mut notes);
        if vault.from_backup && !store::journal_exists(&vault.env.data_dir) {
            // Items protected after that backup was written still have their recovery copies.
            if let Ok(n) = vault.import_locked(&[]) {
                if n > 0 {
                    vault.notes.push(format!(
                        "{n} item(s) missing from the backup were found again through their recovery copies."
                    ));
                }
            }
        }
        Ok(vault)
    }

    pub fn env(&self) -> &Env {
        &self.env
    }

    /// True when [`Vault::open`] had to fall back to a backup of the map.
    pub fn opened_from_backup(&self) -> bool {
        self.from_backup
    }

    /// What [`Vault::open`] had to do (journal recovery, backup fallback), one line each.
    pub fn open_notes(&self) -> &[String] {
        &self.notes
    }

    /// Constant-time check of the master password against the open map's key.
    pub fn verify_password(&self, password: &str) -> bool {
        match crypto::Keys::derive(password, &self.keys.kdf, &self.keys.salt) {
            Ok(k) => k.same(&self.keys),
            Err(_) => false,
        }
    }

    /// Re-seals the map (and every recovery copy) under a new password.
    pub fn change_password(&mut self, current: &str, new: &str) -> Result<()> {
        let _lock = self.enter()?;
        if !self.verify_password(current) {
            return Err(Error::WrongPassword);
        }
        let keys = crypto::Keys::fresh(new, &self.env.kdf).map_err(Error::Io)?;
        self.keys = keys;
        self.save(self.items.clone())?;
        // The old backups open with the old password: replace them.
        let path = map_path(&self.env);
        for n in 1..=store::BACKUPS {
            let _ = std::fs::remove_file(store::backup_path(&path, n));
        }
        let _ = std::fs::copy(&path, store::backup_path(&path, 1));
        let mut first_error = None;
        for it in &self.items {
            if let Err(e) = self.write_recovery(it) {
                first_error.get_or_insert(e);
            }
        }
        match first_error {
            None => Ok(()),
            Some(e) => Err(Error::Io(format!(
                "The password was changed, but a recovery copy could not be rewritten ({e}). Run the health check and repair."
            ))),
        }
    }

    /// Reloads the map if another process changed it. Returns true if it did.
    pub fn refresh(&mut self) -> Result<bool> {
        let path = map_path(&self.env);
        let mut head = [0u8; crypto::HEADER_LEN];
        {
            use std::io::Read;
            let mut f = std::fs::File::open(&path)
                .map_err(|e| io_text("The map file cannot be read", e))?;
            if f.read_exact(&mut head).is_err() {
                return Err(Error::Corrupt("The map file is damaged.".into()));
            }
        }
        if stamp_of(&head) == self.stamp {
            return Ok(false);
        }
        let bytes = std::fs::read(&path).map_err(|e| io_text("The map file cannot be read", e))?;
        let plain = self
            .keys
            .open(crypto::MAGIC_MAP, &bytes)
            .map_err(|e| match e {
                crypto::OpenError::WrongKey => Error::WrongPassword,
                _ => Error::Corrupt("The map file is damaged.".into()),
            })?;
        let map: Map = serde_json::from_slice(&plain)
            .map_err(|_| Error::Corrupt("The map file is damaged.".into()))?;
        self.items = map.items;
        self.groups = map.groups;
        self.created = map.created;
        self.stamp = stamp_of(&bytes);
        Ok(true)
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Group names in use, sorted.
    pub fn groups(&self) -> Vec<String> {
        let mut all: Vec<String> = self
            .groups
            .iter()
            .cloned()
            .chain(self.items.iter().map(|i| i.group.clone()))
            .filter(|g| !g.is_empty())
            .collect();
        all.sort_by_key(|g| g.to_lowercase());
        all.dedup();
        all
    }

    pub fn set_group(&mut self, id: &str, group: &str) -> Result<()> {
        let _lock = self.enter()?;
        let mut items = self.items.clone();
        let it = items
            .iter_mut()
            .find(|i| i.id == id)
            .ok_or_else(|| Error::NotFound("This item is no longer in the list.".into()))?;
        it.group = group.to_string();
        let changed = it.clone();
        if !group.is_empty() && !self.groups.iter().any(|g| g == group) {
            self.groups.push(group.to_string());
        }
        self.save(items)?;
        let _ = self.write_recovery(&changed);
        Ok(())
    }

    /// Looks at the disk (cheap: a few metadata calls per item).
    pub fn state(&self, id: &str) -> State {
        match self.item(id) {
            Some(it) => self.inspect(it, false).0,
            None => State::Missing,
        }
    }

    /// Applies the refusal rules and gathers size, file system and warnings.
    /// Never changes anything.
    pub fn precheck(&self, path: &Path) -> Precheck {
        safety::precheck(&self.env, path, &self.items)
    }

    /// Protects a file or folder. Returns the new item's id.
    pub fn protect(
        &mut self,
        path: &Path,
        options: &ProtectOptions,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<String> {
        let _lock = self.enter()?;
        say(progress, 0, 0, "Looking at the item");
        let plan = self.plan(path, options, None)?;
        let id = plan.id.clone();
        let r = self.run_apply("protect", plan, progress);
        self.finish(r).map(|()| id)
    }

    /// Temporarily unlocks: the item is back at its original path with its
    /// original ACL until [`Vault::relock`]. Returns the original path.
    pub fn open_temporarily(
        &mut self,
        id: &str,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<PathBuf> {
        let _lock = self.enter()?;
        let it = self.find(id)?;
        let original = PathBuf::from(&it.original);
        if it.open_since.is_some() {
            return Ok(original);
        }
        let r = self.run_open(&it, progress);
        self.finish(r).map(|()| original)
    }

    /// Protects a temporarily open item again, in its recorded mode.
    pub fn relock(&mut self, id: &str, progress: &mut dyn FnMut(Progress)) -> Result<()> {
        let _lock = self.enter()?;
        let it = self.find(id)?;
        if it.open_since.is_none() {
            return Ok(());
        }
        let options = ProtectOptions {
            mode: it.mode,
            ask_hidden: it.ask_hidden,
            scramble: it.scrambled,
            group: it.group.clone(),
        };
        say(progress, 0, 0, "Looking at the item");
        let plan = self.plan(Path::new(&it.original), &options, Some(&it))?;
        let r = self.run_apply("relock", plan, progress);
        self.finish(r)
    }

    /// Unlocks permanently: restores location, name, ACL, owner, attributes and
    /// timestamps, removes the stand-in and the recovery copy, and forgets the item.
    pub fn release(&mut self, id: &str, progress: &mut dyn FnMut(Progress)) -> Result<PathBuf> {
        let _lock = self.enter()?;
        let it = self.find(id)?;
        let original = PathBuf::from(&it.original);
        let r = self.run_release(&it, progress);
        self.finish(r).map(|()| original)
    }

    /// Locks every temporarily open item again.
    pub fn relock_all(&mut self, progress: &mut dyn FnMut(Progress)) -> Vec<(String, Result<()>)> {
        let ids: Vec<String> = self
            .items
            .iter()
            .filter(|i| i.open_since.is_some())
            .map(|i| i.id.clone())
            .collect();
        ids.into_iter()
            .map(|id| {
                let r = self.relock(&id, progress);
                (id, r)
            })
            .collect()
    }

    /// Panic restore: releases every item. Items that fail stay in the map.
    pub fn release_all(
        &mut self,
        progress: &mut dyn FnMut(Progress),
    ) -> Vec<(String, Result<PathBuf>)> {
        let ids: Vec<String> = self.items.iter().map(|i| i.id.clone()).collect();
        ids.into_iter()
            .map(|id| {
                let r = self.release(&id, progress);
                (id, r)
            })
            .collect()
    }

    /// Verifies every item against the disk.
    pub fn health(&self) -> Vec<Health> {
        self.items
            .iter()
            .map(|it| {
                let (state, problem, repairable) = self.inspect(it, true);
                Health {
                    id: it.id.clone(),
                    name: it.name.clone(),
                    state,
                    problem,
                    repairable,
                }
            })
            .collect()
    }

    /// Re-applies the recorded protection (or re-creates the stand-in, the
    /// holding folder attributes, the recovery copy). Returns what was done.
    pub fn repair(&mut self, id: &str) -> Result<String> {
        let _lock = self.enter()?;
        let it = self.find(id)?;
        let (state, problem, _) = self.inspect(&it, true);
        match state {
            State::Missing => Err(Error::NotFound(problem)),
            State::Open => {
                self.write_recovery(&it)?;
                Ok("The item is temporarily open; its recovery copy was written again.".into())
            }
            State::Locked if problem.is_empty() => Ok("Nothing needed repairing.".into()),
            _ => {
                self.apply(&it, &QUIET, false, &mut |_| {})?;
                let (after, still, _) = self.inspect(&it, true);
                if after == State::Locked && still.is_empty() {
                    Ok(format!(
                        "Repaired: {problem} The protection was applied again."
                    ))
                } else {
                    Err(Error::Io(format!("The repair did not hold: {still}")))
                }
            }
        }
    }

    /// Finishes or rolls back operations left in the journal by a crash.
    /// Returns one line per recovered operation. Called by [`Vault::open`] too.
    pub fn recover(&mut self) -> Vec<String> {
        if self.crashed.load(Ordering::SeqCst) {
            return Vec::new();
        }
        let Ok(_lock) = store::MapLock::acquire(&self.env.data_dir, store::LOCK_WAIT) else {
            return Vec::new();
        };
        if self.refresh().is_err() {
            return Vec::new();
        }
        self.recover_locked()
    }

    /// Scans the holding folders for recovery copies of items that are not in
    /// the map (lost app-data folder) and adds the ones this password opens.
    pub fn import_recovery(&mut self) -> Result<usize> {
        let _lock = self.enter()?;
        self.import_locked(&[])
    }

    /// Removes holding folders that are empty. Returns how many were removed.
    pub fn remove_empty_holds(&mut self) -> usize {
        let Ok(_lock) = self.enter() else {
            return 0;
        };
        hold::known(&self.env)
            .iter()
            .filter(|dir| hold::remove_if_empty(dir))
            .count()
    }

    // ----------------------------------------------------------------------------------
    // Map file
    // ----------------------------------------------------------------------------------

    fn save(&mut self, items: Vec<Item>) -> Result<()> {
        let map = Map {
            version: 1,
            created: self.created,
            groups: self.groups.clone(),
            items,
        };
        let plain = zeroize::Zeroizing::new(
            serde_json::to_vec(&map).map_err(|e| Error::Io(e.to_string()))?,
        );
        let sealed = self.keys.seal(crypto::MAGIC_MAP, &plain);
        let path = map_path(&self.env);
        std::fs::create_dir_all(&self.env.data_dir)
            .map_err(|e| io_text("The data folder cannot be created", e))?;
        store::rotate_backups(&path, store::BACKUPS)
            .map_err(|e| io_text("The map backup could not be written", e))?;
        store::write_atomic(&path, &sealed)
            .map_err(|e| io_text("The map could not be saved", e))?;
        self.stamp = stamp_of(&sealed);
        self.items = map.items;
        Ok(())
    }

    /// Start of every mutating call: lock, reload, finish interrupted work.
    fn enter(&mut self) -> Result<store::MapLock> {
        if self.crashed.load(Ordering::SeqCst) {
            return Err(Error::Io(
                "This handle stopped at a simulated crash; open the map again.".into(),
            ));
        }
        let lock =
            store::MapLock::acquire(&self.env.data_dir, store::LOCK_WAIT).map_err(Error::Busy)?;
        self.refresh()?;
        let notes = self.recover_locked();
        if store::journal_exists(&self.env.data_dir) {
            return Err(Error::Io(format!(
                "An interrupted operation has to be finished first, and that did not work yet. {}",
                notes.last().cloned().unwrap_or_default()
            )));
        }
        Ok(lock)
    }

    /// End of a journaled operation: forget the journal, or roll back.
    fn finish(&mut self, r: Result<()>) -> Result<()> {
        if self.crashed.load(Ordering::SeqCst) {
            return r;
        }
        match r {
            Ok(()) => store::journal_clear(&self.env.data_dir)
                .map_err(|e| io_text("The journal could not be cleared", e)),
            Err(e) => {
                let _ = self.recover_journal(true);
                Err(e)
            }
        }
    }

    fn find(&self, id: &str) -> Result<Item> {
        self.item(id)
            .cloned()
            .ok_or_else(|| Error::NotFound("This item is no longer in the list.".into()))
    }

    // ----------------------------------------------------------------------------------
    // Journal
    // ----------------------------------------------------------------------------------

    fn journal_put(&self, record: &Record) -> Result<()> {
        let plain = zeroize::Zeroizing::new(
            serde_json::to_vec(record).map_err(|e| Error::Io(e.to_string()))?,
        );
        store::journal_append(&self.env.data_dir, &self.keys.seal_record(&plain))
            .map_err(|e| io_text("The journal could not be written", e))
    }

    fn journal_begin(&self, op: &str, plan: &Item) -> Result<()> {
        store::journal_clear(&self.env.data_dir)
            .map_err(|e| io_text("The journal could not be cleared", e))?;
        self.journal_put(&Record::Begin {
            op: op.to_string(),
            plan: Box::new(plan.clone()),
        })
    }

    fn step_begin(&self, ctx: &Ctx, name: &str) -> Result<()> {
        match ctx.prefix {
            Some(prefix) => self.journal_put(&Record::Step(format!("{prefix}.{name}"))),
            None => Ok(()),
        }
    }

    /// The `crash_after` test hook: stop exactly as a dying process would.
    fn step_end(&self, ctx: &Ctx, name: &str) -> Result<()> {
        if let (Some(prefix), Some(stop)) = (ctx.prefix, self.env.crash_after.as_deref()) {
            if stop.strip_prefix(prefix).and_then(|s| s.strip_prefix('.')) == Some(name) {
                self.crashed.store(true, Ordering::SeqCst);
                return Err(Error::Io(format!("simulated crash after {prefix}.{name}")));
            }
        }
        Ok(())
    }

    /// Looks at the journal and brings the item it names to a consistent state.
    /// The caller holds the lock and has the current map loaded.
    fn recover_locked(&mut self) -> Vec<String> {
        self.recover_journal(false)
    }

    /// `failed_cleanly`: the operation returned an error in this very process
    /// (nothing crashed). A release that could not be completed is then rolled
    /// back, so the item stays protected and in the list, exactly as the error
    /// message tells the user. After a crash a release is finished instead.
    fn recover_journal(&mut self, failed_cleanly: bool) -> Vec<String> {
        let dir = self.env.data_dir.clone();
        if !store::journal_exists(&dir) {
            return Vec::new();
        }
        let begin = store::journal_read(&dir)
            .first()
            .and_then(|r| self.keys.open_record(r))
            .and_then(|plain| serde_json::from_slice::<Record>(&plain).ok());
        let Some(Record::Begin { op, plan }) = begin else {
            // Written under another password or torn before the first record
            // was complete (then nothing had been touched yet).
            let aside = dir.join(format!("{}.unreadable", store::JOURNAL_FILE));
            let _ = std::fs::remove_file(&aside);
            let _ = std::fs::rename(dir.join(store::JOURNAL_FILE), &aside);
            return vec!["A journal that cannot be read was set aside.".to_string()];
        };
        let in_map = self.item(&plan.id).cloned();
        let name = plan.name.clone();
        let mut quiet = |_: Progress| {};
        let (result, line) = match (op.as_str(), in_map) {
            ("protect", None) => (
                self.undo(&plan, &QUIET, None, &mut quiet),
                format!("Protecting \"{name}\" was interrupted. It was put back exactly as it was."),
            ),
            ("protect", Some(m)) => (
                self.apply(&m, &QUIET, false, &mut quiet),
                format!("Protecting \"{name}\" was interrupted at the very end. It is protected."),
            ),
            ("relock", Some(m)) if m.open_since.is_none() => (
                self.apply(&m, &QUIET, false, &mut quiet),
                format!("Locking \"{name}\" again was interrupted at the very end. It is protected."),
            ),
            ("relock", Some(m)) => (
                self.undo(&plan, &QUIET, Some(&m), &mut quiet),
                format!("Locking \"{name}\" again was interrupted. It is still open at its usual place."),
            ),
            ("open", Some(m)) if m.open_since.is_some() => (
                self.undo(&plan, &QUIET, Some(&m), &mut quiet),
                format!("Opening \"{name}\" was interrupted at the very end. It is open."),
            ),
            ("open", Some(m)) => (
                self.apply(&m, &QUIET, false, &mut quiet),
                format!("Opening \"{name}\" was interrupted. It is protected again."),
            ),
            ("release", Some(m)) if failed_cleanly && m.open_since.is_none() => (
                self.apply(&m, &QUIET, false, &mut quiet),
                String::new(),
            ),
            ("release", Some(m)) => {
                let r = if m.open_since.is_none() {
                    self.undo(&m, &QUIET, None, &mut quiet)
                } else {
                    self.delete_recovery(&m)
                };
                let r = r.and_then(|()| {
                    let items = self
                        .items
                        .iter()
                        .filter(|i| i.id != m.id)
                        .cloned()
                        .collect();
                    self.save(items)
                });
                (
                    r,
                    format!("Unlocking \"{name}\" was interrupted. It was finished: the item is back as it was."),
                )
            }
            // The map was already saved without the item: only the journal was left.
            ("release", None) => (
                Ok(()),
                format!("Unlocking \"{name}\" was interrupted at the very end. The item is back as it was."),
            ),
            _ => (Ok(()), String::new()),
        };
        match result {
            Ok(()) => {
                let _ = store::journal_clear(&dir);
                if line.is_empty() {
                    Vec::new()
                } else {
                    vec![line]
                }
            }
            Err(Error::NotFound(why)) => {
                // The item itself is gone; there is nothing left to bring back.
                let _ = store::journal_clear(&dir);
                vec![format!(
                    "An interrupted operation on \"{name}\" could not be finished: {why}"
                )]
            }
            Err(e) => vec![format!(
                "An interrupted operation on \"{name}\" could not be finished yet: {e}"
            )],
        }
    }

    // ----------------------------------------------------------------------------------
    // Operations
    // ----------------------------------------------------------------------------------

    fn run_apply(
        &mut self,
        op: &'static str,
        plan: Item,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<()> {
        let ctx = Ctx { prefix: Some(op) };
        self.journal_begin(op, &plan)?;
        step!(self, &ctx, "begin", Ok::<(), Error>(()));
        self.apply(&plan, &ctx, true, progress)?;
        say(progress, 0, 0, "Saving");
        let mut items = self.items.clone();
        match items.iter_mut().find(|i| i.id == plan.id) {
            Some(slot) => *slot = plan,
            None => items.push(plan),
        }
        step!(self, &ctx, "save", self.save(items));
        Ok(())
    }

    fn run_open(&mut self, it: &Item, progress: &mut dyn FnMut(Progress)) -> Result<()> {
        let ctx = Ctx {
            prefix: Some("open"),
        };
        let mut opened = it.clone();
        opened.open_since = Some(crate::util::now());
        self.journal_begin("open", it)?;
        step!(self, &ctx, "begin", Ok::<(), Error>(()));
        self.undo(it, &ctx, Some(&opened), progress)?;
        say(progress, 0, 0, "Saving");
        let mut items = self.items.clone();
        if let Some(slot) = items.iter_mut().find(|i| i.id == it.id) {
            *slot = opened;
        }
        step!(self, &ctx, "save", self.save(items));
        Ok(())
    }

    fn run_release(&mut self, it: &Item, progress: &mut dyn FnMut(Progress)) -> Result<()> {
        let ctx = Ctx {
            prefix: Some("release"),
        };
        self.journal_begin("release", it)?;
        step!(self, &ctx, "begin", Ok::<(), Error>(()));
        if it.open_since.is_none() {
            self.undo(it, &ctx, None, progress)?;
        } else {
            // Already back as it was (and possibly edited since): only forget it.
            step!(self, &ctx, "recovery", self.delete_recovery(it));
        }
        say(progress, 0, 0, "Saving");
        let items = self
            .items
            .iter()
            .filter(|i| i.id != it.id)
            .cloned()
            .collect();
        step!(self, &ctx, "save", self.save(items));
        Ok(())
    }

    /// Decides everything about a protection before anything is touched:
    /// records the state to restore, picks the random names, plans the renames.
    fn plan(&self, path: &Path, options: &ProtectOptions, existing: Option<&Item>) -> Result<Item> {
        let others: Vec<Item> = self
            .items
            .iter()
            .filter(|i| existing.map(|e| e.id != i.id).unwrap_or(true))
            .cloned()
            .collect();
        // The state to restore is read before anything else looks at the item
        // (even listing a folder moves its last-access time).
        let first_look = win::stat(&safety::canonical(path));
        // Rules only: the one walk over the folder is the scan further down.
        let pre = safety::precheck_with(
            &self.env,
            &safety::Special::real(&self.env),
            path,
            &others,
            0,
        );
        let original = pre.path.clone();
        let name = original
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if pre.verdict == Verdict::Refuse {
            let why = pre.refusal.unwrap_or_default();
            return Err(if win::exists(&original) {
                Error::Refused(why)
            } else {
                Error::NotFound(why)
            });
        }
        let hides = matches!(options.mode, Mode::Hide)
            || (matches!(options.mode, Mode::Ask) && options.ask_hidden);
        let fs = volume::info_for(&original);
        if !fs.acl && !hides {
            return Err(Error::Unsupported(format!(
                "The drive of \"{name}\" ({}) cannot store permissions, so the item cannot be locked in place. It can only be hidden.",
                if fs.name.is_empty() { "unknown file system" } else { &fs.name }
            )));
        }
        let st = first_look.map_err(|e| io_text("The item cannot be read", e))?;
        let sddl = if fs.acl {
            win::get_sddl(&original, true).map_err(|e| acl::acl_error(&original, e))?
        } else {
            String::new()
        };
        if !hides && !win::can_write_dac(&original) {
            return Err(acl::acl_error(
                &original,
                std::io::Error::from_raw_os_error(win::ERROR_ACCESS_DENIED),
            ));
        }
        let kind = pre.kind;
        if kind == Kind::File && win::in_use(&original) {
            return Err(Error::InUse(format!(
                "\"{name}\" is open in another program. Close it and try again."
            )));
        }
        let standin = if matches!(options.mode, Mode::Ask) {
            let s = crate::standin::standin_path(&original);
            if win::exists(&s) {
                return Err(Error::Refused(format!(
                    "A shortcut named \"{name}\" already exists next to the item. Move or rename it, then try again."
                )));
            }
            Some(s.to_string_lossy().into_owned())
        } else {
            None
        };
        let hold_dir = hold::dir_for(&self.env, &original)?;
        let scramble = hides && options.scramble && kind == Kind::Folder;
        let mut extra = Extra::default();
        let (mut size, mut files, mut children) = (st.size, 1u64, Vec::new());
        if kind == Kind::Folder {
            let mut names = hold::random_name;
            let mut found = scan::Scan::default();
            let walked = scan::scan(
                &original,
                &mut scan::ScanOptions {
                    acl: !hides,
                    scramble,
                    probe: !hides,
                    names: &mut names,
                },
                &mut found,
            );
            if let Err(e) = walked {
                // Nothing was changed, but looking into folders moved their
                // last-access times: put those back before giving up.
                for d in found.dir_times.iter().rev() {
                    let _ =
                        win::set_times(&original.join(&d.path), d.created, d.accessed, d.modified);
                }
                let _ = win::set_times(&original, st.created, st.accessed, st.modified);
                return Err(e);
            }
            size = found.size;
            files = found.files;
            children = found.children;
            extra.dir_times = found.dir_times;
            extra.child_attributes = found.attributes;
            extra.skipped_links = found.links;
            scan::fold_acls(found.acls, &mut extra);
        }
        let fresh_name = |dir: &Path| loop {
            let p = dir.join(hold::random_name());
            if !win::exists(&p) {
                break p.to_string_lossy().into_owned();
            }
        };
        extra.recovery = Some(
            existing
                .and_then(|e| e.extra.recovery.clone())
                .filter(|r| Path::new(r).parent() == Some(hold_dir.as_path()))
                .unwrap_or_else(|| fresh_name(&hold_dir)),
        );
        Ok(Item {
            id: existing
                .map(|e| e.id.clone())
                .unwrap_or_else(|| crate::util::hex(&crate::secret::random_array::<8>())),
            name,
            original: original.to_string_lossy().into_owned(),
            kind,
            mode: options.mode,
            ask_hidden: matches!(options.mode, Mode::Ask) && options.ask_hidden,
            scrambled: scramble,
            group: options.group.clone(),
            size,
            files,
            locked_at: existing
                .map(|e| e.locked_at)
                .unwrap_or_else(crate::util::now),
            open_since: None,
            volume: volume::root_of(&original).unwrap_or_default(),
            hold_path: hides.then(|| fresh_name(&hold_dir)),
            standin,
            reduced: !fs.acl,
            saved: Saved {
                sddl,
                attributes: st.attributes,
                created: st.created,
                accessed: st.accessed,
                modified: st.modified,
            },
            children,
            extra,
        })
    }

    fn recovery_path(it: &Item) -> Result<PathBuf> {
        it.extra
            .recovery
            .as_ref()
            .map(PathBuf::from)
            .ok_or_else(|| Error::Io("The item has no recovery copy location.".into()))
    }

    /// Brings the disk to the protected state described by `it`, from the
    /// unprotected state or from anywhere in between (every part checks what
    /// is already done).
    fn apply(
        &self,
        it: &Item,
        ctx: &Ctx,
        fresh: bool,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<()> {
        let orig = PathBuf::from(&it.original);
        let recovery = Self::recovery_path(it)?;
        let hold_dir = recovery
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| Error::Io("The holding folder is not known.".into()))?;
        say(progress, 0, 0, "Preparing the holding folder");
        step!(
            self,
            ctx,
            "hold-folder",
            hold::ensure(&hold_dir, it.reduced)
        );
        if it.hides() {
            let hold =
                it.hold_path.as_ref().map(PathBuf::from).ok_or_else(|| {
                    Error::Io("The item has no place in the holding folder.".into())
                })?;
            say(progress, 0, 0, "Hiding");
            step!(self, ctx, "move", self.move_in(it, &orig, &hold));
            if it.scrambled && !it.children.is_empty() {
                step!(
                    self,
                    ctx,
                    "scramble",
                    scan::scramble(&hold, &it.children, &mut |done, total| {
                        if done % 64 == 0 || done == total {
                            say(progress, done as u64, total as u64, "Renaming the content");
                        }
                        if done == total.div_ceil(2) {
                            self.step_end(ctx, "scramble-part")
                        } else {
                            Ok(())
                        }
                    })
                );
            }
        } else {
            if !fresh {
                // Normalise first: whatever is half locked becomes unlocked.
                self.unlock_tree(it, &orig, &mut || {})?;
            }
            if !win::exists(&orig) {
                return Err(Error::NotFound(format!(
                    "\"{}\" is no longer at its place.",
                    it.name
                )));
            }
            say(progress, 0, 0, "Locking");
            step!(self, ctx, "attributes", {
                let now = win::attributes(&orig).unwrap_or(it.saved.attributes);
                win::set_attributes(&orig, now | win::ATTR_READONLY)
                    .map_err(|e| acl::acl_error(&orig, e))
            });
            if it.kind == Kind::Folder {
                let total = it.files + it.extra.dir_times.len() as u64;
                let mut done = 0u64;
                step!(
                    self,
                    ctx,
                    "acl-children",
                    acl::lock_children(&orig, &mut || {
                        done += 1;
                        if done.is_multiple_of(64) || done == total {
                            say(progress, done, total, "Locking the content");
                        }
                    })
                );
            }
            step!(self, ctx, "acl", acl::set_verified(&orig, acl::lock_sddl()));
        }
        step!(self, ctx, "recovery", self.write_recovery(it));
        if matches!(it.mode, Mode::Ask) {
            step!(self, ctx, "standin", self.make_standin(it));
        }
        Ok(())
    }

    /// Brings the disk back to the state recorded in `it.saved`, from the
    /// protected state or from anywhere in between. `keep` is the item to
    /// leave in the recovery copy (`None` deletes the copy).
    fn undo(
        &self,
        it: &Item,
        ctx: &Ctx,
        keep: Option<&Item>,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<()> {
        let orig = PathBuf::from(&it.original);
        if let Some(s) = &it.standin {
            step!(self, ctx, "standin", self.remove_standin(it, Path::new(s)));
        }
        if it.hides() {
            let hold =
                it.hold_path.as_ref().map(PathBuf::from).ok_or_else(|| {
                    Error::Io("The item has no place in the holding folder.".into())
                })?;
            let (at_hold, at_orig) = (win::exists(&hold), win::exists(&orig));
            if at_hold && at_orig {
                return Err(Error::Refused(format!(
                    "Something else now has the name \"{}\" at the original place. Move or rename it, then try again.",
                    it.name
                )));
            }
            if !at_hold && !at_orig {
                return Err(Error::NotFound(format!(
                    "\"{}\" is neither in the holding folder nor at its original place.",
                    it.name
                )));
            }
            let current = if at_hold { &hold } else { &orig };
            if it.scrambled && !it.children.is_empty() {
                step!(
                    self,
                    ctx,
                    "unscramble",
                    scan::unscramble(current, &it.children, &mut |done, total| {
                        if done % 64 == 0 || done == total {
                            say(
                                progress,
                                done as u64,
                                total as u64,
                                "Restoring the names inside",
                            );
                        }
                        if done == total.div_ceil(2) {
                            self.step_end(ctx, "unscramble-part")
                        } else {
                            Ok(())
                        }
                    })
                );
            }
            say(progress, 0, 0, "Putting it back");
            step!(self, ctx, "move", self.move_out(it, &hold, &orig));
        } else {
            if !win::exists(&orig) {
                return Err(Error::NotFound(format!(
                    "\"{}\" is no longer at its place.",
                    it.name
                )));
            }
            if !it.saved.sddl.is_empty() {
                say(progress, 0, 0, "Unlocking");
                step!(
                    self,
                    ctx,
                    "acl",
                    acl::set_verified(&orig, acl::dacl_part(&it.saved.sddl))
                );
                if it.kind == Kind::Folder {
                    let total = it.files + it.extra.dir_times.len() as u64;
                    let mut done = 0u64;
                    step!(
                        self,
                        ctx,
                        "acl-children",
                        acl::unlock_children(&orig, &it.extra, &mut || {
                            done += 1;
                            if done.is_multiple_of(64) || done == total {
                                say(progress, done, total, "Unlocking the content");
                            }
                        })
                    );
                }
            }
        }
        step!(self, ctx, "attributes", self.restore_attributes(it, &orig));
        step!(self, ctx, "times", self.restore_times(it, &orig));
        step!(
            self,
            ctx,
            "recovery",
            match keep {
                Some(k) => self.write_recovery(k),
                None => self.delete_recovery(it),
            }
        );
        Ok(())
    }

    fn unlock_tree(&self, it: &Item, orig: &Path, tick: &mut dyn FnMut()) -> Result<()> {
        if it.saved.sddl.is_empty() || !win::exists(orig) {
            return Ok(());
        }
        win::set_dacl(orig, acl::dacl_part(&it.saved.sddl)).map_err(|e| acl::acl_error(orig, e))?;
        if it.kind == Kind::Folder {
            acl::unlock_children(orig, &it.extra, tick)?;
        }
        Ok(())
    }

    fn move_in(&self, it: &Item, orig: &Path, hold: &Path) -> Result<()> {
        if win::exists(hold) {
            return Ok(());
        }
        if !win::exists(orig) {
            return Err(Error::NotFound(format!(
                "\"{}\" is no longer at its place.",
                it.name
            )));
        }
        win::rename(orig, hold).map_err(|e| move_error(it, e))?;
        if !win::exists(hold) || win::exists(orig) {
            return Err(Error::Io(format!(
                "Moving \"{}\" did not take effect.",
                it.name
            )));
        }
        Ok(())
    }

    fn move_out(&self, it: &Item, hold: &Path, orig: &Path) -> Result<()> {
        if !win::exists(hold) {
            return Ok(());
        }
        if let Some(parent) = orig.parent() {
            if !win::exists(parent) {
                std::fs::create_dir_all(win::long(parent))
                    .map_err(|e| io_text("The original folder could not be created again", e))?;
            }
        }
        win::rename(hold, orig).map_err(|e| move_error(it, e))?;
        if win::exists(hold) || !win::exists(orig) {
            return Err(Error::Io(format!(
                "Moving \"{}\" back did not take effect.",
                it.name
            )));
        }
        Ok(())
    }

    fn restore_attributes(&self, it: &Item, orig: &Path) -> Result<()> {
        for c in &it.extra.child_attributes {
            let p = orig.join(&c.path);
            if let Ok(now) = win::attributes(&p) {
                if (now ^ c.attributes) & win::ATTR_SETTABLE != 0 {
                    let _ = win::set_attributes(&p, c.attributes);
                }
            }
        }
        win::set_attributes(orig, it.saved.attributes).map_err(|e| acl::acl_error(orig, e))?;
        let now = win::attributes(orig).unwrap_or(0);
        if (now ^ it.saved.attributes) & win::ATTR_SETTABLE != 0 {
            return Err(Error::Io(format!(
                "The attributes of \"{}\" could not be restored.",
                it.name
            )));
        }
        Ok(())
    }

    /// Timestamps last: renames and listings touch them.
    fn restore_times(&self, it: &Item, orig: &Path) -> Result<()> {
        // Deepest folders first: reaching a child through its parent counts
        // as an access to the parent.
        for d in it.extra.dir_times.iter().rev() {
            let _ = win::set_times(&orig.join(&d.path), d.created, d.accessed, d.modified);
        }
        let s = &it.saved;
        win::set_times(orig, s.created, s.accessed, s.modified)
            .map_err(|e| io_text("The dates of the item could not be restored", e))?;
        let st = win::stat(orig).map_err(|e| io_text("The item cannot be read", e))?;
        if (st.created, st.modified) != (s.created, s.modified) {
            return Err(Error::Io(format!(
                "The dates of \"{}\" could not be restored.",
                it.name
            )));
        }
        Ok(())
    }

    /// Writes (or replaces) the item's sealed recovery copy in its holding folder.
    fn write_recovery(&self, it: &Item) -> Result<()> {
        let path = Self::recovery_path(it)?;
        let dir = path
            .parent()
            .ok_or_else(|| Error::Io("The holding folder is not known.".into()))?;
        let plain =
            zeroize::Zeroizing::new(serde_json::to_vec(it).map_err(|e| Error::Io(e.to_string()))?);
        let sealed = self.keys.seal(crypto::MAGIC_RECOVERY, &plain);
        let tmp = dir.join(hold::random_name());
        let result = (|| {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(win::long(&tmp))?;
            f.write_all(&sealed)?;
            f.sync_all()?;
            drop(f);
            std::fs::rename(win::long(&tmp), win::long(&path))
        })();
        if let Err(e) = result {
            let _ = std::fs::remove_file(win::long(&tmp));
            return Err(io_text("The recovery copy could not be written", e));
        }
        Ok(())
    }

    fn delete_recovery(&self, it: &Item) -> Result<()> {
        let Some(path) = &it.extra.recovery else {
            return Ok(());
        };
        let path = PathBuf::from(path);
        // Only ever delete a file that really is a recovery copy.
        match std::fs::read(win::long(&path)) {
            Ok(bytes) if crypto::Header::parse(crypto::MAGIC_RECOVERY, &bytes).is_some() => {
                std::fs::remove_file(win::long(&path))
                    .map_err(|e| io_text("The recovery copy could not be removed", e))
            }
            _ => Ok(()),
        }
    }

    fn make_standin(&self, it: &Item) -> Result<()> {
        let made = crate::standin::create(
            &self.env.exe,
            &it.id,
            Path::new(&it.original),
            it.kind == Kind::Folder,
        )
        .map_err(|e| Error::Io(format!("The shortcut could not be created: {e}")))?;
        if !win::exists(&made) {
            return Err(Error::Io("The shortcut could not be created.".into()));
        }
        Ok(())
    }

    /// Removes the stand-in, but only if it still is the shortcut VeilForge made.
    fn remove_standin(&self, it: &Item, path: &Path) -> Result<()> {
        if !win::exists(path) {
            return Ok(());
        }
        match crate::standin::read(path) {
            Ok(s) if crate::standin::id_of(&s).as_deref() == Some(it.id.as_str()) => {
                std::fs::remove_file(win::long(path))
                    .map_err(|e| io_text("The shortcut could not be removed", e))
            }
            _ => Ok(()),
        }
    }

    /// State, what is wrong (empty when fine) and whether `repair` can fix it.
    fn inspect(&self, it: &Item, deep: bool) -> (State, String, bool) {
        let orig = PathBuf::from(&it.original);
        if it.open_since.is_some() {
            return if win::exists(&orig) {
                (State::Open, String::new(), false)
            } else {
                (
                    State::Missing,
                    "The item is no longer at its original place.".into(),
                    false,
                )
            };
        }
        let mut problems: Vec<&str> = Vec::new();
        let mut tampered = false;
        let hold_problem;
        if it.hides() {
            let Some(hold) = it.hold_path.as_ref().map(PathBuf::from) else {
                return (State::Missing, "The map entry is incomplete.".into(), false);
            };
            if !win::exists(&hold) {
                return (
                    State::Missing,
                    "The item is no longer in the holding folder.".into(),
                    false,
                );
            }
            if let Some(dir) = hold.parent() {
                hold_problem = hold::problem(dir, it.reduced);
                if let Some(p) = &hold_problem {
                    tampered = true;
                    problems.push(p);
                }
            }
        } else {
            if !win::exists(&orig) {
                return (
                    State::Missing,
                    "The item is no longer at its place.".into(),
                    false,
                );
            }
            if !acl::is_locked(&orig) {
                tampered = true;
                problems.push("The lock on the item was changed from outside.");
            } else if win::attributes(&orig)
                .map(|a| a & win::ATTR_READONLY == 0)
                .unwrap_or(false)
            {
                tampered = true;
                problems.push("The read-only mark that guards against deleting was removed.");
            }
        }
        if let Some(s) = &it.standin {
            let ok = if deep {
                crate::standin::read(Path::new(s))
                    .ok()
                    .and_then(|s| crate::standin::id_of(&s))
                    .as_deref()
                    == Some(it.id.as_str())
            } else {
                win::exists(Path::new(s))
            };
            if !ok {
                tampered = true;
                problems
                    .push("The shortcut that stands in for the item is missing or was changed.");
            }
        }
        let recovery_ok = match &it.extra.recovery {
            None => false,
            Some(r) if deep => std::fs::read(win::long(Path::new(r)))
                .ok()
                .map(|b| self.keys.open(crypto::MAGIC_RECOVERY, &b).is_ok())
                .unwrap_or(false),
            Some(r) => win::exists(Path::new(r)),
        };
        if !recovery_ok {
            problems.push("The recovery copy is missing or cannot be read.");
        }
        let state = if tampered {
            State::Tampered
        } else {
            State::Locked
        };
        (state, problems.join(" "), !problems.is_empty())
    }

    // ----------------------------------------------------------------------------------
    // Recovery copies
    // ----------------------------------------------------------------------------------

    /// Adds every recovery copy that opens with this map's keys or with one of
    /// `more` (keys derived from the same password under an older salt).
    fn import_locked(&mut self, more: &[crypto::Keys]) -> Result<usize> {
        let mut items = self.items.clone();
        let mut added: Vec<(Item, bool)> = Vec::new();
        let mut orphans: Vec<PathBuf> = Vec::new();
        for (path, bytes) in recovery_copies(&self.env, &self.items) {
            let mut opened = self
                .keys
                .open(crypto::MAGIC_RECOVERY, &bytes)
                .ok()
                .map(|p| (p, false));
            if opened.is_none() {
                opened = more
                    .iter()
                    .find_map(|k| k.open(crypto::MAGIC_RECOVERY, &bytes).ok())
                    .map(|p| (p, true));
            }
            let Some((plain, foreign_key)) = opened else {
                continue;
            };
            let Ok(mut it) = serde_json::from_slice::<Item>(&plain) else {
                continue;
            };
            let here = path.to_string_lossy().into_owned();
            if let Some(known) = items.iter().find(|i| i.id == it.id) {
                if known.extra.recovery.as_deref().map(safety::norm) != Some(safety::norm(&here)) {
                    orphans.push(path);
                }
                continue;
            }
            it.extra.recovery = Some(here);
            items.push(it.clone());
            added.push((it, foreign_key));
        }
        if added.is_empty() && orphans.is_empty() {
            return Ok(0);
        }
        if !added.is_empty() {
            self.save(items)?;
        }
        for (it, foreign_key) in &added {
            if *foreign_key {
                let _ = self.write_recovery(it);
            }
        }
        for o in orphans {
            let _ = std::fs::remove_file(win::long(&o));
        }
        Ok(added.len())
    }
}

/// Reads every recovery copy found in the holding folders.
fn recovery_copies(env: &Env, known: &[Item]) -> Vec<(PathBuf, Vec<u8>)> {
    let hidden: std::collections::HashSet<String> = known
        .iter()
        .filter_map(|i| i.hold_path.as_deref().map(safety::norm))
        .collect();
    let mut out = Vec::new();
    for dir in hold::known(env) {
        if !win::is_dir(&dir) {
            continue;
        }
        let Ok(entries) = hold::list(&dir) else {
            continue;
        };
        for path in entries {
            if hidden.contains(&safety::norm(&path.to_string_lossy())) {
                continue;
            }
            let Ok(st) = win::stat(&path) else {
                continue;
            };
            // A recovery copy is a small plain file; hidden user files are not opened.
            if st.is_dir() || st.is_link() || st.size < crypto::HEADER_LEN as u64 {
                continue;
            }
            if st.size > 256 * 1024 * 1024 {
                continue;
            }
            use std::io::Read;
            let Ok(mut f) = std::fs::File::open(win::long(&path)) else {
                continue;
            };
            let mut magic = [0u8; 8];
            if f.read_exact(&mut magic).is_err() || &magic != crypto::MAGIC_RECOVERY {
                continue;
            }
            let mut bytes = magic.to_vec();
            if f.read_to_end(&mut bytes).is_ok() {
                out.push((path, bytes));
            }
        }
    }
    out
}

/// The refusal rules and pre-flight scan, usable without an unlocked map
/// (drag and drop feedback, the Explorer menu).
pub fn precheck(env: &Env, path: &Path) -> Precheck {
    safety::precheck(env, path, &[])
}

/// Rebuilds a map from the recovery copies alone (the app-data folder is gone):
/// creates a new map in `env.data_dir` sealed with `password` and imports every
/// recovery copy that password opens. Returns the vault and the number imported.
pub fn rebuild_from_recovery(env: Env, password: &str) -> Result<(Vault, usize)> {
    if Vault::exists(&env) {
        return Err(Error::Refused(
            "A map already exists here. Open it and use \"Import recovery copies\" instead.".into(),
        ));
    }
    // The copies carry the salt they were sealed under: one key derivation per
    // distinct salt (normally exactly one).
    let copies = recovery_copies(&env, &[]);
    let mut keysets: Vec<crypto::Keys> = Vec::new();
    let mut tried: Vec<[u8; 32]> = Vec::new();
    for (_, bytes) in &copies {
        let Some(header) = crypto::Header::parse(crypto::MAGIC_RECOVERY, bytes) else {
            continue;
        };
        if tried.contains(&header.salt) || tried.len() >= 16 {
            continue;
        }
        tried.push(header.salt);
        let keys = crypto::Keys::derive(password, &header.kdf, &header.salt).map_err(Error::Io)?;
        if keys.open(crypto::MAGIC_RECOVERY, bytes).is_ok() {
            keysets.push(keys);
        }
    }
    if keysets.is_empty() && !copies.is_empty() {
        return Err(Error::WrongPassword);
    }
    // Reuse the first salt so that the existing copies stay readable as they are.
    let own = if keysets.is_empty() {
        crypto::Keys::fresh(password, &env.kdf).map_err(Error::Io)?
    } else {
        keysets.remove(0)
    };
    let mut vault = Vault::create_with(env, own)?;
    let _lock =
        store::MapLock::acquire(&vault.env.data_dir, store::LOCK_WAIT).map_err(Error::Busy)?;
    let n = vault.import_locked(&keysets)?;
    Ok((vault, n))
}

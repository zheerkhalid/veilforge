//! The operations the window asks for, and their results in plain words.
//!
//! The real engine runs them on a worker thread ([`run`]); the demo runs the
//! same operations on invented data in memory (`crate::demo`).

use crate::engine::{Error, Health, Progress, ProtectOptions, Vault};
use crate::model::{Item, State};
use std::collections::HashMap;
use std::path::PathBuf;
use zeroize::Zeroizing;

/// What the window shows: a copy of the map plus what the disk says right now.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub items: Vec<Item>,
    pub states: HashMap<String, State>,
    pub groups: Vec<String>,
}

impl Snapshot {
    pub fn of(vault: &Vault) -> Snapshot {
        let items = vault.items().to_vec();
        let states = items
            .iter()
            .map(|i| (i.id.clone(), vault.state(&i.id)))
            .collect();
        Snapshot {
            groups: vault.groups(),
            items,
            states,
        }
    }

    pub fn state(&self, id: &str) -> State {
        self.states.get(id).copied().unwrap_or(State::Locked)
    }

    pub fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn open_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| self.state(&i.id) == State::Open)
            .count()
    }

    pub fn problem_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(self.state(&i.id), State::Missing | State::Tampered))
            .count()
    }
}

pub enum Op {
    Protect(Vec<(PathBuf, ProtectOptions)>),
    /// Unlock temporarily.
    OpenTemp(Vec<String>),
    Relock(Vec<String>),
    /// Unlock permanently.
    Release(Vec<String>),
    RelockAll,
    /// Panic restore: checks the master password, then releases everything.
    PanicRestore(Zeroizing<String>),
    Repair(Vec<String>),
    SetGroup(Vec<String>, String),
    Health,
    /// Looks for recovery copies of items the map does not know.
    ImportRecovery,
    ChangePassword {
        current: Zeroizing<String>,
        new: Zeroizing<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    Protect,
    OpenTemp,
    Relock,
    Release,
    RelockAll,
    PanicRestore,
    Repair,
    SetGroup,
    Health,
    ImportRecovery,
    ChangePassword,
}

impl Op {
    pub fn kind(&self) -> OpKind {
        match self {
            Op::Protect(_) => OpKind::Protect,
            Op::OpenTemp(_) => OpKind::OpenTemp,
            Op::Relock(_) => OpKind::Relock,
            Op::Release(_) => OpKind::Release,
            Op::RelockAll => OpKind::RelockAll,
            Op::PanicRestore(_) => OpKind::PanicRestore,
            Op::Repair(_) => OpKind::Repair,
            Op::SetGroup(..) => OpKind::SetGroup,
            Op::Health => OpKind::Health,
            Op::ImportRecovery => OpKind::ImportRecovery,
            Op::ChangePassword { .. } => OpKind::ChangePassword,
        }
    }
}

impl OpKind {
    /// Shown while the operation runs.
    pub fn title(self) -> &'static str {
        match self {
            OpKind::Protect => "Protecting",
            OpKind::OpenTemp => "Unlocking",
            OpKind::Relock | OpKind::RelockAll => "Locking again",
            OpKind::Release => "Unlocking permanently",
            OpKind::PanicRestore => "Restoring everything",
            OpKind::Repair => "Repairing",
            OpKind::SetGroup => "Saving",
            OpKind::Health => "Checking every item",
            OpKind::ImportRecovery => "Looking for recovery copies",
            OpKind::ChangePassword => "Changing the master password",
        }
    }

    /// Title of the result list.
    pub fn done_title(self) -> &'static str {
        match self {
            OpKind::Protect => "Protection results",
            OpKind::OpenTemp => "Unlock results",
            OpKind::Relock | OpKind::RelockAll => "Lock results",
            OpKind::Release => "Unlock results",
            OpKind::PanicRestore => "Panic restore results",
            OpKind::Repair => "Repair results",
            _ => "Results",
        }
    }

    /// One short sentence for the toast when everything worked.
    pub fn success(self, n: usize) -> String {
        let items = if n == 1 {
            "1 item".to_string()
        } else {
            format!("{n} items")
        };
        match self {
            OpKind::Protect => format!("{items} protected."),
            OpKind::OpenTemp => format!("{items} unlocked for now."),
            OpKind::Relock | OpKind::RelockAll => {
                if n == 0 {
                    "Nothing was open. Everything is locked.".to_string()
                } else {
                    format!("{items} locked again.")
                }
            }
            OpKind::Release => format!("{items} unlocked permanently and put back as before."),
            OpKind::PanicRestore => format!("{items} restored to where they were."),
            OpKind::Repair => format!("{items} repaired."),
            OpKind::SetGroup => "Group saved.".to_string(),
            OpKind::Health => "Health check finished.".to_string(),
            OpKind::ImportRecovery => {
                if n == 0 {
                    "No lost items were found.".to_string()
                } else {
                    format!("{items} found and added back to the list.")
                }
            }
            OpKind::ChangePassword => "The master password was changed.".to_string(),
        }
    }
}

/// The result for one item.
#[derive(Clone, Debug)]
pub struct Line {
    pub name: String,
    /// `Ok`: what was done (may be empty). `Err`: the engine's error.
    pub result: Result<String, Error>,
    /// Where the item is now, when the operation brought it back.
    pub path: Option<PathBuf>,
}

impl Line {
    pub fn ok(name: impl Into<String>, text: impl Into<String>) -> Line {
        Line {
            name: name.into(),
            result: Ok(text.into()),
            path: None,
        }
    }

    pub fn err(name: impl Into<String>, error: Error) -> Line {
        Line {
            name: name.into(),
            result: Err(error),
            path: None,
        }
    }
}

pub struct Outcome {
    pub kind: OpKind,
    pub lines: Vec<Line>,
    /// A fresh health report, when the operation made one.
    pub health: Option<Vec<Health>>,
    /// The operation did not start at all (wrong password, locked, ...).
    pub fatal: Option<Error>,
    /// For [`OpKind::ImportRecovery`]: how many items were added.
    pub count: usize,
}

impl Outcome {
    pub fn new(kind: OpKind) -> Outcome {
        Outcome {
            kind,
            lines: Vec::new(),
            health: None,
            fatal: None,
            count: 0,
        }
    }

    pub fn failed(kind: OpKind, error: Error) -> Outcome {
        Outcome {
            fatal: Some(error),
            ..Outcome::new(kind)
        }
    }

    pub fn failures(&self) -> usize {
        self.lines.iter().filter(|l| l.result.is_err()).count()
    }

    pub fn successes(&self) -> usize {
        self.lines.len() - self.failures()
    }

    pub fn needs_elevation(&self) -> bool {
        self.lines
            .iter()
            .any(|l| matches!(l.result, Err(Error::NeedsElevation(_))))
    }
}

/// An engine error as one plain sentence, plus the engine's own detail text
/// when it adds something.
pub fn explain(error: &Error) -> (String, String) {
    let detail = |s: &String| s.trim().to_string();
    match error {
        Error::WrongPassword => ("The master password is not correct.".into(), String::new()),
        Error::NoVault => ("No master password has been set yet.".into(), String::new()),
        Error::InUse(s) => (
            "A file is open in another program. Close the program using it and try again.".into(),
            detail(s),
        ),
        Error::NeedsElevation(s) => (
            "Windows only lets an administrator change this item. Restart VeilForge as administrator and try again.".into(),
            detail(s),
        ),
        Error::CrossVolume(s) => (
            "This would mean copying the item to another drive, which VeilForge never does on its own.".into(),
            detail(s),
        ),
        Error::Unsupported(s) => (
            "The drive this item is on cannot do that.".into(),
            detail(s),
        ),
        Error::Busy(s) => (
            "Another VeilForge window is working right now. Try again in a moment.".into(),
            detail(s),
        ),
        Error::Corrupt(s) => (
            "VeilForge's own list of protected items could not be read.".into(),
            detail(s),
        ),
        Error::Refused(s) | Error::NotFound(s) | Error::Io(s) => (detail(s), String::new()),
    }
}

/// The name to show for an id (the item may be gone after the operation).
fn name_of(vault: &Vault, id: &str) -> String {
    vault
        .item(id)
        .map(|i| i.name.clone())
        .unwrap_or_else(|| "Item".to_string())
}

fn path_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

/// Progress of item `index` of `count`, folded into one overall bar.
fn scaled(index: usize, count: usize, name: &str, p: Progress) -> Progress {
    let inner = if p.total > 0 {
        (p.done as f64 / p.total as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Progress {
        done: ((index as f64 + inner) * 1000.0) as u64,
        total: count as u64 * 1000,
        what: if p.what.is_empty() {
            name.to_string()
        } else {
            format!("{name}: {}", p.what)
        },
    }
}

/// Runs one operation on the unlocked map. Called on a worker thread.
pub fn run(vault: &mut Vault, op: Op, progress: &mut dyn FnMut(Progress)) -> Outcome {
    let kind = op.kind();
    let mut out = Outcome::new(kind);
    match op {
        Op::Protect(list) => {
            let count = list.len();
            for (index, (path, options)) in list.into_iter().enumerate() {
                let name = path_name(&path);
                let result = vault.protect(&path, &options, &mut |p| {
                    progress(scaled(index, count, &name, p));
                });
                out.lines.push(Line {
                    name,
                    result: result.map(|_| String::new()),
                    path: None,
                });
            }
        }
        Op::OpenTemp(ids) => {
            let count = ids.len();
            for (index, id) in ids.iter().enumerate() {
                let name = name_of(vault, id);
                let result = vault.open_temporarily(id, &mut |p| {
                    progress(scaled(index, count, &name, p));
                });
                let path = result.as_ref().ok().cloned();
                out.lines.push(Line {
                    name,
                    result: result.map(|p| p.to_string_lossy().to_string()),
                    path,
                });
            }
        }
        Op::Relock(ids) => {
            let count = ids.len();
            for (index, id) in ids.iter().enumerate() {
                let name = name_of(vault, id);
                let result = vault.relock(id, &mut |p| {
                    progress(scaled(index, count, &name, p));
                });
                out.lines.push(Line {
                    name,
                    result: result.map(|()| String::new()),
                    path: None,
                });
            }
        }
        Op::Release(ids) => {
            let count = ids.len();
            for (index, id) in ids.iter().enumerate() {
                let name = name_of(vault, id);
                let result = vault.release(id, &mut |p| {
                    progress(scaled(index, count, &name, p));
                });
                let path = result.as_ref().ok().cloned();
                out.lines.push(Line {
                    name,
                    result: result.map(|p| p.to_string_lossy().to_string()),
                    path,
                });
            }
        }
        Op::RelockAll => {
            let names: HashMap<String, String> = vault
                .items()
                .iter()
                .map(|i| (i.id.clone(), i.name.clone()))
                .collect();
            for (id, result) in vault.relock_all(progress) {
                out.lines.push(Line {
                    name: names.get(&id).cloned().unwrap_or_else(|| "Item".into()),
                    result: result.map(|()| String::new()),
                    path: None,
                });
            }
        }
        Op::PanicRestore(password) => {
            if !vault.verify_password(&password) {
                return Outcome::failed(kind, Error::WrongPassword);
            }
            drop(password);
            let names: HashMap<String, String> = vault
                .items()
                .iter()
                .map(|i| (i.id.clone(), i.name.clone()))
                .collect();
            for (id, result) in vault.release_all(progress) {
                let path = result.as_ref().ok().cloned();
                out.lines.push(Line {
                    name: names.get(&id).cloned().unwrap_or_else(|| "Item".into()),
                    result: result.map(|p| p.to_string_lossy().to_string()),
                    path,
                });
            }
            vault.remove_empty_holds();
            out.health = Some(vault.health());
        }
        Op::Repair(ids) => {
            let count = ids.len();
            for (index, id) in ids.iter().enumerate() {
                let name = name_of(vault, id);
                progress(scaled(index, count, &name, Progress::default()));
                let result = vault.repair(id);
                out.lines.push(Line {
                    name,
                    result,
                    path: None,
                });
            }
            out.health = Some(vault.health());
        }
        Op::SetGroup(ids, group) => {
            for id in &ids {
                let name = name_of(vault, id);
                let result = vault.set_group(id, group.trim());
                out.lines.push(Line {
                    name,
                    result: result.map(|()| String::new()),
                    path: None,
                });
            }
        }
        Op::Health => {
            progress(Progress {
                done: 0,
                total: 0,
                what: "Looking at every protected item".into(),
            });
            out.health = Some(vault.health());
        }
        Op::ImportRecovery => match vault.import_recovery() {
            Ok(n) => {
                out.count = n;
                out.health = Some(vault.health());
            }
            Err(e) => out.fatal = Some(e),
        },
        Op::ChangePassword { current, new } => {
            if let Err(e) = vault.change_password(&current, &new) {
                out.fatal = Some(e);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_folded_into_one_bar() {
        let p = scaled(
            1,
            4,
            "Tax 2025",
            Progress {
                done: 5,
                total: 10,
                what: "Renaming".into(),
            },
        );
        assert_eq!((p.done, p.total), (1500, 4000));
        assert_eq!(p.what, "Tax 2025: Renaming");
        let p = scaled(0, 1, "A", Progress::default());
        assert_eq!((p.done, p.total), (0, 1000));
        assert_eq!(p.what, "A");
    }

    #[test]
    fn errors_are_explained_in_plain_words() {
        let (text, detail) = explain(&Error::InUse("report.docx is open in Word".into()));
        assert!(text.contains("Close the program using it and try again"));
        assert_eq!(detail, "report.docx is open in Word");
        let (text, _) = explain(&Error::NeedsElevation(String::new()));
        assert!(text.contains("administrator"));
        let (text, detail) = explain(&Error::Refused("This is a Windows folder.".into()));
        assert_eq!(text, "This is a Windows folder.");
        assert!(detail.is_empty());
        for kind in [
            OpKind::Protect,
            OpKind::OpenTemp,
            OpKind::Relock,
            OpKind::Release,
            OpKind::PanicRestore,
            OpKind::Repair,
        ] {
            assert!(kind.success(1).starts_with("1 item "));
            assert!(kind.success(3).starts_with("3 items "));
            assert!(!kind.title().is_empty() && !kind.done_title().is_empty());
        }
        let mut out = Outcome::new(OpKind::Protect);
        out.lines.push(Line::ok("a", ""));
        out.lines
            .push(Line::err("b", Error::NeedsElevation(String::new())));
        assert_eq!((out.successes(), out.failures()), (1, 1));
        assert!(out.needs_elevation());
    }
}

//! Invented data for `--demo` and the screenshots.
//!
//! Nothing here reads or changes the disk: the "vault" is a list in memory
//! and every operation only edits that list. All names and paths are made up.

use crate::engine::{Error, Health, Precheck, Verdict};
use crate::model::{ChildRename, Item, Kind, Mode, Saved, State};
use crate::ui::ops::{Line, Op, Outcome, Snapshot};
use crate::util::DAY;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The data folder the demo pretends to use (never read or written).
pub const DEMO_DATA_DIR: &str = "C:\\Users\\alex\\AppData\\Roaming\\VeilForge";

/// A small deterministic hash, for invented ids and sizes.
fn hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn id_for(path: &str) -> String {
    format!("{:016x}", hash(path))
}

fn hold_name(path: &str) -> String {
    format!("{:032x}", u128::from(hash(path)) * 0x9e37_79b9_7f4a_7c15)
}

struct Spec {
    path: &'static str,
    kind: Kind,
    mode: Mode,
    ask_hidden: bool,
    scrambled: bool,
    group: &'static str,
    size: u64,
    files: u64,
    days_ago: i64,
    state: State,
    reduced: bool,
}

const MB: u64 = 1024 * 1024;

const SPECS: &[Spec] = &[
    Spec {
        path: "C:\\Users\\alex\\Documents\\Tax 2025",
        kind: Kind::Folder,
        mode: Mode::Hide,
        ask_hidden: false,
        scrambled: true,
        group: "Finance",
        size: 412 * MB,
        files: 86,
        days_ago: 3,
        state: State::Locked,
        reduced: false,
    },
    Spec {
        path: "D:\\Projects\\Prototype",
        kind: Kind::Folder,
        mode: Mode::Lock,
        ask_hidden: false,
        scrambled: false,
        group: "Work",
        size: 2_350 * MB,
        files: 4_812,
        days_ago: 12,
        state: State::Locked,
        reduced: false,
    },
    Spec {
        path: "C:\\Users\\alex\\Documents\\Scans\\Passport scan.pdf",
        kind: Kind::File,
        mode: Mode::Ask,
        ask_hidden: true,
        scrambled: false,
        group: "Personal",
        size: 3 * MB + 180_000,
        files: 1,
        days_ago: 40,
        state: State::Locked,
        reduced: false,
    },
    Spec {
        path: "C:\\Users\\alex\\Documents\\Diary.docx",
        kind: Kind::File,
        mode: Mode::Hide,
        ask_hidden: false,
        scrambled: false,
        group: "Personal",
        size: 486_000,
        files: 1,
        days_ago: 75,
        state: State::Open,
        reduced: false,
    },
    Spec {
        path: "D:\\Work\\Client contracts",
        kind: Kind::Folder,
        mode: Mode::Ask,
        ask_hidden: false,
        scrambled: false,
        group: "Work",
        size: 148 * MB,
        files: 213,
        days_ago: 21,
        state: State::Locked,
        reduced: false,
    },
    Spec {
        path: "E:\\Photos\\Family photos 2019",
        kind: Kind::Folder,
        mode: Mode::Hide,
        ask_hidden: false,
        scrambled: true,
        group: "Personal",
        size: 7_900 * MB,
        files: 1_934,
        days_ago: 190,
        state: State::Locked,
        reduced: true,
    },
    Spec {
        path: "C:\\Users\\alex\\Documents\\Budget.xlsx",
        kind: Kind::File,
        mode: Mode::Lock,
        ask_hidden: false,
        scrambled: false,
        group: "Finance",
        size: 92_000,
        files: 1,
        days_ago: 9,
        state: State::Tampered,
        reduced: false,
    },
    Spec {
        path: "D:\\Backups\\Old laptop backup",
        kind: Kind::Folder,
        mode: Mode::Hide,
        ask_hidden: false,
        scrambled: false,
        group: "",
        size: 31_400 * MB,
        files: 58_210,
        days_ago: 320,
        state: State::Missing,
        reduced: false,
    },
    Spec {
        path: "C:\\Users\\alex\\Desktop\\Recovery codes.txt",
        kind: Kind::File,
        mode: Mode::Hide,
        ask_hidden: false,
        scrambled: false,
        group: "Personal",
        size: 2_100,
        files: 1,
        days_ago: 1,
        state: State::Locked,
        reduced: false,
    },
    Spec {
        path: "D:\\Videos\\Wedding video.mp4",
        kind: Kind::File,
        mode: Mode::Lock,
        ask_hidden: false,
        scrambled: false,
        group: "Personal",
        size: 4_200 * MB,
        files: 1,
        days_ago: 410,
        state: State::Locked,
        reduced: false,
    },
    Spec {
        path: "D:\\Projects\\Side project",
        kind: Kind::Folder,
        mode: Mode::Ask,
        ask_hidden: true,
        scrambled: true,
        group: "Work",
        size: 640 * MB,
        files: 1_127,
        days_ago: 5,
        state: State::Locked,
        reduced: false,
    },
    Spec {
        path: "C:\\Users\\alex\\Documents\\Medical",
        kind: Kind::Folder,
        mode: Mode::Hide,
        ask_hidden: false,
        scrambled: true,
        group: "Personal",
        size: 58 * MB,
        files: 47,
        days_ago: 64,
        state: State::Locked,
        reduced: false,
    },
    // A Kurdish name in a Kurdish folder: right-to-left text is drawn joined
    // and in reading order ("Annual report 2026.pdf" in "Documents").
    Spec {
        path: "D:\\بەڵگەنامەکان\\ڕاپۆرتی ساڵانە ٢٠٢٦.pdf",
        kind: Kind::File,
        mode: Mode::Hide,
        ask_hidden: false,
        scrambled: false,
        group: "Work",
        size: 3 * MB,
        files: 1,
        days_ago: 2,
        state: State::Locked,
        reduced: false,
    },
];

fn file_name(path: &str) -> String {
    path.rsplit('\\').next().unwrap_or(path).to_string()
}

fn volume_of(path: &str) -> String {
    path.chars().take(3).collect()
}

fn hold_path(path: &str) -> String {
    format!("{}VeilForge.sys\\{}", volume_of(path), hold_name(path))
}

/// Builds an item the way the engine would record it.
#[allow(clippy::too_many_arguments)]
fn item(
    path: &str,
    kind: Kind,
    mode: Mode,
    ask_hidden: bool,
    scrambled: bool,
    group: &str,
    size: u64,
    files: u64,
    locked_at: i64,
    reduced: bool,
) -> Item {
    let hides = matches!(mode, Mode::Hide) || (matches!(mode, Mode::Ask) && ask_hidden);
    let scrambled = scrambled && hides && kind == Kind::Folder;
    Item {
        id: id_for(path),
        name: file_name(path),
        original: path.to_string(),
        kind,
        mode,
        ask_hidden: matches!(mode, Mode::Ask) && ask_hidden,
        scrambled,
        group: group.to_string(),
        size,
        files,
        locked_at,
        open_since: None,
        volume: volume_of(path),
        hold_path: hides.then(|| hold_path(path)),
        standin: matches!(mode, Mode::Ask).then(|| format!("{path}.lnk")),
        reduced,
        saved: Saved::default(),
        children: if scrambled {
            // Only the count is shown; a few invented entries are enough.
            (0..files.min(64))
                .map(|n| ChildRename {
                    dir: String::new(),
                    original: format!("file-{n}"),
                    scrambled: format!("{:016x}", hash(&format!("{path}{n}"))),
                })
                .collect()
        } else {
            Vec::new()
        },
        extra: Default::default(),
    }
}

/// The in-memory stand-in for an unlocked vault.
#[derive(Clone, Debug, Default)]
pub struct DemoVault {
    pub items: Vec<Item>,
    pub states: HashMap<String, State>,
    now: i64,
}

impl DemoVault {
    /// The invented list shown in the demo and the screenshots.
    pub fn sample(now: i64) -> DemoVault {
        let mut v = DemoVault {
            now,
            ..Default::default()
        };
        for s in SPECS {
            let mut it = item(
                s.path,
                s.kind,
                s.mode,
                s.ask_hidden,
                s.scrambled,
                s.group,
                s.size,
                s.files,
                now - s.days_ago * DAY - 3_600,
                s.reduced,
            );
            if s.state == State::Open {
                it.open_since = Some(now - 25 * 60);
            }
            v.states.insert(it.id.clone(), s.state);
            v.items.push(it);
        }
        v
    }

    /// A vault with nothing in it (the empty state).
    pub fn empty(now: i64) -> DemoVault {
        DemoVault {
            now,
            ..Default::default()
        }
    }

    pub fn groups(&self) -> Vec<String> {
        let mut groups: Vec<String> = self
            .items
            .iter()
            .map(|i| i.group.clone())
            .filter(|g| !g.is_empty())
            .collect();
        groups.sort_by_key(|g| g.to_lowercase());
        groups.dedup();
        groups
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            items: self.items.clone(),
            states: self.states.clone(),
            groups: self.groups(),
        }
    }

    fn state(&self, id: &str) -> State {
        self.states.get(id).copied().unwrap_or(State::Locked)
    }

    pub fn health(&self) -> Vec<Health> {
        self.items
            .iter()
            .map(|i| {
                let state = self.state(&i.id);
                let (problem, repairable) = match state {
                    State::Locked | State::Open => (String::new(), false),
                    State::Tampered => (
                        "Its permissions were changed from outside VeilForge, so it can be opened right now.".to_string(),
                        true,
                    ),
                    State::Missing => (
                        "It is not in the holding folder any more. Someone may have moved or deleted it with another tool.".to_string(),
                        false,
                    ),
                };
                Health {
                    id: i.id.clone(),
                    name: i.name.clone(),
                    state,
                    problem,
                    repairable,
                }
            })
            .collect()
    }

    fn name(&self, id: &str) -> String {
        self.items
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.name.clone())
            .unwrap_or_else(|| "Item".to_string())
    }

    fn release(&mut self, id: &str) -> Line {
        let name = self.name(id);
        let Some(pos) = self.items.iter().position(|i| i.id == id) else {
            return Line::err(
                name,
                Error::NotFound("That item is not in the list.".into()),
            );
        };
        if self.state(id) == State::Missing {
            return Line::err(
                name,
                Error::NotFound(
                    "The item is not where VeilForge left it, so it cannot be put back.".into(),
                ),
            );
        }
        let it = self.items.remove(pos);
        self.states.remove(id);
        Line {
            name,
            result: Ok(it.original.clone()),
            path: Some(PathBuf::from(it.original)),
        }
    }

    /// Runs an operation on the invented list. Nothing touches the disk.
    pub fn run(&mut self, op: Op) -> Outcome {
        let kind = op.kind();
        let mut out = Outcome::new(kind);
        match op {
            Op::Protect(list) => {
                for (path, options) in list {
                    let text = path.to_string_lossy().to_string();
                    let pre = precheck(&path);
                    if let Some(reason) = pre.refusal {
                        out.lines
                            .push(Line::err(file_name(&text), Error::Refused(reason)));
                        continue;
                    }
                    if self.items.iter().any(|i| i.original == text) {
                        out.lines.push(Line::err(
                            file_name(&text),
                            Error::Refused("This item is already protected.".into()),
                        ));
                        continue;
                    }
                    let mode = if pre.acl_supported {
                        options.mode
                    } else {
                        Mode::Hide
                    };
                    let it = item(
                        &text,
                        pre.kind,
                        mode,
                        options.ask_hidden,
                        options.scramble,
                        options.group.trim(),
                        pre.size,
                        pre.files,
                        self.now,
                        !pre.acl_supported,
                    );
                    self.states.insert(it.id.clone(), State::Locked);
                    out.lines.push(Line::ok(it.name.clone(), ""));
                    self.items.insert(0, it);
                }
            }
            Op::OpenTemp(ids) => {
                for id in ids {
                    let name = self.name(&id);
                    match self.state(&id) {
                        State::Missing => out.lines.push(Line::err(
                            name,
                            Error::NotFound(
                                "The item is not where VeilForge left it. Run the health check."
                                    .into(),
                            ),
                        )),
                        _ => {
                            let now = self.now;
                            let mut path = None;
                            if let Some(it) = self.items.iter_mut().find(|i| i.id == id) {
                                it.open_since = Some(now);
                                path = Some(PathBuf::from(&it.original));
                            }
                            self.states.insert(id, State::Open);
                            out.lines.push(Line {
                                name,
                                result: Ok(String::new()),
                                path,
                            });
                        }
                    }
                }
            }
            Op::Relock(ids) => {
                for id in ids {
                    let name = self.name(&id);
                    if let Some(it) = self.items.iter_mut().find(|i| i.id == id) {
                        it.open_since = None;
                    }
                    if self.state(&id) == State::Open {
                        self.states.insert(id, State::Locked);
                    }
                    out.lines.push(Line::ok(name, ""));
                }
            }
            Op::RelockAll => {
                let open: Vec<String> = self
                    .items
                    .iter()
                    .filter(|i| self.state(&i.id) == State::Open)
                    .map(|i| i.id.clone())
                    .collect();
                for id in open {
                    let name = self.name(&id);
                    if let Some(it) = self.items.iter_mut().find(|i| i.id == id) {
                        it.open_since = None;
                    }
                    self.states.insert(id, State::Locked);
                    out.lines.push(Line::ok(name, ""));
                }
            }
            Op::Release(ids) => {
                for id in ids {
                    let line = self.release(&id);
                    out.lines.push(line);
                }
            }
            Op::PanicRestore(_) => {
                let ids: Vec<String> = self.items.iter().map(|i| i.id.clone()).collect();
                for id in ids {
                    let line = self.release(&id);
                    out.lines.push(line);
                }
                out.health = Some(self.health());
            }
            Op::Repair(ids) => {
                for id in ids {
                    let name = self.name(&id);
                    match self.state(&id) {
                        State::Tampered => {
                            self.states.insert(id, State::Locked);
                            out.lines
                                .push(Line::ok(name, "The protection was put back."));
                        }
                        State::Missing => out.lines.push(Line::err(
                            name,
                            Error::NotFound(
                                "The item itself is gone, so there is nothing to repair. If you moved it yourself, move it back and check again.".into(),
                            ),
                        )),
                        _ => out.lines.push(Line::ok(name, "Nothing needed repairing.")),
                    }
                }
                out.health = Some(self.health());
            }
            Op::SetGroup(ids, group) => {
                for id in ids {
                    let name = self.name(&id);
                    if let Some(it) = self.items.iter_mut().find(|i| i.id == id) {
                        it.group = group.trim().to_string();
                    }
                    out.lines.push(Line::ok(name, ""));
                }
            }
            Op::Health => out.health = Some(self.health()),
            Op::ImportRecovery => out.health = Some(self.health()),
            Op::ChangePassword { .. } => {}
        }
        out
    }
}

/// An invented wrong-password log for the demo's Settings page.
pub fn attempt_log(now: i64) -> crate::attempts::Log {
    use crate::attempts::{Attempt, Place};
    crate::attempts::Log {
        entries: vec![
            Attempt {
                at: now - 9 * 86_400 - 4_000,
                place: Place::Window,
            },
            Attempt {
                at: now - 2 * 86_400 - 30_500,
                place: Place::StandIn,
            },
            Attempt {
                at: now - 2 * 86_400 - 30_460,
                place: Place::StandIn,
            },
        ],
        ..Default::default()
    }
}

/// Invented paths offered when "Add" is used in the demo (a real file picker
/// would show real names).
pub fn sample_paths() -> Vec<PathBuf> {
    [
        "C:\\Users\\alex\\Documents\\Letters",
        "C:\\Users\\alex\\OneDrive\\Shared plans",
        "D:\\Work\\Payroll 2026.xlsx",
        "C:\\Windows",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

/// What the engine's pre-flight check would say, invented from the path text alone.
pub fn precheck(path: &Path) -> Precheck {
    let text = path.to_string_lossy().to_string();
    let lower = text.to_lowercase();
    let name = file_name(&text);
    let is_file = name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && (1..=5).contains(&ext.len()) && !ext.contains(' ')
    });
    let h = hash(&lower);
    let (size, files) = if is_file {
        (40_000 + h % (6 * MB), 1)
    } else {
        (20 * MB + h % (900 * MB), 12 + h % 700)
    };
    let mut pre = Precheck {
        path: path.to_path_buf(),
        verdict: Verdict::Ok,
        refusal: None,
        warnings: Vec::new(),
        kind: if is_file { Kind::File } else { Kind::Folder },
        size,
        files,
        filesystem: "NTFS".into(),
        acl_supported: true,
        scramble_default: !is_file,
        truncated: false,
    };
    let system = ["c:\\windows", "c:\\program files", "c:\\programdata"];
    if system.iter().any(|s| lower.starts_with(s)) || lower.len() <= 3 {
        pre.verdict = Verdict::Refuse;
        pre.refusal = Some(
            "This is a Windows system folder. Locking it could stop Windows from starting, so VeilForge never touches it.".into(),
        );
        pre.size = 0;
        pre.files = 0;
        return pre;
    }
    if lower.contains("\\onedrive\\") || lower.contains("\\dropbox\\") {
        pre.verdict = Verdict::Warn;
        pre.warnings.push(
            "This folder is synced to the cloud. If you hide it, the sync client would see a delete and may remove it from your other devices. Locking it in place is safer.".into(),
        );
    }
    if lower.starts_with("e:\\") {
        pre.verdict = Verdict::Warn;
        pre.filesystem = "exFAT".into();
        pre.acl_supported = false;
        pre.warnings.push(
            "This drive uses exFAT, which has no permissions. The item can only be hidden, and anyone who finds the holding folder can open it.".into(),
        );
    }
    pre
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ProtectOptions;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn sample_data_is_consistent_and_invented() {
        let v = DemoVault::sample(NOW);
        assert!(v.items.len() >= 10);
        let mut ids: Vec<&str> = v.items.iter().map(|i| i.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), v.items.len(), "ids are unique");
        let real_user = std::env::var("USERNAME").unwrap_or_default().to_lowercase();
        for i in &v.items {
            assert_eq!(i.id.len(), 16);
            assert!(i.id.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(i.original.ends_with(&i.name));
            assert!(i.original.starts_with(&i.volume));
            assert_eq!(i.hold_path.is_some(), i.hides(), "{}", i.name);
            assert_eq!(i.standin.is_some(), i.mode == Mode::Ask, "{}", i.name);
            if let Some(hold) = &i.hold_path {
                assert!(hold.starts_with(&format!("{}VeilForge.sys\\", i.volume)));
                assert!(!hold.contains(&i.name), "the holding name is random");
                assert!(!file_name(hold).contains('.'), "no extension");
            }
            assert_eq!(i.scrambled, !i.children.is_empty());
            assert!(!i.scrambled || (i.kind == Kind::Folder && i.hides()));
            assert!(i.locked_at < NOW);
            assert!(v.states.contains_key(&i.id));
            assert_eq!(i.open_since.is_some(), v.states[&i.id] == State::Open);
            if real_user.len() >= 3 && real_user != "alex" {
                assert!(!i.original.to_lowercase().contains(&real_user));
            }
            if i.kind == Kind::File {
                assert_eq!(i.files, 1);
            }
        }
        // Every state and every mode appears, so each badge is in the screenshots.
        for state in [State::Locked, State::Open, State::Missing, State::Tampered] {
            assert!(v.states.values().any(|s| *s == state), "{state:?}");
        }
        for mode in [Mode::Lock, Mode::Hide, Mode::Ask] {
            assert!(v.items.iter().any(|i| i.mode == mode), "{mode:?}");
        }
        assert_eq!(v.groups(), vec!["Finance", "Personal", "Work"]);
        let health = v.health();
        assert_eq!(health.len(), v.items.len());
        assert!(health
            .iter()
            .all(|h| h.problem.is_empty() == matches!(h.state, State::Locked | State::Open)));
        assert!(health.iter().any(|h| h.repairable));
    }

    #[test]
    fn operations_only_change_memory() {
        let mut v = DemoVault::sample(NOW);
        let before = v.items.len();
        let open_id = v
            .items
            .iter()
            .find(|i| v.states[&i.id] == State::Locked)
            .unwrap()
            .id
            .clone();
        let out = v.run(Op::OpenTemp(vec![open_id.clone()]));
        assert_eq!(out.failures(), 0);
        assert_eq!(v.states[&open_id], State::Open);
        assert!(out.lines[0].path.is_some());
        let out = v.run(Op::RelockAll);
        assert_eq!(out.lines.len(), 2);
        assert!(v.states.values().all(|s| *s != State::Open));
        assert!(v.items.iter().all(|i| i.open_since.is_none()));

        // Protect: one fine, one refused, one already there.
        let options = ProtectOptions::default();
        let out = v.run(Op::Protect(vec![
            (
                PathBuf::from("C:\\Users\\alex\\Documents\\Letters"),
                options.clone(),
            ),
            (PathBuf::from("C:\\Windows"), options.clone()),
            (PathBuf::from("D:\\Projects\\Prototype"), options.clone()),
        ]));
        assert_eq!((out.successes(), out.failures()), (1, 2));
        assert_eq!(v.items.len(), before + 1);
        assert_eq!(v.items[0].name, "Letters");
        assert!(v.items[0].scrambled);

        let tampered = v
            .items
            .iter()
            .find(|i| v.states[&i.id] == State::Tampered)
            .unwrap()
            .id
            .clone();
        let out = v.run(Op::Repair(vec![tampered.clone()]));
        assert_eq!(out.failures(), 0);
        assert_eq!(v.states[&tampered], State::Locked);

        let out = v.run(Op::SetGroup(vec![tampered.clone()], " Bank ".into()));
        assert_eq!(out.failures(), 0);
        assert!(v.groups().contains(&"Bank".to_string()));

        let out = v.run(Op::Release(vec![tampered.clone()]));
        assert_eq!(out.failures(), 0);
        assert!(v.items.iter().all(|i| i.id != tampered));

        // Panic restore releases everything except what is missing.
        let out = v.run(Op::PanicRestore(zeroize::Zeroizing::new("x".into())));
        assert_eq!(out.failures(), 1);
        assert_eq!(v.items.len(), 1);
        assert!(DemoVault::empty(NOW).snapshot().items.is_empty());
    }

    #[test]
    fn invented_precheck_covers_every_verdict() {
        let pre: Vec<Precheck> = sample_paths().iter().map(|p| precheck(p)).collect();
        assert_eq!(pre[0].verdict, Verdict::Ok);
        assert_eq!(pre[0].kind, Kind::Folder);
        assert!(pre[0].scramble_default);
        assert_eq!(pre[1].verdict, Verdict::Warn);
        assert!(pre[1].warnings[0].contains("would see a delete"));
        assert_eq!(pre[2].kind, Kind::File);
        assert_eq!(pre[2].files, 1);
        assert_eq!(pre[3].verdict, Verdict::Refuse);
        assert!(pre[3].refusal.is_some());
        let exfat = precheck(Path::new("E:\\Photos\\Trip"));
        assert!(!exfat.acl_supported);
        assert_eq!(exfat.filesystem, "exFAT");
    }
}

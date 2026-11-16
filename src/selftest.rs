//! `--selftest`: every mode end to end inside one temp folder.
//!
//! Everything it touches is created here: one folder under the system temp
//! folder holding invented files, the map and the holding folder (a sandbox
//! [`Env`], so no `VeilForge.sys` is created at a volume root). It is removed
//! again at the end, also when a step fails.

use crate::cli;
use crate::engine::{self, Env, Error, ProtectOptions, Vault, Verdict};
use crate::model::{Mode, State};
use crate::openflow::{Session, Target};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const PASSWORD: &str = "selftest correct horse 41";

/// Relative path -> bytes of every file below `root` (what can be listed).
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                walk(base, &path, out);
            } else {
                let rel = path
                    .strip_prefix(base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                out.insert(rel, std::fs::read(&path).unwrap_or_default());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Every file and folder name a recursive walk of `root` can see.
pub fn visible_names(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            out.push(entry.file_name().to_string_lossy().to_string());
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                walk(&entry.path(), out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

fn check(ok: bool, what: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(format!("FAILED: {what}"))
    }
}

fn err(e: Error) -> String {
    format!("FAILED: {e}")
}

fn steps(root: &Path, log: &mut Vec<String>) -> Result<(), String> {
    let env = Env::sandbox(root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;

    // Invented content.
    let note = work.join("Quarterly notes.txt");
    std::fs::write(&note, b"alpha beta gamma").map_err(|e| e.to_string())?;
    let tree = work.join("Blueprints");
    std::fs::create_dir_all(tree.join("Inner drafts")).map_err(|e| e.to_string())?;
    std::fs::write(tree.join("Plan one.txt"), b"plan one").map_err(|e| e.to_string())?;
    std::fs::write(tree.join("Inner drafts").join("Sketch.dat"), [7u8; 4096])
        .map_err(|e| e.to_string())?;
    let letter = work.join("Private letter.txt");
    std::fs::write(&letter, b"dear reader").map_err(|e| e.to_string())?;
    let before_tree = snapshot(&tree);

    // 1. Master password.
    check(!Vault::exists(&env), "no map before first run")?;
    let mut vault = Vault::create(env.clone(), PASSWORD).map_err(err)?;
    check(Vault::exists(&env), "map file written")?;
    check(
        matches!(
            Vault::open(env.clone(), "wrong password"),
            Err(Error::WrongPassword)
        ),
        "a wrong master password is rejected",
    )?;
    log.push("ok  master password set; wrong password rejected".into());

    // 2. Refusal rules.
    let windows = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    for refused in [windows.as_str(), r"C:\"] {
        let pre = engine::precheck(&env, Path::new(refused));
        check(
            pre.verdict == Verdict::Refuse,
            "system folders and drive roots are refused",
        )?;
    }
    log.push("ok  the Windows folder and a drive root are refused".into());

    // 3. Lock: stays visible, cannot be read.
    let options = ProtectOptions {
        mode: Mode::Lock,
        ..Default::default()
    };
    let lock_id = vault.protect(&note, &options, &mut |_| {}).map_err(err)?;
    check(note.exists(), "a locked file stays where it is")?;
    check(
        std::fs::read(&note).is_err(),
        "a locked file cannot be read",
    )?;
    check(vault.state(&lock_id) == State::Locked, "state is Locked")?;
    vault.release(&lock_id, &mut |_| {}).map_err(err)?;
    check(
        std::fs::read(&note).ok().as_deref() == Some(b"alpha beta gamma".as_slice()),
        "bytes identical after unlocking",
    )?;
    log.push("ok  lock mode: access denied while locked, identical after restore".into());

    // 4. Hide a folder with scrambled names inside.
    let options = ProtectOptions {
        mode: Mode::Hide,
        scramble: true,
        ..Default::default()
    };
    let hide_id = vault.protect(&tree, &options, &mut |_| {}).map_err(err)?;
    check(!tree.exists(), "a hidden folder is gone from its place")?;
    let names = visible_names(root);
    for secret in ["Blueprints", "Plan one", "Inner drafts", "Sketch"] {
        check(
            !names.iter().any(|n| n.contains(secret)),
            "no original name is found by a recursive walk",
        )?;
    }
    let raw = std::fs::read_dir(&env.data_dir)
        .map(|d| {
            d.flatten()
                .filter_map(|e| std::fs::read(e.path()).ok())
                .any(|bytes| {
                    bytes
                        .windows(b"Blueprints".len())
                        .any(|w| w == b"Blueprints")
                })
        })
        .unwrap_or(false);
    check(
        !raw,
        "the map and journal do not contain the name in the clear",
    )?;
    // Temporarily open, then lock again.
    let opened = vault.open_temporarily(&hide_id, &mut |_| {}).map_err(err)?;
    check(
        opened == tree && snapshot(&tree) == before_tree,
        "temporarily open: all there",
    )?;
    check(
        vault.state(&hide_id) == State::Open,
        "state is Temporarily open",
    )?;
    vault.relock(&hide_id, &mut |_| {}).map_err(err)?;
    check(!tree.exists(), "locked again")?;
    vault.release(&hide_id, &mut |_| {}).map_err(err)?;
    check(
        snapshot(&tree) == before_tree,
        "folder tree identical after restore",
    )?;
    log.push(
        "ok  hide mode with scrambled names: not found by a recursive walk, identical after restore"
            .into(),
    );

    // 5. Ask on open: stand-in shortcut, opened through the command-line flow.
    let options = ProtectOptions {
        mode: Mode::Ask,
        ask_hidden: true,
        ..Default::default()
    };
    let ask_id = vault.protect(&letter, &options, &mut |_| {}).map_err(err)?;
    check(!letter.exists(), "the original is hidden")?;
    let standin = vault
        .item(&ask_id)
        .and_then(|i| i.standin.clone())
        .map(PathBuf::from)
        .ok_or("FAILED: no stand-in recorded")?;
    check(standin.exists(), "the stand-in shortcut exists")?;
    let read = crate::standin::read(&standin).map_err(|e| format!("FAILED: {e}"))?;
    check(
        crate::standin::id_of(&read).as_deref() == Some(ask_id.as_str()),
        "the stand-in runs --open <id>",
    )?;
    drop(vault);
    check(
        matches!(
            Session::begin(env.clone(), "not the password", &Target::Id(ask_id.clone())),
            Err(Error::WrongPassword)
        ),
        "the stand-in flow rejects a wrong password",
    )?;
    let mut session =
        Session::begin(env.clone(), PASSWORD, &Target::Path(standin.clone())).map_err(err)?;
    check(
        std::fs::read(&session.path).ok().as_deref() == Some(b"dear reader".as_slice()),
        "after the password the item is back and readable",
    )?;
    session.lock_again().map_err(err)?;
    check(!letter.exists(), "it is locked again afterwards")?;
    drop(session);
    log.push("ok  ask-on-open: stand-in -> password -> open -> locked again".into());

    // 6. Lost app data: rebuild from the recovery copies, then panic restore.
    std::fs::remove_dir_all(&env.data_dir).map_err(|e| e.to_string())?;
    let (vault, imported) = engine::rebuild_from_recovery(env.clone(), PASSWORD).map_err(err)?;
    check(imported == 1, "the recovery copy was found and opened")?;
    drop(vault);
    let report = cli::restore_all(env.clone(), PASSWORD).map_err(err)?;
    check(
        report.failed.is_empty() && report.restored.len() == 1,
        "panic restore put everything back",
    )?;
    check(
        std::fs::read(&letter).ok().as_deref() == Some(b"dear reader".as_slice()),
        "the letter is back with its bytes",
    )?;
    check(!standin.exists(), "the stand-in is gone after restoring")?;
    let vault = Vault::open(env, PASSWORD).map_err(err)?;
    check(vault.items().is_empty(), "nothing is protected any more")?;
    log.push("ok  lost app data: rebuilt from recovery copies; panic restore restored all".into());
    Ok(())
}

/// Runs the self-test. Returns the log lines, or the first failure.
pub fn run() -> Result<Vec<String>, String> {
    let root = crate::util::unique_temp_dir("selftest");
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let mut log = vec![format!("sandbox: a new folder in the temp folder ({})", {
        root.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    })];
    let result = steps(&root, &mut log);
    if result.is_err() {
        // Put back whatever a failed step left protected, so the folder can go.
        let _ = cli::restore_all(Env::sandbox(&root), PASSWORD);
    }
    let removed = std::fs::remove_dir_all(&root);
    result?;
    if removed.is_err() || root.exists() {
        return Err(format!(
            "FAILED: the sandbox folder could not be removed: {}",
            root.display()
        ));
    }
    log.push("ok  sandbox removed; nothing left behind".into());
    Ok(log)
}

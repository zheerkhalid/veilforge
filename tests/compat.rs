//! Backward compatibility: what VeilForge 1.0.0 wrote must open unchanged.
//!
//! `tests/fixtures/v1.0.0/` holds a map file and a settings file written by
//! the 1.0.0 code (five items: every mode, a scrambled folder, a Kurdish
//! name, one temporarily open). The items themselves lived in a folder that
//! no longer exists, so these tests only read.

use std::path::{Path, PathBuf};
use veilforge::engine::cleanup::SandboxGuard;
use veilforge::engine::{Env, Error, Vault};
use veilforge::model::{Kind, Mode};
use veilforge::settings::Settings;

const PW: &str = "compat-amber-lantern-41";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v1.0.0")
}

#[test]
fn a_map_written_by_1_0_0_opens_unchanged() {
    let sandbox = SandboxGuard::new("compat");
    let env = Env::sandbox(sandbox.path());
    std::fs::create_dir_all(&env.data_dir).unwrap();
    let written = std::fs::read(fixtures().join("map.vfm")).unwrap();
    std::fs::write(env.data_dir.join("map.vfm"), &written).unwrap();

    assert!(Vault::exists(&env));
    assert_eq!(
        Vault::open(env.clone(), "not the password").err(),
        Some(Error::WrongPassword)
    );
    let vault = Vault::open(env.clone(), PW).unwrap();
    assert!(!vault.opened_from_backup());
    assert!(vault.open_notes().is_empty(), "{:?}", vault.open_notes());
    assert_eq!(
        vault.groups(),
        vec!["Family".to_string(), "Work".to_string()]
    );

    let item = |name: &str| {
        vault
            .items()
            .iter()
            .find(|i| i.name == name)
            .unwrap_or_else(|| panic!("{name} is in the map"))
    };
    assert_eq!(vault.items().len(), 5);

    let locked = item("Locked letter.txt");
    assert_eq!((locked.mode, locked.kind), (Mode::Lock, Kind::File));
    assert_eq!(locked.group, "Work");
    assert!(locked.hold_path.is_none() && locked.standin.is_none());
    assert!(!locked.saved.sddl.is_empty(), "the permissions to put back");

    let hidden = item("Family papers");
    assert_eq!((hidden.mode, hidden.kind), (Mode::Hide, Kind::Folder));
    assert_eq!((hidden.files, hidden.group.as_str()), (2, "Family"));
    assert!(hidden.scrambled && hidden.children.len() >= 3);
    assert!(hidden.hold_path.is_some());

    let ask = item("Diary.txt");
    assert_eq!(ask.mode, Mode::Ask);
    assert!(ask.ask_hidden && ask.hides());
    assert!(ask.standin.as_deref().unwrap().ends_with("Diary.txt.lnk"));

    let kurdish = item("بەڵگەنامەکان ڕاپۆرت.txt");
    assert_eq!(kurdish.mode, Mode::Hide);
    assert!(kurdish
        .original
        .ends_with("\\work\\بەڵگەنامەکان ڕاپۆرت.txt"));

    let open = item("Open now.txt");
    assert!(open.open_since.is_some());

    for i in vault.items() {
        assert_eq!(i.id.len(), 16);
        assert!(i.extra.recovery.is_some(), "{}", i.name);
        assert!(i.locked_at > 1_700_000_000);
    }

    // Opening changes nothing on disk: the file is byte for byte the same,
    // and nothing else appeared next to it but the lock file.
    drop(vault);
    assert_eq!(
        std::fs::read(env.data_dir.join("map.vfm")).unwrap(),
        written
    );
    let mut names: Vec<String> = std::fs::read_dir(&env.data_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n != "map.lock")
        .collect();
    names.sort();
    assert_eq!(names, vec!["map.vfm".to_string()]);
}

#[test]
fn settings_written_by_1_0_0_load_with_the_new_options_at_their_defaults() {
    let dir = veilforge::util::unique_temp_dir("compatsettings");
    std::fs::copy(fixtures().join("settings.json"), dir.join("settings.json")).unwrap();
    let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();
    assert!(!text.contains("lock_hotkey") && !text.contains("lock_on_close"));
    let loaded = Settings::load(&dir);
    assert_eq!(loaded, Settings::default());
    assert!(loaded.lock_hotkey.is_empty());
    assert!(loaded.lock_on_close);
    // Saved again, an older VeilForge still reads it (it ignores what it does not know).
    loaded.save(&dir).unwrap();
    assert_eq!(Settings::load(&dir), loaded);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// By hand, once per release: a folder in the temp folder that VeilForge
/// 1.0.0 left *protected* (real files, locked and hidden by the old version)
/// is taken over by this version: checked, opened, locked and restored.
///
///   VF_COMPAT_DIR=%TEMP%\veilforge-compat-100 cargo test --test compat -- --ignored
#[test]
#[ignore]
fn a_live_1_0_0_state_is_taken_over_and_restored() {
    use veilforge::model::State;
    let Some(dir) = std::env::var_os("VF_COMPAT_DIR").map(PathBuf::from) else {
        return;
    };
    assert!(
        dir.starts_with(std::env::temp_dir()),
        "only inside the temp folder"
    );
    let env = Env::sandbox(&dir);
    let work = dir.join("work");
    let mut vault = Vault::open(env.clone(), PW).unwrap();
    assert!(vault.open_notes().is_empty(), "{:?}", vault.open_notes());
    assert_eq!(vault.items().len(), 5);
    let id = |vault: &Vault, name: &str| {
        vault
            .items()
            .iter()
            .find(|i| i.name == name)
            .unwrap()
            .id
            .clone()
    };
    let open = id(&vault, "Open now.txt");
    for i in vault.items() {
        let wanted = if i.id == open {
            State::Open
        } else {
            State::Locked
        };
        assert_eq!(vault.state(&i.id), wanted, "{}", i.name);
    }
    let health = vault.health();
    assert!(
        health.iter().all(|h| h.problem.is_empty()),
        "the health check is clean: {health:?}"
    );
    // Still closed the way 1.0.0 left them.
    assert!(std::fs::read(work.join("Locked letter.txt")).is_err());
    assert!(!work.join("Family papers").exists());

    // Lock the item 1.0.0 left open; open and lock the Kurdish one.
    vault.relock(&open, &mut |_| {}).unwrap();
    assert!(!work.join("Open now.txt").exists());
    let kurdish = id(&vault, "بەڵگەنامەکان ڕاپۆرت.txt");
    let path = vault.open_temporarily(&kurdish, &mut |_| {}).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), "ناوەڕۆک".as_bytes());
    vault.relock(&kurdish, &mut |_| {}).unwrap();
    assert!(!path.exists());
    drop(vault);

    // Everything back, exactly.
    let report = veilforge::cli::restore_all(env.clone(), PW).unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.restored.len(), 5);
    assert_eq!(
        std::fs::read(work.join("Locked letter.txt")).unwrap(),
        b"locked letter"
    );
    assert_eq!(
        std::fs::read(work.join("Family papers/report.txt")).unwrap(),
        b"report"
    );
    assert_eq!(
        std::fs::read(work.join("Family papers/inner/notes.md")).unwrap(),
        b"# notes"
    );
    assert_eq!(
        std::fs::read(work.join("Diary.txt")).unwrap(),
        b"dear diary"
    );
    assert_eq!(
        std::fs::read(work.join("Open now.txt")).unwrap(),
        b"open now"
    );
    assert_eq!(std::fs::read(&path).unwrap(), "ناوەڕۆک".as_bytes());
    assert!(!work.join("Diary.txt.lnk").exists(), "the stand-in is gone");
    let mut names: Vec<String> = std::fs::read_dir(&work)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(names.len(), 5, "{names:?}");
    assert!(Vault::open(env, PW).unwrap().items().is_empty());
    println!("COMPAT_RESTORED={:?}", report.restored);
    assert!(veilforge::engine::cleanup::force_remove(&dir));
}

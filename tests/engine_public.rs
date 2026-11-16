//! The engine through its public contract only, as the window and the command
//! line use it. Everything happens in a `veilforge-*` sandbox in the temp folder.

use std::path::Path;
use veilforge::engine::cleanup::SandboxGuard;
use veilforge::engine::{
    self, Env, Error, ProtectOptions, Vault, Verdict, JOURNAL_STEPS, LARGE_TREE_FILES,
    SCRAMBLE_DEFAULT_MAX_FILES,
};
use veilforge::model::{Kind, Mode, State};

const PW: &str = "a long master password";

fn tree(root: &Path) {
    std::fs::create_dir_all(root.join("inner/deeper")).unwrap();
    std::fs::write(root.join("report.txt"), b"report").unwrap();
    std::fs::write(root.join("inner/photo.jpg"), [0xFFu8, 0xD8, 0xFF, 0xE0]).unwrap();
    std::fs::write(root.join("inner/deeper/notes.md"), b"# notes").unwrap();
}

fn check_tree(root: &Path) {
    assert_eq!(std::fs::read(root.join("report.txt")).unwrap(), b"report");
    assert_eq!(
        std::fs::read(root.join("inner/photo.jpg")).unwrap(),
        [0xFFu8, 0xD8, 0xFF, 0xE0]
    );
    assert_eq!(
        std::fs::read(root.join("inner/deeper/notes.md")).unwrap(),
        b"# notes"
    );
}

#[test]
fn every_mode_through_the_public_api() {
    let sandbox = SandboxGuard::new("public");
    let env = Env::sandbox(sandbox.path());
    assert!(env.sandbox && env.hold_root.is_some());
    assert!(!Vault::exists(&env));
    assert_eq!(Vault::open(env.clone(), PW).err(), Some(Error::NoVault));
    let mut vault = Vault::create(env.clone(), PW).unwrap();
    assert!(vault.items().is_empty() && vault.groups().is_empty());

    let folder = sandbox.path().join("Family papers");
    tree(&folder);
    let pre = engine::precheck(&env, &folder);
    assert_eq!(pre.verdict, Verdict::Ok);
    assert_eq!((pre.kind, pre.files), (Kind::Folder, 3));
    assert!(pre.scramble_default);
    const { assert!(SCRAMBLE_DEFAULT_MAX_FILES < LARGE_TREE_FILES) };

    let modes = [
        (Mode::Lock, true),
        (Mode::Hide, true),
        (Mode::Ask, true),
        (Mode::Ask, false),
    ];
    for (mode, ask_hidden) in modes {
        let options = ProtectOptions {
            mode,
            ask_hidden,
            group: "Home".into(),
            ..Default::default()
        };
        let mut steps = 0;
        let id = vault
            .protect(&folder, &options, &mut |_| steps += 1)
            .unwrap();
        assert!(steps > 0);
        assert_eq!(id.len(), 16);
        assert!(id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        let item = vault.item(&id).unwrap().clone();
        assert_eq!(item.name, "Family papers");
        assert_eq!(item.mode, mode);
        assert_eq!(vault.state(&id), State::Locked);
        assert_eq!(vault.groups(), vec!["Home".to_string()]);
        // Closed: either gone from its place or impossible to list.
        assert!(std::fs::read_dir(&folder).is_err());
        assert_eq!(vault.precheck(&folder).verdict, Verdict::Refuse);
        assert_eq!(item.standin.is_some(), mode == Mode::Ask);
        if let Some(lnk) = &item.standin {
            let read = veilforge::standin::read(Path::new(lnk)).unwrap();
            assert_eq!(veilforge::standin::id_of(&read), Some(id.clone()));
        }

        let opened = vault.open_temporarily(&id, &mut |_| {}).unwrap();
        assert_eq!(opened, folder);
        assert_eq!(vault.state(&id), State::Open);
        check_tree(&folder);
        vault.relock(&id, &mut |_| {}).unwrap();
        assert_eq!(vault.state(&id), State::Locked);
        assert!(vault.health().iter().all(|h| h.problem.is_empty()));

        // A second handle (the `--open` helper) sees and releases it.
        let mut helper = Vault::open(env.clone(), PW).unwrap();
        assert_eq!(helper.items().len(), 1);
        let restored = helper.release(&id, &mut |_| {}).unwrap();
        assert_eq!(restored, folder);
        check_tree(&folder);
        assert!(vault.refresh().unwrap());
        assert!(vault.items().is_empty());
    }
    assert_eq!(vault.remove_empty_holds(), 1);
    assert!(JOURNAL_STEPS.contains(&"protect.save"));
    assert_eq!(Vault::open(env, "wrong").err(), Some(Error::WrongPassword));
}

#[test]
fn a_crash_in_the_middle_is_repaired_on_the_next_open() {
    let sandbox = SandboxGuard::new("publiccrash");
    let env = Env::sandbox(sandbox.path());
    let folder = sandbox.path().join("Half done");
    tree(&folder);
    drop(Vault::create(env.clone(), PW).unwrap());
    let mut dying = Vault::open(
        Env {
            crash_after: Some("protect.scramble-part".into()),
            ..env.clone()
        },
        PW,
    )
    .unwrap();
    let err = dying
        .protect(&folder, &ProtectOptions::default(), &mut |_| {})
        .unwrap_err();
    assert!(matches!(err, Error::Io(ref t) if t.contains("simulated crash")));
    drop(dying);
    assert!(!folder.exists(), "the crash left it half hidden");

    let vault = Vault::open(env, PW).unwrap();
    assert!(vault.items().is_empty());
    check_tree(&folder);
}

/// A release that cannot be completed (something else took the original name)
/// must leave the item protected, in the list, with its stand-in, and must not
/// be "finished" behind the user's back by a later operation.
#[test]
fn a_refused_unlock_keeps_the_item_protected_and_listed() {
    let sandbox = SandboxGuard::new("publicrefused");
    let env = Env::sandbox(sandbox.path());
    let letter = sandbox.path().join("Letter.txt");
    let other = sandbox.path().join("Other.txt");
    std::fs::write(&letter, b"letter").unwrap();
    std::fs::write(&other, b"other").unwrap();
    let mut vault = Vault::create(env.clone(), PW).unwrap();
    let ask = ProtectOptions {
        mode: Mode::Ask,
        ask_hidden: true,
        ..Default::default()
    };
    let id = vault.protect(&letter, &ask, &mut |_| {}).unwrap();
    let standin = veilforge::standin::standin_path(&letter);
    assert!(standin.exists() && !letter.exists());

    // Another file appears under the original name.
    std::fs::write(&letter, b"squatter").unwrap();
    let err = vault.release(&id, &mut |_| {}).unwrap_err();
    assert!(matches!(err, Error::Refused(_)), "{err:?}");
    assert_eq!(std::fs::read(&letter).unwrap(), b"squatter");
    assert!(vault.item(&id).is_some(), "still in the list");
    assert!(standin.exists(), "the stand-in is back");

    // Other work goes on normally and does not touch the refused item.
    let other_id = vault
        .protect(&other, &ProtectOptions::default(), &mut |_| {})
        .unwrap();
    std::fs::remove_file(&letter).unwrap();
    vault.release(&other_id, &mut |_| {}).unwrap();
    assert!(vault.item(&id).is_some(), "nothing finished it on the side");
    assert_eq!(vault.state(&id), State::Locked);
    assert!(!letter.exists());

    // A fresh handle sees the same, and now the release works.
    drop(vault);
    let mut vault = Vault::open(env, PW).unwrap();
    assert!(vault.open_notes().is_empty(), "{:?}", vault.open_notes());
    vault.release(&id, &mut |_| {}).unwrap();
    assert_eq!(std::fs::read(&letter).unwrap(), b"letter");
    assert!(!standin.exists());
    assert_eq!(std::fs::read(&other).unwrap(), b"other");
}

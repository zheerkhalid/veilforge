//! Helpers for a manual end-to-end run of the built program's `--open` flow.
//! Both tests are ignored by default and do nothing without `VF_E2E_DIR`
//! (a folder inside the temp folder that the caller created).
//!
//!   VF_E2E_DIR=<dir> VF_E2E_EXE=<VeilForge.exe> cargo test --test e2e_helper -- --ignored e2e_setup
//!   (run `<exe> --open <id>` with VEILFORGE_DATA=<dir>\data and VEILFORGE_HOLD=<dir>\hold)
//!   VF_E2E_DIR=<dir> cargo test --test e2e_helper -- --ignored e2e_teardown

use std::path::PathBuf;
use veilforge::engine::{Env, ProtectOptions, Vault};
use veilforge::model::{KdfParams, Mode, State};

const PW: &str = "e2e-violet-harbor-27";

fn env() -> Option<(PathBuf, Env)> {
    let dir = PathBuf::from(std::env::var_os("VF_E2E_DIR")?);
    assert!(
        dir.starts_with(std::env::temp_dir()),
        "only inside the temp folder"
    );
    let env = Env {
        data_dir: dir.join("data"),
        hold_root: Some(dir.join("hold")),
        kdf: KdfParams::TEST,
        exe: std::env::var_os("VF_E2E_EXE")
            .map(PathBuf::from)
            .unwrap_or_default(),
        crash_after: None,
        sandbox: true,
    };
    Some((dir, env))
}

#[test]
#[ignore]
fn e2e_setup() {
    let Some((dir, env)) = env() else { return };
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    // "Opening" this item runs a tiny script: it leaves a marker and stays
    // alive for a few seconds, like a program showing a document would.
    let item = work.join("Open me.cmd");
    let marker = dir.join("opened.marker");
    std::fs::write(
        &item,
        format!(
            "@echo off\r\necho opened> \"{}\"\r\nping -n 7 127.0.0.1 >nul\r\n",
            marker.display()
        ),
    )
    .unwrap();
    let mut vault = Vault::create(env, PW).unwrap();
    let options = ProtectOptions {
        mode: Mode::Ask,
        ask_hidden: true,
        ..Default::default()
    };
    let id = vault.protect(&item, &options, &mut |_| {}).unwrap();
    assert!(!item.exists());
    println!("E2E_ID={id}");
}

#[test]
#[ignore]
fn e2e_state() {
    let Some((_, env)) = env() else { return };
    let vault = Vault::open(env, PW).unwrap();
    for item in vault.items() {
        println!(
            "E2E_STATE={:?} open_since={:?}",
            vault.state(&item.id),
            item.open_since
        );
        assert_eq!(vault.state(&item.id), State::Locked);
    }
}

#[test]
#[ignore]
fn e2e_teardown() {
    let Some((dir, env)) = env() else { return };
    let report = veilforge::cli::restore_all(env, PW).unwrap();
    println!(
        "E2E_RESTORED={:?} FAILED={:?}",
        report.restored, report.failed
    );
    assert!(report.failed.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

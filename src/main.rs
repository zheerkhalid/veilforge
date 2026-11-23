#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use std::path::PathBuf;
use veilforge::openflow::{self, Target};
use veilforge::ui::{self, Options};

/// A windowed program has no console; borrow the one it was started from so
/// that command-line output is visible.
fn attach_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: plain Win32 call; failure only means there is no parent console.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    if has("--help") || has("-h") || has("/?") || has("--selftest") {
        attach_console();
    }
    if has("--help") || has("-h") || has("/?") {
        println!("{}", veilforge::cli::USAGE);
        return Ok(());
    }
    if has("--selftest") {
        match veilforge::selftest::run() {
            Ok(log) => {
                for line in log {
                    println!("{line}");
                }
                println!("SELFTEST PASSED");
                std::process::exit(0);
            }
            Err(e) => {
                println!("{e}");
                println!("SELFTEST FAILED");
                std::process::exit(1);
            }
        }
    }
    if has("--lock-now") {
        // Nothing is locked from here: the running VeilForge does it.
        let told = veilforge::sys::broadcast_lock_now(true);
        std::process::exit(if told > 0 { 0 } else { 1 });
    }
    if has("--restore-all") {
        let code = veilforge::cli::restore_all_interactive(window_icon(), has("--uninstall"));
        std::process::exit(code);
    }
    if let Some(id) = value("--open") {
        std::process::exit(openflow::run(Target::Id(id), window_icon()));
    }
    if let Some(path) = value("--unlock") {
        std::process::exit(openflow::run(
            Target::Path(PathBuf::from(path)),
            window_icon(),
        ));
    }

    // Everything after --lock that is not another flag is a path to protect.
    let add: Vec<PathBuf> = args
        .iter()
        .position(|a| a == "--lock")
        .map(|i| {
            args[i + 1..]
                .iter()
                .take_while(|a| !a.starts_with("--"))
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default();

    let options = Options {
        demo: has("--demo"),
        screenshot: value("--screenshot").map(PathBuf::from),
        view: value("--view").or_else(|| std::env::var("VEILFORGE_VIEW").ok()),
        light: has("--light"),
        start_hidden: has("--hidden") || has(veilforge::sys::STARTUP_ARG),
        add,
        ..Default::default()
    };
    ui::run(options, window_icon())
}

fn window_icon() -> egui::IconData {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/icon-256.png"));
    match image::load_from_memory(bytes) {
        Ok(img) => {
            let rgba = img.into_rgba8();
            let (width, height) = rgba.dimensions();
            egui::IconData {
                rgba: rgba.into_raw(),
                width,
                height,
            }
        }
        Err(_) => egui::IconData::default(),
    }
}

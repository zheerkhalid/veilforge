//! Command-line entry points other than the main window.

use crate::engine::{Env, Error, Vault};
use crate::settings::Settings;
use crate::sys;
use crate::ui::prompt::{self, PromptSpec};
use eframe::egui;
use std::sync::{Arc, Mutex};

pub const USAGE: &str = "VeilForge

  VeilForge.exe                      open the program
  VeilForge.exe --lock <path>...     open the program and offer to protect these files or folders
  VeilForge.exe --open <id>          what a stand-in shortcut runs: ask for the master password,
                                     open the item, lock it again when it is closed
  VeilForge.exe --unlock <path>      the same for a stand-in shortcut or a locked item's path
  VeilForge.exe --restore-all        panic restore: ask for the master password and put every
        [--uninstall]                protected item back exactly as it was
                                     exit code 0 = nothing is protected any more,
                                     1 = cancelled, 2 = some items could not be restored
  VeilForge.exe --tray               start in the tray (what the start-with-Windows entry runs)
  VeilForge.exe --demo               show invented data; nothing on this PC is read or changed
  VeilForge.exe --screenshot <png>   render the window (1280x800) to a PNG and exit
        [--view welcome|unlock|main|details|selection|protect|prompt|health|panic|
                settings|about|empty] [--light]
  VeilForge.exe --selftest           protect, verify and restore invented files inside one temp
                                     folder with every mode; nothing else is touched";

/// What a restore of everything did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreReport {
    pub restored: Vec<String>,
    /// (name, plain reason) for every item that is still protected.
    pub failed: Vec<(String, String)>,
}

impl RestoreReport {
    pub fn summary(&self) -> String {
        let mut text = match (self.restored.len(), self.failed.len()) {
            (0, 0) => "Nothing was protected. There is nothing to restore.".to_string(),
            (n, 0) => format!(
                "{n} item{} put back exactly where {} was. Nothing is protected any more.",
                if n == 1 { " was" } else { "s were" },
                if n == 1 { "it" } else { "each" }
            ),
            (n, f) => format!(
                "{n} item{} restored, {f} could not be restored and {} still protected:",
                if n == 1 { " was" } else { "s were" },
                if f == 1 { "is" } else { "are" }
            ),
        };
        for (name, why) in self.failed.iter().take(12) {
            text.push_str(&format!("\n\n{name}\n{why}"));
        }
        if self.failed.len() > 12 {
            text.push_str(&format!("\n\n... and {} more.", self.failed.len() - 12));
        }
        text
    }
}

/// Panic restore without a window: opens the map and releases every item.
/// Items that were temporarily open are simply forgotten in place (they are
/// already where they belong). Used by `--restore-all`, the tests and `--selftest`.
pub fn restore_all(env: Env, password: &str) -> Result<RestoreReport, Error> {
    let mut vault = Vault::open(env, password)?;
    // Finish anything a crash left half done before restoring.
    let _ = vault.recover();
    let names: std::collections::HashMap<String, String> = vault
        .items()
        .iter()
        .map(|i| (i.id.clone(), i.name.clone()))
        .collect();
    let mut report = RestoreReport::default();
    for (id, result) in vault.release_all(&mut |_| {}) {
        let name = names.get(&id).cloned().unwrap_or_else(|| id.clone());
        match result {
            Ok(_) => report.restored.push(name),
            Err(e) => report.failed.push((name, e.to_string())),
        }
    }
    if report.failed.is_empty() {
        vault.remove_empty_holds();
    }
    Ok(report)
}

/// `--restore-all`: the same behind the small password window.
pub fn restore_all_interactive(icon: egui::IconData, uninstall: bool) -> i32 {
    let env = Env::real();
    let settings = Settings::load(&env.data_dir);
    let title = settings.title();
    if !Vault::exists(&env) {
        if !uninstall {
            sys::message_box(
                title,
                "No master password has been set on this PC, so nothing is protected here.",
            );
        }
        return 0;
    }
    let slot: Arc<Mutex<Option<RestoreReport>>> = Arc::new(Mutex::new(None));
    let spec = PromptSpec {
        title: title.to_string(),
        heading: "Restore everything".into(),
        text: if uninstall {
            "Before VeilForge is removed, every protected file and folder is put back \
             exactly as it was. Enter your master password to do that now."
                .into()
        } else {
            "Every protected file and folder is put back exactly as it was: place, name, \
             permissions and dates. Enter your master password to continue."
                .into()
        },
        button: "Restore all".into(),
    };
    let submit_slot = slot.clone();
    let ok = prompt::run(
        spec,
        icon,
        Box::new(move |password| {
            let report = restore_all(env.clone(), password).map_err(|e| e.to_string())?;
            if let Ok(mut g) = submit_slot.lock() {
                *g = Some(report);
            }
            Ok(String::new())
        }),
    );
    let report = slot.lock().ok().and_then(|mut g| g.take());
    match report {
        Some(report) if ok => {
            sys::message_box(title, &report.summary());
            if report.failed.is_empty() {
                0
            } else {
                2
            }
        }
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_read_naturally() {
        let mut r = RestoreReport::default();
        assert!(r.summary().starts_with("Nothing was protected"));
        r.restored.push("Notes.txt".into());
        assert!(r.summary().starts_with("1 item was put back"));
        r.restored.push("Photos".into());
        assert!(r.summary().starts_with("2 items were put back"));
        r.failed
            .push(("Plan.docx".into(), "It is open in a program.".into()));
        let s = r.summary();
        assert!(s.contains("1 could not be restored and is still protected"));
        assert!(s.contains("Plan.docx") && s.contains("open in a program"));
    }
}

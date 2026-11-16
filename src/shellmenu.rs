//! Explorer right-click entries under HKCU (no administrator rights needed).

use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
use winreg::RegKey;

/// Registry root used: `HKCU\Software\Classes`. Tests pass another root
/// (a throwaway key) so that the real one is never touched.
pub const CLASSES_ROOT: &str = "Software\\Classes";

/// (class, verb key, menu text, command line switch)
const ENTRIES: [(&str, &str, &str, &str); 4] = [
    ("*", "VeilForge.Lock", "Lock with VeilForge", "--lock"),
    (
        "Directory",
        "VeilForge.Lock",
        "Lock with VeilForge",
        "--lock",
    ),
    ("*", "VeilForge.Unlock", "Unlock with VeilForge", "--unlock"),
    (
        "Directory",
        "VeilForge.Unlock",
        "Unlock with VeilForge",
        "--unlock",
    ),
];

fn key_path(root: &str, class: &str, verb: &str) -> String {
    format!("{}\\{class}\\shell\\{verb}", root.trim_end_matches('\\'))
}

/// Adds "Lock with VeilForge" / "Unlock with VeilForge" for files (`*`) and
/// folders (`Directory`) below `root`, running `exe --lock "%1"` / `exe --unlock "%1"`.
pub fn install(root: &str, exe: &std::path::Path) -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let exe = exe.to_string_lossy();
    for (class, verb, text, switch) in ENTRIES {
        let path = key_path(root, class, verb);
        let (key, _) = hkcu.create_subkey(&path).map_err(|e| e.to_string())?;
        key.set_value("", &text).map_err(|e| e.to_string())?;
        key.set_value("Icon", &format!("\"{exe}\",0"))
            .map_err(|e| e.to_string())?;
        let (command, _) = key.create_subkey("command").map_err(|e| e.to_string())?;
        command
            .set_value("", &format!("\"{exe}\" {switch} \"%1\""))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Removes every key [`install`] created (and nothing else).
pub fn uninstall(root: &str) -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let mut first_error = None;
    for (class, verb, _, _) in ENTRIES {
        match hkcu.delete_subkey_all(key_path(root, class, verb)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                first_error.get_or_insert(e.to_string());
            }
        }
    }
    match first_error {
        None => Ok(()),
        Some(e) => Err(e),
    }
}

/// True when all four entries are present.
pub fn is_installed(root: &str) -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    ENTRIES.iter().all(|(class, verb, _, _)| {
        hkcu.open_subkey_with_flags(
            format!("{}\\command", key_path(root, class, verb)),
            KEY_READ,
        )
        .is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deletes the throwaway registry key even when the test fails.
    struct TestRoot(String);
    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&self.0);
        }
    }

    #[test]
    fn install_and_uninstall_under_a_throwaway_root() {
        let top = TestRoot(format!("Software\\VeilForgeTest-{}", std::process::id()));
        let root = format!("{}\\Classes", top.0);
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        assert!(!is_installed(&root));
        // A neighbour that must survive the uninstall.
        let (other, _) = hkcu
            .create_subkey(format!("{root}\\*\\shell\\SomethingElse"))
            .unwrap();
        other.set_value("", &"Other entry").unwrap();

        let exe = std::path::Path::new(r"C:\Program Files\VeilForge\VeilForge.exe");
        install(&root, exe).unwrap();
        assert!(is_installed(&root));
        for (class, verb, text, switch) in ENTRIES {
            let key = hkcu.open_subkey(key_path(&root, class, verb)).unwrap();
            let shown: String = key.get_value("").unwrap();
            assert_eq!(shown, text);
            let icon: String = key.get_value("Icon").unwrap();
            assert_eq!(icon, "\"C:\\Program Files\\VeilForge\\VeilForge.exe\",0");
            let command: String = key.open_subkey("command").unwrap().get_value("").unwrap();
            assert_eq!(
                command,
                format!("\"C:\\Program Files\\VeilForge\\VeilForge.exe\" {switch} \"%1\"")
            );
        }
        // Installing twice is harmless.
        install(&root, exe).unwrap();

        uninstall(&root).unwrap();
        assert!(!is_installed(&root));
        for (class, verb, _, _) in ENTRIES {
            assert!(hkcu.open_subkey(key_path(&root, class, verb)).is_err());
        }
        assert!(hkcu
            .open_subkey(format!("{root}\\*\\shell\\SomethingElse"))
            .is_ok());
        uninstall(&root).unwrap();

        drop(top);
        assert!(hkcu
            .open_subkey(format!("Software\\VeilForgeTest-{}", std::process::id()))
            .is_err());
    }
}

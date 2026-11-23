//! User settings (JSON, never secrets).

use crate::model::Mode;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeChoice {
    Dark,
    Light,
    System,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    /// Lock the app after this many idle minutes (0 = never).
    pub auto_lock_minutes: u32,
    /// Lock temporarily opened items again after this many idle minutes (0 = never).
    pub relock_idle_minutes: u32,
    /// Lock everything again when Windows locks, sleeps or the user signs out.
    pub relock_on_session_lock: bool,
    /// Show "Files" instead of "VeilForge" in window titles.
    pub neutral_title: bool,
    pub tray_icon: bool,
    /// Global hotkey that opens the window, for example "Ctrl+Alt+V" (empty = none).
    pub hotkey: String,
    /// Global hotkey that locks everything at once: temporarily open items
    /// and VeilForge itself (empty = none).
    pub lock_hotkey: String,
    /// Explorer right-click entries are installed.
    pub context_menu: bool,
    /// The "start with Windows" entry was switched on (the registry is the truth;
    /// this only remembers the choice for display).
    pub start_with_windows: bool,
    /// The mode the "Protect" dialog starts with.
    pub default_mode: Mode,
    /// "Ask for password on open": hide the item underneath (true) or lock it in place.
    pub ask_default_hidden: bool,
    /// Ask before unlocking an item permanently.
    pub confirm_unlock: bool,
    /// Show the item in File Explorer after unlocking it temporarily.
    pub open_after_unlock: bool,
    /// Closing the window keeps VeilForge running (tray icon or hotkey).
    pub close_to_background: bool,
    /// Closing the window to the tray also locks VeilForge, so that opening
    /// it again asks for the master password.
    pub lock_on_close: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeChoice::System,
            auto_lock_minutes: 5,
            relock_idle_minutes: 15,
            relock_on_session_lock: true,
            neutral_title: false,
            tray_icon: true,
            hotkey: String::new(),
            lock_hotkey: String::new(),
            context_menu: false,
            start_with_windows: false,
            default_mode: Mode::Hide,
            ask_default_hidden: true,
            confirm_unlock: true,
            open_after_unlock: true,
            close_to_background: true,
            lock_on_close: true,
        }
    }
}

/// The longest idle time that can be configured, in minutes (one day).
pub const MAX_MINUTES: u32 = 24 * 60;

impl Settings {
    /// Brings out-of-range values (a hand-edited file) back into range.
    pub fn sanitize(&mut self) {
        self.auto_lock_minutes = self.auto_lock_minutes.min(MAX_MINUTES);
        self.relock_idle_minutes = self.relock_idle_minutes.min(MAX_MINUTES);
        if self.hotkey.chars().count() > 40 {
            self.hotkey.clear();
        }
        if self.lock_hotkey.chars().count() > 40 {
            self.lock_hotkey.clear();
        }
        // One key combination cannot do two things: opening wins.
        let (open, lock) = (
            crate::sys::parse_hotkey(&self.hotkey),
            crate::sys::parse_hotkey(&self.lock_hotkey),
        );
        if open.is_some() && open == lock {
            self.lock_hotkey.clear();
        }
    }

    /// The title windows show: "Files" when the neutral title is on.
    pub fn title(&self) -> &'static str {
        if self.neutral_title {
            "Files"
        } else {
            "VeilForge"
        }
    }

    /// Whether closing the window should only hide it: there must be a way
    /// back (the tray icon or a working hotkey).
    pub fn stays_in_background(&self) -> bool {
        self.close_to_background
            && (self.tray_icon || crate::sys::parse_hotkey(&self.hotkey).is_some())
    }

    pub fn load(dir: &Path) -> Settings {
        let mut s: Settings = std::fs::read(dir.join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        s.sanitize();
        s
    }

    /// Writes the file atomically: a temporary file in the same folder is
    /// renamed over the old one, so a crash never leaves half a file.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let problem = |e: std::io::Error| format!("Could not save the settings: {e}");
        std::fs::create_dir_all(dir).map_err(problem)?;
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let target = dir.join("settings.json");
        let temp = dir.join(format!("settings.json.{}.tmp", std::process::id()));
        std::fs::write(&temp, json).map_err(problem)?;
        std::fs::rename(&temp, &target).map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            problem(e)
        })
    }
}

/// Where the map, settings and journal live: next to the EXE in portable mode
/// (a `portable.flag` file beside it), otherwise `%APPDATA%\VeilForge`.
/// The `VEILFORGE_DATA` environment variable overrides both (tests).
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("VEILFORGE_DATA") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        if dir.join("portable.flag").exists() {
            return dir;
        }
    }
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("VeilForge")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_tolerant_loading() {
        let dir = crate::util::unique_temp_dir("settings");
        assert_eq!(Settings::load(&dir), Settings::default());
        let s = Settings {
            theme: ThemeChoice::Light,
            auto_lock_minutes: 30,
            hotkey: "Ctrl+Alt+V".into(),
            default_mode: Mode::Ask,
            ask_default_hidden: false,
            tray_icon: false,
            ..Default::default()
        };
        s.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), s);
        // No temporary file is left behind.
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["settings.json".to_string()]);
        // Unknown fields are ignored, missing ones get defaults, wild values are clamped.
        std::fs::write(
            dir.join("settings.json"),
            r#"{"theme":"Dark","future":1,"auto_lock_minutes":4000000}"#,
        )
        .unwrap();
        let loaded = Settings::load(&dir);
        assert_eq!(loaded.theme, ThemeChoice::Dark);
        assert_eq!(loaded.auto_lock_minutes, MAX_MINUTES);
        assert_eq!(loaded.relock_idle_minutes, 15);
        assert!(loaded.confirm_unlock);
        std::fs::write(dir.join("settings.json"), "{{{").unwrap();
        assert_eq!(Settings::load(&dir), Settings::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn background_needs_a_way_back() {
        let mut s = Settings::default();
        assert!(s.stays_in_background());
        s.tray_icon = false;
        assert!(!s.stays_in_background());
        s.hotkey = "Ctrl+Alt+V".into();
        assert!(s.stays_in_background());
        s.close_to_background = false;
        assert!(!s.stays_in_background());
        assert_eq!(s.title(), "VeilForge");
        s.neutral_title = true;
        assert_eq!(s.title(), "Files");
    }

    #[test]
    fn the_two_hotkeys_cannot_be_the_same() {
        let mut s = Settings {
            hotkey: "Ctrl+Alt+V".into(),
            lock_hotkey: "ctrl + alt + v".into(),
            ..Default::default()
        };
        s.sanitize();
        assert_eq!(s.hotkey, "Ctrl+Alt+V");
        assert!(s.lock_hotkey.is_empty());
        s.lock_hotkey = "Ctrl+Alt+L".into();
        s.sanitize();
        assert_eq!(s.lock_hotkey, "Ctrl+Alt+L");
    }

    /// The settings file has a fixed, reviewed set of fields. Adding a field
    /// means adding it here, which is the moment to check it is not a secret.
    #[test]
    fn settings_never_serialize_secrets() {
        let json = serde_json::to_value(Settings::default()).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "ask_default_hidden",
                "auto_lock_minutes",
                "close_to_background",
                "confirm_unlock",
                "context_menu",
                "default_mode",
                "hotkey",
                "lock_hotkey",
                "lock_on_close",
                "neutral_title",
                "open_after_unlock",
                "relock_idle_minutes",
                "relock_on_session_lock",
                "start_with_windows",
                "theme",
                "tray_icon",
            ]
        );
    }
}

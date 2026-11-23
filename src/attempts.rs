//! The log of wrong master passwords.
//!
//! Every time a wrong master password is typed (in the main window, at a
//! stand-in shortcut, at "Restore everything"), one line is added: when and
//! where. What was typed is never stored. After the next correct password
//! in the main window the user is told what happened while they were away.
//!
//! The file is plain JSON in the data folder. It has to be writable without
//! the master password, so it is a convenience, not evidence: someone who can
//! write to the data folder can also empty it.

use serde::{Deserialize, Serialize};
use std::path::Path;

const FILE: &str = "attempts.json";
/// The newest entries that are kept.
pub const KEEP: usize = 200;

/// Where the wrong password was typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Place {
    /// The unlock screen of the main window.
    Window,
    /// The small prompt of a stand-in shortcut or "Unlock with VeilForge".
    StandIn,
    /// "Restore everything" (the panic restore, also from the command line).
    Restore,
    /// "Change the master password".
    ChangePassword,
}

impl Place {
    pub fn label(self) -> &'static str {
        match self {
            Place::Window => "in the VeilForge window",
            Place::StandIn => "at a protected item's password prompt",
            Place::Restore => "at \"Restore everything\"",
            Place::ChangePassword => "at \"Change the master password\"",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    /// Unix seconds.
    pub at: i64,
    pub place: Place,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Log {
    /// Oldest first.
    pub entries: Vec<Attempt>,
    /// Wrong passwords in a row since the last correct one, anywhere.
    pub streak: u32,
    /// The user has been told about every entry up to this time. A time, not
    /// a position, so trimming old entries cannot confuse it.
    pub seen_until: i64,
    /// Entries at exactly `seen_until` that were already shown.
    pub seen_at_that_second: u32,
}

/// What to tell the user after a correct password.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notice {
    pub count: usize,
    pub last: Option<Attempt>,
}

impl Notice {
    /// One or two plain sentences, or `None` when there is nothing to tell.
    pub fn text(&self) -> Option<String> {
        let last = self.last?;
        if self.count == 0 {
            return None;
        }
        let when = crate::util::fmt_datetime(last.at);
        Some(if self.count == 1 {
            format!(
                "1 wrong master password was typed since you last unlocked VeilForge: {when}, {}.",
                last.place.label()
            )
        } else {
            format!(
                "{} wrong master passwords were typed since you last unlocked VeilForge. The last one: {when}, {}.",
                self.count,
                last.place.label()
            )
        })
    }
}

impl Log {
    pub fn load(dir: &Path) -> Log {
        std::fs::read(dir.join(FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Atomic, like the settings file. Failing to write the log never stops
    /// anything else, so errors are only returned for the tests.
    fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let temp = dir.join(format!("{FILE}.{}.tmp", std::process::id()));
        std::fs::write(&temp, json)?;
        std::fs::rename(&temp, dir.join(FILE)).inspect_err(|_| {
            let _ = std::fs::remove_file(&temp);
        })
    }

    /// Entries the user has not been told about yet.
    fn unseen(&self) -> Vec<Attempt> {
        let mut skip = self.seen_at_that_second;
        self.entries
            .iter()
            .filter(|e| {
                if e.at > self.seen_until {
                    true
                } else if e.at == self.seen_until && skip > 0 {
                    skip -= 1;
                    false
                } else {
                    // Same second as the last notice but added after it.
                    e.at == self.seen_until
                }
            })
            .copied()
            .collect()
    }

    fn mark_all_seen(&mut self) {
        match self.entries.last() {
            Some(last) => {
                self.seen_until = last.at;
                self.seen_at_that_second =
                    self.entries.iter().filter(|e| e.at == last.at).count() as u32;
            }
            None => {
                self.seen_until = 0;
                self.seen_at_that_second = 0;
            }
        }
    }
}

/// A wrong master password was typed. Returns the new streak.
pub fn record_failure(dir: &Path, place: Place, now: i64) -> u32 {
    let mut log = Log::load(dir);
    log.entries.push(Attempt { at: now, place });
    if log.entries.len() > KEEP {
        let extra = log.entries.len() - KEEP;
        log.entries.drain(..extra);
    }
    log.streak = log.streak.saturating_add(1);
    let _ = log.save(dir);
    log.streak
}

/// The correct master password was typed somewhere other than the main
/// window: only the streak ends. The user is told in the main window later.
pub fn record_success_elsewhere(dir: &Path) {
    let mut log = Log::load(dir);
    if log.streak != 0 {
        log.streak = 0;
        let _ = log.save(dir);
    }
}

/// The main window was unlocked. `own` wrong tries were made at this very
/// unlock screen just before (the user's own slips): they are not reported.
/// Everything is marked as seen.
pub fn record_unlock(dir: &Path, own: u32) -> Notice {
    let mut log = Log::load(dir);
    let mut unseen = log.unseen();
    // The user's own slips are the newest ones made in the window.
    let mut own = own as usize;
    while own > 0 {
        match unseen.iter().rposition(|e| e.place == Place::Window) {
            Some(i) => {
                unseen.remove(i);
                own -= 1;
            }
            None => break,
        }
    }
    let notice = Notice {
        count: unseen.len(),
        last: unseen.last().copied(),
    };
    let changed = log.streak != 0 || !log.unseen().is_empty();
    log.streak = 0;
    log.mark_all_seen();
    if changed {
        let _ = log.save(dir);
    }
    notice
}

/// Empties the log (Settings).
pub fn clear(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(FILE));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_are_counted_reported_once_and_never_hold_a_password() {
        let dir = crate::util::unique_temp_dir("attempts");
        assert_eq!(Log::load(&dir), Log::default());
        assert_eq!(record_unlock(&dir, 0), Notice::default());
        assert!(
            !dir.join(FILE).exists(),
            "nothing to write, nothing written"
        );

        assert_eq!(record_failure(&dir, Place::StandIn, 1000), 1);
        assert_eq!(record_failure(&dir, Place::Window, 1000), 2);
        assert_eq!(record_failure(&dir, Place::Window, 1060), 3);
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(text.contains("StandIn") && text.contains("1060"));
        // Only "when" and "where": the entries have no other field.
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        for entry in json["entries"].as_array().unwrap() {
            let keys: Vec<&String> = entry.as_object().unwrap().keys().collect();
            assert_eq!(keys, ["at", "place"]);
        }

        // A correct password elsewhere ends the streak but keeps the news.
        record_success_elsewhere(&dir);
        assert_eq!(Log::load(&dir).streak, 0);
        assert_eq!(Log::load(&dir).entries.len(), 3);

        // The last window try was the user's own slip just now.
        let notice = record_unlock(&dir, 1);
        assert_eq!(notice.count, 2);
        assert_eq!(notice.last.unwrap().place, Place::Window);
        assert_eq!(notice.last.unwrap().at, 1000);
        let text = notice.text().unwrap();
        assert!(text.starts_with("2 wrong master passwords"), "{text}");
        // Told once.
        assert_eq!(record_unlock(&dir, 0), Notice::default());
        assert_eq!(Notice::default().text(), None);

        // A new one in the very second of the last notice is still new.
        record_failure(&dir, Place::Restore, 1060);
        let notice = record_unlock(&dir, 0);
        assert_eq!(notice.count, 1);
        assert!(notice.text().unwrap().contains("Restore everything"));

        clear(&dir);
        assert_eq!(Log::load(&dir), Log::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_log_is_capped_and_a_damaged_file_is_ignored() {
        let dir = crate::util::unique_temp_dir("attempts-cap");
        std::fs::write(dir.join(FILE), b"{ not json").unwrap();
        assert_eq!(Log::load(&dir), Log::default());
        for n in 0..(KEEP as i64 + 25) {
            record_failure(&dir, Place::Window, n);
        }
        let log = Log::load(&dir);
        assert_eq!(log.entries.len(), KEEP);
        assert_eq!(log.entries[0].at, 25);
        assert_eq!(log.streak, KEEP as u32 + 25);
        // Only the user's own tries: nothing to report.
        assert_eq!(record_unlock(&dir, KEEP as u32 + 25).count, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

//! The Lock-mode ACL.
//!
//! A locked item gets a *protected* DACL (no inheritance from the parent) with
//! two entries:
//!
//! * deny Everyone: every file right except READ_CONTROL, WRITE_DAC,
//!   SYNCHRONIZE and FILE_READ_ATTRIBUTES (so: no read, write, append, list,
//!   traverse, execute, extended attributes, write-attributes, delete,
//!   delete-child, take-ownership);
//! * allow the current user: READ_CONTROL | WRITE_DAC | SYNCHRONIZE |
//!   FILE_READ_ATTRIBUTES, which is exactly what VeilForge needs to see that
//!   the lock is still there and to take it off again, whoever the owner is.
//!
//! For a folder every child gets the same DACL (set child by child, links are
//! skipped), because "bypass traverse checking" would otherwise let a program
//! open a child by its full path. The DACL each child had is recorded first.
//! The owner is never changed.

use super::{win, Error, Result};
use crate::model::Extra;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

const DENY_MASK: u32 = 0x0009_017F;
const UNDO_MASK: u32 = 0x0016_0080;

/// The DACL of a locked item in SDDL, in the spelling Windows reports it.
pub fn lock_sddl() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| {
        let raw = format!(
            "D:P(D;;0x{DENY_MASK:x};;;WD)(A;;0x{UNDO_MASK:x};;;{})",
            win::user_sid()
        );
        win::canonical_dacl(&raw).unwrap_or(raw)
    })
}

/// A DACL that gives the current user full control (used to open a holding
/// folder for listing, and by the test clean-up).
pub fn open_sddl() -> String {
    format!("D:P(A;;FA;;;{})", win::user_sid())
}

pub fn is_locked(path: &Path) -> bool {
    win::get_sddl(path, false)
        .map(|s| s == lock_sddl())
        .unwrap_or(false)
}

pub fn acl_error(path: &Path, e: std::io::Error) -> Error {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if e.raw_os_error() == Some(win::ERROR_ACCESS_DENIED) {
        Error::NeedsElevation(format!(
            "Windows does not let this account change the permissions of \"{name}\". Administrator rights are needed for this item."
        ))
    } else if matches!(
        e.raw_os_error(),
        Some(win::ERROR_FILE_NOT_FOUND) | Some(win::ERROR_PATH_NOT_FOUND)
    ) {
        Error::NotFound(format!("\"{name}\" was not found."))
    } else {
        Error::Io(format!(
            "The permissions of \"{name}\" could not be changed: {e}"
        ))
    }
}

/// Sets a DACL and reads it back to make sure it is what was asked for.
pub fn set_verified(path: &Path, sddl: &str) -> Result<()> {
    win::set_dacl(path, sddl).map_err(|e| acl_error(path, e))?;
    let now = win::get_sddl(path, false).map_err(|e| acl_error(path, e))?;
    let want = dacl_part(sddl);
    if now != want && win::canonical_dacl(want).ok().as_deref() != Some(now.as_str()) {
        return Err(Error::Io(format!(
            "The permissions of \"{}\" did not take effect.",
            win::plain(path)
        )));
    }
    Ok(())
}

/// The `D:...` part of an SDDL string (drops `O:` / `G:` and any `S:` part).
pub fn dacl_part(sddl: &str) -> &str {
    let start = sddl.find("D:").unwrap_or(0);
    let rest = &sddl[start..];
    match rest.find("S:") {
        // "S:" can only start a SACL when it follows a ')' or the flags.
        Some(i) if i > 0 && !rest[..i].ends_with(';') => &rest[..i],
        _ => rest,
    }
}

/// Locks every child of `dir` (deepest first; `dir` itself is the caller's job).
pub fn lock_children(dir: &Path, tick: &mut dyn FnMut()) -> Result<()> {
    let entries = win::list(dir).map_err(|e| acl_error(dir, e))?;
    for e in entries {
        if e.stat.is_link() {
            continue;
        }
        let child = dir.join(&e.name);
        if e.stat.is_dir() {
            lock_children(&child, tick)?;
        }
        win::set_dacl(&child, lock_sddl()).map_err(|err| acl_error(&child, err))?;
        tick();
    }
    Ok(())
}

/// Gives every child of `dir` its recorded DACL back, top-down. `dir` itself
/// must already be unlocked.
pub fn unlock_children(dir: &Path, extra: &Extra, tick: &mut dyn FnMut()) -> Result<()> {
    let special: HashMap<&str, &str> = extra
        .child_acls
        .iter()
        .map(|c| (c.path.as_str(), c.sddl.as_str()))
        .collect();
    unlock_level(dir, "", extra, &special, tick)
}

fn unlock_level(
    dir: &Path,
    rel: &str,
    extra: &Extra,
    special: &HashMap<&str, &str>,
    tick: &mut dyn FnMut(),
) -> Result<()> {
    let entries = win::list(dir).map_err(|e| acl_error(dir, e))?;
    for e in entries {
        if e.stat.is_link() {
            continue;
        }
        let child = dir.join(&e.name);
        let child_rel = if rel.is_empty() {
            e.name.clone()
        } else {
            format!("{rel}\\{}", e.name)
        };
        let fallback = if e.stat.is_dir() {
            extra.acl_dir.as_str()
        } else {
            extra.acl_file.as_str()
        };
        let sddl = special.get(child_rel.as_str()).copied().unwrap_or(fallback);
        if !sddl.is_empty() {
            win::set_dacl(&child, sddl).map_err(|err| acl_error(&child, err))?;
        }
        tick();
        if e.stat.is_dir() {
            unlock_level(&child, &child_rel, extra, special, tick)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dacl_part_strips_owner_group_and_sacl() {
        assert_eq!(
            dacl_part("O:BAG:SYD:PAI(A;;FA;;;SY)(A;;FA;;;BA)"),
            "D:PAI(A;;FA;;;SY)(A;;FA;;;BA)"
        );
        assert_eq!(
            dacl_part("O:BAG:SYD:(A;;FA;;;SY)S:(ML;;NW;;;LW)"),
            "D:(A;;FA;;;SY)"
        );
        assert_eq!(dacl_part("D:P(A;;FA;;;S-1-5-18)"), "D:P(A;;FA;;;S-1-5-18)");
    }

    #[test]
    fn lock_dacl_is_protected_and_keeps_the_undo_rights() {
        let s = lock_sddl();
        assert!(s.starts_with("D:P"), "{s}");
        assert!(s.contains(";;;WD)"), "{s}");
        assert!(s.contains(win::user_sid()), "{s}");
        // The deny entry must not take away what the undo needs.
        assert_eq!(DENY_MASK & UNDO_MASK, 0);
    }
}

//! The per-volume holding folder.
//!
//! `<volume>\VeilForge.sys` is hidden + system + not content indexed, and its
//! protected DACL denies Everyone the right to list it (and to delete it,
//! change its attributes or take ownership), while the current user may still
//! add entries, pass through and move entries out. A hidden item therefore
//! sits under a random name that cannot be looked up: VeilForge reaches it by
//! the full path stored in the encrypted map.

use super::{acl, volume, win, Env, Error, Result};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Name of the holding folder at a volume root.
pub const HOLD_NAME: &str = "VeilForge.sys";

const DENY_MASK: u32 = 0x0009_0119;
const USER_MASK: u32 = 0x0016_00E6;
const HOLD_ATTRS: u32 = win::ATTR_HIDDEN | win::ATTR_SYSTEM | win::ATTR_NOT_INDEXED;

/// The DACL of a holding folder, in the spelling Windows reports it.
pub fn hold_sddl() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| {
        let raw = format!(
            "D:P(D;;0x{DENY_MASK:x};;;WD)(A;;0x{USER_MASK:x};;;{})",
            win::user_sid()
        );
        win::canonical_dacl(&raw).unwrap_or(raw)
    })
}

/// A fresh random name without extension (16 hex characters).
pub fn random_name() -> String {
    crate::util::hex(&crate::secret::random_array::<8>())
}

fn local_fallback() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("VeilForge").join("hold"))
}

/// Every place a holding folder may be (whether it exists or not).
pub fn known(env: &Env) -> Vec<PathBuf> {
    if let Some(root) = &env.hold_root {
        return vec![root.clone()];
    }
    let mut out: Vec<PathBuf> = win::local_drive_roots()
        .into_iter()
        .map(|r| PathBuf::from(r).join(HOLD_NAME))
        .collect();
    if let Some(local) = local_fallback() {
        out.push(local);
    }
    out
}

fn usable(dir: &Path) -> bool {
    if win::exists(dir) {
        win::is_dir(dir)
            && win::attributes(dir)
                .map(|a| a & win::ATTR_REPARSE == 0)
                .unwrap_or(false)
            && win::can_write_dac(dir)
    } else {
        std::fs::create_dir_all(win::long(dir)).is_ok()
    }
}

/// The holding folder for an item, on the item's own volume. Never crosses volumes.
pub fn dir_for(env: &Env, item: &Path) -> Result<PathBuf> {
    if let Some(root) = &env.hold_root {
        let _ = std::fs::create_dir_all(win::long(root));
        if !volume::same_volume(root, item) {
            return Err(Error::CrossVolume(
                "The holding folder is on another drive. VeilForge never copies files between drives."
                    .into(),
            ));
        }
        return Ok(root.clone());
    }
    let vol = volume::root_of(item)
        .ok_or_else(|| Error::Io("The drive of this item cannot be determined.".into()))?;
    let primary = PathBuf::from(&vol).join(HOLD_NAME);
    if usable(&primary) {
        return Ok(primary);
    }
    if let Some(local) = local_fallback() {
        if volume::same_volume(&local, item) && usable(&local) {
            return Ok(local);
        }
    }
    Err(Error::NeedsElevation(format!(
        "VeilForge cannot create its holding folder on drive {vol} without administrator rights."
    )))
}

/// What is wrong with a holding folder, if anything.
pub fn problem(dir: &Path, reduced: bool) -> Option<String> {
    let attrs = match win::attributes(dir) {
        Ok(a) => a,
        Err(_) => return Some("The holding folder is missing.".into()),
    };
    if attrs & HOLD_ATTRS != HOLD_ATTRS {
        return Some("The holding folder is no longer hidden.".into());
    }
    if !reduced && win::get_sddl(dir, false).ok().as_deref() != Some(hold_sddl()) {
        return Some("The permissions of the holding folder were changed.".into());
    }
    None
}

/// Creates the holding folder if needed and (re)applies attributes and DACL.
pub fn ensure(dir: &Path, reduced: bool) -> Result<()> {
    if !win::exists(dir) {
        std::fs::create_dir_all(win::long(dir))
            .map_err(|e| Error::Io(format!("The holding folder could not be created: {e}")))?;
    }
    if problem(dir, reduced).is_none() {
        return Ok(());
    }
    if !reduced {
        // Attributes cannot be changed under the deny entry: open it up first.
        win::set_dacl(dir, &acl::open_sddl()).map_err(|e| acl::acl_error(dir, e))?;
    }
    let attrs = win::attributes(dir).unwrap_or(0);
    win::set_attributes(dir, attrs | HOLD_ATTRS)
        .map_err(|e| Error::Io(format!("The holding folder could not be hidden: {e}")))?;
    if !reduced {
        acl::set_verified(dir, hold_sddl())?;
    }
    match problem(dir, reduced) {
        None => Ok(()),
        Some(p) => Err(Error::Io(p)),
    }
}

/// Lists the entries of a holding folder. Listing is denied by its DACL, so the
/// DACL is opened for the moment of the listing and put back straight after.
pub fn list(dir: &Path) -> Result<Vec<PathBuf>> {
    let acl = volume::info_for(dir).acl;
    let was_protected = acl && win::get_sddl(dir, false).ok().as_deref() == Some(hold_sddl());
    if was_protected {
        win::set_dacl(dir, &acl::open_sddl()).map_err(|e| acl::acl_error(dir, e))?;
    }
    let listed = win::list(dir);
    if was_protected {
        acl::set_verified(dir, hold_sddl())?;
    }
    let listed =
        listed.map_err(|e| Error::Io(format!("The holding folder cannot be read: {e}")))?;
    Ok(listed.into_iter().map(|e| dir.join(e.name)).collect())
}

/// Removes a holding folder that has nothing in it. Returns whether it did.
pub fn remove_if_empty(dir: &Path) -> bool {
    if !win::is_dir(dir) {
        return false;
    }
    match list(dir) {
        Ok(entries) if entries.is_empty() => {}
        _ => return false,
    }
    let acl = volume::info_for(dir).acl;
    if acl && win::set_dacl(dir, &acl::open_sddl()).is_err() {
        return false;
    }
    let _ = win::set_attributes(dir, win::ATTR_NORMAL);
    if std::fs::remove_dir(win::long(dir)).is_ok() {
        true
    } else {
        let _ = ensure(dir, !acl);
        false
    }
}

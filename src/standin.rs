//! The stand-in shortcut left in place of an "ask on open" item.
//!
//! The shortcut is named `<original name>.lnk`, so the original name stays
//! visible in the folder. That is the whole point of this mode ("it is there,
//! but opening it asks for the password"); an item whose name must not be
//! seen has to be hidden instead. The shortcut itself holds only the path of
//! the VeilForge program, `--open <id>` and an icon; not where the item is.

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::BOOL;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::UI::Shell::{
    AssocQueryStringW, IShellLinkW, ShellLink, ASSOCF_INIT_IGNOREUNKNOWN, ASSOCF_NOTRUNCATE,
    ASSOCSTR_DEFAULTICON, SLGP_RAWPATH,
};

/// The neutral description stored in every stand-in.
pub const DESCRIPTION: &str = "Protected item";

/// What a stand-in shortcut says when read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StandIn {
    pub target: PathBuf,
    pub arguments: String,
    pub icon: String,
    pub description: String,
}

/// Path of the stand-in for an original path: the same folder and name plus `.lnk`
/// (Explorer hides the `.lnk` extension, so it shows under the original name).
pub fn standin_path(original: &Path) -> PathBuf {
    let mut s = original.as_os_str().to_os_string();
    s.push(".lnk");
    PathBuf::from(s)
}

fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// COM for the current thread, for the duration of one call. A thread that is
/// already initialised (in either mode) is used as it is.
struct Com(bool);

impl Com {
    fn enter() -> Com {
        // SAFETY: plain COM initialisation; balanced in Drop when it succeeded.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        Com(hr.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }
}

fn expand(text: &str) -> String {
    let w = wide(std::ffi::OsStr::new(text));
    let mut buf = vec![0u16; 1024];
    // SAFETY: `w` is NUL-terminated and `buf` is writable.
    let n = unsafe { ExpandEnvironmentStringsW(PCWSTR(w.as_ptr()), Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() {
        text.to_string()
    } else {
        from_wide(&buf)
    }
}

fn system_icon(index: i32) -> (String, i32) {
    (expand(r"%SystemRoot%\System32\imageres.dll"), index)
}

/// The icon Explorer shows for files with this extension, as (file, index).
/// Types whose icon comes from the file itself (programs, icons, shortcuts)
/// and unknown types get the plain document icon.
fn icon_for_extension(ext: &str) -> (String, i32) {
    const GENERIC_DOCUMENT: i32 = 2;
    if ext.is_empty() {
        return system_icon(GENERIC_DOCUMENT);
    }
    let assoc = wide(std::ffi::OsStr::new(&format!(".{ext}")));
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `assoc` is NUL-terminated; `buf` has `len` characters.
    let hr = unsafe {
        AssocQueryStringW(
            ASSOCF_INIT_IGNOREUNKNOWN | ASSOCF_NOTRUNCATE,
            ASSOCSTR_DEFAULTICON,
            PCWSTR(assoc.as_ptr()),
            PCWSTR::null(),
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    if hr.is_err() {
        return system_icon(GENERIC_DOCUMENT);
    }
    let raw = from_wide(&buf);
    let (file, index) = match raw.rsplit_once(',') {
        Some((f, i)) if i.trim().parse::<i32>().is_ok() => (f, i.trim().parse::<i32>().unwrap()),
        _ => (raw.as_str(), 0),
    };
    let file = expand(file.trim().trim_matches('"'));
    if file.is_empty() || file.starts_with('@') || file.contains('%') || !Path::new(&file).is_file()
    {
        return system_icon(GENERIC_DOCUMENT);
    }
    (file, index)
}

/// Writes `<original>.lnk` targeting `exe --open <id>`, with the icon the
/// original shows in Explorer (the file type's icon, or the folder icon).
/// Returns the shortcut's path.
///
/// The icon is looked up from the extension in the name, so this works the
/// same before and after the item itself has been moved away.
pub fn create(exe: &Path, id: &str, original: &Path, is_folder: bool) -> Result<PathBuf, String> {
    const FOLDER: i32 = 3;
    let lnk = standin_path(original);
    let (icon, index) = if is_folder {
        system_icon(FOLDER)
    } else {
        let ext = original
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
        icon_for_extension(&ext)
    };
    let _com = Com::enter();
    // Shell links are saved through an ordinary (short) path; a long
    // destination gets the finished file moved into place.
    let direct = lnk.as_os_str().len() < 240;
    let save_to = if direct {
        lnk.clone()
    } else {
        std::env::temp_dir().join(format!(
            "veilforge-lnk-{}.lnk",
            crate::util::hex(&crate::secret::random_array::<6>())
        ))
    };
    // SAFETY: standard IShellLinkW / IPersistFile use; every string is a
    // NUL-terminated wide string that outlives its call.
    let saved: windows::core::Result<()> = unsafe {
        (|| {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(PCWSTR(wide(exe.as_os_str()).as_ptr()))?;
            let args = wide(std::ffi::OsStr::new(&format!("--open {id}")));
            link.SetArguments(PCWSTR(args.as_ptr()))?;
            let desc = wide(std::ffi::OsStr::new(DESCRIPTION));
            link.SetDescription(PCWSTR(desc.as_ptr()))?;
            let icon_w = wide(std::ffi::OsStr::new(&icon));
            link.SetIconLocation(PCWSTR(icon_w.as_ptr()), index)?;
            if let Some(dir) = exe.parent() {
                link.SetWorkingDirectory(PCWSTR(wide(dir.as_os_str()).as_ptr()))?;
            }
            let file: IPersistFile = link.cast()?;
            file.Save(PCWSTR(wide(save_to.as_os_str()).as_ptr()), BOOL(1))
        })()
    };
    saved.map_err(|e| e.message().to_string())?;
    if !direct {
        let dest = long(&lnk);
        let moved = std::fs::copy(&save_to, &dest).map(|_| ());
        let _ = std::fs::remove_file(&save_to);
        moved.map_err(|e| e.to_string())?;
    }
    Ok(lnk)
}

fn long(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s.starts_with(r"\\?\") {
        path.to_path_buf()
    } else if let Some(rest) = s.strip_prefix(r"\\") {
        PathBuf::from(format!(r"\\?\UNC\{rest}"))
    } else {
        PathBuf::from(format!(r"\\?\{s}"))
    }
}

/// Reads a shortcut back (used by tests and the health check).
pub fn read(lnk: &Path) -> Result<StandIn, String> {
    let _com = Com::enter();
    let direct = lnk.as_os_str().len() < 240 && !lnk.to_string_lossy().starts_with(r"\\?\");
    let load_from = if direct {
        lnk.to_path_buf()
    } else {
        let tmp = std::env::temp_dir().join(format!(
            "veilforge-lnk-{}.lnk",
            crate::util::hex(&crate::secret::random_array::<6>())
        ));
        std::fs::copy(long(lnk), &tmp).map_err(|e| e.to_string())?;
        tmp
    };
    // SAFETY: standard IShellLinkW / IPersistFile use with valid buffers.
    let result: windows::core::Result<StandIn> = unsafe {
        (|| {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            let file: IPersistFile = link.cast()?;
            file.Load(PCWSTR(wide(load_from.as_os_str()).as_ptr()), STGM_READ)?;
            let mut buf = vec![0u16; 2048];
            link.GetPath(&mut buf, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)?;
            let target = PathBuf::from(from_wide(&buf));
            buf.fill(0);
            link.GetArguments(&mut buf)?;
            let arguments = from_wide(&buf);
            buf.fill(0);
            let mut index = 0i32;
            link.GetIconLocation(&mut buf, &mut index)?;
            let icon = format!("{},{index}", from_wide(&buf));
            buf.fill(0);
            link.GetDescription(&mut buf)?;
            let description = from_wide(&buf);
            Ok(StandIn {
                target,
                arguments,
                icon,
                description,
            })
        })()
    };
    if !direct {
        let _ = std::fs::remove_file(&load_from);
    }
    result.map_err(|e| e.message().to_string())
}

/// Extracts the item id from a stand-in's arguments (`--open <id>`).
pub fn id_of(standin: &StandIn) -> Option<String> {
    let mut parts = standin.arguments.split_whitespace();
    while let Some(p) = parts.next() {
        if p == "--open" {
            return parts.next().map(str::to_string);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_read_back() {
        let dir = crate::util::unique_temp_dir("standin");
        let exe = std::env::current_exe().unwrap();
        let original = dir.join("Quarterly report.txt");
        let lnk = create(&exe, "0123456789abcdef", &original, false).unwrap();
        assert_eq!(lnk, dir.join("Quarterly report.txt.lnk"));
        assert!(lnk.is_file());
        let back = read(&lnk).unwrap();
        assert_eq!(
            back.target.to_string_lossy().to_lowercase(),
            exe.to_string_lossy().to_lowercase()
        );
        assert_eq!(back.arguments, "--open 0123456789abcdef");
        assert_eq!(back.description, DESCRIPTION);
        assert_eq!(id_of(&back).as_deref(), Some("0123456789abcdef"));
        let (icon_file, _) = back.icon.rsplit_once(',').unwrap();
        assert!(Path::new(icon_file).is_file(), "icon file: {}", back.icon);
        // The shortcut does not say where the item went.
        let bytes = std::fs::read(&lnk).unwrap();
        let wide_hold: Vec<u8> = "VeilForge.sys"
            .encode_utf16()
            .flat_map(|c| c.to_le_bytes())
            .collect();
        assert!(!bytes.windows(wide_hold.len()).any(|w| w == wide_hold));

        // A folder gets the folder icon.
        let folder = dir.join("Private things");
        let lnk2 = create(&exe, "fedcba9876543210", &folder, true).unwrap();
        let back2 = read(&lnk2).unwrap();
        assert!(back2.icon.to_lowercase().ends_with("imageres.dll,3"));
        assert_eq!(id_of(&back2).as_deref(), Some("fedcba9876543210"));

        assert!(read(&dir.join("missing.lnk")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

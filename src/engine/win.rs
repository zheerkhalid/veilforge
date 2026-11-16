//! Thin wrappers over the Win32 file and security calls the engine needs.
//! Every path goes to Windows with a `\\?\` prefix, so long paths work.

use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, LocalFree, FILETIME, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows::Win32::Security::{
    GetKernelObjectSecurity, GetSecurityDescriptorControl, GetTokenInformation,
    SetKernelObjectSecurity, SetSecurityDescriptorControl, TokenUser, OBJECT_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, SECURITY_DESCRIPTOR_CONTROL, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetFileAttributesW, GetVolumeInformationW, GetVolumePathNameW, MoveFileExW,
    SetFileAttributesW, SetFileTime, FILE_FLAGS_AND_ATTRIBUTES, MOVE_FILE_FLAGS,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub const ATTR_READONLY: u32 = 0x1;
pub const ATTR_HIDDEN: u32 = 0x2;
pub const ATTR_SYSTEM: u32 = 0x4;
pub const ATTR_DIRECTORY: u32 = 0x10;
pub const ATTR_ARCHIVE: u32 = 0x20;
pub const ATTR_NORMAL: u32 = 0x80;
pub const ATTR_REPARSE: u32 = 0x400;
pub const ATTR_OFFLINE: u32 = 0x1000;
pub const ATTR_NOT_INDEXED: u32 = 0x2000;
pub const ATTR_RECALL_ON_OPEN: u32 = 0x4_0000;
pub const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
/// The attribute bits `SetFileAttributesW` can change.
pub const ATTR_SETTABLE: u32 = 0x1 | 0x2 | 0x4 | 0x20 | 0x100 | 0x1000 | 0x2000;

const READ_CONTROL: u32 = 0x2_0000;
const WRITE_DAC: u32 = 0x4_0000;
const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
const GENERIC_READ: u32 = 0x8000_0000;
const FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const OWNER_INFO: u32 = 1;
const GROUP_INFO: u32 = 2;
const DACL_INFO: u32 = 4;
const SE_DACL_AUTO_INHERIT_REQ: u16 = 0x0100;
const SE_DACL_AUTO_INHERITED: u16 = 0x0400;

pub const ERROR_FILE_NOT_FOUND: i32 = 2;
pub const ERROR_PATH_NOT_FOUND: i32 = 3;
pub const ERROR_ACCESS_DENIED: i32 = 5;
pub const ERROR_NOT_SAME_DEVICE: i32 = 17;
pub const ERROR_SHARING_VIOLATION: i32 = 32;
pub const ERROR_LOCK_VIOLATION: i32 = 33;

/// The path without a `\\?\` prefix, as text.
pub fn plain(path: &Path) -> String {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.into_owned()
    }
}

/// The same path with a `\\?\` prefix (no length limit, no normalisation by Win32).
pub fn long(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s.starts_with(r"\\?\") || s.starts_with(r"\\.\") {
        return path.to_path_buf();
    }
    let s = s.replace('/', "\\");
    if let Some(rest) = s.strip_prefix(r"\\") {
        PathBuf::from(format!(r"\\?\UNC\{rest}"))
    } else if s.len() >= 2 && s.as_bytes()[1] == b':' {
        PathBuf::from(format!(r"\\?\{s}"))
    } else {
        // Relative: make it absolute first.
        match std::path::absolute(path) {
            Ok(abs) if abs.is_absolute() && abs != path => long(&abs),
            _ => path.to_path_buf(),
        }
    }
}

fn wide(path: &Path) -> Vec<u16> {
    long(path)
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn wide_str(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn win_err(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
}

/// Raw attribute bits, without following a link.
pub fn attributes(path: &Path) -> io::Result<u32> {
    let w = wide(path);
    // SAFETY: `w` is a NUL-terminated wide string that outlives the call.
    let a = unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) };
    if a == u32::MAX {
        Err(io::Error::last_os_error())
    } else {
        Ok(a)
    }
}

/// Whether something has this name. "Access denied" counts as present.
pub fn exists(path: &Path) -> bool {
    match attributes(path) {
        Ok(_) => true,
        Err(e) => !matches!(
            e.raw_os_error(),
            Some(ERROR_FILE_NOT_FOUND) | Some(ERROR_PATH_NOT_FOUND) | Some(123) | Some(53)
        ),
    }
}

pub fn is_dir(path: &Path) -> bool {
    attributes(path)
        .map(|a| a & ATTR_DIRECTORY != 0)
        .unwrap_or(false)
}

/// Sets the changeable attribute bits exactly (other bits are left alone by Windows).
pub fn set_attributes(path: &Path, attrs: u32) -> io::Result<()> {
    let mut a = attrs & ATTR_SETTABLE;
    if a == 0 {
        a = ATTR_NORMAL;
    }
    let w = wide(path);
    // SAFETY: `w` is a NUL-terminated wide string that outlives the call.
    unsafe { SetFileAttributesW(PCWSTR(w.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(a)) }.map_err(win_err)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stat {
    pub attributes: u32,
    pub created: u64,
    pub accessed: u64,
    pub modified: u64,
    pub size: u64,
}

impl Stat {
    pub fn of(md: &std::fs::Metadata) -> Stat {
        Stat {
            attributes: md.file_attributes(),
            created: md.creation_time(),
            accessed: md.last_access_time(),
            modified: md.last_write_time(),
            size: md.file_size(),
        }
    }
    pub fn is_dir(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }
    pub fn is_link(&self) -> bool {
        self.attributes & ATTR_REPARSE != 0
    }
}

/// Attributes, timestamps and size of the item itself (links are not followed).
pub fn stat(path: &Path) -> io::Result<Stat> {
    std::fs::symlink_metadata(long(path)).map(|m| Stat::of(&m))
}

fn open_with(path: &Path, access: u32, share: u32) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .access_mode(access)
        .share_mode(share)
        .custom_flags(FLAG_BACKUP_SEMANTICS | FLAG_OPEN_REPARSE_POINT)
        .open(long(path))
}

fn handle(f: &std::fs::File) -> HANDLE {
    HANDLE(f.as_raw_handle())
}

fn filetime(t: u64) -> FILETIME {
    FILETIME {
        dwLowDateTime: t as u32,
        dwHighDateTime: (t >> 32) as u32,
    }
}

/// Sets creation, last access and last write time of a file or folder.
pub fn set_times(path: &Path, created: u64, accessed: u64, modified: u64) -> io::Result<()> {
    let f = open_with(path, FILE_WRITE_ATTRIBUTES, 7)?;
    let (c, a, m) = (filetime(created), filetime(accessed), filetime(modified));
    // SAFETY: the handle is open for FILE_WRITE_ATTRIBUTES; the FILETIMEs live on the stack.
    unsafe { SetFileTime(handle(&f), Some(&c), Some(&a), Some(&m)) }.map_err(win_err)
}

/// Same-volume rename that never replaces an existing name and never copies.
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    let (a, b) = (wide(from), wide(to));
    // SAFETY: both are NUL-terminated wide strings that outlive the call. Flags 0:
    // no MOVEFILE_COPY_ALLOWED (fails across volumes), no MOVEFILE_REPLACE_EXISTING.
    unsafe { MoveFileExW(PCWSTR(a.as_ptr()), PCWSTR(b.as_ptr()), MOVE_FILE_FLAGS(0)) }
        .map_err(win_err)
}

/// True when another program has the file open (an exclusive open is refused).
pub fn in_use(path: &Path) -> bool {
    match open_with(path, GENERIC_READ, 0) {
        Ok(_) => false,
        Err(e) => matches!(
            e.raw_os_error(),
            Some(ERROR_SHARING_VIOLATION) | Some(ERROR_LOCK_VIOLATION)
        ),
    }
}

struct LocalMem(*mut c_void);
impl Drop for LocalMem {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a Win32 call documented to need LocalFree.
            unsafe {
                let _ = LocalFree(HLOCAL(self.0));
            }
        }
    }
}

fn pwstr_to_string(p: PWSTR) -> String {
    // SAFETY: Win32 returned a valid NUL-terminated wide string.
    unsafe { String::from_utf16_lossy(p.as_wide()) }
}

/// The security descriptor of the item itself as SDDL: `D:...` only, or
/// `O:...G:...D:...` with `with_owner`.
pub fn get_sddl(path: &Path, with_owner: bool) -> io::Result<String> {
    let f = open_with(path, READ_CONTROL, 7)?;
    let info = if with_owner {
        OWNER_INFO | GROUP_INFO | DACL_INFO
    } else {
        DACL_INFO
    };
    let mut needed = 0u32;
    // SAFETY: a zero-length query that only reports the size needed.
    let _ = unsafe {
        GetKernelObjectSecurity(
            handle(&f),
            info,
            PSECURITY_DESCRIPTOR(std::ptr::null_mut()),
            0,
            &mut needed,
        )
    };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0u8; needed as usize];
    let sd = PSECURITY_DESCRIPTOR(buf.as_mut_ptr() as *mut c_void);
    // SAFETY: `buf` has `needed` bytes.
    unsafe { GetKernelObjectSecurity(handle(&f), info, sd, needed, &mut needed) }
        .map_err(win_err)?;
    sd_to_sddl(sd, info)
}

fn sd_to_sddl(sd: PSECURITY_DESCRIPTOR, info: u32) -> io::Result<String> {
    let mut out = PWSTR::null();
    // SAFETY: `sd` is a valid self-relative descriptor; `out` receives a LocalAlloc'd string.
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            1,
            OBJECT_SECURITY_INFORMATION(info),
            &mut out,
            None,
        )
    }
    .map_err(win_err)?;
    let _free = LocalMem(out.0 as *mut c_void);
    Ok(pwstr_to_string(out))
}

fn sddl_to_sd(sddl: &str) -> io::Result<(LocalMem, PSECURITY_DESCRIPTOR)> {
    let w = wide_str(sddl);
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `w` is NUL-terminated; `sd` receives a LocalAlloc'd descriptor.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(w.as_ptr()), 1, &mut sd, None)
    }
    .map_err(win_err)?;
    Ok((LocalMem(sd.0), sd))
}

/// Canonical spelling of the `D:` part of an SDDL string (what [`get_sddl`]
/// would report for an item carrying exactly this DACL).
pub fn canonical_dacl(sddl: &str) -> io::Result<String> {
    let (_mem, sd) = sddl_to_sd(sddl)?;
    sd_to_sddl(sd, DACL_INFO)
}

/// Replaces the DACL of the item itself with exactly the `D:` part of `sddl`
/// (protected flag and inherited ACEs included). Nothing is propagated to
/// children and the owner is not touched.
pub fn set_dacl(path: &Path, sddl: &str) -> io::Result<()> {
    let (_mem, sd) = sddl_to_sd(sddl)?;
    let mut control = 0u16;
    let mut revision = 0u32;
    // SAFETY: `sd` is a valid descriptor produced above.
    unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) }.map_err(win_err)?;
    if control & SE_DACL_AUTO_INHERITED != 0 {
        // Without the "request" bit the kernel drops the auto-inherited flag.
        // SAFETY: only the control word of our own descriptor copy is changed.
        unsafe {
            SetSecurityDescriptorControl(
                sd,
                SECURITY_DESCRIPTOR_CONTROL(SE_DACL_AUTO_INHERIT_REQ),
                SECURITY_DESCRIPTOR_CONTROL(SE_DACL_AUTO_INHERIT_REQ),
            )
        }
        .map_err(win_err)?;
    }
    let f = open_with(path, WRITE_DAC, 7)?;
    // SAFETY: the handle is open for WRITE_DAC and `sd` is valid.
    unsafe { SetKernelObjectSecurity(handle(&f), OBJECT_SECURITY_INFORMATION(DACL_INFO), sd) }
        .map_err(win_err)
}

/// Whether the current user may change the item's DACL.
pub fn can_write_dac(path: &Path) -> bool {
    open_with(path, WRITE_DAC | READ_CONTROL, 7).is_ok()
}

/// The current user's SID as text (`S-1-5-21-...`).
pub fn user_sid() -> &'static str {
    static SID: OnceLock<String> = OnceLock::new();
    SID.get_or_init(|| query_user_sid().unwrap_or_else(|| "OW".to_string()))
}

fn query_user_sid() -> Option<String> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle of this process is always valid.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.ok()?;
    let mut needed = 0u32;
    // SAFETY: a size query.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
    let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    // SAFETY: `buf` is at least `needed` bytes and 8-byte aligned.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut c_void),
            needed,
            &mut needed,
        )
    };
    // SAFETY: the token handle was opened above.
    unsafe {
        let _ = CloseHandle(token);
    }
    ok.ok()?;
    // SAFETY: on success the buffer starts with a TOKEN_USER whose SID points into it.
    let sid = unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid };
    let mut out = PWSTR::null();
    // SAFETY: `sid` is valid while `buf` lives; `out` receives a LocalAlloc'd string.
    unsafe { ConvertSidToStringSidW(sid, &mut out) }.ok()?;
    let _free = LocalMem(out.0 as *mut c_void);
    Some(pwstr_to_string(out))
}

/// Root of the volume a path lives on, for example `D:\`.
pub fn volume_root(path: &Path) -> io::Result<String> {
    let w = wide(path);
    let mut buf = vec![0u16; w.len() + 8];
    // SAFETY: `w` is NUL-terminated; `buf` is writable for its whole length.
    unsafe { GetVolumePathNameW(PCWSTR(w.as_ptr()), &mut buf) }.map_err(win_err)?;
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    let root = PathBuf::from(std::ffi::OsString::from_wide(&buf[..len]));
    let mut s = plain(&root);
    if !s.ends_with('\\') {
        s.push('\\');
    }
    Ok(s)
}

/// File system name and ACL support of a volume root, plus whether it is remote.
pub fn volume_info(root: &str) -> io::Result<(String, bool, bool)> {
    let mut r = root.to_string();
    if !r.ends_with('\\') {
        r.push('\\');
    }
    let w = wide_str(&r);
    let mut name = [0u16; 64];
    let mut flags = 0u32;
    // SAFETY: `w` is NUL-terminated; the out buffers are valid.
    unsafe {
        GetVolumeInformationW(
            PCWSTR(w.as_ptr()),
            None,
            None,
            None,
            Some(&mut flags),
            Some(&mut name),
        )
    }
    .map_err(win_err)?;
    let len = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    // SAFETY: `w` is NUL-terminated.
    let remote = unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) } == 4;
    const FILE_PERSISTENT_ACLS: u32 = 0x8;
    Ok((
        String::from_utf16_lossy(&name[..len]),
        flags & FILE_PERSISTENT_ACLS != 0,
        remote,
    ))
}

/// Drive roots of fixed and removable volumes (`C:\`, `D:\`, ...).
pub fn local_drive_roots() -> Vec<String> {
    // SAFETY: no arguments.
    let mask = unsafe { windows::Win32::Storage::FileSystem::GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) != 0 {
            let root = format!("{}:\\", (b'A' + i as u8) as char);
            let w = wide_str(&root);
            // SAFETY: `w` is NUL-terminated.
            let kind = unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) };
            if kind == 2 || kind == 3 {
                out.push(root);
            }
        }
    }
    out
}

/// One directory entry, read without following links.
pub struct Entry {
    pub name: String,
    pub stat: Stat,
}

/// Lists a folder (names sorted, so that plans are deterministic).
pub fn list(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(long(dir))? {
        let e = e?;
        let md = e.metadata()?;
        out.push(Entry {
            name: e.file_name().to_string_lossy().into_owned(),
            stat: Stat::of(&md),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// The reparse tag of a link or placeholder (`None` when it has none).
pub fn reparse_tag(path: &Path) -> Option<u32> {
    use windows::Win32::Storage::FileSystem::{FindClose, FindFirstFileW, WIN32_FIND_DATAW};
    let w = wide(path);
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `w` is NUL-terminated and `data` is a valid out structure.
    let h = unsafe { FindFirstFileW(PCWSTR(w.as_ptr()), &mut data) }.ok()?;
    // SAFETY: the find handle was just opened.
    unsafe {
        let _ = FindClose(h);
    }
    (data.dwFileAttributes & ATTR_REPARSE != 0).then_some(data.dwReserved0)
}

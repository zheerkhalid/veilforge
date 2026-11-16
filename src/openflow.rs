//! `--open <id>` and `--unlock <path>`: ask for the master password, unlock
//! one item temporarily, open it with its normal program, and lock it again
//! automatically.
//!
//! The item is locked again when the first of these happens:
//! * the program that was started for it exits (files), or the Explorer window
//!   showing it is closed (folders);
//! * nobody touched keyboard or mouse for the configured idle time;
//! * Windows is locked, goes to sleep, or the user signs out;
//! * "Lock now" is chosen from the tray icon;
//! * the main window locked it already.
//!
//! The parts without a window ([`resolve`], [`Session`], [`decide`]) are plain
//! functions so that tests and `--selftest` can drive the same code.

use crate::engine::{Env, Error, Vault};
use crate::model::{Kind, State};
use crate::settings::Settings;
use crate::sys::{self, SysEvent, SysWindow};
use crate::ui::prompt::{self, PromptSpec};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What the command line named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// `--open <id>` (what a stand-in shortcut runs).
    Id(String),
    /// `--unlock <path>`: a stand-in shortcut, or the original path of an item.
    Path(PathBuf),
}

fn same_path(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.trim_start_matches(r"\\?\")
            .trim_end_matches('\\')
            .replace('/', "\\")
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// Finds the item a target means. Needs the unlocked map: ids and paths are
/// not stored anywhere in the clear.
pub fn resolve(vault: &Vault, target: &Target) -> Option<String> {
    match target {
        Target::Id(id) => vault.item(id).map(|i| i.id.clone()),
        Target::Path(path) => {
            let text = path.to_string_lossy().to_string();
            if let Some(item) = vault.items().iter().find(|i| {
                same_path(&i.original, &text)
                    || i.standin.as_deref().is_some_and(|s| same_path(s, &text))
            }) {
                return Some(item.id.clone());
            }
            // A stand-in that was moved or copied somewhere else still carries the id.
            let is_lnk = path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
            if is_lnk {
                let id = crate::standin::read(path)
                    .ok()
                    .and_then(|s| crate::standin::id_of(&s))?;
                return vault.item(&id).map(|i| i.id.clone());
            }
            None
        }
    }
}

/// One temporarily unlocked item, owned by this process until it is locked again.
pub struct Session {
    pub vault: Vault,
    pub id: String,
    /// Where the item is right now (its original path).
    pub path: PathBuf,
    pub kind: Kind,
    /// False when the item was already open before this process asked: then
    /// whoever opened it first is responsible for locking it again.
    pub owns_lock: bool,
}

impl Session {
    /// Opens the map with `password` and unlocks the target temporarily.
    pub fn begin(env: Env, password: &str, target: &Target) -> Result<Session, Error> {
        let mut vault = Vault::open(env, password)?;
        let id = resolve(&vault, target).ok_or_else(|| {
            Error::NotFound("This item is not in VeilForge's list any more.".into())
        })?;
        let (kind, original) = {
            let item = vault
                .item(&id)
                .ok_or_else(|| Error::NotFound("This item is not in the list.".into()))?;
            (item.kind, PathBuf::from(&item.original))
        };
        if vault.state(&id) == State::Open {
            return Ok(Session {
                vault,
                id,
                path: original,
                kind,
                owns_lock: false,
            });
        }
        let path = vault.open_temporarily(&id, &mut |_| {})?;
        Ok(Session {
            vault,
            id,
            path,
            kind,
            owns_lock: true,
        })
    }

    /// True while the item is still temporarily open according to the map on
    /// disk (the main window may have locked or released it meanwhile).
    pub fn still_open(&mut self) -> bool {
        let _ = self.vault.refresh();
        self.vault.item(&self.id).is_some() && self.vault.state(&self.id) == State::Open
    }

    /// Locks the item again. `Error::InUse` means a program still has it open.
    pub fn lock_again(&mut self) -> Result<(), Error> {
        if !self.still_open() {
            return Ok(());
        }
        self.vault.relock(&self.id, &mut |_| {})
    }
}

// --------------------------------------------------------------------------------------
// When to lock again
// --------------------------------------------------------------------------------------

/// Why the item is being locked again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    ProgramExited,
    FolderClosed,
    Idle,
    SessionLocked,
    LockNow,
    /// Somebody else (the main window) already locked or released it.
    AlreadyLocked,
}

/// What the watcher has seen so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    /// Seconds since the item was opened.
    pub elapsed: u64,
    /// Seconds without keyboard or mouse input.
    pub idle: u64,
    /// A process handle was returned for the launched program.
    pub has_process: bool,
    /// ... and that process has exited, after living this many seconds.
    pub process_exited_after: Option<u64>,
    /// An Explorer window showing the folder was seen at least once.
    pub folder_seen: bool,
    /// How many checks in a row found no such window (after it was seen).
    pub folder_gone_checks: u32,
    pub lock_now: bool,
    pub session_event: bool,
    pub still_open: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// 0 = never lock because of idleness.
    pub idle_limit_secs: u64,
    pub lock_on_session_events: bool,
}

/// A program that exits this quickly only handed the file to another, already
/// running program (or was a launcher stub): its exit says nothing.
pub const QUICK_EXIT_SECS: u64 = 3;
/// A folder window must be missing for this many checks in a row.
pub const FOLDER_GONE_CHECKS: u32 = 2;

/// The whole decision, as a pure function.
pub fn decide(o: &Observation, p: &Policy) -> Option<Stop> {
    if !o.still_open {
        return Some(Stop::AlreadyLocked);
    }
    if o.lock_now {
        return Some(Stop::LockNow);
    }
    if o.session_event && p.lock_on_session_events {
        return Some(Stop::SessionLocked);
    }
    if let Some(lived) = o.process_exited_after {
        if lived >= QUICK_EXIT_SECS {
            return Some(Stop::ProgramExited);
        }
    }
    if o.folder_seen && o.folder_gone_checks >= FOLDER_GONE_CHECKS {
        return Some(Stop::FolderClosed);
    }
    if p.idle_limit_secs > 0 && o.idle >= p.idle_limit_secs {
        return Some(Stop::Idle);
    }
    None
}

// --------------------------------------------------------------------------------------
// Launching and watching (Windows shell)
// --------------------------------------------------------------------------------------

/// A process started for the item; closed on drop.
pub struct Launched(Option<windows::Win32::Foundation::HANDLE>);

impl Launched {
    pub fn has_process(&self) -> bool {
        self.0.is_some()
    }

    pub fn exited(&self) -> bool {
        use windows::Win32::Foundation::WAIT_OBJECT_0;
        use windows::Win32::System::Threading::WaitForSingleObject;
        match self.0 {
            // SAFETY: the handle is owned by this struct and still open.
            Some(h) => unsafe { WaitForSingleObject(h, 0) == WAIT_OBJECT_0 },
            None => false,
        }
    }
}

impl Drop for Launched {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            // SAFETY: closing a handle this struct owns, once.
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Opens a file with its normal program, or a folder in Explorer.
pub fn launch(path: &Path) -> Result<Launched, String> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
        SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let file = wide(&path.to_string_lossy());
    let dir = wide(
        &path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
    );
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("open"),
        lpFile: PCWSTR(file.as_ptr()),
        lpDirectory: PCWSTR(dir.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` is fully initialised and the strings outlive the call.
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| {
        format!(
            "Windows could not open it ({}). It is unlocked; open it by hand, then choose Lock now.",
            e.message()
        )
    })?;
    Ok(Launched(
        (!info.hProcess.is_invalid() && !info.hProcess.0.is_null()).then_some(info.hProcess),
    ))
}

/// Turns the `file:///C:/...` location of an Explorer window into a path.
pub fn url_to_path(url: &str) -> Option<String> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::UI::Shell::PathCreateFromUrlW;
    if !url.to_ascii_lowercase().starts_with("file:") {
        return None;
    }
    let wurl = wide(url);
    let mut buf = vec![0u16; 33_000];
    let mut len = buf.len() as u32;
    // SAFETY: the buffer is as long as `len` says; the URL is NUL-terminated.
    unsafe {
        PathCreateFromUrlW(PCWSTR(wurl.as_ptr()), PWSTR(buf.as_mut_ptr()), &mut len, 0).ok()?;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// True if `location` is `folder` or something inside it.
pub fn is_inside(location: &str, folder: &str) -> bool {
    let norm = |s: &str| {
        s.trim_start_matches(r"\\?\")
            .trim_end_matches('\\')
            .replace('/', "\\")
            .to_lowercase()
    };
    let (l, f) = (norm(location), norm(folder));
    l == f || l.starts_with(&format!("{f}\\"))
}

/// Whether an Explorer window currently shows `folder` (or something inside it).
/// `None` when the shell could not be asked.
pub fn folder_window_open(folder: &Path) -> Option<bool> {
    use windows::core::{Interface, VARIANT};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{IShellWindows, IWebBrowser2, ShellWindows};
    let folder = folder.to_string_lossy().to_string();
    // SAFETY: standard COM use on this thread; every interface is released by
    // its wrapper. CoInitializeEx is balanced by leaving COM initialised for
    // the life of the (single-purpose) watcher thread.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let windows: IShellWindows =
            CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER).ok()?;
        let count = windows.Count().ok()?;
        for i in 0..count {
            let Ok(disp) = windows.Item(&VARIANT::from(i)) else {
                continue;
            };
            let Ok(browser) = disp.cast::<IWebBrowser2>() else {
                continue;
            };
            let Ok(url) = browser.LocationURL() else {
                continue;
            };
            if let Some(path) = url_to_path(&url.to_string()) {
                if is_inside(&path, &folder) {
                    return Some(true);
                }
            }
        }
        Some(false)
    }
}

// --------------------------------------------------------------------------------------
// The helper process
// --------------------------------------------------------------------------------------

/// Runs the whole flow. Returns the process exit code.
pub fn run(target: Target, icon: egui::IconData) -> i32 {
    let env = Env::real();
    let settings = Settings::load(&env.data_dir);
    let title = settings.title().to_string();
    if !Vault::exists(&env) {
        sys::message_box(
            &title,
            "No master password has been set on this PC yet, so there is nothing to unlock here.",
        );
        return 2;
    }
    let slot: Arc<Mutex<Option<Session>>> = Arc::new(Mutex::new(None));
    let spec = PromptSpec {
        title: title.clone(),
        heading: "Enter your master password".into(),
        text: "This item is protected. It opens after you enter the password and is locked \
               again when you are done."
            .into(),
        button: "Unlock".into(),
    };
    let submit_slot = slot.clone();
    let submit_env = env.clone();
    let ok = prompt::run(
        spec,
        icon,
        Box::new(move |password| {
            let session =
                Session::begin(submit_env.clone(), password, &target).map_err(|e| e.to_string())?;
            if let Ok(mut g) = submit_slot.lock() {
                *g = Some(session);
            }
            Ok(String::new())
        }),
    );
    let session = slot.lock().ok().and_then(|mut g| g.take());
    let Some(mut session) = session else {
        return if ok { 3 } else { 1 };
    };

    let launched = match launch(&session.path) {
        Ok(l) => l,
        Err(text) => {
            sys::message_box(&title, &text);
            Launched(None)
        }
    };
    if !session.owns_lock {
        // Already open: the process that opened it first locks it again.
        return 0;
    }
    watch_and_lock(&mut session, launched, &settings, &title);
    0
}

fn watch_and_lock(session: &mut Session, launched: Launched, settings: &Settings, title: &str) {
    let (tx, rx) = std::sync::mpsc::channel::<SysEvent>();
    let sys_window = SysWindow::start(0, tx, Box::new(|| {}));
    if let Some(w) = &sys_window {
        w.set_tray_title(title);
        w.set_tray(settings.tray_icon);
    }
    // A sign-out or shutdown waits a few seconds for the item to be locked.
    sys::hold_session_end(settings.relock_on_session_lock);
    let policy = Policy {
        idle_limit_secs: u64::from(settings.relock_idle_minutes) * 60,
        lock_on_session_events: settings.relock_on_session_lock,
    };
    let started = Instant::now();
    let mut seen = Observation {
        has_process: launched.has_process(),
        still_open: true,
        ..Default::default()
    };
    let is_folder = session.kind == Kind::Folder;
    let mut tick: u64 = 0;
    let stop = loop {
        std::thread::sleep(Duration::from_millis(500));
        tick += 1;
        while let Ok(event) = rx.try_recv() {
            match event {
                SysEvent::TrayLock | SysEvent::TrayExit => seen.lock_now = true,
                SysEvent::SessionLocked | SysEvent::Suspending | SysEvent::SessionEnding => {
                    seen.session_event = true
                }
                SysEvent::TrayShow | SysEvent::Hotkey => {}
            }
        }
        seen.elapsed = started.elapsed().as_secs();
        seen.idle = sys::idle_seconds();
        if seen.process_exited_after.is_none() && launched.exited() {
            seen.process_exited_after = Some(seen.elapsed);
        }
        // The shell and the map file are asked every two seconds, not every tick.
        if tick.is_multiple_of(4) {
            if is_folder {
                match folder_window_open(&session.path) {
                    Some(true) => {
                        seen.folder_seen = true;
                        seen.folder_gone_checks = 0;
                    }
                    Some(false) if seen.folder_seen => seen.folder_gone_checks += 1,
                    _ => {}
                }
            }
            seen.still_open = session.still_open();
        }
        if let Some(stop) = decide(&seen, &policy) {
            break stop;
        }
    };
    if stop == Stop::AlreadyLocked {
        sys::session_end_done();
        return;
    }
    // Lock again. A program may still hold a file for a moment after its
    // window closed, so "in use" is retried; the user is told once.
    let urgent = stop == Stop::SessionLocked;
    let mut told = false;
    let mut tries = 0u32;
    loop {
        match session.lock_again() {
            Ok(()) => break,
            Err(Error::InUse(_)) => {
                tries += 1;
                if urgent && tries > 20 {
                    break;
                }
                if !urgent && tries == 6 && !told {
                    told = true;
                    sys::message_box(
                        title,
                        "The item is still open in a program, so it cannot be locked yet.\n\n\
                         Close that program. VeilForge keeps trying and locks the item as \
                         soon as it is free.",
                    );
                }
                std::thread::sleep(Duration::from_secs(if urgent { 1 } else { 3 }));
            }
            Err(e) => {
                sys::message_box(
                    title,
                    &format!(
                        "The item could not be locked again:\n\n{e}\n\nIt is still unlocked. \
                         Open VeilForge to lock it."
                    ),
                );
                break;
            }
        }
    }
    sys::session_end_done();
    drop(sys_window);
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: Policy = Policy {
        idle_limit_secs: 600,
        lock_on_session_events: true,
    };

    fn open() -> Observation {
        Observation {
            still_open: true,
            ..Default::default()
        }
    }

    #[test]
    fn nothing_happened_keeps_waiting() {
        assert_eq!(decide(&open(), &POLICY), None);
    }

    #[test]
    fn program_exit_locks_but_a_launcher_stub_does_not() {
        let mut o = open();
        o.has_process = true;
        o.process_exited_after = Some(1);
        assert_eq!(
            decide(&o, &POLICY),
            None,
            "handed over to a running program"
        );
        o.process_exited_after = Some(40);
        assert_eq!(decide(&o, &POLICY), Some(Stop::ProgramExited));
    }

    #[test]
    fn folder_must_be_seen_and_then_gone_twice() {
        let mut o = open();
        o.folder_gone_checks = 5;
        assert_eq!(decide(&o, &POLICY), None, "never seen: no conclusion");
        o.folder_seen = true;
        o.folder_gone_checks = 1;
        assert_eq!(decide(&o, &POLICY), None);
        o.folder_gone_checks = 2;
        assert_eq!(decide(&o, &POLICY), Some(Stop::FolderClosed));
    }

    #[test]
    fn idle_session_and_lock_now() {
        let mut o = open();
        o.idle = 599;
        assert_eq!(decide(&o, &POLICY), None);
        o.idle = 600;
        assert_eq!(decide(&o, &POLICY), Some(Stop::Idle));
        let never = Policy {
            idle_limit_secs: 0,
            lock_on_session_events: false,
        };
        assert_eq!(decide(&o, &never), None);
        o.session_event = true;
        assert_eq!(decide(&o, &never), None, "the setting is off");
        assert_eq!(decide(&o, &POLICY), Some(Stop::SessionLocked));
        o.lock_now = true;
        assert_eq!(decide(&o, &never), Some(Stop::LockNow));
    }

    #[test]
    fn locked_by_someone_else_ends_the_watch() {
        let mut o = open();
        o.still_open = false;
        o.lock_now = true;
        assert_eq!(decide(&o, &POLICY), Some(Stop::AlreadyLocked));
    }

    #[test]
    fn explorer_locations() {
        assert!(is_inside(r"C:\Temp\Box", r"C:\Temp\Box"));
        assert!(is_inside(r"c:\temp\box\Inner", r"C:\Temp\Box\"));
        assert!(!is_inside(r"C:\Temp\Boxes", r"C:\Temp\Box"));
        assert!(!is_inside(r"C:\Temp", r"C:\Temp\Box"));
        assert_eq!(
            url_to_path("file:///C:/Temp/My%20Box").as_deref(),
            Some(r"C:\Temp\My Box")
        );
        assert_eq!(url_to_path("https://example.com/"), None);
    }

    #[test]
    fn paths_compare_without_case_or_prefix() {
        assert!(same_path(r"\\?\C:\Temp\A.txt", r"c:\temp\a.TXT"));
        assert!(!same_path(r"C:\Temp\A.txt", r"C:\Temp\B.txt"));
    }
}

//! Small pieces of Windows: idle time, Caps Lock, time zone, opening links,
//! the "start with Windows" entry, and a background message window for
//! session lock / sleep / sign-out notifications, the global hotkey and the
//! tray icon. The background window also works in a process that has no main
//! window (the `--open` helper).

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Condvar, Mutex};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetLastInputInfo, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
    LASTINPUTINFO, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, VK_CAPITAL,
};
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

// --------------------------------------------------------------------------------------
// Simple queries
// --------------------------------------------------------------------------------------

/// Seconds since the last keyboard or mouse input anywhere in the session.
pub fn idle_seconds() -> u64 {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: the structure is correctly sized and initialised.
    unsafe {
        if !GetLastInputInfo(&mut info).as_bool() {
            return 0;
        }
        u64::from(GetTickCount().wrapping_sub(info.dwTime)) / 1000
    }
}

pub fn caps_lock_on() -> bool {
    // SAFETY: plain key state query.
    unsafe { GetKeyState(i32::from(VK_CAPITAL.0)) & 1 != 0 }
}

/// The current offset of local time from UTC, in seconds.
pub fn local_utc_offset() -> i64 {
    let mut tz = TIME_ZONE_INFORMATION::default();
    // SAFETY: tz is a valid out pointer.
    let id = unsafe { GetTimeZoneInformation(&mut tz) };
    let bias = match id {
        2 => tz.Bias + tz.DaylightBias,
        1 => tz.Bias + tz.StandardBias,
        _ => tz.Bias,
    };
    -i64::from(bias) * 60
}

fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

/// Opens a web address in the default browser. Only `http://` and
/// `https://` are ever opened; anything else is refused.
pub fn open_url(url: &str) -> Result<(), String> {
    let u = url.trim();
    let lower = u.to_lowercase();
    let full = if lower.starts_with("https://") || lower.starts_with("http://") {
        u.to_string()
    } else if !u.contains("://") && !u.contains(' ') && u.contains('.') && !u.starts_with('-') {
        format!("https://{u}")
    } else {
        return Err("Only web addresses (http or https) can be opened.".into());
    };
    if full.chars().any(|c| c.is_control() || c == '"') {
        return Err("This address contains characters that are not allowed.".into());
    }
    shell_open(&full)
}

/// Opens a folder in File Explorer.
pub fn open_folder(path: &std::path::Path) -> Result<(), String> {
    if !path.is_dir() {
        return Err("That folder does not exist.".into());
    }
    shell_open(&path.to_string_lossy())
}

fn shell_open(target: &str) -> Result<(), String> {
    shell_execute("open", target, "")
}

fn shell_execute(verb: &str, target: &str, arguments: &str) -> Result<(), String> {
    let (verb, target, arguments) = (
        HSTRING::from(verb),
        HSTRING::from(target),
        HSTRING::from(arguments),
    );
    // SAFETY: valid strings; the return value is an error code when <= 32.
    let r = unsafe {
        ShellExecuteW(
            HWND::default(),
            &verb,
            &target,
            &arguments,
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if r.0 as isize > 32 {
        Ok(())
    } else {
        Err("Windows could not open it.".into())
    }
}

/// Opens File Explorer with the file or folder selected (or its parent
/// folder when the item itself is not there).
pub fn reveal_in_explorer(path: &std::path::Path) -> Result<(), String> {
    if path.exists() {
        let text = path.to_string_lossy();
        if text.contains('"') {
            return Err("That path contains characters that are not allowed.".into());
        }
        return shell_execute("open", "explorer.exe", &format!("/select,\"{text}\""));
    }
    match path.parent() {
        Some(parent) if parent.is_dir() => shell_open(&parent.to_string_lossy()),
        _ => Err("That folder does not exist.".into()),
    }
}

/// Opens a web page that VeilForge itself just saved (the emergency sheet) in
/// the default browser, so it can be printed. Only `.html` files are opened.
pub fn open_saved_page(path: &std::path::Path) -> Result<(), String> {
    let is_html = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"));
    if !is_html || !path.is_file() {
        return Err("Only a saved web page can be opened.".into());
    }
    shell_open(&path.to_string_lossy())
}

/// Starts this program again with administrator rights (Windows asks first).
/// `arguments` is passed on as written.
pub fn restart_as_admin(arguments: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    shell_execute("runas", &exe.to_string_lossy(), arguments)
        .map_err(|_| "Windows did not start VeilForge as administrator.".to_string())
}

/// Whether this process already runs with administrator rights.
pub fn is_elevated() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: the token handle is closed again; the buffer is correctly sized.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(std::ptr::from_mut(&mut elevation).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

// --------------------------------------------------------------------------------------
// Start with Windows
// --------------------------------------------------------------------------------------

/// The per-user "run at sign-in" key. Tests pass a throwaway key instead.
pub const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "VeilForge";
/// The command-line flag the startup entry passes: start hidden in the tray.
pub const STARTUP_ARG: &str = "--tray";

/// The command line written to the startup entry.
pub fn startup_command(exe: &std::path::Path) -> String {
    format!("\"{}\" {STARTUP_ARG}", exe.display())
}

/// Whether the startup entry exists below `run_key` (in HKEY_CURRENT_USER).
pub fn startup_enabled(run_key: &str) -> bool {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(run_key)
        .and_then(|k| k.get_value::<String, _>(RUN_VALUE))
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

/// Adds or removes the startup entry (the value `VeilForge`, nothing else).
pub fn set_startup(run_key: &str, on: bool, exe: &std::path::Path) -> Result<(), String> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let problem = |e: std::io::Error| format!("Could not change the startup entry: {e}");
    if on {
        let (key, _) = hkcu.create_subkey(run_key).map_err(problem)?;
        key.set_value(RUN_VALUE, &startup_command(exe))
            .map_err(problem)
    } else {
        match hkcu.open_subkey_with_flags(run_key, winreg::enums::KEY_SET_VALUE) {
            Ok(key) => match key.delete_value(RUN_VALUE) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(problem(e)),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(problem(e)),
        }
    }
}

// --------------------------------------------------------------------------------------
// Windows of this process
// --------------------------------------------------------------------------------------

pub fn minimize(raw: isize) {
    // SAFETY: a window handle of this process.
    unsafe {
        let _ = ShowWindow(hwnd(raw), SW_MINIMIZE);
    }
}

pub fn is_minimized(raw: isize) -> bool {
    // SAFETY: a window handle of this process.
    unsafe { IsIconic(hwnd(raw)).as_bool() }
}

/// Shows the window (also when it was hidden to the tray or minimised) and
/// brings it to the front.
pub fn show_and_focus(raw: isize) {
    if raw == 0 {
        return;
    }
    // SAFETY: a window handle of this process.
    unsafe {
        if !IsWindowVisible(hwnd(raw)).as_bool() {
            let _ = ShowWindow(hwnd(raw), SW_SHOW);
        }
        if IsIconic(hwnd(raw)).as_bool() {
            let _ = ShowWindow(hwnd(raw), SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd(raw));
    }
}

/// Hides the window completely (no taskbar button). [`show_and_focus`] brings it back.
pub fn hide_window(raw: isize) {
    // SAFETY: a window handle of this process.
    unsafe {
        let _ = ShowWindow(hwnd(raw), SW_HIDE);
    }
}

pub fn is_visible(raw: isize) -> bool {
    // SAFETY: a window handle of this process.
    unsafe { IsWindowVisible(hwnd(raw)).as_bool() }
}

/// Moves the window to the middle of the work area of the primary screen.
pub fn center_on_screen(raw: isize) {
    // SAFETY: valid out pointers; a window handle of this process.
    unsafe {
        let mut window = RECT::default();
        let mut area = RECT::default();
        if GetWindowRect(hwnd(raw), &mut window).is_err()
            || SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some(std::ptr::from_mut(&mut area).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
            .is_err()
        {
            return;
        }
        let (w, h) = (window.right - window.left, window.bottom - window.top);
        let x = area.left + ((area.right - area.left) - w).max(0) / 2;
        let y = area.top + ((area.bottom - area.top) - h).max(0) / 2;
        let _ = SetWindowPos(
            hwnd(raw),
            HWND::default(),
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Asks the window to close, as if the user had clicked the close button.
pub fn close_window(raw: isize) {
    // SAFETY: posting to a window of this process; a stale handle just fails.
    unsafe {
        let _ = PostMessageW(hwnd(raw), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

/// A named mutex that exists for as long as the returned handle is kept.
/// Setup programs use it to notice that VeilForge is running.
pub struct AppMutex(windows::Win32::Foundation::HANDLE);

// SAFETY: a mutex handle may be used and closed from any thread.
unsafe impl Send for AppMutex {}

impl Drop for AppMutex {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

pub fn app_mutex(name: &str) -> Option<AppMutex> {
    let name = HSTRING::from(name);
    // SAFETY: plain Win32 call with a valid name.
    unsafe { windows::Win32::System::Threading::CreateMutexW(None, false, &name) }
        .ok()
        .map(AppMutex)
}

/// Like [`app_mutex`], but returns `None` when another process already holds
/// a mutex with this name (VeilForge is already running).
pub fn first_instance(name: &str) -> Option<AppMutex> {
    let name = HSTRING::from(name);
    // SAFETY: plain Win32 calls; the last error is read right after the call.
    unsafe {
        let handle = windows::Win32::System::Threading::CreateMutexW(None, false, &name).ok()?;
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        let mutex = AppMutex(handle);
        if exists {
            None
        } else {
            Some(mutex)
        }
    }
}

/// Asks the VeilForge window that is already running to show itself.
/// Returns false when none was found.
pub fn signal_running_instance() -> bool {
    // SAFETY: plain Win32 calls with constant strings.
    unsafe {
        match FindWindowW(SYS_CLASS, MAIN_TITLE) {
            Ok(window) if !window.is_invalid() => {
                PostMessageW(window, WM_SHOW_MAIN, WPARAM(0), LPARAM(0)).is_ok()
            }
            _ => false,
        }
    }
}

/// A plain message box, for errors before the window exists.
pub fn message_box(title: &str, text: &str) {
    let (title, text) = (HSTRING::from(title), HSTRING::from(text));
    // SAFETY: valid strings.
    unsafe {
        MessageBoxW(HWND::default(), &text, &title, MB_OK | MB_ICONWARNING);
    }
}

// --------------------------------------------------------------------------------------
// Hotkey text
// --------------------------------------------------------------------------------------

/// A parsed hotkey: modifier flags and a virtual-key code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: u32,
    pub vk: u32,
}

/// Parses text such as `Ctrl+Alt+A` or `Ctrl+Shift+F9`. At least one of
/// Ctrl / Alt / Win is required so a plain letter can never be captured.
pub fn parse_hotkey(text: &str) -> Option<Hotkey> {
    let mut modifiers = 0u32;
    let mut vk = None;
    for part in text.split('+') {
        let p = part.trim().to_uppercase();
        match p.as_str() {
            "CTRL" | "CONTROL" => modifiers |= MOD_CONTROL.0,
            "ALT" => modifiers |= MOD_ALT.0,
            "SHIFT" => modifiers |= MOD_SHIFT.0,
            "WIN" | "WINDOWS" => modifiers |= MOD_WIN.0,
            key => {
                if vk.is_some() {
                    return None;
                }
                let bytes = key.as_bytes();
                vk = Some(if bytes.len() == 1 && bytes[0].is_ascii_alphanumeric() {
                    u32::from(bytes[0])
                } else {
                    let n: u32 = key.strip_prefix('F')?.parse().ok()?;
                    if !(1..=24).contains(&n) {
                        return None;
                    }
                    0x70 + n - 1
                });
            }
        }
    }
    if modifiers & (MOD_CONTROL.0 | MOD_ALT.0 | MOD_WIN.0) == 0 {
        return None;
    }
    Some(Hotkey { modifiers, vk: vk? })
}

// --------------------------------------------------------------------------------------
// Background message window
// --------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SysEvent {
    /// Windows was locked, the user switched, or the session disconnected.
    SessionLocked,
    /// The PC is going to sleep.
    Suspending,
    /// The user is signing out, or Windows is shutting down or restarting.
    SessionEnding,
    /// The global hotkey was pressed.
    Hotkey,
    /// "Open" in the tray menu, a click on the tray icon, or a second start
    /// of the program. The main window (if any) has already been shown.
    TrayShow,
    TrayLock,
    TrayExit,
}

const WM_TRAY: u32 = WM_APP + 1;
const WM_SET_HOTKEY: u32 = WM_APP + 2;
const WM_SET_TRAY: u32 = WM_APP + 3;
const WM_QUIT_THREAD: u32 = WM_APP + 4;
const WM_SET_TITLE: u32 = WM_APP + 5;
const WM_SHOW_MAIN: u32 = WM_APP + 6;
const HOTKEY_ID: i32 = 1;
const MENU_SHOW: usize = 1;
const MENU_LOCK: usize = 2;
const MENU_EXIT: usize = 3;
const SYS_CLASS: PCWSTR = w!("VeilForgeSysWindow");
/// Window title of the background window that belongs to the main window.
const MAIN_TITLE: PCWSTR = w!("VeilForge.Main");
const HELPER_TITLE: PCWSTR = w!("VeilForge.Helper");
/// How long sign-out or shutdown is held back while items are locked again.
const SESSION_END_WAIT: std::time::Duration = std::time::Duration::from_secs(4);

struct State {
    tx: Sender<SysEvent>,
    wake: Box<dyn Fn() + Send>,
    main_window: isize,
    tray: bool,
    title: String,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static HOLD_SESSION_END: AtomicBool = AtomicBool::new(false);
static SESSION_END_DONE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

/// While this is on, a sign-out or shutdown is held back for a few seconds
/// after [`SysEvent::SessionEnding`] was sent, until [`session_end_done`] is
/// called. Switch it on only while there is something to lock again.
pub fn hold_session_end(on: bool) {
    HOLD_SESSION_END.store(on, Ordering::SeqCst);
}

/// Tells the background window that the work started by
/// [`SysEvent::SessionEnding`] is finished, so Windows may go on.
pub fn session_end_done() {
    if let Ok(mut done) = SESSION_END_DONE.0.lock() {
        *done = true;
    }
    SESSION_END_DONE.1.notify_all();
}

fn wait_for_session_end() {
    if !HOLD_SESSION_END.load(Ordering::SeqCst) {
        return;
    }
    if let Ok(done) = SESSION_END_DONE.0.lock() {
        let _ = SESSION_END_DONE
            .1
            .wait_timeout_while(done, SESSION_END_WAIT, |done| !*done);
    }
}

fn emit(event: SysEvent) {
    if let Ok(guard) = STATE.lock() {
        if let Some(s) = guard.as_ref() {
            let _ = s.tx.send(event);
            (s.wake)();
        }
    }
}

fn state_title() -> String {
    STATE
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.title.clone()))
        .unwrap_or_else(|| "VeilForge".into())
}

fn main_window() -> isize {
    STATE
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.main_window))
        .unwrap_or(0)
}

unsafe fn tray_data(window: HWND) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        ..Default::default()
    };
    let module = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
    // Icon resource 1 is the application icon embedded by the build script.
    data.hIcon = LoadIconW(HINSTANCE(module.0), PCWSTR(std::ptr::without_provenance(1)))
        .or_else(|_| LoadIconW(HINSTANCE::default(), IDI_APPLICATION))
        .unwrap_or_default();
    let limit = data.szTip.len() - 1;
    for (i, c) in state_title().encode_utf16().take(limit).enumerate() {
        data.szTip[i] = c;
    }
    data
}

unsafe fn set_tray(window: HWND, on: bool) {
    let was = STATE
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.tray))
        .unwrap_or(false);
    if on == was {
        return;
    }
    let data = tray_data(window);
    let _ = Shell_NotifyIconW(if on { NIM_ADD } else { NIM_DELETE }, &data);
    if let Ok(mut g) = STATE.lock() {
        if let Some(s) = g.as_mut() {
            s.tray = on;
        }
    }
}

unsafe fn tray_menu(window: HWND) {
    let Ok(menu) = CreatePopupMenu() else { return };
    // The helper process has no window to open: it only offers "Lock now".
    let has_main = main_window() != 0;
    if has_main {
        let open = HSTRING::from(format!("Open {}", state_title()));
        let _ = AppendMenuW(menu, MF_STRING, MENU_SHOW, &open);
    }
    let _ = AppendMenuW(menu, MF_STRING, MENU_LOCK, w!("Lock now"));
    if has_main {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Exit"));
    }
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    // Needed so the menu closes when the user clicks elsewhere.
    let _ = SetForegroundWindow(window);
    let choice = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
        pt.x,
        pt.y,
        0,
        window,
        None,
    );
    let _ = DestroyMenu(menu);
    match choice.0 as usize {
        MENU_SHOW => show_main(),
        MENU_LOCK => emit(SysEvent::TrayLock),
        MENU_EXIT => emit(SysEvent::TrayExit),
        _ => {}
    }
}

fn show_main() {
    show_and_focus(main_window());
    emit(SysEvent::TrayShow);
}

unsafe extern "system" fn wnd_proc(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_WTSSESSION_CHANGE => {
            // WTS_CONSOLE_DISCONNECT 2, WTS_REMOTE_DISCONNECT 4, WTS_SESSION_LOGOFF 6, WTS_SESSION_LOCK 7
            if matches!(wparam.0, 2 | 4 | 6 | 7) {
                emit(SysEvent::SessionLocked);
            }
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            // PBT_APMSUSPEND
            if wparam.0 == 4 {
                emit(SysEvent::Suspending);
            }
            LRESULT(1)
        }
        WM_QUERYENDSESSION => {
            if let Ok(mut done) = SESSION_END_DONE.0.lock() {
                *done = false;
            }
            emit(SysEvent::SessionEnding);
            wait_for_session_end();
            // Never a veto: the answer only says "this program is ready".
            LRESULT(1)
        }
        WM_ENDSESSION => {
            // The session really ends (wparam) and the work may still be
            // running: give it one more short chance.
            if wparam.0 != 0 {
                wait_for_session_end();
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            emit(SysEvent::Hotkey);
            LRESULT(0)
        }
        WM_SHOW_MAIN => {
            show_main();
            LRESULT(0)
        }
        WM_TRAY => {
            match lparam.0 as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => show_main(),
                WM_RBUTTONUP | WM_CONTEXTMENU => tray_menu(window),
                _ => {}
            }
            LRESULT(0)
        }
        WM_SET_HOTKEY => {
            let _ = UnregisterHotKey(window, HOTKEY_ID);
            if lparam.0 != 0 {
                let _ = RegisterHotKey(
                    window,
                    HOTKEY_ID,
                    HOT_KEY_MODIFIERS(wparam.0 as u32) | MOD_NOREPEAT,
                    lparam.0 as u32,
                );
            }
            LRESULT(0)
        }
        WM_SET_TRAY => {
            set_tray(window, wparam.0 != 0);
            LRESULT(0)
        }
        WM_SET_TITLE => {
            let shown = STATE
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|s| s.tray))
                .unwrap_or(false);
            if shown {
                let data = tray_data(window);
                let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
            }
            LRESULT(0)
        }
        WM_QUIT_THREAD => {
            let _ = UnregisterHotKey(window, HOTKEY_ID);
            set_tray(window, false);
            let _ = WTSUnRegisterSessionNotification(window);
            let _ = DestroyWindow(window);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(window, msg, wparam, lparam),
    }
}

/// Handle to the background message window.
pub struct SysWindow {
    window: isize,
}

impl SysWindow {
    /// Starts the background thread. `wake` is called after every event so
    /// the receiver runs and picks it up. `main_window` is the window that
    /// "Open" in the tray menu shows; pass 0 in a process without one (the
    /// tray menu then only offers "Lock now"). One per process.
    pub fn start(
        main_window: isize,
        tx: Sender<SysEvent>,
        wake: Box<dyn Fn() + Send>,
    ) -> Option<SysWindow> {
        if let Ok(mut g) = STATE.lock() {
            *g = Some(State {
                tx,
                wake,
                main_window,
                tray: false,
                title: "VeilForge".into(),
            });
        }
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<isize>();
        std::thread::Builder::new()
            .name("veilforge-sys".into())
            .spawn(move || {
                // SAFETY: a standard window class, window and message loop,
                // all owned by this thread.
                unsafe {
                    let module = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
                    let class = WNDCLASSW {
                        lpfnWndProc: Some(wnd_proc),
                        hInstance: HINSTANCE(module.0),
                        lpszClassName: SYS_CLASS,
                        ..Default::default()
                    };
                    RegisterClassW(&class);
                    // A hidden top-level window (not message-only: power and
                    // session broadcasts only reach top-level windows).
                    let window = CreateWindowExW(
                        WS_EX_TOOLWINDOW,
                        SYS_CLASS,
                        if main_window != 0 {
                            MAIN_TITLE
                        } else {
                            HELPER_TITLE
                        },
                        WS_POPUP,
                        0,
                        0,
                        0,
                        0,
                        HWND::default(),
                        HMENU::default(),
                        HINSTANCE(module.0),
                        None,
                    );
                    let Ok(window) = window else {
                        let _ = ready_tx.send(0);
                        return;
                    };
                    let _ = WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION);
                    let _ = ready_tx.send(window.0 as isize);
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            })
            .ok()?;
        let window = ready_rx.recv().ok().filter(|w| *w != 0)?;
        Some(SysWindow { window })
    }

    fn post(&self, msg: u32, wparam: usize, lparam: isize) {
        // SAFETY: posting to our own window; a stale handle just fails.
        unsafe {
            let _ = PostMessageW(hwnd(self.window), msg, WPARAM(wparam), LPARAM(lparam));
        }
    }

    /// Registers (or with `None`, removes) the global hotkey.
    pub fn set_hotkey(&self, hotkey: Option<Hotkey>) {
        match hotkey {
            Some(h) => self.post(WM_SET_HOTKEY, h.modifiers as usize, h.vk as isize),
            None => self.post(WM_SET_HOTKEY, 0, 0),
        }
    }

    pub fn set_tray(&self, on: bool) {
        self.post(WM_SET_TRAY, usize::from(on), 0);
    }

    /// The name shown in the tray tooltip and the tray menu ("VeilForge", or
    /// the neutral title). May be called before or after [`SysWindow::set_tray`].
    pub fn set_tray_title(&self, text: &str) {
        if let Ok(mut g) = STATE.lock() {
            if let Some(s) = g.as_mut() {
                s.title = text.to_string();
            }
        }
        self.post(WM_SET_TITLE, 0, 0);
    }
}

impl Drop for SysWindow {
    fn drop(&mut self) {
        self.post(WM_QUIT_THREAD, 0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_text() {
        let h = parse_hotkey("Ctrl+Alt+A").unwrap();
        assert_eq!(h.modifiers, MOD_CONTROL.0 | MOD_ALT.0);
        assert_eq!(h.vk, 0x41);
        assert_eq!(parse_hotkey(" ctrl + shift + f9 ").unwrap().vk, 0x78);
        assert_eq!(parse_hotkey("Win+Alt+7").unwrap().vk, 0x37);
        for bad in [
            "A",
            "Shift+A",
            "Ctrl+",
            "Ctrl+Alt",
            "Ctrl+A+B",
            "Ctrl+F25",
            "Ctrl+Space",
            "",
        ] {
            assert!(parse_hotkey(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn urls_are_filtered() {
        for bad in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "calc.exe /x",
            "ms-settings:",
            "-arg.example",
            "",
            "C:\\Windows\\notepad.exe a",
        ] {
            assert!(open_url(bad).is_err(), "{bad}");
        }
        assert!(local_utc_offset().abs() <= 16 * 3600);
        let _ = idle_seconds();
        let _ = caps_lock_on();
        let _ = is_elevated();
    }

    #[test]
    fn startup_entry_round_trip_in_a_throwaway_key() {
        // Never the real Run key: a key of our own that is removed again.
        let key = format!("Software\\VeilForgeTest-{}", std::process::id());
        let exe = std::path::Path::new("C:\\Program Files\\VeilForge\\VeilForge.exe");
        assert!(!startup_enabled(&key));
        set_startup(&key, false, exe).unwrap();
        set_startup(&key, true, exe).unwrap();
        assert!(startup_enabled(&key));
        let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        let value: String = hkcu
            .open_subkey(&key)
            .unwrap()
            .get_value("VeilForge")
            .unwrap();
        assert_eq!(
            value,
            "\"C:\\Program Files\\VeilForge\\VeilForge.exe\" --tray"
        );
        set_startup(&key, false, exe).unwrap();
        assert!(!startup_enabled(&key));
        hkcu.delete_subkey_all(&key).unwrap();
    }

    #[test]
    fn session_end_is_not_held_unless_asked() {
        let started = std::time::Instant::now();
        hold_session_end(false);
        wait_for_session_end();
        assert!(started.elapsed().as_millis() < 500);
        hold_session_end(true);
        session_end_done();
        wait_for_session_end();
        assert!(started.elapsed().as_millis() < 1500);
        hold_session_end(false);
    }
}

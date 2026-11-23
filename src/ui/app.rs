//! Application state, background-work plumbing and everything that sits
//! above the individual screens: locking, the tray, toasts, screenshots.
//!
//! The unlocked [`Vault`] lives in [`Core`], behind a mutex. The window never
//! locks that mutex for long: every engine call runs on a worker thread and
//! the window draws from a [`Snapshot`]. A small controller thread handles
//! Windows events (session lock, sleep, sign-out, tray, hotkey) and the idle
//! timers, so they keep working while the window is hidden in the tray and
//! does not draw.

use super::main_view::Main;
use super::ops::Snapshot;
use super::start::{CreateState, UnlockState};
use super::theme::{self, pal_ctx, Icon};
use crate::demo::DemoVault;
use crate::engine::{self, Env, Error, Precheck, Vault};
use crate::settings::Settings;
use crate::sys::{self, HotkeyKind, SysEvent, SysWindow};
use eframe::egui::{
    self, vec2, Align, Align2, CornerRadius, Frame, Layout, Margin, RichText, Stroke,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// How the app was started.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Show invented items; nothing on this PC is read or changed.
    pub demo: bool,
    /// Render one frame of the chosen view to this PNG (1280x800), then exit.
    pub screenshot: Option<PathBuf>,
    /// Which screen to open first (see `--help`).
    pub view: Option<String>,
    /// Force the light theme (for screenshots).
    pub light: bool,
    /// Start hidden in the tray (from the startup entry).
    pub start_hidden: bool,
    /// Paths to offer in the "Protect" dialog right after unlocking (`--lock <path>`).
    pub add: Vec<PathBuf>,
    /// Tests only: use this engine environment instead of the real one.
    pub sandbox: Option<crate::engine::Env>,
}

/// The views `--view` understands (demo mode and screenshots).
pub const VIEWS: &[&str] = &[
    "welcome",
    "unlock",
    "main",
    "details",
    "selection",
    "protect",
    "prompt",
    "health",
    "panic",
    "settings",
    "about",
    "empty",
    "results",
    "change-password",
    "locked-open",
    "settings-log",
    "notice",
    "kurdish",
];

/// How the map was opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnlockKind {
    Create,
    Open,
    Rebuild,
}

/// Results coming back from worker threads.
pub enum Msg {
    /// Create / open / rebuild finished. The number is how many items a
    /// rebuild found. The last value is the attempt it answers.
    Unlocked(Result<(Snapshot, usize), Error>, UnlockKind, u64),
    Done(Box<super::ops::Outcome>, Option<Snapshot>),
    /// A periodic look at the map (`None`: it was busy or locked).
    Snapshot(Option<Snapshot>),
    Precheck(u64, usize, Box<Precheck>),
    /// Something happened in the background (auto-lock, re-lock): a toast,
    /// and the list should be read again.
    Background(String, bool),
    /// The window was asked to show itself (tray, second start).
    Shown,
    Toast(String, bool),
}

/// Where to go next; returned by the screens.
pub enum Go {
    /// The map is open. `fresh`: it was just created.
    Opened {
        snapshot: Snapshot,
        kind: UnlockKind,
        found: usize,
    },
    /// Lock the app (back to the unlock screen).
    Lock,
    /// Show "rebuild from recovery copies".
    ShowRebuild,
    /// Back to the first screen: unlock, or first run when there is no map.
    ShowStart,
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub born: Instant,
}

struct Shot {
    path: PathBuf,
    frames: u32,
    requested: bool,
}

/// What the window and the background threads share.
pub struct Core {
    /// The unlocked map. `None` while the app is locked, except that it is
    /// kept while items are temporarily open, so that they can still be
    /// locked again automatically (see [`Core::lock_app`]).
    pub vault: Mutex<Option<Vault>>,
    settings: Mutex<Settings>,
    /// The window must show the unlock screen.
    pub ui_locked: AtomicBool,
    /// "Exit" was chosen in the tray menu: closing must really close.
    pub quit: AtomicBool,
    sys: Mutex<Option<SysWindow>>,
    hwnd: AtomicIsize,
    /// How many items are temporarily open, as far as this process knows.
    /// Kept up to date by the controller thread for the lock screen and the
    /// tray icon, which must not wait for the map.
    open_items: AtomicUsize,
    /// The real program (not a test, not the demo): other VeilForge
    /// processes of this user may be asked to lock their items too.
    real: bool,
}

fn relax<T>(result: Result<T, std::sync::PoisonError<T>>) -> T {
    result.unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn has_open(vault: &Vault) -> bool {
    vault.items().iter().any(|i| i.open_since.is_some())
}

impl Core {
    fn new(settings: Settings) -> Core {
        Core {
            vault: Mutex::new(None),
            settings: Mutex::new(settings),
            ui_locked: AtomicBool::new(true),
            quit: AtomicBool::new(false),
            sys: Mutex::new(None),
            hwnd: AtomicIsize::new(0),
            open_items: AtomicUsize::new(0),
            real: false,
        }
    }

    /// How many items are temporarily open right now (see `open_items`).
    pub fn open_items(&self) -> usize {
        self.open_items.load(Ordering::SeqCst)
    }

    fn count_open(&self, guard: &MutexGuard<'_, Option<Vault>>) {
        let open = guard.as_ref().map_or(0, |v| {
            v.items().iter().filter(|i| i.open_since.is_some()).count()
        });
        self.open_items.store(open, Ordering::SeqCst);
    }

    /// "VeilForge: 2 items are temporarily open", for the tray icon. With the
    /// neutral title switched on the icon says nothing but "Files".
    fn tray_tip(&self) -> String {
        let settings = self.settings();
        let title = settings.title();
        if settings.neutral_title {
            return title.to_string();
        }
        match self.open_items() {
            0 if self.ui_locked.load(Ordering::SeqCst) => format!("{title}: locked"),
            0 => format!("{title}: unlocked, nothing is open"),
            1 => format!("{title}: 1 item is temporarily open"),
            n => format!("{title}: {n} items are temporarily open"),
        }
    }

    pub fn lock_vault(&self) -> MutexGuard<'_, Option<Vault>> {
        relax(self.vault.lock())
    }

    pub fn settings(&self) -> Settings {
        relax(self.settings.lock()).clone()
    }

    fn set_settings(&self, settings: &Settings) {
        *relax(self.settings.lock()) = settings.clone();
    }

    fn with_sys(&self, f: impl FnOnce(&SysWindow)) {
        if let Some(sys) = relax(self.sys.lock()).as_ref() {
            f(sys);
        }
    }

    /// Locks the app: the window goes back to the unlock screen. The map is
    /// dropped, unless items are temporarily open: then it is kept (only for
    /// locking them again later) and dropped as soon as nothing is open.
    pub fn lock_app(&self, guard: &mut MutexGuard<'_, Option<Vault>>) {
        self.ui_locked.store(true, Ordering::SeqCst);
        if guard.as_ref().is_some_and(|v| !has_open(v)) {
            **guard = None;
        }
    }

    /// Sign-out is only held back while there is something to lock again.
    fn update_session_hold(&self, guard: &MutexGuard<'_, Option<Vault>>) {
        let wanted = self.settings().relock_on_session_lock && guard.as_ref().is_some_and(has_open);
        if self.real {
            sys::hold_session_end(wanted);
        }
        self.count_open(guard);
    }

    /// "Lock everything now" (tray menu, lock hotkey, `--lock-now`, the button
    /// on the lock screen): every temporarily open item is protected again
    /// and the window goes back to the unlock screen. Returns what to tell
    /// the user. Waits for a running operation.
    pub fn lock_everything(&self, why: &str) -> (String, bool) {
        let mut guard = self.lock_vault();
        let mut note = None;
        if let Some(vault) = guard.as_mut() {
            let (locked, failed) = relock_everything(vault);
            note = relock_text(locked, failed, why);
        }
        self.lock_app(&mut guard);
        self.update_session_hold(&guard);
        drop(guard);
        if self.real {
            // An item opened through its stand-in while this window was
            // locked is held open by a helper process: ask those too.
            sys::broadcast_lock_now(false);
        }
        note.unwrap_or(("Locked.".into(), false))
    }

    /// Windows was locked, is going to sleep, or the user is signing out.
    fn session_event(&self) -> Option<(String, bool)> {
        let settings = self.settings();
        let mut guard = self.lock_vault();
        let was_unlocked = !self.ui_locked.load(Ordering::SeqCst);
        let mut note = None;
        if let Some(vault) = guard.as_mut() {
            if settings.relock_on_session_lock && has_open(vault) {
                let (locked, failed) = relock_everything(vault);
                note = relock_text(locked, failed, "when Windows was locked");
            }
        }
        self.lock_app(&mut guard);
        self.update_session_hold(&guard);
        drop(guard);
        note.or_else(|| {
            was_unlocked.then(|| {
                (
                    "VeilForge locked itself because Windows was locked or went to sleep.".into(),
                    false,
                )
            })
        })
    }

    /// One look at the idle timers. `idle`: seconds without keyboard or mouse
    /// input. `relocked_this_idle` remembers that the open items were already
    /// locked during this stretch of inactivity. Never waits for the map.
    fn idle_tick(&self, idle: u64, relocked_this_idle: &mut bool) -> Vec<(String, bool)> {
        let mut notes = Vec::new();
        let settings = self.settings();
        let auto = u64::from(settings.auto_lock_minutes) * 60;
        let relock = u64::from(settings.relock_idle_minutes) * 60;
        if relock == 0 || idle < relock {
            *relocked_this_idle = false;
        }
        // Never wait here: an operation in progress keeps the map busy.
        let Ok(mut guard) = self.vault.try_lock() else {
            return notes;
        };
        if relock > 0 && idle >= relock && !*relocked_this_idle {
            *relocked_this_idle = true;
            if let Some(vault) = guard.as_mut() {
                let _ = vault.refresh();
                if has_open(vault) {
                    let (locked, failed) = relock_everything(vault);
                    notes.extend(relock_text(
                        locked,
                        failed,
                        "after a period without activity",
                    ));
                }
            }
        }
        let ui_locked = self.ui_locked.load(Ordering::SeqCst);
        if auto > 0 && idle >= auto && !ui_locked && guard.is_some() {
            self.lock_app(&mut guard);
            notes.push((
                "VeilForge locked itself after a period without activity.".into(),
                false,
            ));
        } else if ui_locked {
            // Drops a kept map once nothing is open any more.
            self.lock_app(&mut guard);
        }
        self.update_session_hold(&guard);
        notes
    }
}

/// Locks every temporarily open item again. Returns (locked, failed).
fn relock_everything(vault: &mut Vault) -> (usize, usize) {
    let _ = vault.refresh();
    let results = vault.relock_all(&mut |_| {});
    let failed = results.iter().filter(|(_, r)| r.is_err()).count();
    (results.len() - failed, failed)
}

fn relock_text(locked: usize, failed: usize, why: &str) -> Option<(String, bool)> {
    match (locked, failed) {
        (0, 0) => None,
        (n, 0) => Some((
            format!(
                "{n} temporarily open item{} locked again {why}.",
                if n == 1 { " was" } else { "s were" }
            ),
            false,
        )),
        (_, f) => Some((
            format!(
                "{f} item{} could not be locked again {why}, probably because a file is still open in a program. Close it and choose \"Lock all\".",
                if f == 1 { "" } else { "s" }
            ),
            true,
        )),
    }
}

/// Handles Windows events and the idle timers. Runs for the life of the process.
fn controller(core: Arc<Core>, rx: Receiver<SysEvent>, tx: Sender<Msg>, ctx: egui::Context) {
    let notify = |text: String, error: bool| {
        let _ = tx.send(Msg::Background(text, error));
        ctx.request_repaint();
    };
    let mut relocked_this_idle = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(1000)) {
            Ok(
                event @ (SysEvent::SessionLocked | SysEvent::Suspending | SysEvent::SessionEnding),
            ) => {
                let note = core.session_event();
                if event == SysEvent::SessionEnding {
                    sys::session_end_done();
                }
                if let Some((text, error)) = note {
                    notify(text, error);
                }
            }
            Ok(SysEvent::TrayLock) => {
                let (text, error) = core.lock_everything("from the tray menu");
                notify(text, error);
            }
            Ok(SysEvent::LockAll) => {
                let (text, error) = core.lock_everything("on request");
                notify(text, error);
            }
            Ok(SysEvent::Hotkey) => {
                sys::show_and_focus(core.hwnd.load(Ordering::SeqCst));
                let _ = tx.send(Msg::Shown);
                ctx.request_repaint();
            }
            Ok(SysEvent::TrayShow) => {
                let _ = tx.send(Msg::Shown);
                ctx.request_repaint();
            }
            Ok(SysEvent::TrayExit) => {
                core.quit.store(true, Ordering::SeqCst);
                sys::close_window(core.hwnd.load(Ordering::SeqCst));
                ctx.request_repaint();
                // A hidden window does not draw, so it may never see the close
                // request. Give it a moment, then leave from here: wait for a
                // running operation, remove the tray icon, exit.
                std::thread::sleep(Duration::from_millis(1500));
                let guard = core.lock_vault();
                *relax(core.sys.lock()) = None;
                std::thread::sleep(Duration::from_millis(150));
                drop(guard);
                std::process::exit(0);
            }
            Err(RecvTimeoutError::Timeout) => {
                for (text, error) in core.idle_tick(sys::idle_seconds(), &mut relocked_this_idle) {
                    notify(text, error);
                }
                let tip = core.tray_tip();
                core.with_sys(|sys| sys.set_tray_tip(&tip));
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// State shared by every screen.
pub struct Shared {
    pub settings: Settings,
    saved_settings: Settings,
    pub data_dir: PathBuf,
    pub env: Env,
    pub demo: bool,
    /// Screenshot mode: no dialogs from the system, no hints that depend on the PC.
    pub shot: bool,
    pub tx: Sender<Msg>,
    pub toasts: Vec<Toast>,
    pub hwnd: isize,
    /// Unix time, refreshed every frame.
    pub now: i64,
    pub core: Arc<Core>,
    pub elevated: bool,
    /// Paths waiting for the "Protect" dialog (command line, drop while locked).
    pub pending_add: Vec<PathBuf>,
    /// The demo's invented list, kept while the demo is "locked".
    pub demo_keep: Option<DemoVault>,
    /// What the system says right now (not what the settings file remembers).
    pub context_menu_on: bool,
    pub startup_on: bool,
    /// The background window exists (tray icon and hotkey can work).
    pub has_sys: bool,
    /// Not the demo and not a test sandbox: the registry may be changed.
    pub real_system: bool,
}

impl Shared {
    pub fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toasts.push(Toast {
            text: text.into(),
            error,
            born: Instant::now(),
        });
    }

    /// Runs `work` on a new thread and delivers its result as a message.
    pub fn spawn(&self, ctx: &egui::Context, work: impl FnOnce() -> Msg + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        });
    }

    /// Creates, opens or rebuilds the map on a worker thread.
    pub fn start_unlock(
        &self,
        ctx: &egui::Context,
        kind: UnlockKind,
        password: Zeroizing<String>,
        attempt: u64,
    ) {
        let (env, core, tx) = (self.env.clone(), self.core.clone(), self.tx.clone());
        self.spawn(ctx, move || {
            let result = match kind {
                UnlockKind::Create => Vault::create(env, &password).map(|v| (v, 0)),
                UnlockKind::Open => Vault::open(env, &password).map(|v| (v, 0)),
                UnlockKind::Rebuild => engine::rebuild_from_recovery(env, &password),
            };
            drop(password);
            let result = result.map(|(vault, found)| {
                // What opening had to do (finish an interrupted operation,
                // fall back to a backup of the list): tell the user.
                for note in vault.open_notes().iter().take(3) {
                    let _ = tx.send(Msg::Toast(note.clone(), false));
                }
                let snapshot = Snapshot::of(&vault);
                let mut guard = core.lock_vault();
                *guard = Some(vault);
                core.ui_locked.store(false, Ordering::SeqCst);
                core.update_session_hold(&guard);
                (snapshot, found)
            });
            Msg::Unlocked(result, kind, attempt)
        });
    }

    fn persist_settings(&mut self) {
        if self.settings == self.saved_settings {
            return;
        }
        self.settings.sanitize();
        if !self.demo {
            if let Err(e) = self.settings.save(&self.data_dir) {
                self.toast(e, true);
            }
        }
        self.core.set_settings(&self.settings);
        self.saved_settings = self.settings.clone();
    }

    /// The window title: neutral if asked for, and it says when this is the demo.
    pub fn window_title(&self) -> String {
        if self.demo {
            format!("{} (demo)", self.settings.title())
        } else {
            self.settings.title().to_string()
        }
    }

    /// Restarts VeilForge with administrator rights and closes this window.
    pub fn restart_as_admin(&mut self, ctx: &egui::Context) {
        if self.demo {
            self.toast("Demo: VeilForge is not restarted.", false);
            return;
        }
        // The new copy must not find this one still "running".
        release_instance();
        match sys::restart_as_admin("") {
            Ok(()) => {
                self.core.quit.store(true, Ordering::SeqCst);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => {
                claim_instance();
                self.toast(e, true);
            }
        }
    }
}

#[cfg(test)]
impl Shared {
    /// The shared state of a window that does not exist, on a sandbox: for
    /// tests that drive the same code the buttons run.
    pub(super) fn for_test(env: Env) -> (Shared, Receiver<Msg>) {
        let (tx, rx) = channel();
        let settings = Settings {
            // Never open File Explorer windows from a test.
            open_after_unlock: false,
            ..Default::default()
        };
        let shared = Shared {
            saved_settings: settings.clone(),
            core: Arc::new(Core::new(settings.clone())),
            settings,
            data_dir: env.data_dir.clone(),
            env,
            demo: false,
            shot: false,
            tx,
            toasts: Vec::new(),
            hwnd: 0,
            now: crate::util::now(),
            elevated: false,
            pending_add: Vec::new(),
            demo_keep: None,
            context_menu_on: false,
            startup_on: false,
            has_sys: false,
            real_system: false,
        };
        (shared, rx)
    }
}

pub enum Screen {
    Welcome(Box<CreateState>),
    Unlock(Box<UnlockState>),
    Main(Box<Main>),
}

pub struct App {
    pub(super) sh: Shared,
    pub(super) screen: Screen,
    rx: Receiver<Msg>,
    pub(super) logo: Option<egui::TextureHandle>,
    shot: Option<Shot>,
    frames: u64,
    title_applied: String,
    start_hidden: bool,
    last_inbox_check: Instant,
    rebuild_from_welcome: bool,
}

// --------------------------------------------------------------------------------------
// One window per user
// --------------------------------------------------------------------------------------

const INSTANCE_NAME: &str = "VeilForge-single-window";
static INSTANCE: Mutex<Option<sys::AppMutex>> = Mutex::new(None);

/// True when this is the only VeilForge window of this user.
fn claim_instance() -> bool {
    let mut held = relax(INSTANCE.lock());
    if held.is_some() {
        return true;
    }
    *held = sys::first_instance(INSTANCE_NAME);
    held.is_some()
}

fn release_instance() {
    *relax(INSTANCE.lock()) = None;
}

/// Paths handed from a second start to the window that is already open are
/// written as small text files into the data folder and picked up there.
fn write_inbox(dir: &Path, paths: &[PathBuf]) {
    if paths.is_empty() || std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let text: String = paths
        .iter()
        .map(|p| format!("{}\n", p.to_string_lossy()))
        .collect();
    let name = format!("inbox-{}-{}.txt", std::process::id(), crate::util::now());
    let _ = std::fs::write(dir.join(name), text);
}

fn read_inbox(dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return paths;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !(name.starts_with("inbox-") && name.ends_with(".txt")) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(entry.path()) {
            paths.extend(
                text.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .take(500)
                    .map(PathBuf::from),
            );
        }
        let _ = std::fs::remove_file(entry.path());
    }
    paths
}

/// Starts the window.
pub fn run(mut options: Options, icon: egui::IconData) -> eframe::Result<()> {
    // Fallbacks for the flags, so that screenshots can be made by any caller.
    if options.view.is_none() {
        options.view = std::env::var("VEILFORGE_VIEW")
            .ok()
            .filter(|v| !v.is_empty());
    }
    if options.screenshot.is_none() {
        options.screenshot = std::env::var_os("VEILFORGE_SHOT")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
    }
    if std::env::var("VEILFORGE_LIGHT").is_ok_and(|v| v == "1") {
        options.light = true;
    }
    // Screenshots are always taken from the demo.
    if options.screenshot.is_some() {
        options.demo = true;
    }

    let real = !options.demo && options.sandbox.is_none();
    if real && !claim_instance() {
        // Already running: hand over what we were asked to protect, show
        // that window, and leave.
        write_inbox(&crate::settings::data_dir(), &options.add);
        if !sys::signal_running_instance() {
            sys::message_box(
                "VeilForge",
                "VeilForge is already running. Look for its icon near the clock, or press its hotkey.",
            );
        }
        return Ok(());
    }

    let title = if options.demo {
        "VeilForge (demo)"
    } else {
        "VeilForge"
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([1040.0, 640.0])
        .with_drag_and_drop(true)
        .with_icon(icon.clone());
    if options.start_hidden && real {
        // The first frame is drawn off screen; the window is then hidden and
        // moved back, so nothing flashes at sign-in.
        viewport = viewport.with_position([-32000.0, -32000.0]);
    }
    let native = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let result = eframe::run_native(
        "VeilForge",
        native,
        Box::new(move |cc| Ok(Box::new(App::new(cc, options, icon)))),
    );
    release_instance();
    result
}

fn window_handle(cc: &eframe::CreationContext<'_>) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
        _ => 0,
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, options: Options, icon: egui::IconData) -> Self {
        let ctx = &cc.egui_ctx;
        theme::install(ctx);
        crate::util::set_local_offset(sys::local_utc_offset());
        let shot = options.screenshot.is_some();
        let demo = options.demo;
        let env = if demo {
            // Never used for anything but showing the invented folder name.
            Env {
                data_dir: PathBuf::from(crate::demo::DEMO_DATA_DIR),
                ..Env::sandbox(Path::new(crate::demo::DEMO_DATA_DIR))
            }
        } else {
            options.sandbox.clone().unwrap_or_else(Env::real)
        };
        let data_dir = env.data_dir.clone();
        let mut settings = if demo {
            Settings::default()
        } else {
            Settings::load(&data_dir)
        };
        if options.light {
            settings.theme = crate::settings::ThemeChoice::Light;
        } else if shot {
            settings.theme = crate::settings::ThemeChoice::Dark;
        }
        theme::apply_choice(ctx, settings.theme);

        let logo = (icon.width > 0).then(|| {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            ctx.load_texture("logo", image, egui::TextureOptions::LINEAR)
        });
        let (tx, rx) = channel();
        let hwnd = window_handle(cc);
        let real_system = !demo && options.sandbox.is_none();
        let core = Arc::new(Core {
            real: real_system,
            ..Core::new(settings.clone())
        });
        core.hwnd.store(hwnd, Ordering::SeqCst);

        // The background window: session notices, hotkey, tray. Not started
        // for the demo or for screenshots, so nothing global is registered.
        let mut has_sys = false;
        if !demo && hwnd != 0 {
            let (sys_tx, sys_rx) = channel();
            let wake = ctx.clone();
            let sys = SysWindow::start(hwnd, sys_tx, Box::new(move || wake.request_repaint()));
            if let Some(sys) = sys {
                sys.set_tray_title(settings.title());
                sys.set_tray(settings.tray_icon);
                sys.set_hotkey(HotkeyKind::Show, sys::parse_hotkey(&settings.hotkey));
                sys.set_hotkey(HotkeyKind::Lock, sys::parse_hotkey(&settings.lock_hotkey));
                *relax(core.sys.lock()) = Some(sys);
                has_sys = true;
                let (core, tx, ctx) = (core.clone(), tx.clone(), ctx.clone());
                let _ = std::thread::Builder::new()
                    .name("veilforge-controller".into())
                    .spawn(move || controller(core, sys_rx, tx, ctx));
            }
        }

        let mut sh = Shared {
            saved_settings: settings.clone(),
            settings,
            data_dir,
            env,
            demo,
            shot,
            tx,
            toasts: Vec::new(),
            hwnd,
            now: crate::util::now(),
            core,
            elevated: !demo && sys::is_elevated(),
            pending_add: if demo {
                Vec::new()
            } else {
                options.add.clone()
            },
            demo_keep: None,
            context_menu_on: real_system
                && crate::shellmenu::is_installed(crate::shellmenu::CLASSES_ROOT),
            startup_on: real_system && sys::startup_enabled(sys::RUN_KEY),
            has_sys,
            real_system,
        };

        let view = options.view.clone().unwrap_or_default();
        let screen = if demo {
            super::main_view::demo_screen(&view, &mut sh)
        } else if Vault::exists(&sh.env) {
            Screen::Unlock(Box::default())
        } else {
            Screen::Welcome(Box::default())
        };
        App {
            start_hidden: options.start_hidden && has_sys && !shot,
            sh,
            screen,
            rx,
            logo,
            shot: options.screenshot.clone().map(|path| Shot {
                path,
                frames: 0,
                requested: false,
            }),
            frames: 0,
            title_applied: String::new(),
            last_inbox_check: Instant::now(),
            rebuild_from_welcome: false,
        }
    }

    // ----------------------------------------------------------------------------------
    // Screen changes and locking
    // ----------------------------------------------------------------------------------

    fn show_lock_screen(&mut self, ctx: &egui::Context) {
        if let Screen::Main(main) = &mut self.screen {
            self.sh.demo_keep = main.take_demo();
        }
        self.screen = Screen::Unlock(Box::default());
        // Text-edit state (cursor, undo history), scroll positions, etc.
        ctx.memory_mut(|m| m.data.clear());
    }

    pub(super) fn go(&mut self, ctx: &egui::Context, go: Go) {
        match go {
            Go::Opened {
                snapshot,
                kind,
                found,
            } => {
                let demo = self.sh.demo.then(|| {
                    self.sh
                        .demo_keep
                        .take()
                        .unwrap_or_else(|| DemoVault::sample(self.sh.now))
                });
                let mut main = Main::new(snapshot, demo);
                match kind {
                    UnlockKind::Create => main.offer_emergency_sheet(),
                    UnlockKind::Rebuild => self.sh.toast(
                        match found {
                            0 => "No recovery copies were found for this password. The list starts empty.".to_string(),
                            1 => "1 protected item was found and is back in the list.".to_string(),
                            n => format!("{n} protected items were found and are back in the list."),
                        },
                        found == 0,
                    ),
                    UnlockKind::Open => {}
                }
                self.screen = Screen::Main(Box::new(main));
                self.offer_pending(ctx);
            }
            Go::Lock => {
                if !matches!(self.screen, Screen::Main(_)) {
                    return;
                }
                if !self.sh.demo {
                    let core = self.sh.core.clone();
                    let Ok(mut guard) = core.vault.try_lock() else {
                        self.sh
                            .toast("VeilForge is still working. Try again in a moment.", true);
                        return;
                    };
                    core.lock_app(&mut guard);
                    if guard.is_some() {
                        self.sh.toast(
                            "VeilForge is locked. Items you opened temporarily stay open until they are locked again.",
                            false,
                        );
                    }
                }
                self.show_lock_screen(ctx);
            }
            Go::ShowRebuild => {
                self.rebuild_from_welcome = matches!(self.screen, Screen::Welcome(_));
                self.screen = Screen::Unlock(Box::new(UnlockState::rebuild()));
            }
            Go::ShowStart => {
                let first_run = if self.sh.demo {
                    self.rebuild_from_welcome
                } else {
                    !Vault::exists(&self.sh.env)
                };
                self.screen = if first_run {
                    Screen::Welcome(Box::default())
                } else {
                    Screen::Unlock(Box::default())
                };
            }
        }
    }

    /// Opens the "Protect" dialog for paths that were waiting (command line,
    /// a second start, a drop on the locked window).
    fn offer_pending(&mut self, ctx: &egui::Context) {
        if self.sh.pending_add.is_empty() {
            return;
        }
        if let Screen::Main(main) = &mut self.screen {
            let paths = std::mem::take(&mut self.sh.pending_add);
            main.offer_protect(ctx, &mut self.sh, paths);
        }
    }

    fn check_inbox(&mut self, ctx: &egui::Context) {
        if self.sh.demo {
            return;
        }
        self.last_inbox_check = Instant::now();
        let paths = read_inbox(&self.sh.data_dir);
        if paths.is_empty() {
            return;
        }
        let n = paths.len();
        self.sh.pending_add.extend(paths);
        if matches!(self.screen, Screen::Main(_)) {
            self.offer_pending(ctx);
        } else {
            self.sh.toast(
                format!(
                    "Unlock VeilForge to protect the {} you chose.",
                    if n == 1 {
                        "item".to_string()
                    } else {
                        format!("{n} items")
                    }
                ),
                false,
            );
        }
    }

    fn drain_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Toast(text, error) => self.sh.toast(text, error),
                Msg::Shown => self.check_inbox(ctx),
                Msg::Unlocked(result, kind, attempt) => {
                    // The wrong-password log: when and where, never what.
                    let logged = !self.sh.demo && kind != UnlockKind::Create;
                    if logged && matches!(&result, Err(Error::WrongPassword)) {
                        crate::attempts::record_failure(
                            &self.sh.data_dir,
                            crate::attempts::Place::Window,
                            crate::util::now(),
                        );
                    }
                    let go = match &mut self.screen {
                        Screen::Unlock(s) => s.unlocked(result, kind, attempt),
                        Screen::Welcome(s) => s.unlocked(result, kind, attempt),
                        Screen::Main(_) => None,
                    };
                    if let Some(go) = go {
                        // The user's own slips at this unlock screen are not news.
                        let own = match &self.screen {
                            Screen::Unlock(s) => s.own_failures(),
                            _ => 0,
                        };
                        let notice = (logged && matches!(go, Go::Opened { .. }))
                            .then(|| crate::attempts::record_unlock(&self.sh.data_dir, own))
                            .and_then(|n| n.text());
                        self.go(ctx, go);
                        if let (Some(text), Screen::Main(main)) = (notice, &mut self.screen) {
                            main.attempt_notice = Some(text);
                        }
                    }
                }
                Msg::Background(text, error) => {
                    self.sh.toast(text, error);
                    if let Screen::Main(main) = &mut self.screen {
                        main.refresh_soon();
                    }
                }
                other => {
                    if let Screen::Main(main) = &mut self.screen {
                        main.message(ctx, &mut self.sh, other);
                    }
                }
            }
        }
        // The controller thread locked the app (idle, Windows lock, tray).
        if !self.sh.demo
            && self.sh.core.ui_locked.load(Ordering::SeqCst)
            && matches!(self.screen, Screen::Main(_))
        {
            self.show_lock_screen(ctx);
        }
    }

    fn timers(&mut self, ctx: &egui::Context) {
        self.sh.now = crate::util::now();
        if self.shot.is_some() || self.sh.demo {
            return;
        }
        if self.last_inbox_check.elapsed() > Duration::from_secs(3) {
            self.check_inbox(ctx);
        }
        if let Screen::Main(main) = &mut self.screen {
            // The `--open` helper changes the map too: look again every two
            // seconds while the window is in front.
            if ctx.input(|i| i.focused) {
                main.periodic_refresh(ctx, &self.sh);
                ctx.request_repaint_after(Duration::from_millis(1000));
            }
        }
    }

    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.shot else { return };
        ctx.request_repaint();
        shot.frames += 1;
        if shot.frames == 1 {
            ctx.set_pixels_per_point(1.0);
        }
        if shot.frames == 3 {
            // With one point per pixel this gives an exact 1280 x 800 image.
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(1280.0, 800.0)));
        }
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            if let Some(parent) = shot.path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let result = image::save_buffer(
                &shot.path,
                image.as_raw(),
                w as u32,
                h as u32,
                image::ColorType::Rgba8,
            );
            if let Err(e) = result {
                eprintln!("screenshot failed: {e}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            self.shot = None;
            return;
        }
        if !shot.requested && shot.frames > 40 {
            shot.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
    }

    /// Only web links are opened; nothing else egui asks for is passed on.
    fn handle_output(&mut self, ctx: &egui::Context) {
        let commands = ctx.output_mut(|o| std::mem::take(&mut o.commands));
        for command in commands {
            match command {
                egui::OutputCommand::OpenUrl(open) => {
                    if self.sh.shot {
                        continue;
                    }
                    if let Err(e) = sys::open_url(&open.url) {
                        self.sh.toast(e, true);
                    }
                }
                other => ctx.output_mut(|o| o.commands.push(other)),
            }
        }
    }

    /// Files dropped on a screen that cannot protect them yet.
    fn drops_while_locked(&mut self, ctx: &egui::Context) {
        if matches!(self.screen, Screen::Main(_)) || self.sh.demo {
            return;
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.sh.pending_add.extend(dropped);
            self.sh.toast(
                "Unlock VeilForge first. The protect dialog opens right after.",
                false,
            );
        }
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        let quit = self.sh.core.quit.load(Ordering::SeqCst);
        if let Screen::Main(main) = &self.screen {
            if main.busy() && !self.sh.demo {
                // Leaving in the middle of an operation would only make the
                // next start finish it from the journal: wait instead.
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.sh.toast(
                    "Please wait until the current operation has finished.",
                    true,
                );
                return;
            }
        }
        let stay = !quit
            && !self.sh.demo
            && self.shot.is_none()
            && self.sh.has_sys
            && self.sh.settings.stays_in_background();
        if stay {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.sh.settings.lock_on_close && matches!(self.screen, Screen::Main(_)) {
                // Out of sight, locked: opening the window again asks for
                // the master password. Open items stay open (they have their
                // own timers) and the lock screen offers to lock them.
                self.go(ctx, Go::Lock);
                self.sh.toasts.clear();
            }
            sys::hide_window(self.sh.hwnd);
        }
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        self.sh
            .toasts
            .retain(|t| t.born.elapsed().as_secs_f32() < if t.error { 9.0 } else { 3.5 });
        if self.sh.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        let p = pal_ctx(ctx);
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-18.0, -44.0))
            .order(egui::Order::Tooltip)
            .show(ctx, |ui| {
                ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
                    for t in self.sh.toasts.iter().rev().take(4) {
                        let color = if t.error { p.danger } else { p.ok };
                        Frame::new()
                            .fill(p.card)
                            .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.7)))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::symmetric(14, 10))
                            .show(ui, |ui| {
                                ui.set_max_width(440.0);
                                ui.horizontal(|ui| {
                                    let icon = if t.error { Icon::ERROR } else { Icon::DONE };
                                    ui.label(RichText::new(icon.glyph()).color(color));
                                    let fg = pal_ctx(ui.ctx()).text;
                                    super::widgets::text(
                                        ui,
                                        &t.text,
                                        super::widgets::BODY_SIZE,
                                        false,
                                        fg,
                                    );
                                });
                            });
                    }
                });
            });
    }

    /// Settings changed this frame: save them and apply the ones with an
    /// effect outside the window.
    fn apply_settings(&mut self, ctx: &egui::Context) {
        if self.sh.settings != self.sh.saved_settings {
            let old = self.sh.saved_settings.clone();
            self.sh.persist_settings();
            let new = &self.sh.settings;
            self.sh.core.with_sys(|sys| {
                if new.neutral_title != old.neutral_title {
                    sys.set_tray_title(new.title());
                }
                if new.tray_icon != old.tray_icon {
                    sys.set_tray(new.tray_icon);
                }
                if new.hotkey != old.hotkey || new.lock_hotkey != old.lock_hotkey {
                    // Both are taken off first, so that swapping the two
                    // combinations never meets one that is still registered.
                    sys.set_hotkey(HotkeyKind::Show, None);
                    sys.set_hotkey(HotkeyKind::Lock, None);
                    sys.set_hotkey(HotkeyKind::Show, sys::parse_hotkey(&new.hotkey));
                    sys.set_hotkey(HotkeyKind::Lock, sys::parse_hotkey(&new.lock_hotkey));
                }
            });
        }
        let title = self.sh.window_title();
        if self.title_applied != title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title_applied = title;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frames += 1;
        if self.start_hidden {
            // Frame 1 was drawn off screen. Hide, then move to the middle so
            // the window is in the right place when the tray icon opens it.
            if self.frames == 2 {
                sys::hide_window(self.sh.hwnd);
                sys::center_on_screen(self.sh.hwnd);
                self.start_hidden = false;
            } else {
                ctx.request_repaint();
            }
        }
        self.drain_messages(ctx);
        self.timers(ctx);
        self.handle_screenshot(ctx);
        theme::apply_choice(ctx, self.sh.settings.theme);
        self.drops_while_locked(ctx);

        let logo = self.logo.clone();
        let go = match &mut self.screen {
            Screen::Welcome(s) => s.ui(ctx, &mut self.sh, logo.as_ref()),
            Screen::Unlock(s) => s.ui(ctx, &mut self.sh, logo.as_ref()),
            Screen::Main(s) => s.ui(ctx, &mut self.sh, logo.as_ref()),
        };
        if let Some(go) = go {
            self.go(ctx, go);
        }

        self.toasts_ui(ctx);
        self.handle_output(ctx);
        self.apply_settings(ctx);
        self.handle_close(ctx);
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if self.shot.is_some() {
            // Screenshots must not depend on where the mouse happens to be.
            raw_input.events.retain(|e| {
                !matches!(
                    e,
                    egui::Event::PointerMoved(_)
                        | egui::Event::MouseMoved(_)
                        | egui::Event::PointerButton { .. }
                        | egui::Event::MouseWheel { .. }
                        | egui::Event::Paste(_)
                )
            });
            raw_input.events.push(egui::Event::PointerGone);
            raw_input.dropped_files.clear();
            raw_input.hovered_files.clear();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Removes the tray icon and the hotkey.
        *relax(self.sh.core.sys.lock()) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_round_trip() {
        let dir = crate::util::unique_temp_dir("inbox");
        assert!(read_inbox(&dir).is_empty());
        write_inbox(&dir, &[]);
        assert!(read_inbox(&dir).is_empty());
        let paths = vec![
            PathBuf::from("C:\\Users\\alex\\Documents\\Tax 2025"),
            PathBuf::from("D:\\Projects\\Prototype"),
        ];
        write_inbox(&dir, &paths);
        std::fs::write(dir.join("settings.json"), "{}").unwrap();
        assert_eq!(read_inbox(&dir), paths);
        // Read once, then gone; other files are left alone.
        assert!(read_inbox(&dir).is_empty());
        assert!(dir.join("settings.json").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    const PW: &str = "a long master password";

    /// A sandbox with one hidden file that is temporarily open, in a `Core`
    /// whose window is unlocked: the state the lock triggers start from.
    fn core_with_an_open_item(
        tag: &str,
        settings: Settings,
    ) -> (crate::engine::cleanup::SandboxGuard, Core, PathBuf, String) {
        let sandbox = crate::engine::cleanup::SandboxGuard::new(tag);
        let env = Env::sandbox(sandbox.path());
        let file = sandbox.path().join("Diary.txt");
        std::fs::write(&file, b"dear diary").unwrap();
        let mut vault = Vault::create(env, PW).unwrap();
        let id = vault
            .protect(
                &file,
                &crate::engine::ProtectOptions::default(),
                &mut |_| {},
            )
            .unwrap();
        assert!(!file.exists());
        vault.open_temporarily(&id, &mut |_| {}).unwrap();
        assert!(file.exists());
        let core = Core::new(settings);
        *core.lock_vault() = Some(vault);
        core.ui_locked.store(false, Ordering::SeqCst);
        (sandbox, core, file, id)
    }

    fn reopen(sandbox: &crate::engine::cleanup::SandboxGuard) -> Vault {
        Vault::open(Env::sandbox(sandbox.path()), PW).unwrap()
    }

    /// Windows lock, sleep and sign-out all arrive here: open items are
    /// protected again and the window asks for the password.
    #[test]
    fn a_windows_lock_or_sleep_locks_open_items_and_the_window() {
        let (sandbox, core, file, id) = core_with_an_open_item("sessionlock", Settings::default());
        let (text, error) = core.session_event().unwrap();
        assert_eq!(
            text,
            "1 temporarily open item was locked again when Windows was locked."
        );
        assert!(!error);
        assert!(!file.exists(), "hidden again");
        assert!(core.ui_locked.load(Ordering::SeqCst));
        assert!(
            core.lock_vault().is_none(),
            "nothing open: the map is dropped"
        );
        assert_eq!(core.open_items(), 0);
        // A second notice (sleep right after lock) has nothing left to do.
        assert_eq!(core.session_event(), None);

        let mut vault = reopen(&sandbox);
        assert_eq!(vault.state(&id), crate::model::State::Locked);
        vault.release(&id, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"dear diary");
    }

    /// With "also when Windows locks" switched off the window still locks,
    /// the item stays open, and the lock screen can lock it without a password.
    #[test]
    fn open_items_can_be_locked_from_the_lock_screen() {
        let settings = Settings {
            relock_on_session_lock: false,
            ..Default::default()
        };
        let (sandbox, core, file, id) = core_with_an_open_item("lockscreen", settings);
        let (text, _) = core.session_event().unwrap();
        assert!(text.starts_with("VeilForge locked itself"), "{text}");
        assert!(file.exists(), "left open, as the setting says");
        assert!(core.ui_locked.load(Ordering::SeqCst));
        assert!(core.lock_vault().is_some(), "kept for locking it later");
        assert_eq!(core.open_items(), 1);
        assert_eq!(core.tray_tip(), "VeilForge: 1 item is temporarily open");

        // A program still has the file open: it cannot be locked, and says so.
        use std::os::windows::fs::OpenOptionsExt;
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&file)
            .unwrap();
        let (text, error) = core.lock_everything("from the lock screen");
        assert!(error, "{text}");
        assert!(text.starts_with("1 item could not be locked again from the lock screen"));
        assert!(file.exists());
        assert_eq!(core.open_items(), 1);
        drop(held);

        let (text, error) = core.lock_everything("from the lock screen");
        assert_eq!(
            text,
            "1 temporarily open item was locked again from the lock screen."
        );
        assert!(!error);
        assert!(!file.exists());
        assert!(core.lock_vault().is_none());
        assert_eq!(core.open_items(), 0);
        assert_eq!(core.tray_tip(), "VeilForge: locked");
        // Nothing left: the answer is short.
        assert_eq!(core.lock_everything("on request").0, "Locked.");
        // The neutral title gives nothing away in the tray either.
        core.set_settings(&Settings {
            neutral_title: true,
            ..Default::default()
        });
        assert_eq!(core.tray_tip(), "Files");

        let mut vault = reopen(&sandbox);
        vault.release(&id, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"dear diary");
    }

    /// The two idle timers, with the idle time handed in instead of waiting.
    #[test]
    fn idle_time_locks_the_window_first_and_open_items_later() {
        let settings = Settings {
            auto_lock_minutes: 5,
            relock_idle_minutes: 15,
            ..Default::default()
        };
        let (sandbox, core, file, id) = core_with_an_open_item("idle", settings);
        let mut relocked = false;
        assert!(core.idle_tick(10, &mut relocked).is_empty());
        assert!(!core.ui_locked.load(Ordering::SeqCst));
        assert_eq!(core.tray_tip(), "VeilForge: 1 item is temporarily open");

        // An operation in progress holds the map: the timer never waits for it.
        {
            let _busy = core.lock_vault();
            assert!(core.idle_tick(100_000, &mut relocked).is_empty());
            assert!(!relocked);
        }

        // Five minutes: the window locks, the item stays open.
        let notes = core.idle_tick(5 * 60, &mut relocked);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].0.starts_with("VeilForge locked itself after"));
        assert!(core.ui_locked.load(Ordering::SeqCst));
        assert!(file.exists() && !relocked);
        assert!(core.lock_vault().is_some());

        // Fifteen minutes: the item is locked again, once.
        let notes = core.idle_tick(15 * 60, &mut relocked);
        assert_eq!(
            notes,
            vec![(
                "1 temporarily open item was locked again after a period without activity."
                    .to_string(),
                false
            )]
        );
        assert!(relocked && !file.exists());
        assert!(core.lock_vault().is_none());
        assert!(core.idle_tick(16 * 60, &mut relocked).is_empty());
        // Activity ends the stretch of inactivity.
        assert!(core.idle_tick(0, &mut relocked).is_empty());
        assert!(!relocked);

        let mut vault = reopen(&sandbox);
        vault.release(&id, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"dear diary");
    }

    /// "Never" for both timers: nothing happens however long nobody is there.
    #[test]
    fn idle_timers_set_to_never_do_nothing() {
        let settings = Settings {
            auto_lock_minutes: 0,
            relock_idle_minutes: 0,
            ..Default::default()
        };
        let (sandbox, core, file, id) = core_with_an_open_item("idlenever", settings);
        let mut relocked = false;
        assert!(core.idle_tick(48 * 3600, &mut relocked).is_empty());
        assert!(!core.ui_locked.load(Ordering::SeqCst));
        assert!(file.exists() && !relocked);
        assert_eq!(
            core.lock_everything("from the tray menu").0,
            "1 temporarily open item was locked again from the tray menu."
        );
        let mut vault = reopen(&sandbox);
        vault.release(&id, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"dear diary");
    }

    #[test]
    fn relock_messages() {
        assert!(relock_text(0, 0, "x").is_none());
        let (text, error) = relock_text(1, 0, "from the tray menu").unwrap();
        assert_eq!(
            text,
            "1 temporarily open item was locked again from the tray menu."
        );
        assert!(!error);
        let (text, error) = relock_text(2, 1, "when Windows was locked").unwrap();
        assert!(text.starts_with("1 item could not be locked again"));
        assert!(error);
    }
}

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
use crate::sys::{self, SysEvent, SysWindow};
use eframe::egui::{
    self, vec2, Align, Align2, CornerRadius, Frame, Layout, Margin, RichText, Stroke,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
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
        sys::hold_session_end(wanted);
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
                let settings = core.settings();
                let mut guard = core.lock_vault();
                let was_unlocked = !core.ui_locked.load(Ordering::SeqCst);
                let mut note = None;
                if let Some(vault) = guard.as_mut() {
                    if settings.relock_on_session_lock && has_open(vault) {
                        let (locked, failed) = relock_everything(vault);
                        note = relock_text(locked, failed, "when Windows was locked");
                    }
                }
                core.lock_app(&mut guard);
                core.update_session_hold(&guard);
                drop(guard);
                if event == SysEvent::SessionEnding {
                    sys::session_end_done();
                }
                match note {
                    Some((text, error)) => notify(text, error),
                    None if was_unlocked => notify(
                        "VeilForge locked itself because Windows was locked or went to sleep."
                            .into(),
                        false,
                    ),
                    None => {}
                }
            }
            Ok(SysEvent::TrayLock) => {
                let mut guard = core.lock_vault();
                let mut note = None;
                if let Some(vault) = guard.as_mut() {
                    let (locked, failed) = relock_everything(vault);
                    note = relock_text(locked, failed, "from the tray menu");
                }
                core.lock_app(&mut guard);
                core.update_session_hold(&guard);
                drop(guard);
                let (text, error) = note.unwrap_or(("Locked.".into(), false));
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
                let settings = core.settings();
                let idle = sys::idle_seconds();
                let auto = u64::from(settings.auto_lock_minutes) * 60;
                let relock = u64::from(settings.relock_idle_minutes) * 60;
                if relock == 0 || idle < relock {
                    relocked_this_idle = false;
                }
                // Never wait here: an operation in progress keeps the map busy.
                let Ok(mut guard) = core.vault.try_lock() else {
                    continue;
                };
                if relock > 0 && idle >= relock && !relocked_this_idle {
                    relocked_this_idle = true;
                    if let Some(vault) = guard.as_mut() {
                        let _ = vault.refresh();
                        if has_open(vault) {
                            let (locked, failed) = relock_everything(vault);
                            if let Some((text, error)) =
                                relock_text(locked, failed, "after a period without activity")
                            {
                                notify(text, error);
                            }
                        }
                    }
                }
                let ui_locked = core.ui_locked.load(Ordering::SeqCst);
                if auto > 0 && idle >= auto && !ui_locked && guard.is_some() {
                    core.lock_app(&mut guard);
                    notify(
                        "VeilForge locked itself after a period without activity.".into(),
                        false,
                    );
                } else if ui_locked {
                    // Drops a kept map once nothing is open any more.
                    core.lock_app(&mut guard);
                }
                core.update_session_hold(&guard);
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
        let core = Arc::new(Core::new(settings.clone()));
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
                sys.set_hotkey(sys::parse_hotkey(&settings.hotkey));
                *relax(core.sys.lock()) = Some(sys);
                has_sys = true;
                let (core, tx, ctx) = (core.clone(), tx.clone(), ctx.clone());
                let _ = std::thread::Builder::new()
                    .name("veilforge-controller".into())
                    .spawn(move || controller(core, sys_rx, tx, ctx));
            }
        }

        let real_system = !demo && options.sandbox.is_none();
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
                    let go = match &mut self.screen {
                        Screen::Unlock(s) => s.unlocked(result, kind, attempt),
                        Screen::Welcome(s) => s.unlocked(result, kind, attempt),
                        Screen::Main(_) => None,
                    };
                    if let Some(go) = go {
                        self.go(ctx, go);
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
                                    ui.add(egui::Label::new(t.text.as_str()).wrap());
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
                if new.hotkey != old.hotkey {
                    sys.set_hotkey(sys::parse_hotkey(&new.hotkey));
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

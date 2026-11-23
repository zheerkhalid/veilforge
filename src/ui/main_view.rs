//! The unlocked window: header, navigation, and the plumbing between the
//! views and the engine (or, in the demo, the invented list).

use super::app::{Go, Msg, Screen, Shared};
use super::dialogs::Dialog;
use super::ops::{self, Line, Op, OpKind, Outcome, Snapshot};
use super::start::{paint_logo, Attempts, CreateState, HONEST_LINE};
use super::theme::{pal_ctx, regular, semibold, Icon};
use super::widgets;
use crate::demo::DemoVault;
use crate::engine::{Error, Health, Progress};
use crate::model::State;
use eframe::egui::{
    self, vec2, Align, Align2, CornerRadius, Frame, Key, Layout, Margin, Rect, RichText, Sense,
    Stroke, TextureHandle,
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Items,
    Health,
    Settings,
    About,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Filter {
    All,
    Group(String),
    NoGroup,
    Open,
    Problems,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortCol {
    Name,
    Location,
    Mode,
    State,
    Size,
    Date,
}

/// An operation that is running on a worker thread.
pub struct Job {
    pub kind: OpKind,
    pub progress: Arc<Mutex<Progress>>,
    pub started: Instant,
}

pub struct Main {
    pub tab: Tab,
    pub snap: Snapshot,
    /// The invented list, in demo mode.
    pub(super) demo: Option<DemoVault>,
    pub search: String,
    pub(super) focus_search: bool,
    pub filter: Filter,
    pub sort: (SortCol, bool),
    pub checked: HashSet<String>,
    pub selected: Option<String>,
    pub dialog: Option<Dialog>,
    pub job: Option<Job>,
    pub health: Option<Vec<Health>>,
    /// Wrong-password counter shared by the dialogs that ask for it again.
    pub attempts: Attempts,
    last_refresh: Instant,
    refreshing: bool,
    refresh_wanted: bool,
    pub(super) precheck_gen: u64,
    /// The group box in the details panel: (item id, text being typed).
    pub(super) group_edit: Option<(String, String)>,
    pub(super) hotkey_text: Option<String>,
    pub(super) lock_hotkey_text: Option<String>,
    /// Wrong master passwords typed while the user was away, told once
    /// after unlocking (see `crate::attempts`).
    pub attempt_notice: Option<String>,
    /// The wrong-password log as the Settings page shows it; read again
    /// when the page is opened.
    pub(super) attempt_log: Option<crate::attempts::Log>,
    /// Show the end of the Settings page (the `settings-log` view), until
    /// the user scrolls or clicks.
    pub(super) settings_at_end: bool,
}

impl Main {
    pub fn new(snapshot: Snapshot, demo: Option<DemoVault>) -> Main {
        let snap = match &demo {
            Some(d) => d.snapshot(),
            None => snapshot,
        };
        Main {
            tab: Tab::Items,
            snap,
            demo,
            search: String::new(),
            focus_search: false,
            filter: Filter::All,
            sort: (SortCol::Date, false),
            checked: HashSet::new(),
            selected: None,
            dialog: None,
            job: None,
            health: None,
            attempts: Attempts::default(),
            last_refresh: Instant::now(),
            refreshing: false,
            refresh_wanted: false,
            precheck_gen: 0,
            group_edit: None,
            hotkey_text: None,
            lock_hotkey_text: None,
            attempt_notice: None,
            attempt_log: None,
            settings_at_end: false,
        }
    }

    pub fn take_demo(&mut self) -> Option<DemoVault> {
        self.demo.take()
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn offer_emergency_sheet(&mut self) {
        self.dialog = Some(Dialog::Emergency { first_run: true });
    }

    // ----------------------------------------------------------------------------------
    // Engine plumbing
    // ----------------------------------------------------------------------------------

    /// Starts an operation. In the demo it runs at once on the invented list.
    pub(super) fn start(&mut self, ctx: &egui::Context, sh: &mut Shared, op: Op) {
        if self.job.is_some() {
            sh.toast("VeilForge is still working on the last request.", true);
            return;
        }
        let kind = op.kind();
        if let Some(demo) = &mut self.demo {
            let outcome = demo.run(op);
            let snapshot = demo.snapshot();
            self.finished(ctx, sh, outcome, Some(snapshot));
            return;
        }
        let progress = Arc::new(Mutex::new(Progress {
            done: 0,
            total: 0,
            what: String::new(),
        }));
        self.job = Some(Job {
            kind,
            progress: progress.clone(),
            started: Instant::now(),
        });
        let core = sh.core.clone();
        sh.spawn(ctx, move || {
            let mut guard = core.lock_vault();
            let outcome = match guard.as_mut() {
                Some(vault) => ops::run(vault, op, &mut |p| {
                    if let Ok(mut shared) = progress.lock() {
                        *shared = p;
                    }
                }),
                None => Outcome::failed(
                    kind,
                    Error::Io("VeilForge is locked. Unlock it and try again.".into()),
                ),
            };
            let snapshot = guard.as_ref().map(Snapshot::of);
            Msg::Done(Box::new(outcome), snapshot)
        });
    }

    pub(super) fn message(&mut self, ctx: &egui::Context, sh: &mut Shared, msg: Msg) {
        match msg {
            Msg::Done(outcome, snapshot) => {
                self.job = None;
                self.finished(ctx, sh, *outcome, snapshot);
            }
            Msg::Snapshot(snapshot) => {
                self.refreshing = false;
                if let Some(snapshot) = snapshot {
                    self.set_snapshot(snapshot);
                }
            }
            Msg::Precheck(generation, index, pre) if generation == self.precheck_gen => {
                if let Some(Dialog::Protect(dlg)) = &mut self.dialog {
                    dlg.checked(index, *pre, &sh.settings);
                }
            }
            _ => {}
        }
    }

    fn set_snapshot(&mut self, snapshot: Snapshot) {
        self.snap = snapshot;
        let ids: HashSet<&str> = self.snap.items.iter().map(|i| i.id.as_str()).collect();
        self.checked.retain(|id| ids.contains(id.as_str()));
        if self.selected.as_deref().is_some_and(|id| !ids.contains(id)) {
            self.selected = None;
        }
        if let Filter::Group(g) = &self.filter {
            if !self.snap.groups.contains(g) {
                self.filter = Filter::All;
            }
        }
    }

    /// Reads the list again at the next opportunity.
    pub fn refresh_soon(&mut self) {
        self.refresh_wanted = true;
    }

    /// Looks at the map again every two seconds (the `--open` helper and the
    /// background timers change it too). Runs on a worker thread and gives
    /// up at once when the map is busy.
    pub fn periodic_refresh(&mut self, ctx: &egui::Context, sh: &Shared) {
        if self.demo.is_some() || self.refreshing || self.job.is_some() {
            return;
        }
        if !self.refresh_wanted && self.last_refresh.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.refresh_wanted = false;
        self.refreshing = true;
        self.last_refresh = Instant::now();
        let core = sh.core.clone();
        sh.spawn(ctx, move || {
            let snapshot = match core.vault.try_lock() {
                Ok(mut guard) => guard.as_mut().map(|vault| {
                    let _ = vault.refresh();
                    Snapshot::of(vault)
                }),
                Err(_) => None,
            };
            Msg::Snapshot(snapshot)
        });
    }

    /// An operation finished: update the list and tell the user.
    fn finished(
        &mut self,
        _ctx: &egui::Context,
        sh: &mut Shared,
        outcome: Outcome,
        snapshot: Option<Snapshot>,
    ) {
        if let Some(snapshot) = snapshot {
            self.set_snapshot(snapshot);
        }
        let kind = outcome.kind;
        let failures = outcome.failures();
        let successes = outcome.successes();
        if let Some(health) = outcome.health {
            self.health = Some(health);
        } else if !matches!(
            kind,
            OpKind::Health | OpKind::SetGroup | OpKind::ChangePassword
        ) {
            // The list changed: the next visit to the health page checks again.
            self.health = None;
        }
        if let Some(error) = outcome.fatal {
            let wrong = matches!(error, Error::WrongPassword);
            if wrong {
                self.attempts.failed();
                if !sh.demo {
                    crate::attempts::record_failure(
                        &sh.data_dir,
                        if kind == OpKind::ChangePassword {
                            crate::attempts::Place::ChangePassword
                        } else {
                            crate::attempts::Place::Restore
                        },
                        crate::util::now(),
                    );
                    self.attempt_log = None;
                }
            }
            let (text, detail) = ops::explain(&error);
            let text = if detail.is_empty() {
                text
            } else {
                format!("{text} {detail}")
            };
            match kind {
                OpKind::PanicRestore => self.dialog = Some(Dialog::panic_with_error(text)),
                OpKind::ChangePassword => {
                    self.dialog = Some(Dialog::change_password_with_error(if wrong {
                        "The current master password is not correct.".to_string()
                    } else {
                        text
                    }));
                }
                _ => sh.toast(text, true),
            }
            return;
        }
        match kind {
            OpKind::Health => {}
            OpKind::ChangePassword => {
                self.attempts = Attempts::default();
                sh.toast(kind.success(1), false);
            }
            OpKind::ImportRecovery => sh.toast(kind.success(outcome.count), outcome.count == 0),
            OpKind::PanicRestore => {
                self.attempts = Attempts::default();
                self.checked.clear();
                self.selected = None;
                self.dialog = Some(Dialog::results(kind, outcome.lines, sh.elevated));
            }
            _ if failures > 0 => {
                self.dialog = Some(Dialog::results(kind, outcome.lines, sh.elevated));
            }
            _ => {
                if kind == OpKind::OpenTemp && sh.settings.open_after_unlock {
                    self.show_in_explorer(sh, &outcome.lines);
                }
                if kind == OpKind::Repair && outcome.lines.len() == 1 {
                    let text = outcome.lines[0]
                        .result
                        .clone()
                        .ok()
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| kind.success(1));
                    sh.toast(text, false);
                } else {
                    sh.toast(kind.success(successes), false);
                }
            }
        }
    }

    /// Opens File Explorer at the items that were just unlocked (at most three windows).
    fn show_in_explorer(&mut self, sh: &mut Shared, lines: &[Line]) {
        if sh.demo || sh.shot {
            return;
        }
        for path in lines.iter().filter_map(|l| l.path.as_ref()).take(3) {
            if let Err(e) = crate::sys::reveal_in_explorer(path) {
                sh.toast(e, true);
                break;
            }
        }
    }

    /// Opens the "Protect" dialog for these paths.
    pub fn offer_protect(&mut self, ctx: &egui::Context, sh: &mut Shared, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        if self.job.is_some() {
            sh.toast("VeilForge is still working on the last request.", true);
            return;
        }
        self.tab = Tab::Items;
        self.precheck_gen += 1;
        let mut dlg = super::protect::ProtectDlg::new(Vec::new(), &sh.settings);
        let added = dlg.add(paths);
        self.check_rows(ctx, sh, &mut dlg, added);
        self.dialog = Some(Dialog::Protect(Box::new(dlg)));
    }

    /// Asks the engine about the rows `added` of the "Protect" dialog.
    fn check_rows(
        &self,
        ctx: &egui::Context,
        sh: &Shared,
        dlg: &mut super::protect::ProtectDlg,
        added: std::ops::Range<usize>,
    ) {
        let generation = self.precheck_gen;
        let rows: Vec<(usize, PathBuf)> = added
            .filter_map(|index| dlg.rows.get(index).map(|r| (index, r.path.clone())))
            .collect();
        if self.demo.is_some() {
            for (index, path) in rows {
                dlg.checked(index, crate::demo::precheck(&path), &sh.settings);
            }
            return;
        }
        if rows.is_empty() {
            return;
        }
        // One worker goes through the paths in order; counting a large
        // folder takes a while and must never block the window.
        let (tx, env, ctx) = (sh.tx.clone(), sh.env.clone(), ctx.clone());
        std::thread::spawn(move || {
            for (index, path) in rows {
                let pre = crate::engine::precheck(&env, &path);
                if tx
                    .send(Msg::Precheck(generation, index, Box::new(pre)))
                    .is_err()
                {
                    return;
                }
                ctx.request_repaint();
            }
        });
    }

    /// More files were dropped while the "Protect" dialog is open: they join
    /// the list in it. Returns how many were new.
    pub(super) fn add_to_protect(
        &mut self,
        ctx: &egui::Context,
        sh: &Shared,
        paths: Vec<PathBuf>,
    ) -> usize {
        let Some(Dialog::Protect(mut dlg)) = self.dialog.take() else {
            return 0;
        };
        let added = dlg.add(paths);
        let count = added.len();
        self.check_rows(ctx, sh, &mut dlg, added);
        self.dialog = Some(Dialog::Protect(dlg));
        count
    }

    /// The file pickers. In the demo, invented paths are offered instead: a
    /// real picker would put real names on the screen.
    pub(super) fn pick_and_offer(&mut self, ctx: &egui::Context, sh: &mut Shared, folders: bool) {
        if self.demo.is_some() {
            self.offer_protect(ctx, sh, crate::demo::sample_paths());
            return;
        }
        let dialog = rfd::FileDialog::new().set_title(if folders {
            "Choose folders to protect"
        } else {
            "Choose files to protect"
        });
        let picked = if folders {
            dialog.pick_folders()
        } else {
            dialog.pick_files()
        };
        if let Some(paths) = picked {
            self.offer_protect(ctx, sh, paths);
        }
    }

    pub(super) fn open_count(&self) -> usize {
        self.snap.open_count()
    }

    pub(super) fn state_of(&self, id: &str) -> State {
        self.snap.state(id)
    }

    // ----------------------------------------------------------------------------------
    // Frame
    // ----------------------------------------------------------------------------------

    /// Returns true when Ctrl+L asked to lock VeilForge.
    fn handle_keys(&mut self, ctx: &egui::Context) -> bool {
        let (find, escape, lock) = ctx.input(|i| {
            (
                i.modifiers.command && i.key_pressed(Key::F),
                i.key_pressed(Key::Escape),
                i.modifiers.command && !i.modifiers.shift && i.key_pressed(Key::L),
            )
        });
        if lock && self.dialog.is_none() && self.job.is_none() {
            return true;
        }
        if find && self.dialog.is_none() {
            self.tab = Tab::Items;
            self.focus_search = true;
        }
        if escape && self.dialog.is_none() && self.job.is_none() {
            if self.selected.is_some() {
                self.selected = None;
            } else if !self.search.is_empty() {
                self.search.clear();
            } else {
                self.checked.clear();
            }
        }
        false
    }

    fn header(
        &mut self,
        ctx: &egui::Context,
        sh: &mut Shared,
        logo: Option<&TextureHandle>,
        go: &mut Option<Go>,
    ) {
        let p = pal_ctx(ctx);
        egui::TopBottomPanel::top("header")
            .exact_height(64.0)
            .frame(
                Frame::new()
                    .fill(p.rail)
                    .inner_margin(Margin::symmetric(18, 0)),
            )
            .show(ctx, |ui| {
                let full = ui.max_rect();
                ui.painter().hline(
                    full.x_range().expand(18.0),
                    full.bottom(),
                    Stroke::new(1.0_f32, p.border),
                );
                ui.horizontal_centered(|ui| {
                    paint_logo(ui, logo, 32.0);
                    ui.add_space(2.0);
                    ui.label(RichText::new(sh.settings.title()).font(semibold(19.0)));
                    if sh.demo {
                        widgets::badge(ui, "DEMO", p.info)
                            .on_hover_text("Invented data. Nothing on this PC is changed.");
                    }
                    ui.add_space(190.0 - ui.min_rect().width().min(190.0));

                    let search_width = (ui.available_width() - 330.0).clamp(220.0, 460.0);
                    Frame::new()
                        .fill(if p.dark { p.card } else { p.bg })
                        .stroke(Stroke::new(1.0_f32, p.border))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::symmetric(12, 6))
                        .show(ui, |ui| {
                            ui.set_width(search_width);
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(Icon::SEARCH.glyph())
                                        .size(14.0)
                                        .color(p.faint),
                                );
                                let edit = egui::TextEdit::singleline(&mut self.search)
                                    .hint_text(
                                        RichText::new("Search protected items").color(p.faint),
                                    )
                                    .frame(false)
                                    .desired_width(ui.available_width() - 60.0)
                                    .font(regular(15.0));
                                let resp = ui.add_enabled(self.tab == Tab::Items, edit);
                                if self.focus_search {
                                    resp.request_focus();
                                    self.focus_search = false;
                                }
                                if !self.search.is_empty() {
                                    if widgets::icon_button(ui, Icon::CLOSE, "Clear search (Esc)")
                                        .clicked()
                                    {
                                        self.search.clear();
                                    }
                                } else {
                                    ui.label(RichText::new("Ctrl+F").size(11.5).color(p.faint));
                                }
                            });
                        });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::ghost_button(ui, &Icon::LOCK.with("Lock VeilForge"))
                            .on_hover_text(
                                "Close the list and ask for the master password again (Ctrl+L). Protected items stay protected.",
                            )
                            .clicked()
                        {
                            *go = Some(Go::Lock);
                        }
                        ui.add_space(6.0);
                        let open = self.open_count();
                        if open > 0 {
                            widgets::badge(
                                ui,
                                &Icon::UNLOCK.with(&format!("{open} temporarily open")),
                                p.warn,
                            )
                            .on_hover_text(
                                "These items are unlocked for now. \"Lock all\" protects them again.",
                            );
                        } else if !self.snap.items.is_empty() {
                            widgets::badge(ui, &Icon::SHIELD.with("Everything locked"), p.ok);
                        }
                    });
                });
            });
    }

    fn rail(&mut self, ctx: &egui::Context) {
        let p = pal_ctx(ctx);
        egui::SidePanel::left("rail")
            .exact_width(208.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(p.rail)
                    .inner_margin(Margin::symmetric(10, 14)),
            )
            .show(ctx, |ui| {
                let full = ui.max_rect();
                ui.painter().vline(
                    full.right() + 10.0,
                    full.y_range().expand(14.0),
                    Stroke::new(1.0_f32, p.border),
                );
                ui.spacing_mut().item_spacing.y = 4.0;
                let problems = match &self.health {
                    Some(h) => h.iter().filter(|l| !l.problem.is_empty()).count(),
                    None => self.snap.problem_count(),
                };
                let items: [(Tab, Icon, &str, Option<usize>); 4] = [
                    (
                        Tab::Items,
                        Icon::LOCK,
                        "Protected items",
                        Some(self.snap.items.len()),
                    ),
                    (Tab::Health, Icon::HEALTH, "Health check", Some(problems)),
                    (Tab::Settings, Icon::SETTINGS, "Settings", None),
                    (Tab::About, Icon::INFO, "About", None),
                ];
                for (tab, icon, label, count) in items {
                    if tab == Tab::Settings {
                        ui.add_space(8.0);
                        widgets::divider(ui);
                        ui.add_space(8.0);
                    }
                    let active = self.tab == tab;
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                    if active {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(9), p.selected);
                        let bar = Rect::from_min_size(
                            rect.left_center() - vec2(0.0, 9.0),
                            vec2(3.0, 18.0),
                        );
                        ui.painter()
                            .rect_filled(bar, CornerRadius::same(2), p.accent);
                    } else if resp.hovered() {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(9), p.hover);
                    }
                    let fg = if active { p.text } else { p.muted };
                    ui.painter().text(
                        rect.left_center() + vec2(24.0, 0.0),
                        Align2::CENTER_CENTER,
                        icon.glyph(),
                        regular(16.0),
                        if active { p.accent } else { fg },
                    );
                    ui.painter().text(
                        rect.left_center() + vec2(46.0, -1.0),
                        Align2::LEFT_CENTER,
                        label,
                        if active {
                            semibold(14.5)
                        } else {
                            regular(14.5)
                        },
                        fg,
                    );
                    if let Some(n) = count {
                        let warn = tab == Tab::Health && n > 0;
                        if tab != Tab::Health || n > 0 {
                            ui.painter().text(
                                rect.right_center() - vec2(12.0, 1.0),
                                Align2::RIGHT_CENTER,
                                n.to_string(),
                                regular(12.5),
                                if warn { p.warn } else { p.faint },
                            );
                        }
                    }
                    if resp.clicked() {
                        self.tab = tab;
                        self.selected = None;
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                                .size(12.0)
                                .color(p.faint),
                        );
                    });
                });
            });
    }

    /// The honest one-line footer.
    fn footer(&mut self, ctx: &egui::Context) {
        let p = pal_ctx(ctx);
        egui::TopBottomPanel::bottom("footer")
            .exact_height(30.0)
            .frame(
                Frame::new()
                    .fill(p.rail)
                    .inner_margin(Margin::symmetric(24, 0)),
            )
            .show(ctx, |ui| {
                let full = ui.max_rect();
                ui.painter().hline(
                    full.x_range().expand(24.0),
                    full.top(),
                    Stroke::new(1.0_f32, p.border),
                );
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(RichText::new(Icon::INFO.glyph()).size(12.5).color(p.faint));
                    ui.label(RichText::new(HONEST_LINE).size(12.5).color(p.muted));
                    if self.tab != Tab::About
                        && ui
                            .add(
                                egui::Label::new(
                                    RichText::new("What that means").size(12.5).color(p.accent),
                                )
                                .sense(Sense::click()),
                            )
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                    {
                        self.tab = Tab::About;
                        self.selected = None;
                    }
                });
            });
    }

    /// Files dragged over or dropped on the window.
    fn drops(&mut self, ctx: &egui::Context, sh: &mut Shared) {
        if sh.shot {
            return;
        }
        let hovering = ctx.input(|i| i.raw.hovered_files.len());
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        // While the "Protect" dialog is open, a drop adds to its list.
        let adding = matches!(self.dialog, Some(Dialog::Protect(_)));
        let can_take = (self.dialog.is_none() || adding) && self.job.is_none();
        if hovering > 0 {
            let p = pal_ctx(ctx);
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop-overlay"),
            ));
            painter.rect_filled(screen, 0, p.bg.gamma_multiply(0.86));
            let inner = screen.shrink(28.0);
            painter.rect_stroke(
                inner,
                CornerRadius::same(18),
                Stroke::new(2.0_f32, p.accent),
                egui::StrokeKind::Inside,
            );
            painter.text(
                inner.center() - vec2(0.0, 34.0),
                Align2::CENTER_CENTER,
                Icon::LOCK.glyph(),
                regular(44.0),
                p.accent,
            );
            painter.text(
                inner.center() + vec2(0.0, 14.0),
                Align2::CENTER_CENTER,
                drop_title(hovering, can_take, adding),
                semibold(24.0),
                p.text,
            );
            painter.text(
                inner.center() + vec2(0.0, 44.0),
                Align2::CENTER_CENTER,
                if !can_take {
                    "Close the dialog or wait for the running operation, then drop again."
                } else if adding {
                    "They join the list in the dialog. Nothing happens until you confirm."
                } else {
                    "You choose how in the next step. Nothing happens until you confirm."
                },
                regular(14.5),
                p.muted,
            );
        }
        if !dropped.is_empty() {
            if !can_take {
                sh.toast(
                    "Finish what is open first, then drop the files again.",
                    true,
                );
            } else if adding {
                let paths = if self.demo.is_some() {
                    crate::demo::sample_paths()
                } else {
                    dropped
                };
                match self.add_to_protect(ctx, sh, paths) {
                    0 => sh.toast("Those are already in the list.", false),
                    1 => sh.toast("1 more item added to the list.", false),
                    n => sh.toast(format!("{n} more items added to the list."), false),
                }
            } else if self.demo.is_some() {
                // Real names never appear in the demo.
                self.offer_protect(ctx, sh, crate::demo::sample_paths());
            } else {
                self.offer_protect(ctx, sh, dropped);
            }
        }
    }

    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        sh: &mut Shared,
        logo: Option<&TextureHandle>,
    ) -> Option<Go> {
        let mut go = None;
        if self.handle_keys(ctx) {
            go = Some(Go::Lock);
        }
        if self.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if self.tab == Tab::Health && self.health.is_none() && self.job.is_none() {
            self.start(ctx, sh, Op::Health);
        }

        self.header(ctx, sh, logo, &mut go);
        self.rail(ctx);
        self.footer(ctx);

        let p = pal_ctx(ctx);
        if self.tab == Tab::Items && self.selected.is_some() {
            egui::SidePanel::right("details")
                .exact_width(340.0)
                .resizable(false)
                .frame(Frame::new().fill(p.rail).inner_margin(Margin::same(18)))
                .show(ctx, |ui| self.details_ui(ui, sh));
        }
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(p.bg)
                    .inner_margin(Margin::symmetric(24, 18)),
            )
            .show(ctx, |ui| match self.tab {
                Tab::Items => self.items_ui(ui, sh),
                Tab::Health => self.health_ui(ui, sh),
                Tab::Settings => self.settings_ui(ui, sh),
                Tab::About => self.about_ui(ui, sh, logo),
            });

        self.dialogs(ctx, sh, logo);
        self.working_overlay(ctx);
        self.drops(ctx, sh);
        go
    }
}

/// The big line of the drop overlay.
fn drop_title(hovering: usize, can_take: bool, adding: bool) -> String {
    match (can_take, adding, hovering) {
        (false, ..) => "Finish what is open first".to_string(),
        (true, false, 0 | 1) => "Drop to protect".to_string(),
        (true, false, n) => format!("Drop to protect {n} items"),
        (true, true, 0 | 1) => "Drop to add to the list".to_string(),
        (true, true, n) => format!("Drop to add {n} items to the list"),
    }
}

/// The screen a `--view` name opens in the demo (and in screenshots).
pub fn demo_screen(view: &str, sh: &mut Shared) -> Screen {
    let now = sh.now;
    let mut main = Main::new(Snapshot::default(), Some(DemoVault::sample(now)));
    let id_of = |main: &Main, name: &str| {
        main.snap
            .items
            .iter()
            .find(|i| i.name == name)
            .map(|i| i.id.clone())
    };
    match view {
        "welcome" => return Screen::Welcome(Box::new(CreateState::demo_filled())),
        "unlock" => return Screen::Unlock(Box::default()),
        // The lock screen while an item is still temporarily open.
        "locked-open" => {
            sh.demo_keep = Some(DemoVault::sample(now));
            return Screen::Unlock(Box::default());
        }
        "empty" => main = Main::new(Snapshot::default(), Some(DemoVault::empty(now))),
        "details" => main.selected = id_of(&main, "Side project"),
        // An item with a Kurdish name, in the list and in the details panel.
        "kurdish" => {
            main.selected = main
                .snap
                .items
                .iter()
                .find(|i| !i.name.is_ascii())
                .map(|i| i.id.clone());
        }
        "selection" => {
            for name in ["Tax 2025", "Passport scan.pdf", "Client contracts"] {
                if let Some(id) = id_of(&main, name) {
                    main.checked.insert(id);
                }
            }
        }
        "protect" => {
            let paths = crate::demo::sample_paths();
            let mut dlg = super::protect::ProtectDlg::new(paths.clone(), &sh.settings);
            for (index, path) in paths.iter().enumerate() {
                dlg.checked(index, crate::demo::precheck(path), &sh.settings);
            }
            dlg.set_group("Work");
            main.dialog = Some(Dialog::Protect(Box::new(dlg)));
        }
        "prompt" => main.dialog = Some(Dialog::PromptPreview(Box::default())),
        "health" | "panic" => {
            main.tab = Tab::Health;
            main.health = main.demo.as_ref().map(DemoVault::health);
            if view == "panic" {
                main.dialog = Some(Dialog::panic());
            }
        }
        "settings" => main.tab = Tab::Settings,
        // The lower half of the Settings page: hotkeys, wrong-password log.
        "settings-log" => {
            main.tab = Tab::Settings;
            main.settings_at_end = true;
            sh.settings.lock_hotkey = "Ctrl+Alt+L".into();
        }
        // The list with the notice about wrong passwords typed meanwhile.
        "notice" => {
            let log = crate::demo::attempt_log(now);
            main.attempt_notice = crate::attempts::Notice {
                count: 2,
                last: log.entries.last().copied(),
            }
            .text();
        }
        "about" => main.tab = Tab::About,
        "results" => {
            let lines = vec![
                Line::ok("Letters", ""),
                Line::err(
                    "Payroll 2026.xlsx",
                    Error::InUse("Payroll 2026.xlsx is open in a spreadsheet program.".into()),
                ),
                Line::err("Shared tools", Error::NeedsElevation(String::new())),
            ];
            main.dialog = Some(Dialog::results(OpKind::Protect, lines, false));
        }
        "change-password" => main.dialog = Some(Dialog::change_password()),
        _ => {}
    }
    Screen::Main(Box::new(main))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_drop_overlay_says_what_a_drop_will_do() {
        assert_eq!(drop_title(1, true, false), "Drop to protect");
        assert_eq!(drop_title(4, true, false), "Drop to protect 4 items");
        assert_eq!(drop_title(1, true, true), "Drop to add to the list");
        assert_eq!(drop_title(3, true, true), "Drop to add 3 items to the list");
        assert_eq!(drop_title(3, false, false), "Finish what is open first");
        assert_eq!(drop_title(3, false, true), "Finish what is open first");
    }
}

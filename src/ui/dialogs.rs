//! Modal dialogs of the unlocked window. Every dialog has its own id (a modal
//! remembers its size) and its button row has a fixed height.

use super::app::Shared;
use super::main_view::Main;
use super::ops::{self, Line, Op, OpKind};
use super::prompt::{self, PromptState};
use super::protect::{Choice, ProtectDlg};
use super::start::{strength_block, wait_label, StrengthCache};
use super::theme::{pal, pal_ctx, semibold, Icon};
use super::widgets;
use crate::secret::SecretInput;
use crate::strength;
use eframe::egui::{self, CornerRadius, Frame, Key, Margin, RichText, TextureHandle, Ui};
use zeroize::Zeroizing;

pub enum ConfirmAction {
    /// Unlock permanently.
    Release(Vec<String>),
    /// Unlock temporarily.
    OpenTemp(Vec<String>),
}

/// A dialog that asks for the master password again.
pub struct PwDlg {
    password: SecretInput,
    show: bool,
    error: Option<String>,
    focus: bool,
}

impl PwDlg {
    fn new(error: Option<String>) -> Box<PwDlg> {
        Box::new(PwDlg {
            password: SecretInput::new(),
            show: false,
            error,
            focus: true,
        })
    }
}

pub struct ChangePw {
    current: SecretInput,
    password: SecretInput,
    confirm: SecretInput,
    show: bool,
    strength: StrengthCache,
    error: Option<String>,
    focus: bool,
}

pub struct ResultsDlg {
    kind: OpKind,
    lines: Vec<Line>,
    elevated: bool,
}

pub enum Dialog {
    Confirm {
        title: String,
        text: String,
        button: String,
        danger: bool,
        action: ConfirmAction,
    },
    Protect(Box<ProtectDlg>),
    Results(Box<ResultsDlg>),
    ChangePassword(Box<ChangePw>),
    Panic(Box<PwDlg>),
    Emergency {
        first_run: bool,
    },
    Group {
        ids: Vec<String>,
        name: String,
        focus: bool,
    },
    /// The `--open` prompt drawn inside the main window (demo and screenshots).
    PromptPreview(Box<PromptState>),
}

impl Dialog {
    pub fn panic() -> Dialog {
        Dialog::Panic(PwDlg::new(None))
    }

    pub fn panic_with_error(text: String) -> Dialog {
        Dialog::Panic(PwDlg::new(Some(text)))
    }

    pub fn change_password() -> Dialog {
        Dialog::change_password_error(None)
    }

    pub fn change_password_with_error(text: String) -> Dialog {
        Dialog::change_password_error(Some(text))
    }

    fn change_password_error(error: Option<String>) -> Dialog {
        Dialog::ChangePassword(Box::new(ChangePw {
            current: SecretInput::new(),
            password: SecretInput::new(),
            confirm: SecretInput::new(),
            show: false,
            strength: StrengthCache::default(),
            error,
            focus: true,
        }))
    }

    pub fn results(kind: OpKind, lines: Vec<Line>, elevated: bool) -> Dialog {
        Dialog::Results(Box::new(ResultsDlg {
            kind,
            lines,
            elevated,
        }))
    }

    /// Whether a click outside or Esc may close it.
    fn dismissable(&self) -> bool {
        !matches!(self, Dialog::Protect(_) | Dialog::ChangePassword(_))
    }
}

fn caps_hint(ui: &mut Ui, sh: &Shared) {
    if crate::sys::caps_lock_on() && !sh.shot {
        let p = pal(ui);
        ui.colored_label(p.warn, Icon::WARNING.with("Caps Lock is on"));
    }
}

fn results_ui(ui: &mut Ui, dlg: &ResultsDlg) -> (bool, bool) {
    let p = pal(ui);
    let (mut close, mut restart) = (false, false);
    ui.set_width(580.0);
    widgets::dialog_title(ui, dlg.kind.done_title());
    let failed = dlg.lines.iter().filter(|l| l.result.is_err()).count();
    let worked = dlg.lines.len() - failed;
    widgets::para(
        ui,
        &match (worked, failed) {
            (_, 0) => format!("All {} worked.", dlg.lines.len()),
            (0, _) => "None of them worked. Nothing was changed for the items below.".to_string(),
            _ => format!(
                "{worked} worked, {failed} did not. Items that did not work were left as they were."
            ),
        },
    );
    ui.add_space(6.0);
    Frame::new()
        .fill(p.bg)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    // Problems first: they are what the user has to read.
                    let ordered = dlg
                        .lines
                        .iter()
                        .filter(|l| l.result.is_err())
                        .chain(dlg.lines.iter().filter(|l| l.result.is_ok()));
                    for line in ordered {
                        ui.horizontal_top(|ui| {
                            let (icon, color) = match &line.result {
                                Ok(_) => (Icon::DONE, p.ok),
                                Err(_) => (Icon::ERROR, p.danger),
                            };
                            ui.label(RichText::new(icon.glyph()).color(color));
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&line.name).font(semibold(14.0)),
                                    )
                                    .wrap(),
                                );
                                match &line.result {
                                    Ok(text) => {
                                        let text = match (dlg.kind, text.is_empty()) {
                                            (OpKind::PanicRestore | OpKind::Release, false) => {
                                                format!("Back at {text}")
                                            }
                                            (_, true) => "Done".to_string(),
                                            (_, false) => text.clone(),
                                        };
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(text).size(13.0).color(p.muted),
                                            )
                                            .wrap(),
                                        );
                                    }
                                    Err(e) => {
                                        let (text, detail) = ops::explain(e);
                                        ui.add(
                                            egui::Label::new(RichText::new(text).size(13.0)).wrap(),
                                        );
                                        if !detail.is_empty() {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(detail).size(12.5).color(p.faint),
                                                )
                                                .wrap(),
                                            );
                                        }
                                    }
                                }
                            });
                        });
                        ui.add_space(2.0);
                    }
                });
        });
    let needs_admin = dlg
        .lines
        .iter()
        .any(|l| matches!(l.result, Err(crate::engine::Error::NeedsElevation(_))));
    if needs_admin && !dlg.elevated {
        ui.add_space(6.0);
        widgets::note(
            ui,
            "Some items belong to Windows or to another account. VeilForge can change them when it runs as administrator. Windows will ask for permission.",
        );
    }
    ui.add_space(12.0);
    widgets::button_row(ui, |ui| {
        if widgets::primary_button(ui, "Close", true).clicked() {
            close = true;
        }
        if needs_admin
            && !dlg.elevated
            && widgets::ghost_button(ui, &Icon::ADMIN.with("Restart VeilForge as administrator"))
                .clicked()
        {
            restart = true;
        }
    });
    (close, restart)
}

impl Main {
    /// Saves the emergency sheet where the user chooses and opens it for printing.
    pub(super) fn save_emergency_sheet(&mut self, sh: &mut Shared) {
        if sh.demo {
            sh.toast("Demo: no file is saved.", false);
            return;
        }
        let Some(mut path) = rfd::FileDialog::new()
            .set_title("Save the emergency sheet")
            .set_file_name("VeilForge emergency sheet.html")
            .add_filter("Web page", &["html"])
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension("html");
        }
        let volumes: Vec<String> = self
            .snap
            .items
            .iter()
            .filter(|i| i.hides())
            .map(|i| i.volume.clone())
            .collect();
        let page = crate::emergency::html(&sh.data_dir.to_string_lossy(), &volumes, sh.now);
        match std::fs::write(&path, page) {
            Ok(()) => {
                sh.toast(
                    "Sheet saved. It opens in your browser: print it, then write the password on it by hand.",
                    false,
                );
                if let Err(e) = crate::sys::open_saved_page(&path) {
                    sh.toast(e, true);
                }
            }
            Err(e) => sh.toast(format!("Could not save the sheet: {e}"), true),
        }
    }

    pub(super) fn dialogs(
        &mut self,
        ctx: &egui::Context,
        sh: &mut Shared,
        logo: Option<&TextureHandle>,
    ) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let p = pal_ctx(ctx);
        let mut close = false;
        let mut op: Option<Op> = None;
        let mut confirmed = false;
        let mut save_sheet = false;
        let mut restart = false;
        let wait = self.attempts.tick(ctx);
        let groups = self.snap.groups.clone();
        let item_count = self.snap.items.len();
        // One id per kind of dialog: a modal keeps the size it had last time,
        // so dialogs sharing an id would inherit each other's size.
        let modal = egui::Modal::new(egui::Id::new("dialog").with(std::mem::discriminant(&dialog)))
            .frame(widgets::modal_frame(ctx))
            .show(ctx, |ui| match &mut dialog {
                Dialog::Confirm {
                    title,
                    text,
                    button,
                    danger,
                    ..
                } => {
                    ui.set_width(500.0);
                    widgets::dialog_title(ui, title);
                    widgets::para(ui, text);
                    ui.add_space(12.0);
                    widgets::button_row(ui, |ui| {
                        let yes = if *danger {
                            widgets::danger_button(ui, button, true).clicked()
                        } else {
                            widgets::primary_button(ui, button, true).clicked()
                        };
                        if yes {
                            confirmed = true;
                            close = true;
                        }
                        if widgets::ghost_button(ui, "Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::Protect(dlg) => match dlg.ui(ui, &groups) {
                    Some(Choice::Cancel) => close = true,
                    Some(Choice::Protect(plan)) => {
                        op = Some(Op::Protect(plan));
                        close = true;
                    }
                    None => {}
                },
                Dialog::Results(dlg) => {
                    let (c, r) = results_ui(ui, dlg);
                    close = c;
                    restart = r;
                }
                Dialog::ChangePassword(dlg) => {
                    let w = 480.0;
                    ui.set_width(w);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    widgets::dialog_title(ui, "Change the master password");
                    widgets::caption(ui, "Current master password");
                    let resp = widgets::password_row(
                        ui,
                        &mut dlg.current,
                        &mut dlg.show,
                        "",
                        w,
                        true,
                    );
                    if dlg.focus {
                        resp.request_focus();
                        dlg.focus = false;
                    }
                    caps_hint(ui, sh);
                    ui.add_space(4.0);
                    widgets::caption(ui, "New master password");
                    widgets::secret_input(ui, &mut dlg.password, dlg.show, "", w);
                    let empty = dlg.password.is_empty();
                    let s = dlg.strength.get(dlg.password.as_str()).clone();
                    strength_block(ui, &s, empty, w);
                    ui.add_space(4.0);
                    widgets::caption(ui, "Type the new one again");
                    widgets::secret_input(ui, &mut dlg.confirm, dlg.show, "", w);
                    let matches = dlg.password.as_str() == dlg.confirm.as_str();
                    if !dlg.confirm.is_empty() && !matches {
                        ui.colored_label(p.danger, "The two new passwords are different.");
                    }
                    let refusal = strength::master_refusal(dlg.password.as_str(), &s);
                    if !empty {
                        if let Some(reason) = refusal {
                            ui.add(egui::Label::new(RichText::new(reason).color(p.danger)).wrap());
                        }
                    }
                    if let Some(e) = &dlg.error {
                        ui.add(egui::Label::new(RichText::new(e).color(p.danger)).wrap());
                    }
                    ui.add_space(2.0);
                    widgets::note(
                        ui,
                        "Your protected items are not touched. Only VeilForge's own list and its recovery copies are sealed with the new password. Update your emergency sheet afterwards.",
                    );
                    ui.add_space(8.0);
                    let ready = !dlg.current.is_empty()
                        && refusal.is_none()
                        && matches
                        && wait.is_none();
                    widgets::button_row(ui, |ui| {
                        let label = wait_label(wait, "Change password");
                        if widgets::primary_button(ui, &label, ready).clicked() {
                            if sh.demo {
                                sh.toast("Demo: the password was not changed.", false);
                            } else {
                                op = Some(Op::ChangePassword {
                                    current: Zeroizing::new(dlg.current.as_str().to_string()),
                                    new: Zeroizing::new(dlg.password.as_str().to_string()),
                                });
                            }
                            dlg.current.wipe();
                            dlg.password.wipe();
                            dlg.confirm.wipe();
                            close = true;
                        }
                        if widgets::ghost_button(ui, "Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::Panic(dlg) => {
                    let w = 520.0;
                    ui.set_width(w);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    widgets::dialog_title(ui, "Panic restore");
                    widgets::para(
                        ui,
                        &match item_count {
                            1 => "The 1 protected item will go back to where it was, exactly as it was: same place, same name, same permissions and dates. VeilForge then forgets it. Nothing is deleted.".to_string(),
                            n => format!("All {n} protected items will go back to where they were, exactly as they were: same place, same name, same permissions and dates. VeilForge then forgets them. Nothing is deleted."),
                        },
                    );
                    widgets::para(
                        ui,
                        "Use this before reinstalling Windows, before giving this PC away, or whenever you simply want everything back.",
                    );
                    ui.add_space(4.0);
                    widgets::caption(ui, "Type the master password to confirm");
                    let resp =
                        widgets::password_row(ui, &mut dlg.password, &mut dlg.show, "", w, true);
                    if dlg.focus {
                        resp.request_focus();
                        dlg.focus = false;
                    }
                    caps_hint(ui, sh);
                    if let Some(e) = &dlg.error {
                        ui.add(egui::Label::new(RichText::new(e).color(p.danger)).wrap());
                    }
                    ui.add_space(8.0);
                    let ready = !dlg.password.is_empty() && wait.is_none();
                    let enter = ui.input(|i| i.key_pressed(Key::Enter));
                    widgets::button_row(ui, |ui| {
                        let label = wait_label(wait, "Restore everything");
                        if widgets::danger_button(ui, &label, ready).clicked() || (enter && ready) {
                            op = Some(Op::PanicRestore(Zeroizing::new(
                                dlg.password.as_str().to_string(),
                            )));
                            dlg.password.wipe();
                            close = true;
                        }
                        if widgets::ghost_button(ui, "Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::Emergency { first_run } => {
                    ui.set_width(540.0);
                    widgets::dialog_title(
                        ui,
                        if *first_run {
                            "One more thing: make an emergency sheet"
                        } else {
                            "Make an emergency sheet"
                        },
                    );
                    widgets::para(
                        ui,
                        "If you ever forget the master password, nobody can unlock your items for you. An emergency sheet is a page you print, write the master password on by hand, and lock away.",
                    );
                    ui.add_space(4.0);
                    widgets::para(
                        ui,
                        "It also says what to do if VeilForge itself is lost: install it again, type the password, and use \"Panic restore\". VeilForge never writes the password into the sheet.",
                    );
                    ui.add_space(12.0);
                    widgets::button_row(ui, |ui| {
                        if widgets::primary_button(ui, &Icon::PRINT.with("Save the sheet…"), true)
                            .clicked()
                        {
                            save_sheet = true;
                            close = true;
                        }
                        if widgets::ghost_button(ui, "Not now").clicked() {
                            close = true;
                        }
                    });
                    if *first_run {
                        ui.label(
                            RichText::new("You can also do this later in Settings.")
                                .size(12.0)
                                .color(p.faint),
                        );
                    }
                }
                Dialog::Group { ids, name, focus } => {
                    let w = 400.0;
                    ui.set_width(w);
                    widgets::dialog_title(
                        ui,
                        &match ids.len() {
                            1 => "Set group".to_string(),
                            n => format!("Set group for {n} items"),
                        },
                    );
                    widgets::para(
                        ui,
                        "Groups only sort the list. Leave the box empty to take the items out of their group.",
                    );
                    ui.add_space(4.0);
                    let resp = widgets::text_input(ui, name, "For example: Work", w);
                    if *focus {
                        resp.request_focus();
                        *focus = false;
                    }
                    if !groups.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for g in groups.iter().take(8) {
                                if widgets::chip(ui, g, None, name.trim() == g).clicked() {
                                    *name = g.clone();
                                }
                            }
                        });
                    }
                    ui.add_space(12.0);
                    let enter = ui.input(|i| i.key_pressed(Key::Enter));
                    widgets::button_row(ui, |ui| {
                        if widgets::primary_button(ui, "Save", true).clicked() || enter {
                            op = Some(Op::SetGroup(ids.clone(), name.trim().to_string()));
                            close = true;
                        }
                        if widgets::ghost_button(ui, "Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::PromptPreview(state) => {
                    let spec = prompt::preview_spec();
                    // The same area the real window gives it.
                    let size = [prompt::SIZE[0] - 44.0, prompt::SIZE[1] - 44.0];
                    match prompt::content(ui, state, &spec, logo, size, sh.shot) {
                        prompt::Action::Submit => {
                            state.password.wipe();
                            sh.toast("Demo: this is only a preview of the prompt.", false);
                            close = true;
                        }
                        prompt::Action::Cancel => close = true,
                        prompt::Action::None => {}
                    }
                }
            });
        if modal.should_close() && dialog.dismissable() {
            close = true;
        }
        if !close {
            self.dialog = Some(dialog);
        } else if confirmed {
            if let Dialog::Confirm { action, .. } = dialog {
                op = Some(match action {
                    ConfirmAction::Release(ids) => Op::Release(ids),
                    ConfirmAction::OpenTemp(ids) => Op::OpenTemp(ids),
                });
            }
        }
        if save_sheet {
            self.save_emergency_sheet(sh);
        }
        if restart {
            sh.restart_as_admin(ctx);
        }
        if let Some(op) = op {
            self.start(ctx, sh, op);
        }
    }

    /// Progress of the running operation, on top of everything else.
    pub(super) fn working_overlay(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else { return };
        // Quick operations finish before anything needs to be shown.
        if job.started.elapsed().as_millis() < 300 {
            return;
        }
        let p = pal_ctx(ctx);
        let progress = job
            .progress
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        egui::Modal::new(egui::Id::new("working"))
            .frame(widgets::modal_frame(ctx))
            .show(ctx, |ui| {
                let w = 420.0;
                ui.set_width(w);
                widgets::left_row(ui, 30.0, |ui| {
                    ui.add(egui::Spinner::new().size(20.0).color(p.accent));
                    ui.label(RichText::new(job.kind.title()).font(semibold(17.0)));
                });
                ui.add_space(4.0);
                if progress.total > 0 {
                    let fraction = progress.done as f32 / progress.total as f32;
                    widgets::progress_bar(ui, fraction, w, p.accent_fill);
                    ui.add_space(2.0);
                }
                let what = if progress.what.is_empty() {
                    "Please wait. Do not turn the PC off."
                } else {
                    &progress.what
                };
                ui.add(egui::Label::new(RichText::new(what).size(13.0).color(p.muted)).truncate());
            });
    }
}

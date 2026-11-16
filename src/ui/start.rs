//! The screens before the list is open: first run (choose a master password)
//! and unlock (also "rebuild from recovery copies").

use super::app::{Go, Shared, UnlockKind};
use super::ops::Snapshot;
use super::theme::{pal, pal_ctx, semibold, Icon};
use super::widgets;
use crate::engine::Error;
use crate::secret::SecretInput;
use crate::strength::{self, Strength};
use eframe::egui::{
    self, vec2, Color32, CornerRadius, Frame, Key, Margin, Rect, RichText, Sense, Stroke,
    TextureHandle, Ui,
};
use std::hash::{Hash, Hasher};
use std::time::Instant;
use zeroize::Zeroizing;

pub(super) fn paint_logo(ui: &mut Ui, logo: Option<&TextureHandle>, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    if let Some(logo) = logo {
        ui.painter().image(
            logo.id(),
            rect,
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

fn page(ctx: &egui::Context, add: impl FnOnce(&mut Ui)) {
    let p = pal_ctx(ctx);
    egui::CentralPanel::default()
        .frame(
            Frame::new()
                .fill(p.bg)
                .inner_margin(Margin::symmetric(24, 18)),
        )
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, add);
        });
}

/// The one honest line shown on every screen.
pub const HONEST_LINE: &str =
    "Not encryption: VeilForge protects against normal use of this Windows account.";

fn hash_of(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// A strength estimate that is only recomputed when the text changes.
#[derive(Default)]
pub struct StrengthCache {
    key: Option<u64>,
    pub value: Option<Strength>,
}

impl StrengthCache {
    pub fn get(&mut self, password: &str) -> &Strength {
        let key = hash_of(password);
        if self.key != Some(key) || self.value.is_none() {
            self.value = Some(strength::estimate(password, &["veilforge"]));
            self.key = Some(key);
        }
        self.value.as_ref().expect("set above")
    }

    pub fn clear(&mut self) {
        self.key = None;
        self.value = None;
    }
}

/// The strength meter with plain-language feedback.
pub(super) fn strength_block(ui: &mut Ui, s: &Strength, empty: bool, width: f32) {
    let p = pal(ui);
    widgets::strength_bar(ui, s.score, empty, width);
    if empty {
        return;
    }
    widgets::left_row(ui, 20.0, |ui| {
        ui.label(
            RichText::new(strength::score_label(s.score))
                .font(semibold(13.5))
                .color(widgets::strength_color(&p, s.score)),
        );
        ui.label(
            RichText::new(format!(
                "{} of guessing work · time to guess it: {}",
                s.bits_text(),
                s.offline_slow
            ))
            .size(12.5)
            .color(p.muted),
        );
    });
    if let Some(w) = &s.warning {
        ui.add(egui::Label::new(RichText::new(w).size(12.5).color(p.warn)).wrap());
    }
    for tip in s.suggestions.iter().take(1) {
        ui.add(egui::Label::new(RichText::new(tip).size(12.5).color(p.muted)).wrap());
    }
}

/// The pause after `failures` wrong attempts. This only slows down someone
/// typing guesses at this window; the map file itself is protected by Argon2id.
pub fn retry_delay(failures: u32) -> f32 {
    match failures {
        0 | 1 => 0.0,
        n => (0.5 * 2f32.powi(n as i32 - 2)).min(8.0),
    }
}

/// Counts wrong passwords and enforces [`retry_delay`].
#[derive(Default)]
pub struct Attempts {
    failures: u32,
    retry_at: Option<Instant>,
}

impl Attempts {
    pub fn failed(&mut self) {
        self.failures += 1;
        let delay = retry_delay(self.failures);
        self.retry_at =
            (delay > 0.0).then(|| Instant::now() + std::time::Duration::from_secs_f32(delay));
    }

    /// Seconds still to wait before the next try, if any.
    pub fn wait(&self) -> Option<f32> {
        self.retry_at
            .map(|t| t.saturating_duration_since(Instant::now()).as_secs_f32())
            .filter(|s| *s > 0.0)
    }

    /// Keeps the countdown on the button moving.
    pub fn tick(&self, ctx: &egui::Context) -> Option<f32> {
        let wait = self.wait();
        if wait.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        wait
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }
}

/// "Wait 3 s" while the delay runs, otherwise `label`.
pub fn wait_label(wait: Option<f32>, label: &str) -> String {
    match wait {
        Some(s) => format!("Wait {:.0} s", s.ceil()),
        None => label.to_string(),
    }
}

fn caps_hint(ui: &mut Ui, sh: &Shared) {
    if crate::sys::caps_lock_on() && !sh.shot {
        let p = pal(ui);
        ui.colored_label(p.warn, Icon::WARNING.with("Caps Lock is on"));
    }
}

fn honest_footer(ui: &mut Ui) {
    let p = pal(ui);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(HONEST_LINE).size(12.5).color(p.faint));
    });
}

// --------------------------------------------------------------------------------------
// First run
// --------------------------------------------------------------------------------------

pub struct CreateState {
    pub password: SecretInput,
    pub confirm: SecretInput,
    show: bool,
    strength: StrengthCache,
    understood: bool,
    busy: bool,
    error: Option<String>,
    attempt: u64,
    focus: bool,
}

impl CreateState {
    pub fn new() -> Self {
        CreateState {
            password: SecretInput::new(),
            confirm: SecretInput::new(),
            show: false,
            strength: StrengthCache::default(),
            understood: false,
            busy: false,
            error: None,
            attempt: 0,
            focus: true,
        }
    }

    /// The first-run screen as the screenshots show it: filled in.
    pub fn demo_filled() -> Self {
        let mut s = CreateState::new();
        s.password.set("stoop-unrated-blizzard");
        s.confirm.set("stoop-unrated-blizzard");
        s.understood = true;
        s.focus = false;
        s
    }

    pub fn unlocked(
        &mut self,
        result: Result<(Snapshot, usize), Error>,
        kind: UnlockKind,
        attempt: u64,
    ) -> Option<Go> {
        if attempt != self.attempt {
            return None;
        }
        self.busy = false;
        match result {
            Ok((snapshot, found)) => Some(Go::Opened {
                snapshot,
                kind,
                found,
            }),
            Err(e) => {
                self.error = Some(e.to_string());
                None
            }
        }
    }

    fn ready(&mut self) -> bool {
        let s = self.strength.get(self.password.as_str()).clone();
        !self.busy
            && self.understood
            && strength::master_refusal(self.password.as_str(), &s).is_none()
            && self.password.as_str() == self.confirm.as_str()
    }

    fn submit(&mut self, ctx: &egui::Context, sh: &mut Shared) -> Option<Go> {
        if sh.demo {
            self.password.wipe();
            self.confirm.wipe();
            sh.toast("Demo: nothing was created. This list is invented.", false);
            return Some(Go::Opened {
                snapshot: Snapshot::default(),
                kind: UnlockKind::Create,
                found: 0,
            });
        }
        let password = Zeroizing::new(self.password.as_str().to_string());
        // The fields are wiped as soon as the password has been handed over.
        self.password.wipe();
        self.confirm.wipe();
        self.strength.clear();
        self.busy = true;
        self.error = None;
        self.attempt += 1;
        sh.start_unlock(ctx, UnlockKind::Create, password, self.attempt);
        None
    }

    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        sh: &mut Shared,
        logo: Option<&TextureHandle>,
    ) -> Option<Go> {
        let mut go = None;
        let p = pal_ctx(ctx);
        let mut submit = false;
        page(ctx, |ui| {
            let total = ui.available_width().min(1000.0);
            let pad = ((ui.available_width() - total) / 2.0).max(0.0);
            ui.add_space(((ui.available_height() - 560.0) / 2.0).clamp(4.0, 120.0));
            ui.horizontal(|ui| {
                ui.add_space(pad);
                paint_logo(ui, logo, 52.0);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add_space(2.0);
                    ui.label(RichText::new("Welcome to VeilForge").font(semibold(26.0)));
                    ui.label(
                        RichText::new(
                            "Lock or hide files and folders behind one master password. Start by choosing it.",
                        )
                        .size(15.0)
                        .color(p.muted),
                    );
                });
            });
            ui.add_space(14.0);
            ui.horizontal_top(|ui| {
                ui.add_space(pad);
                ui.spacing_mut().item_spacing.x = 18.0;
                let left_w = (total * 0.54).floor();
                ui.vertical(|ui| {
                    ui.set_width(left_w);
                    widgets::card(ui, |ui| {
                        let w = ui.available_width();
                        ui.spacing_mut().item_spacing.y = 6.0;
                        widgets::section(ui, "Choose your master password");
                        ui.add_space(2.0);
                        widgets::caption(ui, "Master password");
                        let resp = widgets::password_row(
                            ui,
                            &mut self.password,
                            &mut self.show,
                            "The one password you have to remember",
                            w,
                            !self.busy,
                        );
                        if self.focus && !self.busy {
                            resp.request_focus();
                            self.focus = false;
                        }
                        let empty = self.password.is_empty();
                        let s = self.strength.get(self.password.as_str()).clone();
                        strength_block(ui, &s, empty, w);
                        caps_hint(ui, sh);

                        ui.add_space(4.0);
                        widgets::caption(ui, "Type it again");
                        ui.add_enabled_ui(!self.busy, |ui| {
                            widgets::secret_input(ui, &mut self.confirm, self.show, "", w);
                        });
                        let matches = self.password.as_str() == self.confirm.as_str();
                        if !self.confirm.is_empty() && !matches {
                            ui.colored_label(p.danger, "The two passwords are different.");
                        }
                        let refusal = strength::master_refusal(self.password.as_str(), &s);
                        if !empty {
                            if let Some(reason) = refusal {
                                ui.add(
                                    egui::Label::new(RichText::new(reason).color(p.danger)).wrap(),
                                );
                            }
                        }

                        ui.add_space(6.0);
                        widgets::checkbox(
                            ui,
                            &mut self.understood,
                            "I understand that a forgotten password cannot be recovered",
                        );
                        if let Some(e) = &self.error {
                            widgets::banner(ui, Icon::ERROR, p.danger, e);
                        }
                        ui.add_space(6.0);
                        let ready = self.ready();
                        widgets::left_row(ui, 36.0, |ui| {
                            if self.busy {
                                ui.add(egui::Spinner::new().size(20.0).color(p.accent));
                                ui.label("Setting up. This takes a second or two.");
                            } else if widgets::primary_button(
                                ui,
                                &Icon::LOCK.with("Set master password"),
                                ready,
                            )
                            .clicked()
                            {
                                submit = true;
                            }
                        });
                        ui.add_space(2.0);
                        widgets::note(
                            ui,
                            "Right after this you can print an emergency sheet: a page to write the password on by hand and lock away.",
                        );
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(4.0);
                        ui.label(RichText::new("Used VeilForge on this PC before?").color(p.muted));
                        if widgets::link(ui, "Rebuild from recovery copies").clicked() && !self.busy
                        {
                            go = Some(Go::ShowRebuild);
                        }
                    });
                });
                ui.vertical(|ui| {
                    ui.set_width(total - left_w - 18.0);
                    Frame::new()
                        .fill(p.danger.gamma_multiply(0.10))
                        .stroke(Stroke::new(1.0_f32, p.danger.gamma_multiply(0.55)))
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(Margin::same(16))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(Icon::WARNING.glyph()).color(p.danger));
                                ui.label(RichText::new("There is no recovery").font(semibold(15.0)));
                            });
                            ui.add(
                                egui::Label::new(
                                    RichText::new(
                                        "If you forget this password, VeilForge cannot unlock your items. Nobody can reset it, not even the developer.",
                                    )
                                    .size(13.5),
                                )
                                .wrap(),
                            );
                            ui.add_space(2.0);
                            widgets::note(
                                ui,
                                "In practice: your files are not destroyed, because nothing is encrypted. But without the password, getting them back takes someone who knows Windows well, working as an administrator. Write the password down and keep it somewhere safe.",
                            );
                        });
                    ui.add_space(4.0);
                    widgets::card(ui, |ui| {
                        widgets::section(ui, "What you can do next");
                        ui.add_space(2.0);
                        for (icon, title, text) in [
                            (
                                Icon::LOCK,
                                "Lock",
                                "The item stays where it is, but opening it says \"access denied\".",
                            ),
                            (
                                Icon::HIDE,
                                "Hide",
                                "The item disappears from its folder and from search.",
                            ),
                            (
                                Icon::KEY,
                                "Ask for password on open",
                                "A stand-in stays in place. Double-click it, type the password, and it opens.",
                            ),
                        ] {
                            ui.horizontal_top(|ui| {
                                ui.label(RichText::new(icon.glyph()).color(p.accent));
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 1.0;
                                    ui.label(RichText::new(title).font(semibold(14.0)));
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(text).size(13.0).color(p.muted),
                                        )
                                        .wrap(),
                                    );
                                });
                            });
                            ui.add_space(2.0);
                        }
                    });
                });
            });
            ui.add_space(14.0);
            honest_footer(ui);
        });
        let enter = ctx.input(|i| i.key_pressed(Key::Enter));
        if (submit || enter) && self.ready() {
            if let Some(g) = self.submit(ctx, sh) {
                go = Some(g);
            }
        }
        go
    }
}

impl Default for CreateState {
    fn default() -> Self {
        Self::new()
    }
}

// --------------------------------------------------------------------------------------
// Unlock
// --------------------------------------------------------------------------------------

pub struct UnlockState {
    password: SecretInput,
    show: bool,
    busy: bool,
    error: Option<String>,
    attempts: Attempts,
    attempt: u64,
    focus: bool,
    /// "Rebuild from recovery copies" instead of a normal unlock.
    rebuild: bool,
}

impl UnlockState {
    pub fn new() -> Self {
        UnlockState {
            password: SecretInput::new(),
            show: false,
            busy: false,
            error: None,
            attempts: Attempts::default(),
            attempt: 0,
            focus: true,
            rebuild: false,
        }
    }

    pub fn rebuild() -> Self {
        UnlockState {
            rebuild: true,
            ..UnlockState::new()
        }
    }

    pub fn unlocked(
        &mut self,
        result: Result<(Snapshot, usize), Error>,
        kind: UnlockKind,
        attempt: u64,
    ) -> Option<Go> {
        if attempt != self.attempt {
            return None;
        }
        self.busy = false;
        match result {
            Ok((snapshot, found)) => Some(Go::Opened {
                snapshot,
                kind,
                found,
            }),
            Err(e) => {
                if matches!(e, Error::WrongPassword) {
                    self.attempts.failed();
                }
                self.error = Some(e.to_string());
                self.focus = true;
                None
            }
        }
    }

    fn submit(&mut self, ctx: &egui::Context, sh: &mut Shared) -> Option<Go> {
        let kind = if self.rebuild {
            UnlockKind::Rebuild
        } else {
            UnlockKind::Open
        };
        if sh.demo {
            self.password.wipe();
            return Some(Go::Opened {
                snapshot: Snapshot::default(),
                kind: UnlockKind::Open,
                found: 0,
            });
        }
        let password = Zeroizing::new(self.password.as_str().to_string());
        // The field is wiped as soon as the password has been handed over.
        self.password.wipe();
        self.busy = true;
        self.error = None;
        self.attempt += 1;
        sh.start_unlock(ctx, kind, password, self.attempt);
        None
    }

    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        sh: &mut Shared,
        logo: Option<&TextureHandle>,
    ) -> Option<Go> {
        let mut go = None;
        let p = pal_ctx(ctx);
        let wait = self.attempts.tick(ctx);
        let mut submit = false;
        page(ctx, |ui| {
            ui.add_space((ui.available_height() * 0.16).clamp(10.0, 150.0));
            let w = 400.0;
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - w - 56.0).max(0.0) / 2.0);
                Frame::new()
                    .fill(p.card)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .corner_radius(CornerRadius::same(16))
                    .inner_margin(Margin::same(28))
                    .show(ui, |ui| {
                        ui.set_width(w);
                        ui.vertical(|ui| {
                            ui.vertical_centered(|ui| {
                                paint_logo(ui, logo, 64.0);
                                ui.add_space(6.0);
                                if self.rebuild {
                                    ui.label(
                                        RichText::new("Rebuild from recovery copies")
                                            .font(semibold(20.0)),
                                    );
                                } else {
                                    ui.label(
                                        RichText::new(sh.settings.title()).font(semibold(22.0)),
                                    );
                                    ui.label(
                                        RichText::new(Icon::LOCK.with("Locked")).color(p.muted),
                                    );
                                }
                            });
                            if self.rebuild {
                                ui.add_space(4.0);
                                widgets::note(
                                    ui,
                                    "VeilForge keeps a small recovery copy next to everything it protects. If its own data was lost (a new Windows, a cleaned-up profile), type your master password and it finds your items again.",
                                );
                            }
                            ui.add_space(12.0);
                            widgets::caption(ui, "Master password");
                            let resp = widgets::password_row(
                                ui,
                                &mut self.password,
                                &mut self.show,
                                "",
                                w,
                                !self.busy,
                            );
                            if self.focus && !self.busy {
                                resp.request_focus();
                                self.focus = false;
                            }
                            caps_hint(ui, sh);
                            ui.add_space(8.0);
                            if let Some(e) = &self.error {
                                ui.add(egui::Label::new(RichText::new(e).color(p.danger)).wrap());
                                ui.add_space(4.0);
                            }
                            let ready = !self.busy && wait.is_none() && !self.password.is_empty();
                            widgets::left_row(ui, 36.0, |ui| {
                                if self.busy {
                                    ui.add(egui::Spinner::new().size(20.0).color(p.accent));
                                    ui.label(if self.rebuild {
                                        "Looking for your items…"
                                    } else {
                                        "Unlocking…"
                                    });
                                } else {
                                    let label = wait_label(
                                        wait,
                                        if self.rebuild { "Rebuild" } else { "Unlock" },
                                    );
                                    if widgets::primary_button(ui, &label, ready).clicked() {
                                        submit = true;
                                    }
                                }
                            });
                            if ready && ui.input(|i| i.key_pressed(Key::Enter)) {
                                submit = true;
                            }
                            ui.add_space(8.0);
                            ui.vertical_centered(|ui| {
                                if self.busy {
                                    return;
                                }
                                if self.rebuild {
                                    if widgets::link(ui, "Back").clicked() {
                                        go = Some(Go::ShowStart);
                                    }
                                } else {
                                    ui.label(
                                        RichText::new("Lost the app data?")
                                            .size(12.5)
                                            .color(p.faint),
                                    );
                                    if widgets::link(ui, "Rebuild from recovery copies").clicked() {
                                        go = Some(Go::ShowRebuild);
                                    }
                                }
                            });
                        });
                    });
            });
            ui.add_space(14.0);
            honest_footer(ui);
        });
        if submit {
            if let Some(g) = self.submit(ctx, sh) {
                go = Some(g);
            }
        }
        go
    }
}

impl Default for UnlockState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_grows_and_is_capped() {
        assert_eq!(retry_delay(0), 0.0);
        assert_eq!(retry_delay(1), 0.0);
        assert_eq!(retry_delay(2), 0.5);
        assert_eq!(retry_delay(3), 1.0);
        assert_eq!(retry_delay(5), 4.0);
        assert_eq!(retry_delay(6), 8.0);
        assert_eq!(retry_delay(60), 8.0);
    }

    #[test]
    fn attempts_enforce_the_delay() {
        let mut a = Attempts::default();
        assert!(a.wait().is_none());
        a.failed();
        assert!(a.wait().is_none(), "the first mistake costs nothing");
        a.failed();
        let wait = a.wait().expect("the second one does");
        assert!(wait > 0.0 && wait <= 0.5);
        for _ in 0..10 {
            a.failed();
        }
        assert_eq!(a.failures(), 12);
        let wait = a.wait().unwrap();
        assert!(wait > 7.0 && wait <= 8.0);
        assert_eq!(wait_label(Some(2.2), "Unlock"), "Wait 3 s");
        assert_eq!(wait_label(None, "Unlock"), "Unlock");
    }
}

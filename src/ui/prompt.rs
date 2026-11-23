//! The small password prompt used by `--open`, `--unlock`, `--restore-all`:
//! a fixed-size, centred, always-on-top window in the same style as the app.

use super::start::{paint_logo, wait_label, Attempts};
use super::theme::{self, pal, pal_ctx, semibold, Icon};
use super::widgets;
use crate::secret::SecretInput;
use eframe::egui::{self, Frame, Key, Margin, RichText, TextureHandle, Ui};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// What the prompt window says.
#[derive(Clone, Debug, Default)]
pub struct PromptSpec {
    /// Window title (already neutral if the user asked for that).
    pub title: String,
    pub heading: String,
    /// One or two plain sentences under the heading.
    pub text: String,
    /// Label of the confirm button, for example "Unlock".
    pub button: String,
}

/// Called on a worker thread with the typed password. `Ok(message)` closes the
/// window (a non-empty message is shown for a moment first); `Err(text)` is
/// shown in the window and the user may try again after the wrong-password delay.
pub type Submit = Box<dyn FnMut(&str) -> Result<String, String> + Send>;

/// Inner size of the prompt window, in points.
pub const SIZE: [f32; 2] = [440.0, 290.0];
/// Padding inside the window.
const MARGIN: i8 = 22;
/// How long a success message stays before the window closes.
const DONE_SHOWN: Duration = Duration::from_millis(1200);

/// What the user did this frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    None,
    Submit,
    Cancel,
}

/// The state of the prompt's content, shared by the real window and the
/// preview inside the main window (`--view prompt`).
pub struct PromptState {
    pub password: SecretInput,
    show: bool,
    pub busy: bool,
    pub error: Option<String>,
    /// The success message and when it appeared.
    pub done: Option<(String, Instant)>,
    pub attempts: Attempts,
    focus: bool,
}

impl Default for PromptState {
    fn default() -> Self {
        PromptState {
            password: SecretInput::new(),
            show: false,
            busy: false,
            error: None,
            done: None,
            attempts: Attempts::default(),
            focus: true,
        }
    }
}

impl PromptState {
    /// A wrong password or another problem: show it and let the user try again.
    pub fn failed(&mut self, text: String) {
        self.busy = false;
        self.attempts.failed();
        self.error = Some(text);
        self.focus = true;
    }
}

/// The spec shown by `--view prompt`: the prompt of an "ask on open" item.
pub fn preview_spec() -> PromptSpec {
    PromptSpec {
        title: "VeilForge".into(),
        heading: "Open \"Side project\"".into(),
        text: "This folder is protected by VeilForge. Type your master password to open it. It locks again on its own when you are done.".into(),
        button: "Unlock".into(),
    }
}

/// Draws the prompt into a `width` x `height` area, with the buttons at the
/// bottom. `shot` leaves out hints that depend on this PC (Caps Lock).
pub fn content(
    ui: &mut Ui,
    state: &mut PromptState,
    spec: &PromptSpec,
    logo: Option<&TextureHandle>,
    [width, height]: [f32; 2],
    shot: bool,
) -> Action {
    let p = pal(ui);
    let mut action = Action::None;
    let top = ui.cursor().top();
    ui.set_width(width);
    ui.spacing_mut().item_spacing.y = 6.0;
    widgets::left_row(ui, 48.0, |ui| {
        paint_logo(ui, logo, 44.0);
        ui.add_space(6.0);
        ui.add(
            egui::Label::new(
                RichText::new(if spec.heading.is_empty() {
                    "Master password"
                } else {
                    &spec.heading
                })
                .font(semibold(18.0)),
            )
            .truncate(),
        );
    });
    ui.add_space(4.0);
    if !spec.text.is_empty() {
        ui.add(egui::Label::new(RichText::new(&spec.text).size(13.5).color(p.muted)).wrap());
    }
    ui.add_space(10.0);

    if let Some((message, _)) = &state.done {
        widgets::left_row(ui, 34.0, |ui| {
            ui.label(RichText::new(Icon::DONE.glyph()).size(16.0).color(p.ok));
            ui.add(egui::Label::new(RichText::new(message).color(p.text)).wrap());
        });
        return Action::None;
    }

    let wait = state.attempts.tick(ui.ctx());
    let resp = widgets::password_row(
        ui,
        &mut state.password,
        &mut state.show,
        "Master password",
        width,
        !state.busy,
    );
    if state.focus && !state.busy {
        resp.request_focus();
        state.focus = false;
    }
    if let Some(e) = &state.error {
        ui.add(egui::Label::new(RichText::new(e).size(13.0).color(p.danger)).wrap());
    } else if !shot && crate::sys::caps_lock_on() {
        ui.label(
            RichText::new(Icon::WARNING.with("Caps Lock is on"))
                .size(13.0)
                .color(p.warn),
        );
    }
    // The button row sits at the bottom of the window.
    let used = ui.cursor().top() - top;
    ui.add_space((height - used - 36.0 - 6.0).max(4.0));
    let ready = !state.busy && wait.is_none() && !state.password.is_empty();
    let (enter, escape) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
    widgets::button_row(ui, |ui| {
        let label = wait_label(
            wait,
            if spec.button.is_empty() {
                "Unlock"
            } else {
                &spec.button
            },
        );
        if widgets::primary_button(ui, &label, ready).clicked() || (enter && ready) {
            action = Action::Submit;
        }
        if widgets::ghost_button_enabled(ui, "Cancel", !state.busy).clicked()
            || (escape && !state.busy)
        {
            action = Action::Cancel;
        }
        if state.busy {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Spinner::new().size(18.0).color(p.accent));
                ui.label(RichText::new("Checking…").color(p.muted));
            });
        }
    });
    action
}

struct PromptApp {
    spec: PromptSpec,
    state: PromptState,
    logo: Option<TextureHandle>,
    submit: Arc<Mutex<Submit>>,
    tx: Sender<Result<String, String>>,
    rx: Receiver<Result<String, String>>,
    ok: Arc<AtomicBool>,
}

impl eframe::App for PromptApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // The worker's answer.
        while let Ok(result) = self.rx.try_recv() {
            match result {
                Ok(message) => {
                    self.ok.store(true, Ordering::SeqCst);
                    self.state.busy = false;
                    if message.trim().is_empty() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    } else {
                        self.state.done = Some((message, Instant::now()));
                    }
                }
                Err(text) => self.state.failed(text),
            }
        }
        if let Some((_, since)) = &self.state.done {
            if since.elapsed() >= DONE_SHOWN {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        // Closing while the password is being checked would leave the caller
        // without the answer: wait for it.
        if self.state.busy && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }

        let p = pal_ctx(ctx);
        let action = egui::CentralPanel::default()
            .frame(Frame::new().fill(p.card).inner_margin(Margin::same(MARGIN)))
            .show(ctx, |ui| {
                let size = [ui.available_width(), ui.available_height()];
                content(
                    ui,
                    &mut self.state,
                    &self.spec,
                    self.logo.as_ref(),
                    size,
                    false,
                )
            })
            .inner;
        match action {
            Action::Submit => {
                let password = Zeroizing::new(self.state.password.as_str().to_string());
                // The field is wiped as soon as the password has been handed over.
                self.state.password.wipe();
                self.state.busy = true;
                self.state.error = None;
                let (submit, tx, ctx) = (self.submit.clone(), self.tx.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let result = match submit.lock() {
                        Ok(mut submit) => (*submit)(&password),
                        Err(_) => {
                            Err("Something went wrong. Close this window and try again.".into())
                        }
                    };
                    drop(password);
                    let _ = tx.send(result);
                    ctx.request_repaint();
                });
            }
            Action::Cancel => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::None => {}
        }
    }
}

/// Shows the prompt and blocks until it closes. Returns true if `submit`
/// succeeded, false if the user cancelled. May be called once per process.
pub fn run(spec: PromptSpec, icon: egui::IconData, submit: Submit) -> bool {
    let ok = Arc::new(AtomicBool::new(false));
    let title = if spec.title.trim().is_empty() {
        "VeilForge".to_string()
    } else {
        spec.title.clone()
    };
    let viewport = egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size(SIZE)
        .with_min_inner_size(SIZE)
        .with_max_inner_size(SIZE)
        .with_resizable(false)
        .with_maximize_button(false)
        .with_minimize_button(false)
        .with_always_on_top()
        .with_icon(icon.clone());
    let native = eframe::NativeOptions {
        viewport,
        centered: true,
        ..Default::default()
    };
    let result_ok = ok.clone();
    let _ = eframe::run_native(
        "VeilForge",
        native,
        Box::new(move |cc| {
            let ctx = &cc.egui_ctx;
            theme::install(ctx);
            // Only the theme is read from the settings.
            let settings = crate::settings::Settings::load(&crate::settings::data_dir());
            theme::apply_choice(ctx, settings.theme);
            let logo = (icon.width > 0).then(|| {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [icon.width as usize, icon.height as usize],
                    &icon.rgba,
                );
                ctx.load_texture("logo", image, egui::TextureOptions::LINEAR)
            });
            let (tx, rx) = channel();
            // Wrong passwords typed earlier, anywhere, still count for the delay.
            let streak = crate::attempts::Log::load(&crate::settings::data_dir()).streak;
            Ok(Box::new(PromptApp {
                spec,
                state: PromptState {
                    attempts: Attempts::resumed(streak),
                    ..PromptState::default()
                },
                logo,
                submit: Arc::new(Mutex::new(submit)),
                tx,
                rx,
                ok: result_ok,
            }))
        }),
    );
    ok.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_try_shows_the_text_and_starts_the_delay() {
        let mut state = PromptState {
            busy: true,
            ..Default::default()
        };
        state.failed("The master password is not correct.".into());
        assert!(!state.busy);
        assert_eq!(
            state.error.as_deref(),
            Some("The master password is not correct.")
        );
        assert!(state.attempts.wait().is_none());
        state.failed("again".into());
        assert!(state.attempts.wait().is_some());
        let spec = preview_spec();
        assert!(!spec.heading.is_empty() && !spec.text.is_empty() && !spec.button.is_empty());
    }
}

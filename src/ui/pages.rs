//! The other pages of the unlocked window: health check (with panic
//! restore), settings and about.

use super::app::Shared;
use super::dialogs::Dialog;
use super::list::{page_header, state_color};
use super::main_view::Main;
use super::ops::Op;
use super::start::paint_logo;
use super::theme::{pal, regular, semibold, Icon};
use super::widgets;
use crate::engine::Health;
use crate::model::{Kind, Mode, State};
use crate::settings::ThemeChoice;
use eframe::egui::{
    self, pos2, vec2, Align, Align2, CornerRadius, Frame, Layout, Margin, Rect, RichText, Sense,
    Stroke, TextureHandle, Ui, UiBuilder,
};

/// Width of the control column in a settings row.
const CONTROL_W: f32 = 340.0;

/// One settings row: title and explanation on the left, the control on the
/// right. The geometry is fixed, so nothing stretches inside the scroll area.
fn setting_row(ui: &mut Ui, title: &str, text: &str, control: impl FnOnce(&mut Ui)) {
    let p = pal(ui);
    let total = ui.available_width();
    let text_w = (total - CONTROL_W - 16.0).max(160.0);
    let title_g = ui
        .painter()
        .layout(title.to_string(), regular(14.5), p.text, text_w);
    let text_g = ui
        .painter()
        .layout(text.to_string(), regular(12.5), p.muted, text_w);
    let text_h = if text.is_empty() {
        0.0
    } else {
        text_g.size().y + 1.0
    };
    let height = (title_g.size().y + text_h).max(34.0);
    let (rect, _) = ui.allocate_exact_size(vec2(total, height), Sense::hover());
    let top = if text.is_empty() {
        rect.center().y - title_g.size().y / 2.0
    } else {
        rect.top()
    };
    let title_h = title_g.size().y;
    ui.painter().galley(pos2(rect.left(), top), title_g, p.text);
    if !text.is_empty() {
        ui.painter()
            .galley(pos2(rect.left(), top + title_h + 1.0), text_g, p.muted);
    }
    let control_rect = Rect::from_min_max(pos2(rect.right() - CONTROL_W, rect.top()), rect.max);
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(control_rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    control(&mut child);
}

fn toggle_row(ui: &mut Ui, title: &str, text: &str, value: &mut bool) -> bool {
    let mut changed = false;
    setting_row(ui, title, text, |ui| {
        if widgets::toggle(ui, *value, true).clicked() {
            *value = !*value;
            changed = true;
        }
    });
    changed
}

/// A segmented choice of minutes; 0 means "Never".
fn minutes_row(ui: &mut Ui, title: &str, text: &str, value: &mut u32, options: &[u32]) {
    setting_row(ui, title, text, |ui| {
        let labels: Vec<String> = options
            .iter()
            .map(|m| match m {
                0 => "Never".to_string(),
                60 => "1 hour".to_string(),
                m => format!("{m} min"),
            })
            .collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let current = options
            .iter()
            .position(|m| m == value)
            .unwrap_or(usize::MAX);
        if let Some(i) = widgets::segmented(ui, &labels, current) {
            *value = options[i];
        }
    });
}

/// What to say next to a hotkey box.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HotkeyNote {
    Empty,
    Invalid,
    /// The same combination as the other hotkey.
    Same,
    Active,
    /// Windows refused it: another program has it.
    Taken,
}

fn hotkey_note(text: &str, other: &str, status: Option<crate::sys::HotkeyStatus>) -> HotkeyNote {
    if text.trim().is_empty() {
        return HotkeyNote::Empty;
    }
    let Some(parsed) = crate::sys::parse_hotkey(text) else {
        return HotkeyNote::Invalid;
    };
    if crate::sys::parse_hotkey(other) == Some(parsed) {
        return HotkeyNote::Same;
    }
    match status {
        Some(crate::sys::HotkeyStatus::Taken) => HotkeyNote::Taken,
        _ => HotkeyNote::Active,
    }
}

/// A settings row with a text box for a hotkey. Returns the text to keep in
/// the settings when what was typed can be used.
fn hotkey_row(
    ui: &mut Ui,
    title: &str,
    explanation: &str,
    text: &mut String,
    other: &str,
    status: Option<crate::sys::HotkeyStatus>,
) -> Option<String> {
    let p = pal(ui);
    let note = hotkey_note(text, other, status);
    setting_row(ui, title, explanation, |ui| {
        widgets::text_input(ui, text, "None", 170.0);
        let (label, color) = match note {
            HotkeyNote::Empty => ("No hotkey".to_string(), p.faint),
            HotkeyNote::Invalid => ("Not a valid hotkey".to_string(), p.danger),
            HotkeyNote::Same => ("Same as the other hotkey".to_string(), p.danger),
            HotkeyNote::Active => (Icon::DONE.with("Active"), p.ok),
            HotkeyNote::Taken => ("Used by another program".to_string(), p.danger),
        };
        ui.label(RichText::new(label).size(12.5).color(color));
    });
    match note {
        HotkeyNote::Empty | HotkeyNote::Active | HotkeyNote::Taken => Some(text.trim().to_string()),
        HotkeyNote::Invalid | HotkeyNote::Same => None,
    }
}

fn bullet(ui: &mut Ui, icon: Icon, color: egui::Color32, text: &str) {
    let p = pal(ui);
    ui.horizontal_top(|ui| {
        ui.label(RichText::new(icon.glyph()).color(color));
        ui.add(egui::Label::new(RichText::new(text).size(13.5).color(p.muted)).wrap());
    });
    ui.add_space(2.0);
}

impl Main {
    // ----------------------------------------------------------------------------------
    // Health check
    // ----------------------------------------------------------------------------------

    pub(super) fn health_ui(&mut self, ui: &mut Ui, sh: &mut Shared) {
        let p = pal(ui);
        let ctx = ui.ctx().clone();
        let lines: Option<Vec<Health>> = self.health.clone();
        let repairable: Vec<String> = lines
            .iter()
            .flatten()
            .filter(|l| l.repairable && !l.problem.is_empty())
            .map(|l| l.id.clone())
            .collect();
        let (mut again, mut repair_all, mut find_lost) = (false, false, false);
        page_header(
            ui,
            "Health check",
            "Is every item where VeilForge left it, with its protection in place?",
            |ui| {
                if widgets::primary_button(
                    ui,
                    &Icon::REPAIR.with("Repair all"),
                    !repairable.is_empty(),
                )
                .on_disabled_hover_text("Nothing needs repairing")
                .clicked()
                {
                    repair_all = true;
                }
                if widgets::ghost_button(ui, &Icon::REFRESH.with("Check again")).clicked() {
                    again = true;
                }
                if widgets::ghost_button(ui, "Find lost items")
                    .on_hover_text(
                        "Looks in the holding folders for protected items that are not in the list, for example after VeilForge's data was restored from an old backup.",
                    )
                    .clicked()
                {
                    find_lost = true;
                }
            },
        );

        let Some(lines) = lines else {
            widgets::busy_state(
                ui,
                "Checking every item",
                "Large folders take a little longer.",
            );
            return;
        };
        let problems = lines.iter().filter(|l| !l.problem.is_empty()).count();

        // ---- Summary ----------------------------------------------------------------
        let (color, icon, title, text) = if lines.is_empty() {
            (
                p.muted,
                Icon::SHIELD,
                "Nothing to check".to_string(),
                "Nothing is protected yet.".to_string(),
            )
        } else if problems == 0 {
            (
                p.ok,
                Icon::SHIELD,
                format!(
                    "All {} item{} are fine",
                    lines.len(),
                    if lines.len() == 1 { "" } else { "s" }
                )
                .replace("All 1 item are", "The 1 item is"),
                "Everything is where VeilForge left it, with its protection in place.".to_string(),
            )
        } else {
            (
                p.danger,
                Icon::WARNING,
                format!(
                    "{problems} of {} item{} need{} attention",
                    lines.len(),
                    if lines.len() == 1 { "" } else { "s" },
                    if problems == 1 { "s" } else { "" }
                ),
                "\"Repair\" puts the recorded protection back. It never deletes anything."
                    .to_string(),
            )
        };
        Frame::new()
            .fill(p.card)
            .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.5)))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                widgets::left_row(ui, 44.0, |ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
                    ui.painter()
                        .circle_filled(rect.center(), 22.0, color.gamma_multiply(0.15));
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        icon.glyph(),
                        regular(20.0),
                        color,
                    );
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.add_space(3.0);
                        ui.label(RichText::new(title).font(semibold(16.0)));
                        ui.label(RichText::new(text).size(13.0).color(p.muted));
                    });
                });
            });
        ui.add_space(4.0);

        // ---- Lines ------------------------------------------------------------------
        const PANIC_H: f32 = 132.0;
        let mut repair: Option<String> = None;
        let list_h = (ui.available_height() - PANIC_H - 14.0).max(120.0);
        Frame::new()
            .fill(p.card)
            .stroke(Stroke::new(1.0_f32, p.border))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::symmetric(6, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height(list_h - 14.0);
                if lines.is_empty() {
                    widgets::empty_state(
                        ui,
                        Icon::SHIELD,
                        p.muted,
                        "No items",
                        "Protect something first; this page then checks on it.",
                    );
                    return;
                }
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Problems first.
                        let ordered = lines
                            .iter()
                            .filter(|l| !l.problem.is_empty())
                            .chain(lines.iter().filter(|l| l.problem.is_empty()));
                        for line in ordered {
                            let width = ui.available_width();
                            let fine = line.problem.is_empty();
                            let text_w = width - 56.0 - 280.0;
                            let problem = if fine {
                                match line.state {
                                    State::Open => "Temporarily open, as you asked.",
                                    _ => "Fine.",
                                }
                            } else {
                                line.problem.as_str()
                            };
                            let galley = ui.painter().layout(
                                problem.to_string(),
                                regular(13.0),
                                p.muted,
                                text_w,
                            );
                            let height = (galley.size().y + 34.0).max(50.0);
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(width, height), Sense::hover());
                            ui.painter().hline(
                                rect.x_range().shrink(8.0),
                                rect.bottom(),
                                Stroke::new(1.0_f32, p.border.gamma_multiply(0.6)),
                            );
                            let folder = self
                                .snap
                                .item(&line.id)
                                .is_none_or(|i| i.kind == Kind::Folder);
                            let tile = Rect::from_center_size(
                                pos2(rect.left() + 28.0, rect.top() + 25.0),
                                vec2(30.0, 30.0),
                            );
                            widgets::kind_tile(ui, tile, &line.name, folder);
                            let left = rect.left() + 56.0;
                            widgets::text_in(
                                ui,
                                Rect::from_min_max(
                                    pos2(left, rect.top() + 6.0),
                                    pos2(left + text_w, rect.top() + 28.0),
                                ),
                                &line.name,
                                semibold(14.5),
                                p.text,
                            );
                            ui.painter()
                                .galley(pos2(left, rect.top() + 28.0), galley, p.muted);
                            let badge = Rect::from_min_max(
                                pos2(rect.right() - 270.0, rect.top() + 4.0),
                                pos2(rect.right() - 110.0, rect.top() + 46.0),
                            );
                            widgets::paint_badge(
                                ui,
                                badge,
                                line.state.label(),
                                state_color(&p, line.state),
                            );
                            if !fine {
                                let button = Rect::from_min_size(
                                    pos2(rect.right() - 104.0, rect.top() + 8.0),
                                    vec2(94.0, 34.0),
                                );
                                let widget = egui::Button::new(
                                    RichText::new("Repair").color(if line.repairable {
                                        p.text
                                    } else {
                                        p.faint
                                    }),
                                )
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(Stroke::new(1.0_f32, p.border))
                                .corner_radius(CornerRadius::same(8));
                                let resp = ui
                                    .add_enabled_ui(line.repairable, |ui| ui.put(button, widget))
                                    .inner
                                    .on_disabled_hover_text(
                                        "VeilForge cannot fix this on its own. The text on the left says what happened.",
                                    );
                                if resp.clicked() {
                                    repair = Some(line.id.clone());
                                }
                            }
                        }
                    });
            });
        ui.add_space(8.0);

        // ---- Panic restore ----------------------------------------------------------
        let mut panic = false;
        Frame::new()
            .fill(p.card)
            .stroke(Stroke::new(1.0_f32, p.danger.gamma_multiply(0.55)))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 4.0;
                widgets::left_row(ui, 22.0, |ui| {
                    ui.label(RichText::new(Icon::UNDO.glyph()).color(p.danger));
                    ui.label(RichText::new("Panic restore").font(semibold(15.0)));
                });
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "Puts everything back where it was, exactly as it was, and stops protecting it. Asks for the master password first. Use it before reinstalling Windows or giving this PC away.",
                        )
                        .size(13.0)
                        .color(p.muted),
                    )
                    .wrap(),
                );
                ui.add_space(2.0);
                widgets::left_row(ui, 34.0, |ui| {
                    if widgets::danger_button(
                        ui,
                        "Panic restore…",
                        !self.snap.items.is_empty(),
                    )
                    .on_disabled_hover_text("Nothing is protected")
                    .clicked()
                    {
                        panic = true;
                    }
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Also available without this window:")
                            .size(12.5)
                            .color(p.faint),
                    );
                    ui.label(
                        RichText::new("VeilForge.exe --restore-all")
                            .font(egui::FontId::monospace(12.5))
                            .color(p.muted),
                    );
                });
            });

        if again {
            self.health = None;
        }
        if repair_all {
            self.start(&ctx, sh, Op::Repair(repairable));
        } else if let Some(id) = repair {
            self.start(&ctx, sh, Op::Repair(vec![id]));
        } else if find_lost {
            self.start(&ctx, sh, Op::ImportRecovery);
        }
        if panic {
            self.dialog = Some(Dialog::panic());
        }
    }

    // ----------------------------------------------------------------------------------
    // Settings
    // ----------------------------------------------------------------------------------

    pub(super) fn settings_ui(&mut self, ui: &mut Ui, sh: &mut Shared) {
        let p = pal(ui);
        page_header(ui, "Settings", "Saved automatically", |_| {});
        let (mut change_password, mut emergency, mut open_data) = (false, false, false);
        let mut clear_log = false;
        if self.attempt_log.is_none() {
            self.attempt_log = Some(if sh.demo {
                crate::demo::attempt_log(sh.now)
            } else {
                crate::attempts::Log::load(&sh.data_dir)
            });
        }
        let attempt_log = self.attempt_log.clone().unwrap_or_default();
        let mut set_menu: Option<bool> = None;
        let mut set_startup: Option<bool> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(840.0);
                ui.spacing_mut().item_spacing.y = 12.0;

                widgets::card(ui, |ui| {
                    widgets::section(ui, "Appearance");
                    let mut theme = sh.settings.theme;
                    setting_row(
                        ui,
                        "Theme",
                        "\"System\" follows the Windows light or dark setting.",
                        |ui| {
                            let options =
                                [ThemeChoice::Dark, ThemeChoice::Light, ThemeChoice::System];
                            let current =
                                options.iter().position(|t| *t == theme).unwrap_or(0);
                            if let Some(i) =
                                widgets::segmented(ui, &["Dark", "Light", "System"], current)
                            {
                                theme = options[i];
                            }
                        },
                    );
                    sh.settings.theme = theme;
                });

                widgets::card(ui, |ui| {
                    widgets::section(ui, "Locking again");
                    minutes_row(
                        ui,
                        "Lock VeilForge after",
                        "With no keyboard or mouse activity for this long, this window asks for the master password again.",
                        &mut sh.settings.auto_lock_minutes,
                        &[0, 1, 5, 15, 30],
                    );
                    widgets::divider(ui);
                    minutes_row(
                        ui,
                        "Lock temporarily open items after",
                        "Items you unlocked \"for now\" are protected again after this long without activity.",
                        &mut sh.settings.relock_idle_minutes,
                        &[0, 5, 15, 30, 60],
                    );
                    widgets::divider(ui);
                    toggle_row(
                        ui,
                        "Also when Windows locks, sleeps or signs out",
                        "Temporarily open items are locked again the moment you leave the PC.",
                        &mut sh.settings.relock_on_session_lock,
                    );
                    widgets::divider(ui);
                    toggle_row(
                        ui,
                        "Keep running when the window is closed",
                        "Needed for locking items again automatically. VeilForge stays reachable through its tray icon or hotkey.",
                        &mut sh.settings.close_to_background,
                    );
                    widgets::divider(ui);
                    toggle_row(
                        ui,
                        "Lock VeilForge when its window is closed",
                        "Closing the window to the tray also locks this list, so opening it again asks for the master password. Items you opened temporarily stay open until their own timers lock them.",
                        &mut sh.settings.lock_on_close,
                    );
                });

                widgets::card(ui, |ui| {
                    widgets::section(ui, "Protecting and unlocking");
                    let mut mode = sh.settings.default_mode;
                    setting_row(
                        ui,
                        "Usual way to protect",
                        "What the \"Protect\" dialog starts with. You can still choose each time.",
                        |ui| {
                            let options = [Mode::Lock, Mode::Hide, Mode::Ask];
                            let current = options.iter().position(|m| *m == mode).unwrap_or(1);
                            if let Some(i) =
                                widgets::segmented(ui, &["Lock", "Hide", "Ask on open"], current)
                            {
                                mode = options[i];
                            }
                        },
                    );
                    sh.settings.default_mode = mode;
                    widgets::divider(ui);
                    let mut hidden = sh.settings.ask_default_hidden;
                    setting_row(
                        ui,
                        "Under an \"ask on open\" stand-in",
                        "Hiding the real item is safer. Locking it in place is the better choice inside cloud-synced folders.",
                        |ui| {
                            if let Some(i) = widgets::segmented(
                                ui,
                                &["Hide it (recommended)", "Lock it in place"],
                                if hidden { 0 } else { 1 },
                            ) {
                                hidden = i == 0;
                            }
                        },
                    );
                    sh.settings.ask_default_hidden = hidden;
                    widgets::divider(ui);
                    toggle_row(
                        ui,
                        "Ask before unlocking permanently",
                        "Shows what will be restored before VeilForge stops protecting an item.",
                        &mut sh.settings.confirm_unlock,
                    );
                    widgets::divider(ui);
                    toggle_row(
                        ui,
                        "Show the item in File Explorer after unlocking it",
                        "Opens its folder so you can get to work right away.",
                        &mut sh.settings.open_after_unlock,
                    );
                });

                widgets::card(ui, |ui| {
                    widgets::section(ui, "Windows");
                    let mut menu = sh.context_menu_on;
                    if toggle_row(
                        ui,
                        "Right-click entries in File Explorer",
                        "Adds \"Lock with VeilForge\" and \"Unlock with VeilForge\" to the menu of files and folders, for this Windows account only.",
                        &mut menu,
                    ) {
                        set_menu = Some(menu);
                    }
                    widgets::divider(ui);
                    let mut startup = sh.startup_on;
                    if toggle_row(
                        ui,
                        "Start with Windows",
                        "Starts hidden in the tray when you sign in, so items can be locked again automatically.",
                        &mut startup,
                    ) {
                        set_startup = Some(startup);
                    }
                });

                widgets::card(ui, |ui| {
                    widgets::section(ui, "Keeping a low profile");
                    toggle_row(
                        ui,
                        "Neutral window title",
                        "Windows and the tray icon say \"Files\" instead of \"VeilForge\".",
                        &mut sh.settings.neutral_title,
                    );
                    widgets::divider(ui);
                    toggle_row(
                        ui,
                        "Show the tray icon",
                        "The icon near the clock opens VeilForge and offers \"Lock now\". Without it, use the hotkey.",
                        &mut sh.settings.tray_icon,
                    );
                    widgets::divider(ui);
                    // What Windows answered when the hotkeys were registered.
                    let status = |kind| sh.has_sys.then(|| crate::sys::hotkey_status(kind));
                    let (show_status, lock_status) = (
                        status(crate::sys::HotkeyKind::Show),
                        status(crate::sys::HotkeyKind::Lock),
                    );
                    let mut text = self
                        .hotkey_text
                        .clone()
                        .unwrap_or_else(|| sh.settings.hotkey.clone());
                    let mut lock_text = self
                        .lock_hotkey_text
                        .clone()
                        .unwrap_or_else(|| sh.settings.lock_hotkey.clone());
                    if let Some(keep) = hotkey_row(
                        ui,
                        "Hotkey that opens VeilForge",
                        "Works from anywhere while VeilForge is running. Needs Ctrl, Alt or Win plus a letter, digit or F key, for example Ctrl+Alt+V.",
                        &mut text,
                        "",
                        show_status,
                    ) {
                        sh.settings.hotkey = keep;
                    }
                    widgets::divider(ui);
                    if let Some(keep) = hotkey_row(
                        ui,
                        "Hotkey that locks everything",
                        "Locks every temporarily open item again and locks VeilForge, from anywhere, at once. For example Ctrl+Alt+L.",
                        &mut lock_text,
                        &sh.settings.hotkey,
                        lock_status,
                    ) {
                        sh.settings.lock_hotkey = keep;
                    }
                    self.hotkey_text = Some(text);
                    self.lock_hotkey_text = Some(lock_text);
                    if !sh.settings.tray_icon
                        && crate::sys::parse_hotkey(&sh.settings.hotkey).is_none()
                    {
                        widgets::banner(
                            ui,
                            Icon::INFO,
                            p.warn,
                            "With no tray icon and no hotkey, closing the window quits VeilForge. Items you opened temporarily then stay open until you start it again.",
                        );
                    }
                });

                widgets::card(ui, |ui| {
                    widgets::section(ui, "Master password");
                    setting_row(
                        ui,
                        "Change the master password",
                        "Asks for the current one first. Your protected items are not touched.",
                        |ui| {
                            if widgets::ghost_button(ui, &Icon::KEY.with("Change…")).clicked() {
                                change_password = true;
                            }
                        },
                    );
                    widgets::divider(ui);
                    setting_row(
                        ui,
                        "Emergency sheet",
                        "A page to print and write the password on by hand. There is no other way to recover a forgotten password.",
                        |ui| {
                            if widgets::ghost_button(ui, &Icon::PRINT.with("Save the sheet…"))
                                .clicked()
                            {
                                emergency = true;
                            }
                        },
                    );
                });

                widgets::card(ui, |ui| {
                    widgets::section(ui, "Wrong passwords");
                    let n = attempt_log.entries.len();
                    setting_row(
                        ui,
                        "Wrong-password log",
                        &match n {
                            0 => "Every wrong master password is noted here: when, and where it was typed. Never what was typed. Nothing so far.".to_string(),
                            1 => "1 wrong master password is on record. Only when and where it was typed is kept, never what was typed.".to_string(),
                            n => format!("{n} wrong master passwords are on record. Only when and where they were typed is kept, never what was typed."),
                        },
                        |ui| {
                            if widgets::ghost_button_enabled(ui, "Clear the log", n > 0).clicked() {
                                clear_log = true;
                            }
                        },
                    );
                    // The newest few; the rest is in the file.
                    for entry in attempt_log.entries.iter().rev().take(6) {
                        widgets::left_row(ui, 22.0, |ui| {
                            ui.label(
                                RichText::new(Icon::WARNING.glyph())
                                    .size(12.5)
                                    .color(p.warn),
                            );
                            ui.label(
                                RichText::new(crate::util::fmt_datetime(entry.at)).size(13.0),
                            );
                            ui.label(
                                RichText::new(entry.place.label())
                                    .size(13.0)
                                    .color(p.muted),
                            );
                        });
                    }
                    if n > 6 {
                        ui.label(
                            RichText::new(format!("… and {} older", n - 6))
                                .size(12.5)
                                .color(p.faint),
                        );
                    }
                });

                widgets::card(ui, |ui| {
                    widgets::section(ui, "Data");
                    let dir = sh.data_dir.to_string_lossy().to_string();
                    setting_row(
                        ui,
                        "Data folder",
                        &format!(
                            "{dir}\nHolds the settings and the encrypted list of what is protected where. Your files are never copied here."
                        ),
                        |ui| {
                            if widgets::ghost_button(ui, &Icon::FOLDER_OPEN.with("Open folder"))
                                .clicked()
                            {
                                open_data = true;
                            }
                        },
                    );
                });
                ui.add_space(6.0);
                if self.settings_at_end {
                    ui.scroll_to_cursor(Some(Align::BOTTOM));
                    if ui.input(|i| {
                        i.raw_scroll_delta != egui::Vec2::ZERO || i.pointer.any_pressed()
                    }) {
                        self.settings_at_end = false;
                    }
                }
            });

        if clear_log {
            if !sh.demo {
                crate::attempts::clear(&sh.data_dir);
            }
            self.attempt_log = Some(crate::attempts::Log::default());
            sh.toast("The wrong-password log was cleared.", false);
        }
        if change_password {
            self.dialog = Some(Dialog::change_password());
        }
        if emergency {
            self.dialog = Some(Dialog::Emergency { first_run: false });
        }
        if open_data {
            if sh.demo {
                sh.toast("Demo: there is no data folder.", false);
            } else if let Err(e) = crate::sys::open_folder(&sh.data_dir) {
                sh.toast(e, true);
            }
        }
        if let Some(on) = set_menu {
            if sh.real_system {
                let exe = std::env::current_exe().unwrap_or_default();
                let root = crate::shellmenu::CLASSES_ROOT;
                let result = if on {
                    crate::shellmenu::install(root, &exe)
                } else {
                    crate::shellmenu::uninstall(root)
                };
                match result {
                    Ok(()) => sh.toast(
                        if on {
                            "Right-click entries added."
                        } else {
                            "Right-click entries removed."
                        },
                        false,
                    ),
                    Err(e) => sh.toast(
                        format!("The right-click entries could not be changed: {e}"),
                        true,
                    ),
                }
                sh.context_menu_on = crate::shellmenu::is_installed(root);
            } else {
                // Demo and test runs never touch the registry.
                sh.context_menu_on = on;
            }
            sh.settings.context_menu = sh.context_menu_on;
        }
        if let Some(on) = set_startup {
            if sh.real_system {
                let exe = std::env::current_exe().unwrap_or_default();
                if let Err(e) = crate::sys::set_startup(crate::sys::RUN_KEY, on, &exe) {
                    sh.toast(e, true);
                }
                sh.startup_on = crate::sys::startup_enabled(crate::sys::RUN_KEY);
            } else {
                sh.startup_on = on;
            }
            sh.settings.start_with_windows = sh.startup_on;
        }
    }

    // ----------------------------------------------------------------------------------
    // About
    // ----------------------------------------------------------------------------------

    pub(super) fn about_ui(&mut self, ui: &mut Ui, sh: &mut Shared, logo: Option<&TextureHandle>) {
        let p = pal(ui);
        let _ = sh;
        page_header(ui, "About", "What VeilForge is, and what it is not", |_| {});
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(960.0);
                ui.spacing_mut().item_spacing.y = 12.0;
                widgets::card(ui, |ui| {
                    widgets::left_row(ui, 84.0, |ui| {
                        paint_logo(ui, logo, 76.0);
                        ui.add_space(10.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.add_space(2.0);
                            ui.label(RichText::new("VeilForge").font(semibold(24.0)));
                            ui.label(
                                RichText::new(format!(
                                    "Version {} · free and open source under the MIT License",
                                    env!("CARGO_PKG_VERSION")
                                ))
                                .color(p.muted),
                            );
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                ui.label("Made by Zheer Khalid");
                                ui.label(RichText::new("(").color(p.muted));
                                ui.hyperlink_to(
                                    RichText::new("zheer.dev").color(p.accent),
                                    "https://zheer.dev",
                                );
                                ui.label(RichText::new(")").color(p.muted));
                            });
                        });
                    });
                });

                let total = ui.available_width();
                let gap = 12.0;
                let col = (total - gap) / 2.0;
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    ui.vertical(|ui| {
                        ui.set_width(col);
                        widgets::card(ui, |ui| {
                            widgets::section(ui, "How it works");
                            ui.add_space(2.0);
                            bullet(
                                ui,
                                Icon::INFO,
                                p.accent,
                                "Nothing is encrypted. VeilForge uses ordinary Windows features: permissions (who may open a file) and moving items within the same drive.",
                            );
                            bullet(
                                ui,
                                Icon::LOCK,
                                p.accent,
                                "Lock adds a \"deny\" permission. The item stays in place, and opening it says \"access denied\".",
                            );
                            bullet(
                                ui,
                                Icon::HIDE,
                                p.accent,
                                "Hide moves the item into a holding folder at the top of the same drive, under a random name with no file type. Nothing is copied, so it is instant even for huge folders.",
                            );
                            bullet(
                                ui,
                                Icon::KEY,
                                p.accent,
                                "Ask for password on open leaves a stand-in shortcut with the original name. Opening it asks for the master password, opens the item, and locks it again afterwards.",
                            );
                            bullet(
                                ui,
                                Icon::SHIELD,
                                p.accent,
                                "Only VeilForge's own list (what belongs where) is encrypted, with your master password.",
                            );
                        });
                    });
                    ui.vertical(|ui| {
                        ui.set_width(col);
                        Frame::new()
                            .fill(p.card)
                            .stroke(Stroke::new(1.0_f32, p.warn.gamma_multiply(0.6)))
                            .corner_radius(CornerRadius::same(12))
                            .inner_margin(Margin::same(16))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                widgets::section(ui, "Honest limits");
                                ui.add_space(2.0);
                                bullet(
                                    ui,
                                    Icon::DONE,
                                    p.ok,
                                    "It holds against normal use of this Windows account: File Explorer, search, other programs, and people who sit down at your PC.",
                                );
                                bullet(
                                    ui,
                                    Icon::WARNING,
                                    p.warn,
                                    "An administrator who takes ownership of the files can open them.",
                                );
                                bullet(
                                    ui,
                                    Icon::WARNING,
                                    p.warn,
                                    "Another operating system (a boot stick, the drive in another PC) or a disk tool ignores Windows permissions and can read everything.",
                                );
                                bullet(
                                    ui,
                                    Icon::WARNING,
                                    p.warn,
                                    "Backups, lists of recent files and thumbnail caches made earlier may still mention a name or show a preview.",
                                );
                                bullet(
                                    ui,
                                    Icon::WARNING,
                                    p.warn,
                                    "There is no password recovery. Keep an emergency sheet (Settings).",
                                );
                                bullet(
                                    ui,
                                    Icon::INFO,
                                    p.muted,
                                    "For secrets that must survive the theft of this PC, use real encryption as well, for example BitLocker.",
                                );
                            });
                    });
                });
                ui.add_space(6.0);
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::HotkeyStatus;

    #[test]
    fn a_hotkey_box_says_what_is_wrong_with_it() {
        assert_eq!(hotkey_note("  ", "", None), HotkeyNote::Empty);
        assert_eq!(hotkey_note("Ctrl+", "", None), HotkeyNote::Invalid);
        assert_eq!(hotkey_note("L", "", None), HotkeyNote::Invalid);
        assert_eq!(
            hotkey_note("ctrl+alt+v", "Ctrl+Alt+V", Some(HotkeyStatus::Active)),
            HotkeyNote::Same
        );
        assert_eq!(
            hotkey_note("Ctrl+Alt+L", "Ctrl+Alt+V", Some(HotkeyStatus::Active)),
            HotkeyNote::Active
        );
        // Windows refused it: another program registered it first.
        assert_eq!(
            hotkey_note("Ctrl+Alt+L", "", Some(HotkeyStatus::Taken)),
            HotkeyNote::Taken
        );
        // No background window (the demo): nothing to ask, shown as active.
        assert_eq!(hotkey_note("Ctrl+Alt+L", "", None), HotkeyNote::Active);
    }
}

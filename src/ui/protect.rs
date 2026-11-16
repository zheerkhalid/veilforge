//! The "Protect" dialog: what the engine says about each chosen path, and
//! how the user wants them protected.

use super::theme::{pal, regular, semibold, Icon};
use super::widgets;
use crate::engine::{Precheck, ProtectOptions, Verdict};
use crate::model::{Kind, Mode};
use crate::settings::Settings;
use crate::util::{human_size, thousands};
use eframe::egui::{
    self, pos2, vec2, Align2, CornerRadius, Frame, Margin, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui,
};
use std::path::{Path, PathBuf};

pub struct Row {
    pub path: PathBuf,
    /// `None` while the engine is still looking at it.
    pub pre: Option<Precheck>,
}

impl Row {
    fn allowed(&self) -> bool {
        self.pre
            .as_ref()
            .is_some_and(|p| p.verdict != Verdict::Refuse)
    }
}

pub struct ProtectDlg {
    pub rows: Vec<Row>,
    pub mode: Mode,
    pub ask_hidden: bool,
    /// `None`: every folder uses the engine's own default.
    scramble: Option<bool>,
    group: String,
}

pub enum Choice {
    Cancel,
    Protect(Vec<(PathBuf, ProtectOptions)>),
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

fn parent_text(path: &Path) -> String {
    path.parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn hides(mode: Mode, ask_hidden: bool) -> bool {
    matches!(mode, Mode::Hide) || (matches!(mode, Mode::Ask) && ask_hidden)
}

impl ProtectDlg {
    pub fn new(paths: Vec<PathBuf>, settings: &Settings) -> ProtectDlg {
        let mut rows: Vec<Row> = Vec::new();
        for path in paths {
            let same = |r: &Row| {
                r.path
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&path.to_string_lossy())
            };
            if !rows.iter().any(same) {
                rows.push(Row { path, pre: None });
            }
        }
        ProtectDlg {
            rows,
            mode: settings.default_mode,
            ask_hidden: settings.ask_default_hidden,
            scramble: None,
            group: String::new(),
        }
    }

    pub fn set_group(&mut self, group: &str) {
        self.group = group.to_string();
    }

    /// The engine's answer for row `index` arrived.
    pub fn checked(&mut self, index: usize, pre: Precheck, _settings: &Settings) {
        if let Some(row) = self.rows.get_mut(index) {
            row.pre = Some(pre);
        }
    }

    fn checking(&self) -> bool {
        self.rows.iter().any(|r| r.pre.is_none())
    }

    fn allowed(&self) -> impl Iterator<Item = &Precheck> {
        self.rows
            .iter()
            .filter(|r| r.allowed())
            .filter_map(|r| r.pre.as_ref())
    }

    /// The mode a row really gets: drives without permissions can only hide.
    fn mode_for(&self, pre: &Precheck) -> Mode {
        if pre.acl_supported {
            self.mode
        } else {
            Mode::Hide
        }
    }

    fn scramble_shown(&self) -> bool {
        self.scramble.unwrap_or_else(|| {
            let mut folders = self.allowed().filter(|p| p.kind == Kind::Folder).peekable();
            folders.peek().is_some() && folders.all(|p| p.scramble_default)
        })
    }

    /// What will be handed to the engine.
    pub fn plan(&self) -> Vec<(PathBuf, ProtectOptions)> {
        self.allowed()
            .map(|pre| {
                let mode = self.mode_for(pre);
                let hidden = hides(mode, self.ask_hidden);
                (
                    pre.path.clone(),
                    ProtectOptions {
                        mode,
                        ask_hidden: self.ask_hidden,
                        scramble: hidden
                            && pre.kind == Kind::Folder
                            && self.scramble.unwrap_or(pre.scramble_default),
                        group: self.group.trim().to_string(),
                    },
                )
            })
            .collect()
    }

    fn rows_ui(&self, ui: &mut Ui) {
        let p = pal(ui);
        Frame::new()
            .fill(p.bg)
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                // As many rows as the window has room for (the rest scrolls).
                let room = (ui.ctx().screen_rect().height() - 540.0).clamp(96.0, 260.0);
                egui::ScrollArea::vertical()
                    .max_height(room)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for (n, row) in self.rows.iter().enumerate() {
                            if n > 0 {
                                widgets::divider(ui);
                            }
                            let width = ui.available_width();
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
                            let folder = match &row.pre {
                                Some(pre) => pre.kind == Kind::Folder,
                                None => row.path.extension().is_none(),
                            };
                            let name = name_of(&row.path);
                            let tile = Rect::from_center_size(
                                pos2(rect.left() + 16.0, rect.center().y),
                                vec2(28.0, 28.0),
                            );
                            widgets::kind_tile(ui, tile, &name, folder);
                            let refused = row
                                .pre
                                .as_ref()
                                .is_some_and(|p| p.verdict == Verdict::Refuse);
                            let right_w = 190.0;
                            let text_left = rect.left() + 40.0;
                            let text_right = rect.right() - right_w - 8.0;
                            widgets::text_in(
                                ui,
                                Rect::from_min_max(
                                    pos2(text_left, rect.top() + 2.0),
                                    pos2(text_right, rect.top() + 22.0),
                                ),
                                &name,
                                semibold(14.0),
                                if refused { p.muted } else { p.text },
                            );
                            widgets::text_in(
                                ui,
                                Rect::from_min_max(
                                    pos2(text_left, rect.top() + 21.0),
                                    pos2(text_right, rect.bottom() - 1.0),
                                ),
                                &parent_text(&row.path),
                                regular(12.0),
                                p.faint,
                            );
                            let right = Rect::from_min_max(
                                pos2(rect.right() - right_w, rect.top()),
                                pos2(rect.right() - 4.0, rect.bottom()),
                            );
                            match &row.pre {
                                None => {
                                    widgets::text_in_right(
                                        ui,
                                        right,
                                        "Checking…",
                                        regular(12.5),
                                        p.muted,
                                    );
                                    ui.ctx().request_repaint_after(
                                        std::time::Duration::from_millis(200),
                                    );
                                }
                                Some(pre) if refused => {
                                    let _ = pre;
                                    let w = widgets_badge_width(ui, "Cannot be protected");
                                    widgets::paint_badge(
                                        ui,
                                        Rect::from_min_max(
                                            pos2(right.right() - w, right.top()),
                                            right.max,
                                        ),
                                        "Cannot be protected",
                                        p.danger,
                                    );
                                }
                                Some(pre) => {
                                    let mut text = human_size(pre.size);
                                    if pre.kind == Kind::Folder {
                                        text.push_str(&format!(
                                            " · {}{} files",
                                            thousands(pre.files),
                                            if pre.truncated { "+" } else { "" }
                                        ));
                                    }
                                    if !pre.filesystem.is_empty() && pre.filesystem != "NTFS" {
                                        text.push_str(&format!(" · {}", pre.filesystem));
                                    }
                                    widgets::text_in_right(
                                        ui,
                                        right,
                                        &text,
                                        regular(12.5),
                                        p.muted,
                                    );
                                }
                            }
                            if let Some(pre) = &row.pre {
                                let note = |ui: &mut Ui, icon: Icon, color, text: &str| {
                                    ui.horizontal_top(|ui| {
                                        ui.add_space(40.0);
                                        ui.label(
                                            RichText::new(icon.glyph()).size(12.5).color(color),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(text).size(12.5).color(color),
                                            )
                                            .wrap(),
                                        );
                                    });
                                    ui.add_space(3.0);
                                };
                                if let Some(reason) = &pre.refusal {
                                    note(ui, Icon::ERROR, p.danger, reason);
                                }
                                if !refused {
                                    for warning in &pre.warnings {
                                        note(ui, Icon::WARNING, p.warn, warning);
                                    }
                                }
                            }
                        }
                    });
            });
    }

    /// One of the three big choices. Returns true when clicked.
    fn mode_card(
        ui: &mut Ui,
        width: f32,
        icon: Icon,
        title: &str,
        text: &str,
        active: bool,
        enabled: bool,
    ) -> bool {
        let p = pal(ui);
        let (rect, resp) = ui.allocate_exact_size(
            vec2(width, 104.0),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        let (fill, stroke) = if active {
            (p.selected, Stroke::new(1.5_f32, p.accent))
        } else if resp.hovered() && enabled {
            (p.hover, Stroke::new(1.0_f32, p.faint))
        } else {
            (p.card, Stroke::new(1.0_f32, p.border))
        };
        ui.painter().rect_filled(rect, CornerRadius::same(12), fill);
        ui.painter()
            .rect_stroke(rect, CornerRadius::same(12), stroke, StrokeKind::Inside);
        let fg = if enabled { p.text } else { p.faint };
        let icon_color = if !enabled {
            p.faint
        } else if active {
            p.accent
        } else {
            p.muted
        };
        ui.painter().text(
            rect.left_top() + vec2(14.0, 22.0),
            Align2::LEFT_CENTER,
            icon.glyph(),
            regular(17.0),
            icon_color,
        );
        ui.painter().text(
            rect.left_top() + vec2(40.0, 21.0),
            Align2::LEFT_CENTER,
            title,
            semibold(14.5),
            fg,
        );
        // The radio mark on the right.
        let dot = pos2(rect.right() - 20.0, rect.top() + 22.0);
        ui.painter().circle_stroke(
            dot,
            7.5,
            Stroke::new(1.5_f32, if active { p.accent } else { p.faint }),
        );
        if active {
            ui.painter().circle_filled(dot, 4.0, p.accent);
        }
        let galley = ui.painter().layout(
            text.to_string(),
            regular(12.5),
            if enabled { p.muted } else { p.faint },
            width - 28.0,
        );
        ui.painter()
            .galley(rect.left_top() + vec2(14.0, 40.0), galley, p.muted);
        resp.clicked() && enabled
    }

    pub fn ui(&mut self, ui: &mut Ui, groups: &[String]) -> Option<Choice> {
        let p = pal(ui);
        ui.set_width(760.0);
        let mut choice = None;
        let allowed = self.allowed().count();
        let refused = self.rows.iter().filter(|r| r.pre.is_some()).count() - allowed;
        let checking = self.checking();

        widgets::dialog_title(
            ui,
            &match self.rows.len() {
                1 => format!("Protect \"{}\"", name_of(&self.rows[0].path)),
                n => format!("Protect {n} items"),
            },
        );
        widgets::para(
            ui,
            "Nothing is encrypted or copied. VeilForge only changes who may open the item, or where it is kept on the same drive.",
        );
        ui.add_space(4.0);
        self.rows_ui(ui);
        ui.add_space(8.0);

        // ---- How -------------------------------------------------------------------
        let any_acl = self.allowed().any(|p| p.acl_supported) || allowed == 0;
        let any_reduced = self.allowed().any(|p| !p.acl_supported);
        if !any_acl {
            self.mode = Mode::Hide;
        }
        let card_w = (ui.available_width() - 20.0) / 3.0;
        widgets::left_row(ui, 104.0, |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if Self::mode_card(
                ui,
                card_w,
                Icon::LOCK,
                "Lock",
                "Stays where it is. Opening, copying or deleting it says \"access denied\".",
                self.mode == Mode::Lock,
                any_acl,
            ) {
                self.mode = Mode::Lock;
            }
            if Self::mode_card(
                ui,
                card_w,
                Icon::HIDE,
                "Hide",
                "Disappears from its folder. Search tools cannot find it by name.",
                self.mode == Mode::Hide,
                true,
            ) {
                self.mode = Mode::Hide;
            }
            if Self::mode_card(
                ui,
                card_w,
                Icon::KEY,
                "Ask for password on open",
                "A stand-in stays in place. Double-click it and type the password to open.",
                self.mode == Mode::Ask,
                any_acl,
            ) {
                self.mode = Mode::Ask;
            }
        });
        if any_reduced {
            ui.add_space(2.0);
            widgets::note(
                ui,
                if any_acl {
                    "Items on drives without permissions (FAT, exFAT) can only be hidden, whatever is chosen here."
                } else {
                    "These drives have no permissions (FAT, exFAT), so items on them can only be hidden."
                },
            );
        }
        ui.add_space(6.0);

        if self.mode == Mode::Ask {
            widgets::left_row(ui, 36.0, |ui| {
                ui.label("Underneath the stand-in:");
                let current = if self.ask_hidden { 0 } else { 1 };
                if let Some(i) =
                    widgets::segmented(ui, &["Hide it (recommended)", "Lock it in place"], current)
                {
                    self.ask_hidden = i == 0;
                }
            });
        }
        let hiding = hides(self.mode, self.ask_hidden) || any_reduced;
        let has_folder = self.allowed().any(|p| p.kind == Kind::Folder);
        if hiding && has_folder {
            let mut scramble = self.scramble_shown();
            if widgets::checkbox(ui, &mut scramble, "Also scramble names inside folders").changed()
            {
                self.scramble = Some(scramble);
            }
            let huge = self
                .allowed()
                .any(|p| p.kind == Kind::Folder && !p.scramble_default);
            let mut text = String::from(
                "Without this, the names of files inside a hidden folder can still be read by tools that read the disk index directly.",
            );
            if huge {
                text.push_str(if scramble {
                    " One of these folders is very large: renaming everything inside will take a while, and so will unlocking it."
                } else {
                    " Left off by default here because one of these folders is very large and renaming everything inside takes a while."
                });
            }
            ui.horizontal_top(|ui| {
                ui.add_space(26.0);
                ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.muted)).wrap());
            });
            ui.add_space(4.0);
        }

        // ---- Group -----------------------------------------------------------------
        widgets::left_row(ui, 36.0, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label("Group (optional):");
            widgets::text_input(ui, &mut self.group, "For example: Work", 180.0);
            for g in groups.iter().take(4) {
                if g.chars().count() <= 16
                    && widgets::chip(ui, g, None, self.group.trim() == g).clicked()
                {
                    self.group = g.clone();
                }
            }
        });
        ui.add_space(8.0);
        widgets::divider(ui);
        ui.add_space(8.0);

        // ---- Buttons ---------------------------------------------------------------
        let total_size: u64 = self.allowed().map(|p| p.size).sum();
        let total_files: u64 = self.allowed().map(|p| p.files).sum();
        let ready = allowed > 0 && !checking;
        widgets::button_row(ui, |ui| {
            let label = match allowed {
                1 => "Protect 1 item".to_string(),
                n => format!("Protect {n} items"),
            };
            if widgets::primary_button(ui, &Icon::LOCK.with(&label), ready).clicked() {
                choice = Some(Choice::Protect(self.plan()));
            }
            if widgets::ghost_button(ui, "Cancel").clicked() {
                choice = Some(Choice::Cancel);
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                if checking {
                    ui.add(egui::Spinner::new().size(16.0).color(p.accent));
                    ui.label(RichText::new("Checking the items…").color(p.muted));
                } else if allowed == 0 {
                    ui.label(RichText::new("None of these can be protected.").color(p.danger));
                } else {
                    let mut text = format!(
                        "{} · {} file{}",
                        human_size(total_size),
                        thousands(total_files),
                        if total_files == 1 { "" } else { "s" }
                    );
                    if refused > 0 {
                        text.push_str(&format!(" · {refused} left out"));
                    }
                    ui.label(RichText::new(text).color(p.muted));
                }
            });
        });
        choice
    }
}

fn widgets_badge_width(ui: &Ui, text: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_string(), semibold(11.5), egui::Color32::WHITE)
        .size()
        .x
        + 14.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog() -> ProtectDlg {
        let paths = crate::demo::sample_paths();
        let settings = Settings::default();
        let mut dlg = ProtectDlg::new(paths.clone(), &settings);
        for (i, path) in paths.iter().enumerate() {
            dlg.checked(i, crate::demo::precheck(path), &settings);
        }
        dlg
    }

    #[test]
    fn duplicates_are_dropped_and_refused_paths_left_out() {
        let settings = Settings::default();
        let dlg = ProtectDlg::new(
            vec![
                PathBuf::from("C:\\A\\b"),
                PathBuf::from("c:\\a\\B"),
                PathBuf::from("C:\\A\\c"),
            ],
            &settings,
        );
        assert_eq!(dlg.rows.len(), 2);
        assert!(dlg.checking());
        assert!(dlg.plan().is_empty(), "nothing is planned before the check");

        let dlg = dialog();
        assert!(!dlg.checking());
        let plan = dlg.plan();
        assert_eq!(plan.len(), 3, "C:\\Windows is refused");
        assert!(plan.iter().all(|(p, _)| !p.ends_with("Windows")));
    }

    #[test]
    fn options_follow_the_choices() {
        let mut dlg = dialog();
        dlg.set_group("  Work ");
        // Default: hide, folders scrambled by the engine's default, files never.
        let plan = dlg.plan();
        assert!(plan.iter().all(|(_, o)| o.mode == Mode::Hide));
        assert!(plan[0].1.scramble && plan[1].1.scramble);
        assert!(!plan[2].1.scramble, "a file has nothing inside");
        assert_eq!(plan[0].1.group, "Work");
        assert!(dlg.scramble_shown());

        dlg.scramble = Some(false);
        assert!(dlg.plan().iter().all(|(_, o)| !o.scramble));

        // Lock in place: nothing is scrambled even when the box is ticked.
        dlg.scramble = Some(true);
        dlg.mode = Mode::Lock;
        assert!(dlg.plan().iter().all(|(_, o)| !o.scramble));
        dlg.mode = Mode::Ask;
        dlg.ask_hidden = false;
        assert!(dlg.plan().iter().all(|(_, o)| !o.scramble && !o.ask_hidden));
        dlg.ask_hidden = true;
        assert!(dlg.plan()[0].1.scramble);

        // A drive without permissions can only hide.
        let settings = Settings::default();
        let path = PathBuf::from("E:\\Photos\\Trip");
        let mut dlg = ProtectDlg::new(vec![path.clone()], &settings);
        dlg.checked(0, crate::demo::precheck(&path), &settings);
        dlg.mode = Mode::Lock;
        assert_eq!(dlg.plan()[0].1.mode, Mode::Hide);
    }
}

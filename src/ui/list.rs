//! The list of protected items: searchable, sortable, with a details panel
//! and a bar for acting on several items at once.

use super::app::Shared;
use super::dialogs::{ConfirmAction, Dialog};
use super::main_view::{Filter, Main, SortCol};
use super::ops::Op;
use super::theme::{pal, regular, semibold, Icon, Palette};
use super::widgets;
use crate::model::{Item, Kind, Mode, State};
use crate::util::{fmt_date, fmt_datetime, human_size, thousands};
use eframe::egui::{
    self, pos2, vec2, Align, Color32, CornerRadius, Frame, Layout, Margin, Rect, RichText, Sense,
    Stroke, Ui,
};

const ROW_H: f32 = 46.0;

pub fn state_color(p: &Palette, state: State) -> Color32 {
    match state {
        State::Locked => p.ok,
        State::Open => p.warn,
        State::Missing | State::Tampered => p.danger,
    }
}

pub fn mode_color(p: &Palette, mode: Mode) -> Color32 {
    match mode {
        Mode::Lock => p.muted,
        Mode::Hide => p.accent,
        Mode::Ask => p.info,
    }
}

/// One plain sentence about how an item is protected.
pub fn mode_sentence(item: &Item) -> &'static str {
    match (item.mode, item.ask_hidden) {
        (Mode::Lock, _) => "Locked in place: it is still visible, but opening it says \"access denied\".",
        (Mode::Hide, _) => "Hidden: moved out of its folder into the holding folder under a random name.",
        (Mode::Ask, true) => "Asks for the password on open: a stand-in with the same name is in its place, and the real item is hidden.",
        (Mode::Ask, false) => "Asks for the password on open: a stand-in with the same name sits next to it, and the real item is locked in place.",
    }
}

fn kind_text(item: &Item) -> String {
    match item.kind {
        Kind::File => "File".to_string(),
        Kind::Folder => format!(
            "Folder · {} file{}",
            thousands(item.files),
            if item.files == 1 { "" } else { "s" }
        ),
    }
}

/// The folder an item lives in (its path without the last part).
fn parent_of(original: &str) -> &str {
    match original.rfind('\\') {
        // Keep the backslash of a drive root ("D:\").
        Some(2) => &original[..3],
        Some(i) => &original[..i],
        None => original,
    }
}

/// When a temporarily open item is protected again, in plain words.
pub fn relock_sentence(idle_minutes: u32, on_session_lock: bool) -> String {
    let idle = match idle_minutes {
        0 => None,
        1 => Some("after 1 minute without keyboard or mouse activity".to_string()),
        60 => Some("after 1 hour without keyboard or mouse activity".to_string()),
        n => Some(format!(
            "after {n} minutes without keyboard or mouse activity"
        )),
    };
    let session = on_session_lock.then_some("when Windows locks, sleeps or you sign out");
    match (idle, session) {
        (Some(a), Some(b)) => format!("On its own {a}, and {b}. Or with \"Lock again\"."),
        (Some(a), None) => format!("On its own {a}. Or with \"Lock again\"."),
        (None, Some(b)) => format!("On its own {b}. Or with \"Lock again\"."),
        (None, None) => {
            "Only when you choose \"Lock again\" or \"Lock all\": the automatic timers are switched off in Settings."
                .to_string()
        }
    }
}

fn count_text(n: usize) -> String {
    if n == 1 {
        "1 item".to_string()
    } else {
        format!("{n} items")
    }
}

struct Columns {
    name: f32,
    location: f32,
    mode: f32,
    state: f32,
    size: f32,
    date: f32,
}

/// Checkbox 40 + tile 44 on the left, quick action 44 + padding on the right.
const LEFT: f32 = 84.0;
const RIGHT: f32 = 52.0;

impl Columns {
    fn for_width(width: f32) -> Columns {
        let mut c = Columns {
            name: 0.0,
            location: 0.0,
            mode: 116.0,
            state: 140.0,
            size: 84.0,
            date: 104.0,
        };
        let mut rest = width - LEFT - RIGHT - c.mode - c.state - c.size - c.date;
        if rest < 380.0 {
            rest += c.date;
            c.date = 0.0;
        }
        if rest < 380.0 {
            // No room for the location column.
            if rest < 200.0 {
                rest += c.size;
                c.size = 0.0;
            }
            c.name = rest.max(120.0);
        } else {
            c.name = (rest * 0.44).max(180.0);
            c.location = rest - c.name;
        }
        c
    }
}

impl Main {
    fn matches(&self, item: &Item, needle: &str) -> bool {
        let state = self.state_of(&item.id);
        let filter_ok = match &self.filter {
            Filter::All => true,
            Filter::Group(g) => &item.group == g,
            Filter::NoGroup => item.group.is_empty(),
            Filter::Open => state == State::Open,
            Filter::Problems => matches!(state, State::Missing | State::Tampered),
        };
        filter_ok
            && (needle.is_empty()
                || item.name.to_lowercase().contains(needle)
                || item.original.to_lowercase().contains(needle)
                || item.group.to_lowercase().contains(needle))
    }

    fn visible_items(&self) -> Vec<Item> {
        let needle = self.search.trim().to_lowercase();
        let mut list: Vec<Item> = self
            .snap
            .items
            .iter()
            .filter(|i| self.matches(i, &needle))
            .cloned()
            .collect();
        let (col, asc) = self.sort;
        let state_rank = |s: State| match s {
            State::Missing => 0,
            State::Tampered => 1,
            State::Open => 2,
            State::Locked => 3,
        };
        list.sort_by(|a, b| {
            let by_name = || a.name.to_lowercase().cmp(&b.name.to_lowercase());
            let ord = match col {
                SortCol::Name => by_name(),
                SortCol::Location => a.original.to_lowercase().cmp(&b.original.to_lowercase()),
                SortCol::Mode => a.mode.label().cmp(b.mode.label()).then_with(by_name),
                SortCol::State => state_rank(self.state_of(&a.id))
                    .cmp(&state_rank(self.state_of(&b.id)))
                    .then_with(by_name),
                SortCol::Size => a.size.cmp(&b.size).then_with(by_name),
                SortCol::Date => a.locked_at.cmp(&b.locked_at).then_with(by_name),
            };
            if asc {
                ord
            } else {
                ord.reverse()
            }
        });
        list
    }

    // ----------------------------------------------------------------------------------
    // Actions
    // ----------------------------------------------------------------------------------

    fn names_of(&self, ids: &[String]) -> String {
        match ids {
            [one] => self
                .snap
                .item(one)
                .map(|i| format!("\"{}\"", i.name))
                .unwrap_or_else(|| "this item".into()),
            many => count_text(many.len()),
        }
    }

    /// Unlock permanently, after a confirmation that says what will happen.
    pub(super) fn ask_release(&mut self, ctx: &egui::Context, sh: &mut Shared, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        if !sh.settings.confirm_unlock {
            self.start(ctx, sh, Op::Release(ids));
            return;
        }
        let what = self.names_of(&ids);
        self.dialog = Some(Dialog::Confirm {
            title: "Unlock permanently?".into(),
            text: format!(
                "VeilForge will stop protecting {what}. Everything is restored exactly as it was: the same place, the same name, the same permissions, attributes and dates. Any stand-in shortcut is removed. You can protect it again at any time."
            ),
            button: "Unlock permanently".into(),
            danger: false,
            action: ConfirmAction::Release(ids),
        });
    }

    fn ask_unlock_all(&mut self) {
        let ids: Vec<String> = self
            .snap
            .items
            .iter()
            .filter(|i| self.state_of(&i.id) == State::Locked)
            .map(|i| i.id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        self.dialog = Some(Dialog::Confirm {
            title: "Unlock everything for now?".into(),
            text: format!(
                "{} will be back in {} original place and open to anyone using this Windows account, until you choose \"Lock all\" or VeilForge locks them again automatically. To stop protecting everything for good, use \"Panic restore\" on the Health check page instead.",
                count_text(ids.len()),
                if ids.len() == 1 { "its" } else { "their" },
            ),
            button: "Unlock all for now".into(),
            danger: false,
            action: ConfirmAction::OpenTemp(ids),
        });
    }

    pub(super) fn ask_group(&mut self, ids: Vec<String>) {
        let current = match ids.as_slice() {
            [one] => self.snap.item(one).map(|i| i.group.clone()),
            _ => None,
        };
        self.dialog = Some(Dialog::Group {
            ids,
            name: current.unwrap_or_default(),
            focus: true,
        });
    }

    /// The most useful single action for an item in this state.
    fn quick_action(&mut self, ctx: &egui::Context, sh: &mut Shared, id: &str) {
        match self.state_of(id) {
            State::Locked => self.start(ctx, sh, Op::OpenTemp(vec![id.to_string()])),
            State::Open => self.start(ctx, sh, Op::Relock(vec![id.to_string()])),
            State::Missing | State::Tampered => {
                self.start(ctx, sh, Op::Repair(vec![id.to_string()]))
            }
        }
    }

    // ----------------------------------------------------------------------------------
    // The page
    // ----------------------------------------------------------------------------------

    pub(super) fn items_ui(&mut self, ui: &mut Ui, sh: &mut Shared) {
        let p = pal(ui);
        let ctx = ui.ctx().clone();
        let visible = self.visible_items();
        let total = self.snap.items.len();
        let total_size: u64 = self.snap.items.iter().map(|i| i.size).sum();
        let open = self.open_count();

        let subtitle = if total == 0 {
            "Nothing is protected yet".to_string()
        } else {
            let mut s = format!("{} · {}", count_text(total), human_size(total_size));
            if open > 0 {
                s.push_str(&format!(" · {open} temporarily open"));
            }
            s
        };
        let narrow = ui.available_width() < 760.0;
        let (mut add_files, mut add_folder, mut lock_all, mut unlock_all) =
            (false, false, false, false);
        page_header(ui, "Protected items", &subtitle, |ui| {
            if widgets::primary_button(ui, &Icon::NEW_FOLDER.with("Add folder"), true)
                .on_hover_text("Choose folders to lock or hide")
                .clicked()
            {
                add_folder = true;
            }
            if widgets::primary_button(ui, &Icon::ADD.with("Add files"), true)
                .on_hover_text("Choose files to lock or hide")
                .clicked()
            {
                add_files = true;
            }
            if total > 0 && !narrow {
                if widgets::ghost_button_enabled(ui, &Icon::LOCK.with("Lock all"), open > 0)
                    .on_hover_text("Lock every temporarily open item again")
                    .on_disabled_hover_text("Nothing is open right now")
                    .clicked()
                {
                    lock_all = true;
                }
                if widgets::ghost_button(ui, &Icon::UNLOCK.with("Unlock all"))
                    .on_hover_text("Unlock every item for now (asks first)")
                    .clicked()
                {
                    unlock_all = true;
                }
            }
        });
        if add_files || add_folder {
            self.pick_and_offer(&ctx, sh, add_folder);
        }
        if lock_all {
            self.start(&ctx, sh, Op::RelockAll);
        }
        if unlock_all {
            self.ask_unlock_all();
        }

        // Wrong master passwords typed while the user was away: told once.
        if let Some(text) = self.attempt_notice.clone() {
            let mut dismiss = false;
            let mut show_log = false;
            Frame::new()
                .fill(p.warn.gamma_multiply(0.12))
                .stroke(Stroke::new(1.0_f32, p.warn.gamma_multiply(0.5)))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::symmetric(12, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(Icon::WARNING.glyph()).color(p.warn));
                        let room = (ui.available_width() - 190.0).max(120.0);
                        ui.vertical(|ui| {
                            ui.set_width(room);
                            ui.add_space(5.0);
                            widgets::text(ui, &text, 13.5, false, p.text);
                            ui.add_space(3.0);
                        });
                        if widgets::ghost_button(ui, "See the log").clicked() {
                            show_log = true;
                        }
                        if widgets::ghost_button(ui, "OK").clicked() {
                            dismiss = true;
                        }
                    });
                });
            ui.add_space(2.0);
            if show_log {
                self.tab = super::main_view::Tab::Settings;
                self.attempt_log = None;
                self.settings_at_end = true;
            }
            if dismiss || show_log {
                self.attempt_notice = None;
            }
        }

        if total == 0 {
            self.first_items_state(ui, sh);
            return;
        }

        // ---- Filter chips -----------------------------------------------------------
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if widgets::chip(ui, "All", Some(total), self.filter == Filter::All).clicked() {
                self.filter = Filter::All;
            }
            let groups = self.snap.groups.clone();
            // Leave room for the two state chips on the right.
            let mut room = ui.available_width() - 250.0;
            for g in &groups {
                let n = self.snap.items.iter().filter(|i| &i.group == g).count();
                room -= 60.0 + g.chars().count() as f32 * 7.5;
                if room < 0.0 {
                    break;
                }
                let filter = Filter::Group(g.clone());
                if widgets::chip(ui, g, Some(n), self.filter == filter)
                    .on_hover_text(format!("Items in the group \"{g}\""))
                    .clicked()
                {
                    self.filter = filter;
                }
            }
            let ungrouped = self
                .snap
                .items
                .iter()
                .filter(|i| i.group.is_empty())
                .count();
            if !groups.is_empty()
                && ungrouped > 0
                && room > 90.0
                && widgets::chip(
                    ui,
                    "No group",
                    Some(ungrouped),
                    self.filter == Filter::NoGroup,
                )
                .clicked()
            {
                self.filter = Filter::NoGroup;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let problems = self.snap.problem_count();
                if widgets::chip(
                    ui,
                    "Problems",
                    Some(problems),
                    self.filter == Filter::Problems,
                )
                .on_hover_text(
                    "Items that are missing or whose protection was changed from outside",
                )
                .clicked()
                {
                    self.filter = Filter::Problems;
                }
                if widgets::chip(ui, "Open now", Some(open), self.filter == Filter::Open)
                    .on_hover_text("Items that are temporarily unlocked")
                    .clicked()
                {
                    self.filter = Filter::Open;
                }
            });
        });
        ui.add_space(2.0);

        if visible.is_empty() {
            if !self.search.trim().is_empty() {
                let text = format!(
                    "Nothing matches \"{}\". Search looks at names, locations and groups.",
                    self.search.trim()
                );
                widgets::empty_state(ui, Icon::SEARCH, p.muted, "No matches", &text);
                ui.vertical_centered(|ui| {
                    if widgets::ghost_button(ui, "Clear search").clicked() {
                        self.search.clear();
                    }
                });
            } else {
                let (title, text) = match &self.filter {
                    Filter::Open => (
                        "Nothing is open",
                        "Every protected item is locked right now.",
                    ),
                    Filter::Problems => (
                        "No problems",
                        "Every item is where VeilForge left it, with its protection in place.",
                    ),
                    _ => ("Nothing to show", "No item matches this filter."),
                };
                widgets::empty_state(ui, Icon::SHIELD, p.ok, title, text);
                ui.vertical_centered(|ui| {
                    if widgets::ghost_button(ui, "Show all items").clicked() {
                        self.filter = Filter::All;
                    }
                });
            }
            return;
        }

        // ---- Table ------------------------------------------------------------------
        let batch_bar = !self.checked.is_empty();
        let table_height = ui.available_height() - if batch_bar { 58.0 } else { 0.0 };
        let mut quick: Option<String> = None;
        let mut menu_action: Option<(u8, String)> = None;
        Frame::new()
            .fill(p.card)
            .stroke(Stroke::new(1.0_f32, p.border))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::symmetric(6, 6))
            .show(ui, |ui| {
                ui.set_height(table_height - 14.0);
                ui.set_width(ui.available_width());
                let width = ui.available_width();
                let cols = Columns::for_width(width);
                self.table_header(ui, &cols, &visible);
                ui.spacing_mut().item_spacing.y = 0.0;
                let mut clicked: Option<String> = None;
                let mut toggled: Option<String> = None;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(ui, ROW_H, visible.len(), |ui, range| {
                        for item in &visible[range] {
                            let state = self.state_of(&item.id);
                            let (rect, resp) =
                                ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
                            let is_selected = self.selected.as_deref() == Some(item.id.as_str());
                            let is_checked = self.checked.contains(&item.id);
                            if is_selected || is_checked {
                                ui.painter().rect_filled(
                                    rect.shrink2(vec2(0.0, 1.0)),
                                    CornerRadius::same(8),
                                    p.selected,
                                );
                            } else if resp.hovered() {
                                ui.painter().rect_filled(
                                    rect.shrink2(vec2(0.0, 1.0)),
                                    CornerRadius::same(8),
                                    p.hover,
                                );
                            }
                            // Checkbox
                            let cb = Rect::from_min_size(rect.min, vec2(40.0, ROW_H));
                            let cb_resp =
                                ui.interact(cb, ui.id().with(("cb", &item.id)), Sense::click());
                            widgets::paint_checkbox(ui, cb, Some(is_checked), cb_resp.hovered());
                            if cb_resp.clicked() {
                                toggled = Some(item.id.clone());
                            }
                            // Tile
                            let mut x = rect.left() + 44.0;
                            let tile = Rect::from_center_size(
                                pos2(x + 16.0, rect.center().y),
                                vec2(30.0, 30.0),
                            );
                            widgets::kind_tile(ui, tile, &item.name, item.kind == Kind::Folder);
                            x += 40.0;
                            let cell = |x: f32, w: f32| {
                                Rect::from_min_max(
                                    pos2(x, rect.top()),
                                    pos2(x + w - 12.0, rect.bottom()),
                                )
                            };
                            let cut = widgets::text_in(
                                ui,
                                cell(x, cols.name),
                                &item.name,
                                semibold(14.5),
                                p.text,
                            );
                            if cut && resp.hovered() {
                                resp.clone().on_hover_text(&item.name);
                            }
                            x += cols.name;
                            if cols.location > 0.0 {
                                widgets::text_in(
                                    ui,
                                    cell(x, cols.location),
                                    parent_of(&item.original),
                                    regular(13.5),
                                    p.muted,
                                );
                                x += cols.location;
                            }
                            widgets::paint_badge(
                                ui,
                                cell(x, cols.mode),
                                item.mode.label(),
                                mode_color(&p, item.mode),
                            );
                            x += cols.mode;
                            widgets::paint_badge(
                                ui,
                                cell(x, cols.state),
                                state.label(),
                                state_color(&p, state),
                            );
                            x += cols.state;
                            if cols.size > 0.0 {
                                widgets::text_in_right(
                                    ui,
                                    cell(x, cols.size),
                                    &human_size(item.size),
                                    regular(13.5),
                                    p.text,
                                );
                                x += cols.size + 12.0;
                            }
                            if cols.date > 0.0 {
                                widgets::text_in(
                                    ui,
                                    cell(x, cols.date - 12.0),
                                    &fmt_date(item.locked_at),
                                    regular(13.5),
                                    p.muted,
                                );
                            }
                            // Quick action at the end of the row.
                            let (icon, tip) = match state {
                                State::Locked => (Icon::UNLOCK, "Unlock temporarily"),
                                State::Open => (Icon::LOCK, "Lock again"),
                                State::Missing | State::Tampered => (Icon::REPAIR, "Repair"),
                            };
                            let act = Rect::from_center_size(
                                pos2(rect.right() - 26.0, rect.center().y),
                                vec2(32.0, 32.0),
                            );
                            let act_resp = ui
                                .interact(act, ui.id().with(("act", &item.id)), Sense::click())
                                .on_hover_text(tip);
                            if act_resp.hovered() {
                                ui.painter()
                                    .rect_filled(act, CornerRadius::same(8), p.border);
                            }
                            ui.painter().text(
                                act.center(),
                                egui::Align2::CENTER_CENTER,
                                icon.glyph(),
                                regular(15.0),
                                if act_resp.hovered() { p.text } else { p.muted },
                            );
                            if act_resp.clicked() {
                                quick = Some(item.id.clone());
                            } else if resp.clicked() {
                                clicked = Some(item.id.clone());
                            }
                            resp.context_menu(|ui| {
                                ui.set_min_width(210.0);
                                let entries: &[(u8, &str)] = match state {
                                    State::Locked => &[
                                        (0, "Unlock temporarily"),
                                        (2, "Unlock permanently…"),
                                        (4, "Set group…"),
                                        (5, "Show details"),
                                    ],
                                    State::Open => &[
                                        (1, "Lock again"),
                                        (2, "Unlock permanently…"),
                                        (4, "Set group…"),
                                        (5, "Show details"),
                                    ],
                                    State::Missing | State::Tampered => &[
                                        (3, "Repair"),
                                        (2, "Unlock permanently…"),
                                        (5, "Show details"),
                                    ],
                                };
                                for (code, label) in entries {
                                    if ui.button(*label).clicked() {
                                        menu_action = Some((*code, item.id.clone()));
                                        ui.close_menu();
                                    }
                                }
                            });
                        }
                    });
                if let Some(id) = toggled {
                    if !self.checked.remove(&id) {
                        self.checked.insert(id);
                    }
                }
                if let Some(id) = clicked {
                    self.selected = if self.selected.as_deref() == Some(id.as_str()) {
                        None
                    } else {
                        Some(id)
                    };
                    self.group_edit = None;
                }
            });
        if let Some(id) = quick {
            self.quick_action(&ctx, sh, &id);
        }
        if let Some((code, id)) = menu_action {
            match code {
                0 => self.start(&ctx, sh, Op::OpenTemp(vec![id])),
                1 => self.start(&ctx, sh, Op::Relock(vec![id])),
                2 => self.ask_release(&ctx, sh, vec![id]),
                3 => self.start(&ctx, sh, Op::Repair(vec![id])),
                4 => self.ask_group(vec![id]),
                _ => {
                    self.selected = Some(id);
                    self.group_edit = None;
                }
            }
        }

        // ---- Batch bar --------------------------------------------------------------
        if batch_bar {
            ui.add_space(6.0);
            self.batch_bar(ui, sh, &visible);
        }
    }

    fn batch_bar(&mut self, ui: &mut Ui, sh: &mut Shared, visible: &[Item]) {
        let p = pal(ui);
        let ctx = ui.ctx().clone();
        let chosen: Vec<&Item> = self
            .snap
            .items
            .iter()
            .filter(|i| self.checked.contains(&i.id))
            .collect();
        let size: u64 = chosen.iter().map(|i| i.size).sum();
        let ids: Vec<String> = chosen.iter().map(|i| i.id.clone()).collect();
        let hidden = chosen
            .iter()
            .filter(|i| !visible.iter().any(|v| v.id == i.id))
            .count();
        let locked: Vec<String> = chosen
            .iter()
            .filter(|i| self.state_of(&i.id) == State::Locked)
            .map(|i| i.id.clone())
            .collect();
        let open: Vec<String> = chosen
            .iter()
            .filter(|i| self.state_of(&i.id) == State::Open)
            .map(|i| i.id.clone())
            .collect();
        let narrow = ui.available_width() < 820.0;
        let mut action = 0u8;
        Frame::new()
            .fill(p.card)
            .stroke(Stroke::new(1.0_f32, p.accent.gamma_multiply(0.5)))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::symmetric(16, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(if narrow {
                            format!("{} selected", ids.len())
                        } else {
                            format!("{} selected", count_text(ids.len()))
                        })
                        .font(semibold(14.5)),
                    );
                    if !narrow {
                        let mut text = format!("· {}", human_size(size));
                        if hidden > 0 {
                            text.push_str(&format!(" · {hidden} not shown by this filter"));
                        }
                        ui.label(RichText::new(text).color(p.muted));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::ghost_button(ui, "Clear").clicked() {
                            action = 5;
                        }
                        if widgets::ghost_button(ui, "Unlock permanently").clicked() {
                            action = 3;
                        }
                        if widgets::ghost_button(ui, "Set group").clicked() {
                            action = 4;
                        }
                        if widgets::ghost_button_enabled(ui, "Lock again", !open.is_empty())
                            .on_disabled_hover_text("None of the selected items is open")
                            .clicked()
                        {
                            action = 2;
                        }
                        if widgets::primary_button(ui, "Unlock temporarily", !locked.is_empty())
                            .on_disabled_hover_text("None of the selected items is locked")
                            .clicked()
                        {
                            action = 1;
                        }
                    });
                });
            });
        match action {
            1 => self.start(&ctx, sh, Op::OpenTemp(locked)),
            2 => self.start(&ctx, sh, Op::Relock(open)),
            3 => self.ask_release(&ctx, sh, ids),
            4 => self.ask_group(ids),
            5 => self.checked.clear(),
            _ => {}
        }
    }

    /// Shown while nothing is protected yet: what the three modes do.
    fn first_items_state(&mut self, ui: &mut Ui, sh: &mut Shared) {
        let p = pal(ui);
        let ctx = ui.ctx().clone();
        let (mut add_files, mut add_folder) = (false, false);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.08).clamp(10.0, 70.0));
                    let (rect, _) = ui.allocate_exact_size(vec2(72.0, 72.0), Sense::hover());
                    ui.painter()
                        .circle_filled(rect.center(), 36.0, p.accent.gamma_multiply(0.14));
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        Icon::LOCK.glyph(),
                        regular(30.0),
                        p.accent,
                    );
                    ui.add_space(10.0);
                    ui.label(RichText::new("Nothing is protected yet").font(semibold(20.0)));
                    ui.label(
                        RichText::new(
                            "Drop files or folders anywhere on this window, or choose them here.",
                        )
                        .color(p.muted),
                    );
                    ui.add_space(8.0);
                    // A fixed-size row so the two buttons sit in the middle.
                    ui.allocate_ui_with_layout(
                        vec2(280.0, 36.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            if widgets::primary_button(ui, &Icon::ADD.with("Add files"), true)
                                .clicked()
                            {
                                add_files = true;
                            }
                            if widgets::primary_button(
                                ui,
                                &Icon::NEW_FOLDER.with("Add folder"),
                                true,
                            )
                            .clicked()
                            {
                                add_folder = true;
                            }
                        },
                    );
                    ui.add_space(22.0);
                });
                let total = ui.available_width().min(900.0);
                let pad = ((ui.available_width() - total) / 2.0).max(0.0);
                let card_w = (total - 32.0) / 3.0;
                ui.horizontal_top(|ui| {
                    ui.add_space(pad);
                    ui.spacing_mut().item_spacing.x = 16.0;
                    for (icon, color, title, text) in [
                        (
                            Icon::LOCK,
                            p.muted,
                            "Lock",
                            "The item stays where it is. Anyone who tries to open, copy or delete it gets \"access denied\".",
                        ),
                        (
                            Icon::HIDE,
                            p.accent,
                            "Hide",
                            "The item leaves its folder and is kept elsewhere on the same drive under a random name, so search tools cannot find it by name.",
                        ),
                        (
                            Icon::KEY,
                            p.info,
                            "Ask for password on open",
                            "A stand-in with the same name stays in place. Double-click it, type the password, and the item opens. It locks again on its own.",
                        ),
                    ] {
                        Frame::new()
                            .fill(p.card)
                            .stroke(Stroke::new(1.0_f32, p.border))
                            .corner_radius(CornerRadius::same(14))
                            .inner_margin(Margin::same(18))
                            .show(ui, |ui| {
                                ui.set_width(card_w - 36.0);
                                ui.set_min_height(150.0);
                                ui.vertical(|ui| {
                                    let (rect, _) =
                                        ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                                    ui.painter().circle_filled(
                                        rect.center(),
                                        20.0,
                                        color.gamma_multiply(0.15),
                                    );
                                    ui.painter().text(
                                        rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        icon.glyph(),
                                        regular(18.0),
                                        color,
                                    );
                                    ui.add_space(4.0);
                                    ui.label(RichText::new(title).font(semibold(16.0)));
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(text).size(13.5).color(p.muted),
                                        )
                                        .wrap(),
                                    );
                                });
                            });
                    }
                });
            });
        if add_files || add_folder {
            self.pick_and_offer(&ctx, sh, add_folder);
        }
    }

    fn table_header(&mut self, ui: &mut Ui, cols: &Columns, visible: &[Item]) {
        let p = pal(ui);
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(vec2(width, 34.0), Sense::hover());
        ui.painter().hline(
            rect.x_range().shrink(6.0),
            rect.bottom() - 1.0,
            Stroke::new(1.0_f32, p.border),
        );
        let all_checked =
            !visible.is_empty() && visible.iter().all(|x| self.checked.contains(&x.id));
        let any_checked = visible.iter().any(|x| self.checked.contains(&x.id));
        let cb = Rect::from_min_size(rect.min, vec2(40.0, rect.height()));
        let cb_resp = ui
            .interact(cb, ui.id().with("cb-all"), Sense::click())
            .on_hover_text("Select all shown items");
        let state = if all_checked {
            Some(true)
        } else if any_checked {
            None
        } else {
            Some(false)
        };
        widgets::paint_checkbox(ui, cb, state, cb_resp.hovered());
        if cb_resp.clicked() {
            if all_checked {
                for x in visible {
                    self.checked.remove(&x.id);
                }
            } else {
                for x in visible {
                    self.checked.insert(x.id.clone());
                }
            }
        }

        let mut x = rect.left() + LEFT;
        let mut sort = self.sort;
        let mut header =
            |ui: &mut Ui, x: &mut f32, label: &str, col: SortCol, w: f32, right: bool| {
                if w <= 0.0 {
                    return;
                }
                let r =
                    Rect::from_min_max(pos2(*x, rect.top()), pos2(*x + w - 12.0, rect.bottom()));
                let resp = ui
                    .interact(r, ui.id().with(("hdr", label)), Sense::click())
                    .on_hover_text(format!("Sort by {}", label.to_lowercase()));
                let active = sort.0 == col;
                let color = if active {
                    p.text
                } else if resp.hovered() {
                    p.muted
                } else {
                    p.faint
                };
                let arrow = if !active {
                    ""
                } else if sort.1 {
                    Icon::SORT_UP.glyph()
                } else {
                    Icon::SORT_DOWN.glyph()
                };
                let text = if right {
                    format!("{arrow} {label}")
                } else {
                    format!("{label} {arrow}")
                };
                if right {
                    widgets::text_in_right(ui, r, text.trim(), semibold(12.0), color);
                } else {
                    widgets::text_in(ui, r, text.trim(), semibold(12.0), color);
                }
                if resp.clicked() {
                    sort = if active {
                        (col, !sort.1)
                    } else {
                        (col, !matches!(col, SortCol::Size | SortCol::Date))
                    };
                }
                *x += w;
            };
        header(ui, &mut x, "NAME", SortCol::Name, cols.name, false);
        header(
            ui,
            &mut x,
            "LOCATION",
            SortCol::Location,
            cols.location,
            false,
        );
        header(ui, &mut x, "PROTECTION", SortCol::Mode, cols.mode, false);
        header(ui, &mut x, "STATE", SortCol::State, cols.state, false);
        header(ui, &mut x, "SIZE", SortCol::Size, cols.size, true);
        if cols.size > 0.0 {
            x += 12.0;
        }
        header(
            ui,
            &mut x,
            "PROTECTED",
            SortCol::Date,
            cols.date - 12.0,
            false,
        );
        self.sort = sort;
    }

    // ----------------------------------------------------------------------------------
    // Details panel
    // ----------------------------------------------------------------------------------

    pub(super) fn details_ui(&mut self, ui: &mut Ui, sh: &mut Shared) {
        let p = pal(ui);
        let ctx = ui.ctx().clone();
        let full = ui.max_rect();
        ui.painter().vline(
            full.left() - 18.0,
            full.y_range().expand(18.0),
            Stroke::new(1.0_f32, p.border),
        );
        let Some(item) = self
            .selected
            .as_deref()
            .and_then(|id| self.snap.item(id))
            .cloned()
        else {
            self.selected = None;
            return;
        };
        let state = self.state_of(&item.id);
        let mut close = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Details").font(semibold(13.0)).color(p.faint));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::icon_button(ui, Icon::CLOSE, "Close (Esc)").clicked() {
                    close = true;
                }
            });
        });
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(52.0, 52.0), Sense::hover());
            widgets::kind_tile(ui, rect, &item.name, item.kind == Kind::Folder);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                widgets::text(ui, &item.name, 17.0, true, p.text);
                ui.label(RichText::new(kind_text(&item)).color(p.muted));
            });
        });
        ui.horizontal_wrapped(|ui| {
            widgets::badge(ui, item.mode.label(), mode_color(&p, item.mode));
            widgets::badge(ui, state.label(), state_color(&p, state));
            if item.reduced {
                widgets::badge(ui, "Reduced protection", p.warn);
            }
        });
        ui.add_space(4.0);

        let mut action = 0u8;
        ui.horizontal_wrapped(|ui| {
            match state {
                State::Locked => {
                    if widgets::primary_button(ui, &Icon::UNLOCK.with("Unlock temporarily"), true)
                        .on_hover_text(
                            "Put it back for now. It can be locked again with one click.",
                        )
                        .clicked()
                    {
                        action = 1;
                    }
                }
                State::Open => {
                    if widgets::primary_button(ui, &Icon::LOCK.with("Lock again"), true).clicked() {
                        action = 2;
                    }
                }
                State::Missing | State::Tampered => {
                    if widgets::primary_button(ui, &Icon::REPAIR.with("Repair"), true)
                        .on_hover_text("Put the recorded protection back")
                        .clicked()
                    {
                        action = 3;
                    }
                }
            }
            if widgets::ghost_button(ui, "Unlock permanently")
                .on_hover_text("Stop protecting it and restore everything exactly as it was")
                .clicked()
            {
                action = 4;
            }
        });
        ui.add_space(6.0);
        widgets::divider(ui);

        let mut save_group: Option<String> = None;
        let mut show_location = false;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(6.0);
                ui.spacing_mut().item_spacing.y = 8.0;
                match state {
                    State::Tampered => widgets::banner(
                        ui,
                        Icon::WARNING,
                        p.danger,
                        "Its protection was changed from outside VeilForge. \"Repair\" puts it back.",
                    ),
                    State::Missing => widgets::banner(
                        ui,
                        Icon::WARNING,
                        p.danger,
                        "The item is not where VeilForge left it. The Health check page says more.",
                    ),
                    _ => {}
                }
                widgets::field(ui, "Original location", &item.original);
                widgets::field(ui, "Protection", mode_sentence(&item));
                let state_text = match (state, item.open_since) {
                    (State::Open, Some(since)) => {
                        format!("Temporarily open since {}", fmt_datetime(since))
                    }
                    _ => state.label().to_string(),
                };
                widgets::field(ui, "State", &state_text);
                if state == State::Open {
                    widgets::field(
                        ui,
                        "Locks again",
                        &relock_sentence(
                            sh.settings.relock_idle_minutes,
                            sh.settings.relock_on_session_lock,
                        ),
                    );
                }
                let size = match item.kind {
                    Kind::File => human_size(item.size),
                    Kind::Folder => format!(
                        "{} in {} file{}",
                        human_size(item.size),
                        thousands(item.files),
                        if item.files == 1 { "" } else { "s" }
                    ),
                };
                widgets::field(ui, "Size when protected", &size);
                widgets::field(ui, "Protected on", &fmt_datetime(item.locked_at));

                // Group: a text box that saves on Enter or with the button.
                ui.label(RichText::new("Group").size(12.0).color(p.faint));
                ui.add_space(-4.0);
                let editing = matches!(&self.group_edit, Some((id, _)) if *id == item.id);
                if !editing {
                    self.group_edit = Some((item.id.clone(), item.group.clone()));
                }
                if let Some((_, text)) = &mut self.group_edit {
                    let changed = text.trim() != item.group;
                    widgets::left_row(ui, 34.0, |ui| {
                        let w = ui.available_width() - if changed { 76.0 } else { 0.0 };
                        let resp = widgets::text_input(ui, text, "No group", w);
                        let enter =
                            resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if changed && (widgets::ghost_button(ui, "Save").clicked() || enter) {
                            save_group = Some(text.trim().to_string());
                        }
                    });
                }
                ui.add_space(2.0);

                let drive = if item.reduced {
                    format!("{} (no permissions on this drive)", item.volume)
                } else {
                    item.volume.clone()
                };
                widgets::field(ui, "Drive", &drive);
                if let Some(hold) = &item.hold_path {
                    widgets::field(
                        ui,
                        if state == State::Open {
                            "Holding folder place (used again when locked)"
                        } else {
                            "Kept in the holding folder as"
                        },
                        hold,
                    );
                }
                if let Some(standin) = &item.standin {
                    widgets::field(ui, "Stand-in shortcut", standin);
                }
                if item.kind == Kind::Folder && item.hides() {
                    let text = if item.scrambled {
                        format!(
                            "Scrambled: {} name{} inside were replaced with random ones",
                            thousands(item.children.len() as u64),
                            if item.children.len() == 1 { "" } else { "s" }
                        )
                    } else {
                        "Not scrambled: names inside are unchanged".to_string()
                    };
                    widgets::field(ui, "Names inside", &text);
                }
                if item.reduced {
                    widgets::banner(
                        ui,
                        Icon::INFO,
                        p.warn,
                        "This drive has no permissions (FAT or exFAT), so the item is only hidden. Anyone who finds the holding folder can open it.",
                    );
                }
                ui.add_space(4.0);
                if widgets::ghost_button(ui, &Icon::FOLDER_OPEN.with("Show original location"))
                    .clicked()
                {
                    show_location = true;
                }
            });

        if close {
            self.selected = None;
            self.group_edit = None;
        }
        if let Some(group) = save_group {
            self.group_edit = None;
            self.start(&ctx, sh, Op::SetGroup(vec![item.id.clone()], group));
        }
        if show_location {
            if sh.demo {
                sh.toast("Demo: File Explorer is not opened.", false);
            } else if let Err(e) =
                crate::sys::reveal_in_explorer(std::path::Path::new(&item.original))
            {
                sh.toast(e, true);
            }
        }
        match action {
            1 => self.start(&ctx, sh, Op::OpenTemp(vec![item.id])),
            2 => self.start(&ctx, sh, Op::Relock(vec![item.id])),
            3 => self.start(&ctx, sh, Op::Repair(vec![item.id])),
            4 => self.ask_release(&ctx, sh, vec![item.id]),
            _ => {}
        }
    }
}

/// The standard page header: title, subtitle and right-aligned actions.
pub(super) fn page_header(ui: &mut Ui, title: &str, subtitle: &str, actions: impl FnOnce(&mut Ui)) {
    let p = pal(ui);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(title).font(semibold(24.0)));
            ui.label(RichText::new(subtitle).color(p.muted));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), actions);
    });
    ui.add_space(6.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_fit_every_width() {
        for width in [520.0_f32, 672.0, 800.0, 1012.0, 1400.0, 2400.0] {
            let c = Columns::for_width(width);
            let used = LEFT + RIGHT + c.name + c.location + c.mode + c.state + c.size + c.date;
            assert!(used <= width + 0.5, "{width}: {used}");
            assert!(c.name >= 120.0);
        }
        let wide = Columns::for_width(1012.0);
        assert!(wide.location > 0.0 && wide.date > 0.0);
        let narrow = Columns::for_width(672.0);
        assert_eq!(narrow.location, 0.0);
    }

    #[test]
    fn relock_sentences_follow_the_settings() {
        assert_eq!(
            relock_sentence(15, true),
            "On its own after 15 minutes without keyboard or mouse activity, and when Windows locks, sleeps or you sign out. Or with \"Lock again\"."
        );
        assert!(relock_sentence(60, false).starts_with("On its own after 1 hour without"));
        assert!(relock_sentence(0, true).starts_with("On its own when Windows locks"));
        assert!(relock_sentence(0, false).starts_with("Only when you choose"));
    }

    // ----------------------------------------------------------------------------------
    // The main window's buttons, without a window: the same functions the
    // clicks call, on generated files in a sandbox in the temp folder.
    // ----------------------------------------------------------------------------------

    use super::super::app::Msg;
    use super::super::ops::Snapshot;
    use crate::engine::cleanup::SandboxGuard;
    use crate::engine::{Env, Vault};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    const PW: &str = "a long master password";

    /// Delivers worker results to the window state until `done` says so.
    fn pump(
        main: &mut Main,
        sh: &mut Shared,
        ctx: &egui::Context,
        rx: &Receiver<Msg>,
        done: impl Fn(&Main) -> bool,
    ) {
        while !done(main) {
            let msg = rx
                .recv_timeout(Duration::from_secs(60))
                .expect("the worker answers");
            main.message(ctx, sh, msg);
        }
    }

    fn idle(main: &Main) -> bool {
        main.job.is_none()
    }

    fn id_of(main: &Main, name: &str) -> String {
        main.snap
            .items
            .iter()
            .find(|i| i.name == name)
            .unwrap_or_else(|| panic!("{name} is in the list"))
            .id
            .clone()
    }

    fn confirm(main: &mut Main, sh: &mut Shared, ctx: &egui::Context) {
        match main.dialog.take() {
            Some(Dialog::Confirm { action, .. }) => {
                let op = match action {
                    ConfirmAction::Release(ids) => Op::Release(ids),
                    ConfirmAction::OpenTemp(ids) => Op::OpenTemp(ids),
                };
                main.start(ctx, sh, op);
            }
            _ => panic!("a confirmation was expected"),
        }
    }

    #[test]
    fn the_main_windows_buttons_protect_unlock_lock_and_restore() {
        let sandbox = SandboxGuard::new("buttons");
        let env = Env::sandbox(sandbox.path());
        let work = sandbox.path().join("work");
        let folder = work.join("Family papers");
        std::fs::create_dir_all(folder.join("inner")).unwrap();
        std::fs::write(folder.join("inner").join("scan.jpg"), [1u8, 2, 3]).unwrap();
        std::fs::write(folder.join("list.txt"), b"list").unwrap();
        let letter = work.join("Letter.txt");
        std::fs::write(&letter, b"letter").unwrap();
        // "Documents" in Kurdish: right-to-left names go through every step too.
        let kurdish = work.join("بەڵگەنامەکان ڕاپۆرت.txt");
        std::fs::write(&kurdish, "ناوەڕۆک".as_bytes()).unwrap();
        let check = |folder: &Path, letter: &Path, kurdish: &Path| {
            assert_eq!(std::fs::read(folder.join("list.txt")).unwrap(), b"list");
            assert_eq!(
                std::fs::read(folder.join("inner").join("scan.jpg")).unwrap(),
                [1u8, 2, 3]
            );
            assert_eq!(std::fs::read(letter).unwrap(), b"letter");
            assert_eq!(std::fs::read(kurdish).unwrap(), "ناوەڕۆک".as_bytes());
        };

        let (mut sh, rx) = Shared::for_test(env.clone());
        let ctx = egui::Context::default();
        let vault = Vault::create(env.clone(), PW).unwrap();
        let mut main = Main::new(Snapshot::of(&vault), None);
        *sh.core.lock_vault() = Some(vault);
        sh.core
            .ui_locked
            .store(false, std::sync::atomic::Ordering::SeqCst);

        // "Add files" / a drop: the dialog opens and the engine looks at each path.
        main.offer_protect(&ctx, &mut sh, vec![folder.clone(), letter.clone()]);
        // A second drop while the dialog is open joins the list; a path that
        // is already in it is not added twice.
        let added = main.add_to_protect(
            &ctx,
            &sh,
            vec![
                kurdish.clone(),
                PathBuf::from(letter.to_string_lossy().to_uppercase()),
            ],
        );
        assert_eq!(added, 1);
        let checked = |main: &Main| match &main.dialog {
            Some(Dialog::Protect(dlg)) => dlg.rows.iter().all(|r| r.pre.is_some()),
            _ => true,
        };
        pump(&mut main, &mut sh, &ctx, &rx, checked);
        let plan = match main.dialog.take() {
            Some(Dialog::Protect(dlg)) => {
                assert_eq!(dlg.rows.len(), 3);
                dlg.plan()
            }
            _ => panic!("the protect dialog is open"),
        };
        assert_eq!(plan.len(), 3, "nothing in the sandbox is refused");

        // "Protect": everything is hidden (the usual way) and listed.
        main.start(&ctx, &mut sh, Op::Protect(plan));
        assert!(main.busy());
        pump(&mut main, &mut sh, &ctx, &rx, idle);
        assert!(main.dialog.is_none(), "no problems to report");
        assert_eq!(main.snap.items.len(), 3);
        assert!(main
            .snap
            .items
            .iter()
            .all(|i| main.state_of(&i.id) == State::Locked));
        assert!(!folder.exists() && !letter.exists() && !kurdish.exists());
        assert_eq!(main.open_count(), 0);
        let (folder_id, letter_id, kurdish_id) = (
            id_of(&main, "Family papers"),
            id_of(&main, "Letter.txt"),
            id_of(&main, "بەڵگەنامەکان ڕاپۆرت.txt"),
        );
        // Search finds the Kurdish name by a part of it.
        main.search = "ڕاپۆرت".to_string();
        assert_eq!(main.visible_items().len(), 1);
        main.search.clear();

        // The quick action of a locked row: unlock temporarily.
        main.quick_action(&ctx, &mut sh, &kurdish_id);
        pump(&mut main, &mut sh, &ctx, &rx, idle);
        assert_eq!(main.state_of(&kurdish_id), State::Open);
        assert_eq!(std::fs::read(&kurdish).unwrap(), "ناوەڕۆک".as_bytes());
        assert_eq!(main.open_count(), 1);
        // The same button on an open row: lock again.
        main.quick_action(&ctx, &mut sh, &kurdish_id);
        pump(&mut main, &mut sh, &ctx, &rx, idle);
        assert_eq!(main.state_of(&kurdish_id), State::Locked);
        assert!(!kurdish.exists());

        // "Unlock all" asks first, then everything is back for now.
        main.ask_unlock_all();
        confirm(&mut main, &mut sh, &ctx);
        pump(&mut main, &mut sh, &ctx, &rx, idle);
        assert_eq!(main.open_count(), 3);
        check(&folder, &letter, &kurdish);
        // "Lock all".
        main.start(&ctx, &mut sh, Op::RelockAll);
        pump(&mut main, &mut sh, &ctx, &rx, idle);
        assert_eq!(main.open_count(), 0);
        assert!(!folder.exists() && !letter.exists() && !kurdish.exists());

        // "Unlock permanently" asks first (the default), then forgets the item.
        main.ask_release(&ctx, &mut sh, vec![letter_id.clone()]);
        confirm(&mut main, &mut sh, &ctx);
        pump(&mut main, &mut sh, &ctx, &rx, idle);
        assert_eq!(std::fs::read(&letter).unwrap(), b"letter");
        assert!(main.snap.item(&letter_id).is_none());
        assert_eq!(main.snap.items.len(), 2);

        // Setting a group from the details panel.
        main.start(
            &ctx,
            &mut sh,
            Op::SetGroup(vec![folder_id.clone()], "خێزان".into()),
        );
        pump(&mut main, &mut sh, &ctx, &rx, idle);
        assert_eq!(main.snap.groups, vec!["خێزان".to_string()]);

        // Panic restore with a wrong password: refused, counted and logged.
        main.start(
            &ctx,
            &mut sh,
            Op::PanicRestore(zeroize::Zeroizing::new("not the password".to_string())),
        );
        pump(&mut main, &mut sh, &ctx, &rx, idle);
        assert!(matches!(main.dialog, Some(Dialog::Panic(_))));
        assert_eq!(main.attempts.failures(), 1);
        assert_eq!(main.snap.items.len(), 2, "nothing was restored");
        let log = crate::attempts::Log::load(&env.data_dir);
        assert_eq!(log.entries.len(), 1);
        assert_eq!(log.entries[0].place, crate::attempts::Place::Restore);
        main.dialog = None;

        // With the right one: everything is back exactly, nothing is listed.
        main.start(
            &ctx,
            &mut sh,
            Op::PanicRestore(zeroize::Zeroizing::new(PW.to_string())),
        );
        pump(&mut main, &mut sh, &ctx, &rx, idle);
        assert!(matches!(main.dialog, Some(Dialog::Results(_))));
        assert!(main.snap.items.is_empty());
        check(&folder, &letter, &kurdish);
        let _ = (folder_id, kurdish_id);

        // Nothing is left behind in the holding folder.
        drop(sh);
        let hold = sandbox.path().join("hold");
        let left: Vec<_> = walk(&hold);
        assert!(left.is_empty(), "left in the holding folder: {left:?}");
    }

    /// Every file below `dir` (folders that cannot be listed count as a file).
    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            if dir.exists() {
                out.push(dir.to_path_buf());
            }
            return out;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }

    #[test]
    fn parent_folder_text() {
        assert_eq!(
            parent_of("C:\\Users\\alex\\Documents\\Tax 2025"),
            "C:\\Users\\alex\\Documents"
        );
        assert_eq!(parent_of("D:\\Prototype"), "D:\\");
        assert_eq!(parent_of("name"), "name");
        assert_eq!(count_text(1), "1 item");
        assert_eq!(count_text(4), "4 items");
    }
}

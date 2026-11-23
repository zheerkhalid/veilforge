//! Small reusable pieces of UI.

use super::theme::{pal, regular, semibold, Icon, Palette};
use eframe::egui::{
    self, pos2, text::LayoutJob, text::TextWrapping, vec2, Align2, Color32, CornerRadius, FontId,
    Frame, Margin, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, Vec2,
};

/// A rounded card.
pub fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let p = pal(ui);
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        });
}

fn filled_button(ui: &mut Ui, text: &str, fill: Color32, fg: Color32, enabled: bool) -> Response {
    let p = pal(ui);
    let (fill, fg) = if enabled {
        (fill, fg)
    } else {
        (p.hover, p.faint)
    };
    let button = egui::Button::new(RichText::new(text).font(semibold(14.0)).color(fg))
        .fill(fill)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(8))
        .min_size(vec2(0.0, 34.0));
    let prev = ui.spacing().button_padding;
    ui.spacing_mut().button_padding = vec2(16.0, 7.0);
    let r = ui.add_enabled(enabled, button);
    ui.spacing_mut().button_padding = prev;
    if r.hovered() && enabled {
        ui.painter()
            .rect_filled(r.rect, CornerRadius::same(8), Color32::from_white_alpha(22));
    }
    r
}

/// The main action on a screen (purple).
pub fn primary_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let p = pal(ui);
    filled_button(ui, text, p.accent_fill, p.on_accent, enabled)
}

/// A destructive action (coral).
pub fn danger_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let p = pal(ui);
    filled_button(ui, text, p.danger, Color32::WHITE, enabled)
}

/// A secondary action.
pub fn ghost_button(ui: &mut Ui, text: &str) -> Response {
    ghost_button_enabled(ui, text, true)
}

pub fn ghost_button_enabled(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let p = pal(ui);
    let button =
        egui::Button::new(RichText::new(text).color(if enabled { p.text } else { p.faint }))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0_f32, p.border))
            .corner_radius(CornerRadius::same(8))
            .min_size(vec2(0.0, 34.0));
    ui.add_enabled(enabled, button)
}

/// A square icon-only button with a tooltip.
pub fn icon_button(ui: &mut Ui, icon: Icon, tooltip: &str) -> Response {
    let p = pal(ui);
    let label = if icon.glyph().is_empty() {
        tooltip.chars().take(1).collect::<String>()
    } else {
        icon.glyph().to_string()
    };
    let button = egui::Button::new(RichText::new(label).size(15.0).color(p.muted))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(8))
        .min_size(vec2(32.0, 32.0));
    ui.add(button).on_hover_text(tooltip)
}

/// A small pill label.
pub fn badge(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let font = semibold(11.5);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let size = vec2(galley.size().x + 14.0, 20.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    paint_badge_at(ui, rect, galley, color);
    resp
}

fn paint_badge_at(ui: &Ui, rect: Rect, galley: std::sync::Arc<egui::Galley>, color: Color32) {
    ui.painter()
        .rect_filled(rect, CornerRadius::same(10), color.gamma_multiply(0.16));
    let pos = pos2(
        rect.center().x - galley.size().x / 2.0,
        rect.center().y - galley.size().y / 2.0,
    );
    ui.painter().galley(pos, galley, color);
}

/// Paints a custom checkbox in `rect`. `state`: Some(true) checked,
/// Some(false) unchecked, None = mixed.
pub fn paint_checkbox(ui: &Ui, rect: Rect, state: Option<bool>, hovered: bool) {
    let p = pal(ui);
    let r = Rect::from_center_size(rect.center(), Vec2::splat(18.0));
    match state {
        Some(false) => {
            ui.painter().rect_stroke(
                r,
                CornerRadius::same(5),
                Stroke::new(1.5_f32, if hovered { p.accent } else { p.faint }),
                StrokeKind::Inside,
            );
        }
        Some(true) => {
            ui.painter()
                .rect_filled(r, CornerRadius::same(5), p.accent_fill);
            let c = r.center();
            let pts = [
                c + vec2(-4.5, 0.0),
                c + vec2(-1.5, 3.2),
                c + vec2(4.8, -3.6),
            ];
            ui.painter()
                .line_segment([pts[0], pts[1]], Stroke::new(2.0_f32, p.on_accent));
            ui.painter()
                .line_segment([pts[1], pts[2]], Stroke::new(2.0_f32, p.on_accent));
        }
        None => {
            ui.painter()
                .rect_filled(r, CornerRadius::same(5), p.accent_fill);
            let c = r.center();
            ui.painter().line_segment(
                [c + vec2(-4.5, 0.0), c + vec2(4.5, 0.0)],
                Stroke::new(2.0_f32, p.on_accent),
            );
        }
    }
}

/// A checkbox with a label, in the brand style.
pub fn checkbox(ui: &mut Ui, value: &mut bool, label: &str) -> Response {
    checkbox_enabled(ui, value, label, true)
}

pub fn checkbox_enabled(ui: &mut Ui, value: &mut bool, label: &str, enabled: bool) -> Response {
    let p = pal(ui);
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        regular(14.0),
        if enabled { p.text } else { p.faint },
    );
    let size = vec2(26.0 + galley.size().x, 24.0);
    let (rect, mut resp) = ui.allocate_exact_size(
        size,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if resp.clicked() {
        *value = !*value;
        resp.mark_changed();
    }
    let box_rect = Rect::from_min_size(rect.min, vec2(20.0, rect.height()));
    if enabled {
        paint_checkbox(ui, box_rect, Some(*value), resp.hovered());
    } else {
        ui.painter().rect_stroke(
            Rect::from_center_size(box_rect.center(), Vec2::splat(18.0)),
            CornerRadius::same(5),
            Stroke::new(1.5_f32, p.border),
            StrokeKind::Inside,
        );
    }
    ui.painter().galley(
        pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.text,
    );
    resp
}

/// An on/off switch. Returns true when clicked.
pub fn toggle(ui: &mut Ui, on: bool, enabled: bool) -> Response {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(
        vec2(38.0, 22.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let t = ui.ctx().animate_bool_with_time(resp.id, on, 0.12);
    let track = if !enabled {
        p.border
    } else if on {
        p.accent_fill
    } else {
        p.faint.gamma_multiply(0.6)
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(11), track);
    let x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
    ui.painter().circle_filled(
        pos2(x, rect.center().y),
        8.0,
        if enabled { Color32::WHITE } else { p.faint },
    );
    resp
}

/// A row of mutually exclusive choices. Returns the newly picked index.
pub fn segmented(ui: &mut Ui, options: &[&str], current: usize) -> Option<usize> {
    let p = pal(ui);
    let mut picked = None;
    Frame::new()
        .fill(p.hover)
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                // In a right-to-left row the first widget lands on the right:
                // add the options backwards so they still read left to right.
                let mut order: Vec<usize> = (0..options.len()).collect();
                if ui.layout().prefer_right_to_left() {
                    order.reverse();
                }
                for i in order {
                    let label = &options[i];
                    let active = i == current;
                    let text = RichText::new(*label).color(if active { p.text } else { p.muted });
                    let text = if active {
                        text.font(semibold(13.5))
                    } else {
                        text.size(13.5)
                    };
                    let b = egui::Button::new(text)
                        .fill(if active { p.card } else { Color32::TRANSPARENT })
                        .stroke(if active {
                            Stroke::new(1.0_f32, p.border)
                        } else {
                            Stroke::NONE
                        })
                        .corner_radius(CornerRadius::same(7))
                        .min_size(vec2(0.0, 28.0));
                    if ui.add(b).clicked() && !active {
                        picked = Some(i);
                    }
                }
            });
        });
    picked
}

/// A filter chip.
pub fn chip(ui: &mut Ui, label: &str, count: Option<usize>, active: bool) -> Response {
    let p = pal(ui);
    let text = match count {
        Some(n) => format!("{label}  {n}"),
        None => label.to_string(),
    };
    let (fill, fg, stroke) = if active {
        (
            p.accent.gamma_multiply(0.16),
            p.accent,
            Stroke::new(1.0_f32, p.accent.gamma_multiply(0.6)),
        )
    } else {
        (
            Color32::TRANSPARENT,
            p.muted,
            Stroke::new(1.0_f32, p.border),
        )
    };
    if super::bidi::needs_native(&text) {
        // A group named in Kurdish or Arabic: the same chip, drawn by hand
        // around text that Windows shaped.
        if let Some(size) = super::bidi::measure(ui, &text, 13.0, false) {
            let (rect, resp) = ui.allocate_exact_size(vec2(size.x + 22.0, 30.0), Sense::click());
            let fill = if resp.hovered() && !active {
                p.hover
            } else {
                fill
            };
            ui.painter().rect(
                rect,
                CornerRadius::same(15),
                fill,
                stroke,
                egui::StrokeKind::Inside,
            );
            super::bidi::paint(
                ui,
                rect.shrink2(vec2(11.0, 0.0)),
                &text,
                13.0,
                false,
                fg,
                false,
            );
            return resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
    }
    let b = egui::Button::new(RichText::new(text).size(13.0).color(fg))
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(15))
        .min_size(vec2(0.0, 30.0));
    ui.add(b)
}

/// Lays out one line of text, cut off with an ellipsis at `max_width`.
pub fn elided(
    ui: &Ui,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = TextWrapping {
        max_width: max_width.max(8.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.fonts(|f| f.layout_job(job))
}

/// Whether `font` is the semibold face (for text drawn by Windows).
fn is_bold(font: &FontId) -> bool {
    matches!(&font.family, egui::FontFamily::Name(name) if name.as_ref() == "semibold")
}

/// Paints one elided line, vertically centred in `rect`. Returns whether it was cut.
/// Kurdish, Arabic and other right-to-left text is drawn by Windows, joined
/// and in reading order (see `bidi`).
pub fn text_in(ui: &Ui, rect: Rect, text: &str, font: FontId, color: Color32) -> bool {
    if super::bidi::needs_native(text) {
        if let Some(cut) =
            super::bidi::paint(ui, rect, text, font.size, is_bold(&font), color, false)
        {
            return cut;
        }
    }
    let galley = elided(ui, text, font, color, rect.width());
    let cut = galley.elided;
    ui.painter().galley(
        pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    cut
}

/// Like [`text_in`], right-aligned.
pub fn text_in_right(ui: &Ui, rect: Rect, text: &str, font: FontId, color: Color32) {
    if super::bidi::needs_native(text)
        && super::bidi::paint(ui, rect, text, font.size, is_bold(&font), color, true).is_some()
    {
        return;
    }
    let galley = elided(ui, text, font, color, rect.width());
    ui.painter().galley(
        pos2(
            rect.right() - galley.size().x,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        color,
    );
}

fn tile_color(name: &str) -> Color32 {
    const COLORS: [Color32; 8] = [
        Color32::from_rgb(0x10, 0xa3, 0x7f),
        Color32::from_rgb(0x08, 0x6f, 0xa8),
        Color32::from_rgb(0x7c, 0x5c, 0xd6),
        Color32::from_rgb(0xd9, 0x6a, 0x2b),
        Color32::from_rgb(0xc2, 0x48, 0x7a),
        Color32::from_rgb(0x2f, 0x86, 0xc9),
        Color32::from_rgb(0x4e, 0x9a, 0x3c),
        Color32::from_rgb(0xb8, 0x8a, 0x1f),
    ];
    let h = name
        .bytes()
        .fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    COLORS[(h % COLORS.len() as u32) as usize]
}

/// Paints a coloured tile with the item's initial (no icons are downloaded).
pub fn letter_tile(ui: &Ui, rect: Rect, name: &str) {
    let radius = (rect.width() * 0.26) as u8;
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius), tile_color(name));
    let initial: String = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_string());
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        initial,
        semibold(rect.height() * 0.5),
        Color32::WHITE,
    );
}

/// A designed empty / error state: icon, title, explanation.
pub fn empty_state(ui: &mut Ui, icon: Icon, color: Color32, title: &str, text: &str) {
    let p = pal(ui);
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.22).clamp(24.0, 140.0));
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(72.0), Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 36.0, color.gamma_multiply(0.14));
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            icon.glyph(),
            regular(30.0),
            color,
        );
        ui.add_space(10.0);
        ui.label(RichText::new(title).font(semibold(18.0)).color(p.text));
        ui.add_space(2.0);
        ui.scope(|ui| {
            ui.set_max_width((ui.available_width() - 12.0).min(460.0));
            ui.label(RichText::new(text).color(p.muted));
        });
        ui.add_space(8.0);
    });
}

/// A spinner with a line of text, centred.
pub fn busy_state(ui: &mut Ui, title: &str, text: &str) {
    let p = pal(ui);
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.25).clamp(24.0, 160.0));
        ui.add(egui::Spinner::new().size(34.0).color(p.accent));
        ui.add_space(12.0);
        ui.label(RichText::new(title).font(semibold(18.0)));
        ui.add_space(2.0);
        ui.scope(|ui| {
            ui.set_max_width((ui.available_width() - 12.0).min(560.0));
            ui.add(egui::Label::new(RichText::new(text).color(p.muted)).truncate());
        });
    });
}

/// Section title inside a page.
pub fn section(ui: &mut Ui, title: &str) {
    ui.label(RichText::new(title).font(semibold(15.0)));
}

/// A thin horizontal line.
pub fn divider(ui: &mut Ui) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0, p.border);
}

fn input_frame(ui: &Ui) -> Frame {
    let p = pal(ui);
    Frame::new()
        .fill(if p.dark { p.bg } else { Color32::WHITE })
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 5))
}

/// A single-line text box in the family style.
pub fn text_input(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    let p = pal(ui);
    input_frame(ui)
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(text)
                    .hint_text(RichText::new(hint).color(p.faint))
                    .frame(false)
                    .desired_width(width - 28.0)
                    .font(regular(14.5)),
            )
        })
        .inner
}

/// A multi-line text box.
pub fn text_area(
    ui: &mut Ui,
    text: &mut dyn egui::TextBuffer,
    hint: &str,
    width: f32,
    rows: usize,
) -> Response {
    let p = pal(ui);
    input_frame(ui)
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(text)
                    .hint_text(RichText::new(hint).color(p.faint))
                    .frame(false)
                    .desired_width(width - 28.0)
                    .desired_rows(rows)
                    .font(regular(14.5)),
            )
        })
        .inner
}

/// A single-line box for a secret. The text lives in a [`SecretInput`]; egui's
/// undo history for the field is cleared after every frame so it does not
/// accumulate copies of what was typed.
pub fn secret_input(
    ui: &mut Ui,
    secret: &mut crate::secret::SecretInput,
    show: bool,
    hint: &str,
    width: f32,
) -> Response {
    let p = pal(ui);
    let resp = input_frame(ui)
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(secret)
                    .password(!show)
                    .hint_text(RichText::new(hint).color(p.faint))
                    .frame(false)
                    .desired_width(width - 28.0)
                    .font(if show {
                        FontId::new(14.5, egui::FontFamily::Monospace)
                    } else {
                        regular(14.5)
                    }),
            )
        })
        .inner;
    forget_undo(ui, resp.id);
    resp
}

/// Drops the undo history egui keeps for a text field.
pub fn forget_undo(ui: &Ui, id: egui::Id) {
    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
        state.clear_undoer();
        egui::TextEdit::store_state(ui.ctx(), id, state);
    }
}

/// A small caption above an input.
pub fn caption(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    ui.label(RichText::new(text).size(12.5).color(p.muted));
}

pub fn strength_color(p: &Palette, score: u8) -> Color32 {
    match score {
        0 | 1 => p.danger,
        2 => p.warn,
        3 => p.info,
        _ => p.ok,
    }
}

/// Five segments, filled up to the score (0–4).
pub fn strength_bar(ui: &mut Ui, score: u8, empty: bool, width: f32) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 6.0), Sense::hover());
    let gap = 4.0;
    let seg = (width - gap * 4.0) / 5.0;
    for i in 0..5u8 {
        let r = Rect::from_min_size(
            pos2(rect.left() + f32::from(i) * (seg + gap), rect.top()),
            vec2(seg, 6.0),
        );
        let on = !empty && i <= score;
        ui.painter().rect_filled(
            r,
            CornerRadius::same(3),
            if on {
                strength_color(&p, score)
            } else {
                p.border
            },
        );
    }
}

/// A thin horizontal progress bar (fraction 0–1).
pub fn progress_bar(ui: &mut Ui, fraction: f32, width: f32, color: Color32) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 6.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(3), p.border);
    let mut fill = rect;
    fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    ui.painter().rect_filled(fill, CornerRadius::same(3), color);
}

/// Paints a ring: a faint full circle and a coloured arc for `fraction`
/// (0–1), starting at the top and going clockwise.
pub fn paint_ring(
    ui: &Ui,
    center: egui::Pos2,
    radius: f32,
    width: f32,
    fraction: f32,
    color: Color32,
) {
    let p = pal(ui);
    ui.painter()
        .circle_stroke(center, radius, Stroke::new(width, p.border));
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction <= 0.0 {
        return;
    }
    let steps = ((fraction * 64.0).ceil() as usize).max(2);
    let points: Vec<egui::Pos2> = (0..=steps)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2
                + std::f32::consts::TAU * fraction * i as f32 / steps as f32;
            center + radius * vec2(a.cos(), a.sin())
        })
        .collect();
    ui.painter().line(points, Stroke::new(width, color));
}

/// A text link that looks like a button-less action.
pub fn link(ui: &mut Ui, text: &str) -> Response {
    let p = pal(ui);
    ui.add(egui::Label::new(RichText::new(text).color(p.accent)).sense(Sense::click()))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A wrapped paragraph in the muted colour.
pub fn note(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    ui.add(egui::Label::new(RichText::new(text).size(13.0).color(p.muted)).wrap());
}

/// An inline message strip: warnings, errors, information.
pub fn banner(ui: &mut Ui, icon: Icon, color: Color32, text: &str) {
    Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon.glyph()).color(color));
                let fg = pal(ui).text;
                super::bidi::wrapped(ui, text, 13.5, false, fg);
            });
        });
}

/// The frame used by modal dialogs.
pub fn modal_frame(ctx: &egui::Context) -> Frame {
    let p = super::theme::pal_ctx(ctx);
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::same(22))
}

/// Paints a badge left-aligned inside `rect` and returns the width it used.
pub fn paint_badge(ui: &Ui, rect: Rect, text: &str, color: Color32) -> f32 {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), semibold(11.5), color);
    let w = galley.size().x + 14.0;
    let r = Rect::from_min_size(pos2(rect.left(), rect.center().y - 10.0), vec2(w, 20.0));
    paint_badge_at(ui, r, galley, color);
    w
}

/// A coloured tile with a folder or a document on it (no real icons are read
/// from the disk: a protected item cannot be looked at anyway).
pub fn kind_tile(ui: &Ui, rect: Rect, name: &str, folder: bool) {
    let radius = (rect.width() * 0.26) as u8;
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(radius), tile_color(name));
    let unit = rect.width();
    if folder {
        // A folder like the one in the app icon: a tab behind a body.
        let body = Rect::from_center_size(
            rect.center() + vec2(0.0, unit * 0.045),
            vec2(unit * 0.56, unit * 0.38),
        );
        let tab = Rect::from_min_size(
            body.left_top() - vec2(0.0, unit * 0.09),
            vec2(unit * 0.27, unit * 0.2),
        );
        let r = (unit * 0.06).max(1.0) as u8;
        painter.rect_filled(tab, CornerRadius::same(r), Color32::from_white_alpha(190));
        painter.rect_filled(body, CornerRadius::same(r), Color32::WHITE);
    } else {
        // A sheet of paper with two lines of "text".
        let page = Rect::from_center_size(rect.center(), vec2(unit * 0.42, unit * 0.54));
        let r = (unit * 0.06).max(1.0) as u8;
        painter.rect_filled(page, CornerRadius::same(r), Color32::WHITE);
        let ink = tile_color(name);
        let line = |y: f32, w: f32| {
            let left = page.left() + unit * 0.08;
            painter.line_segment(
                [
                    pos2(left, page.top() + unit * y),
                    pos2(left + unit * w, page.top() + unit * y),
                ],
                Stroke::new((unit * 0.045).max(1.0), ink),
            );
        };
        line(0.17, 0.26);
        line(0.28, 0.26);
        line(0.39, 0.16);
    }
}

/// A labelled value in a details panel.
pub fn field(ui: &mut Ui, label: &str, value: &str) {
    let p = pal(ui);
    ui.label(RichText::new(label).size(12.0).color(p.faint));
    ui.add_space(-6.0);
    let empty = value.trim().is_empty();
    if super::bidi::needs_native(value) {
        super::bidi::wrapped(ui, value, BODY_SIZE, false, p.text);
        ui.add_space(2.0);
        return;
    }
    ui.add(
        egui::Label::new(
            RichText::new(if empty { "None" } else { value }).color(if empty {
                p.faint
            } else {
                p.text
            }),
        )
        .wrap(),
    );
    ui.add_space(2.0);
}

/// A right-aligned row of dialog buttons with a FIXED height. A bare
/// right-to-left layout takes all the height a modal may use, which makes
/// the dialog as tall as the window and leaves a blank panel.
pub fn button_row(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 36.0),
        egui::Layout::right_to_left(egui::Align::Center),
        add,
    );
}

/// Like [`button_row`], for a row that starts at the left.
pub fn left_row(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), height),
        egui::Layout::left_to_right(egui::Align::Center),
        add,
    );
}

/// The title of a modal dialog.
pub fn dialog_title(ui: &mut Ui, text: &str) {
    if super::bidi::needs_native(text) {
        let p = pal(ui);
        super::bidi::label(ui, text, 19.0, true, p.text);
    } else {
        ui.label(RichText::new(text).font(semibold(19.0)));
    }
    ui.add_space(2.0);
}

/// The size of ordinary text (the `Body` text style of the theme).
pub const BODY_SIZE: f32 = 14.0;

/// A wrapped line or paragraph of ordinary text in any script. Use it
/// wherever a file or folder name is part of the text.
pub fn text(ui: &mut Ui, text: &str, size: f32, bold: bool, color: Color32) -> Response {
    super::bidi::wrapped(ui, text, size, bold, color)
}

/// A wrapped paragraph in a dialog.
pub fn para(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    super::bidi::wrapped(ui, text, BODY_SIZE, false, p.muted);
}

/// A password box with a show/hide button on its right; `width` is the
/// width of the whole row. Returns the response of the text box.
pub fn password_row(
    ui: &mut Ui,
    secret: &mut crate::secret::SecretInput,
    show: &mut bool,
    hint: &str,
    width: f32,
    enabled: bool,
) -> Response {
    let mut out = None;
    left_row(ui, 34.0, |ui| {
        ui.set_width(width);
        ui.add_enabled_ui(enabled, |ui| {
            out = Some(secret_input(ui, secret, *show, hint, width - 48.0));
        });
        let icon = if *show { Icon::HIDE } else { Icon::VIEW };
        if icon_button(ui, icon, "Show or hide what you type").clicked() {
            *show = !*show;
        }
    });
    out.expect("the row always adds the text box")
}

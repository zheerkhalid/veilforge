//! Display of right-to-left scripts (Kurdish Sorani, Arabic, Persian, Urdu,
//! Hebrew…).
//!
//! egui lays text out one glyph per character, left to right, and never joins
//! letters, so a Kurdish or Arabic name comes out backwards and in pieces.
//! Substituting "presentation form" characters is not enough either: several
//! Kurdish letters (ڕ ڵ ە ێ) have none, so words still break apart.
//!
//! Instead, text that contains such letters is drawn by Windows itself (GDI,
//! which shapes every script and orders mixed text the same way File Explorer
//! does) into a small white-on-transparent bitmap, cached as an egui texture
//! and tinted with the wanted colour. Everything else still goes through egui,
//! so ordinary names cost one character scan.
//!
//! The same approach as in FindForge, with one addition: whole sentences
//! (dialog texts, notices) that quote such a name are drawn wrapped.

use eframe::egui::{
    self, pos2, vec2, Color32, ColorImage, Rect, Sense, TextureHandle, TextureOptions, Ui, Vec2,
};
use std::cell::RefCell;
use std::collections::HashMap;

fn is_rtl_char(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
}

/// Whether `s` contains right-to-left letters that egui cannot draw properly.
pub fn needs_native(s: &str) -> bool {
    s.chars().any(is_rtl_char)
}

/// A rendered line: white text with coverage in the alpha channel.
struct Rendered {
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    cut: bool,
}

#[derive(Hash, PartialEq, Eq, Clone)]
struct Key {
    text: String,
    size_px: u32,
    bold: bool,
    max_px: u32,
    wrap: bool,
}

struct Entry {
    tex: TextureHandle,
    size_px: [usize; 2],
    cut: bool,
}

/// Rendered lines are kept while they are in use; the cache is simply emptied
/// when it grows large (scrolling through thousands of names).
const CACHE_LIMIT: usize = 1500;

thread_local! {
    static CACHE: RefCell<HashMap<Key, Entry>> = RefCell::new(HashMap::new());
}

#[cfg(windows)]
fn render(text: &str, size_px: u32, bold: bool, max_px: u32, wrap: bool) -> Option<Rendered> {
    use windows::core::w;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::*;

    let mut wide: Vec<u16> = text.encode_utf16().collect();
    if wide.is_empty() || size_px == 0 {
        return None;
    }
    // SAFETY: plain GDI calls on a memory DC and DIB we create and free here.
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return None;
        }
        let mut face = [0u16; 32];
        let name = w!("Segoe UI");
        let name = name.as_wide();
        face[..name.len()].copy_from_slice(name);
        let lf = LOGFONTW {
            lfHeight: -(size_px as i32),
            lfWeight: if bold { 600 } else { 400 },
            lfCharSet: DEFAULT_CHARSET,
            lfOutPrecision: OUT_TT_PRECIS,
            lfClipPrecision: CLIP_DEFAULT_PRECIS,
            lfQuality: ANTIALIASED_QUALITY,
            lfFaceName: face,
            ..Default::default()
        };
        let font = CreateFontIndirectW(&lf);
        let old_font = SelectObject(dc, font);

        let flags = if wrap {
            DT_WORDBREAK | DT_EDITCONTROL | DT_NOPREFIX
        } else {
            DT_SINGLELINE | DT_NOPREFIX
        };
        // When wrapping, the width is given and the height is the answer.
        let mut measure = RECT {
            right: if wrap {
                max_px.clamp(1, 16_000) as i32
            } else {
                0
            },
            ..Default::default()
        };
        DrawTextW(dc, &mut wide, &mut measure, flags | DT_CALCRECT);
        let natural = measure.right.max(1) as u32;
        let height = (measure.bottom.max(1) as usize).min(8_000);
        let cut = !wrap && natural > max_px.max(1);
        let width = natural.min(max_px.max(1)) as usize;

        let header = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let info = BITMAPINFO {
            bmiHeader: header,
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let result = match CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bmp) if !bits.is_null() => {
                let old_bmp = SelectObject(dc, bmp);
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, windows::Win32::Foundation::COLORREF(0x00FF_FFFF));
                let mut area = RECT {
                    left: 0,
                    top: 0,
                    right: width as i32,
                    bottom: height as i32,
                };
                let mut draw_flags = flags | DT_LEFT;
                if !wrap {
                    draw_flags |= DT_VCENTER;
                }
                if cut {
                    draw_flags |= DT_END_ELLIPSIS;
                }
                DrawTextW(dc, &mut wide, &mut area, draw_flags);
                let _ = GdiFlush();
                let src = std::slice::from_raw_parts(bits as *const u8, width * height * 4);
                let mut pixels = Vec::with_capacity(width * height * 4);
                for px in src.as_chunks::<4>().0 {
                    // Grey-scale anti-aliasing: any channel is the coverage.
                    // A mild curve thins the edges to match egui's own text weight.
                    let c = px[0].max(px[1]).max(px[2]);
                    let a = (255.0 * (c as f32 / 255.0).powf(1.45)).round() as u8;
                    pixels.extend_from_slice(&[255, 255, 255, a]);
                }
                SelectObject(dc, old_bmp);
                let _ = DeleteObject(bmp);
                Some(Rendered {
                    pixels,
                    width,
                    height,
                    cut,
                })
            }
            _ => None,
        };
        SelectObject(dc, old_font);
        let _ = DeleteObject(font);
        let _ = DeleteDC(dc);
        result
    }
}

#[cfg(not(windows))]
fn render(_: &str, _: u32, _: bool, _: u32, _: bool) -> Option<Rendered> {
    None
}

/// Looks up or renders a line. Sizes are in physical pixels.
fn line(
    ctx: &egui::Context,
    text: &str,
    size_px: u32,
    bold: bool,
    max_px: u32,
    wrap: bool,
) -> Option<([usize; 2], egui::TextureId, bool)> {
    let key = Key {
        text: text.to_string(),
        size_px,
        bold,
        max_px,
        wrap,
    };
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(e) = cache.get(&key) {
            return Some((e.size_px, e.tex.id(), e.cut));
        }
        let r = render(text, size_px, bold, max_px, wrap)?;
        let image = ColorImage::from_rgba_unmultiplied([r.width, r.height], &r.pixels);
        let tex = ctx.load_texture("rtl-text", image, TextureOptions::NEAREST);
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        let out = ([r.width, r.height], tex.id(), r.cut);
        cache.insert(
            key,
            Entry {
                tex,
                size_px: [r.width, r.height],
                cut: r.cut,
            },
        );
        Some(out)
    })
}

/// Paints `text` on one line inside `rect`, vertically centred, cut with an
/// ellipsis when too wide. Returns `None` when native drawing is unavailable,
/// otherwise whether the text was cut.
pub fn paint(
    ui: &Ui,
    rect: Rect,
    text: &str,
    size_pt: f32,
    bold: bool,
    color: Color32,
    right: bool,
) -> Option<bool> {
    let ppp = ui.ctx().pixels_per_point();
    let size_px = (size_pt * ppp).round() as u32;
    let max_px = (rect.width() * ppp).floor().max(1.0) as u32;
    let (size, tex, cut) = line(ui.ctx(), text, size_px, bold, max_px, false)?;
    let size_pt = vec2(size[0] as f32, size[1] as f32) / ppp;
    let x = if right {
        rect.right() - size_pt.x
    } else {
        rect.left()
    };
    let y = rect.center().y - size_pt.y / 2.0;
    // Snap to whole pixels so the bitmap is drawn 1:1, without blurring.
    let min = pos2((x * ppp).round() / ppp, (y * ppp).round() / ppp);
    ui.painter().image(
        tex,
        Rect::from_min_size(min, size_pt),
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        color,
    );
    Some(cut)
}

/// The size `text` would take on one line, in points.
pub fn measure(ui: &Ui, text: &str, size_pt: f32, bold: bool) -> Option<Vec2> {
    let ppp = ui.ctx().pixels_per_point();
    let size_px = (size_pt * ppp).round() as u32;
    let (size, _, _) = line(ui.ctx(), text, size_px, bold, u32::MAX / 2, false)?;
    Some(vec2(size[0] as f32, size[1] as f32) / ppp)
}

/// A one-line label that fills the available width and truncates, drawing
/// right-to-left text natively. Falls back to an egui label for other text.
pub fn label(ui: &mut Ui, text: &str, size_pt: f32, bold: bool, color: Color32) -> egui::Response {
    if needs_native(text) {
        if let Some(natural) = measure(ui, text, size_pt, bold) {
            let width = natural.x.min(ui.available_width()).max(1.0);
            let (rect, response) = ui.allocate_exact_size(vec2(width, natural.y), Sense::hover());
            paint(ui, rect, text, size_pt, bold, color, false);
            return response.on_hover_text(text);
        }
    }
    let font = if bold {
        super::theme::semibold(size_pt)
    } else {
        egui::FontId::proportional(size_pt)
    };
    ui.add(egui::Label::new(egui::RichText::new(text).font(font).color(color)).truncate())
}

/// A paragraph that wraps at the available width, drawing text that contains
/// right-to-left letters natively. Falls back to a wrapped egui label.
pub fn wrapped(
    ui: &mut Ui,
    text: &str,
    size_pt: f32,
    bold: bool,
    color: Color32,
) -> egui::Response {
    if needs_native(text) {
        let ppp = ui.ctx().pixels_per_point();
        let size_px = (size_pt * ppp).round() as u32;
        let max_px = (ui.available_width() * ppp).floor().max(1.0) as u32;
        if let Some((size, tex, _)) = line(ui.ctx(), text, size_px, bold, max_px, true) {
            let size_pt = vec2(size[0] as f32, size[1] as f32) / ppp;
            let (rect, response) = ui.allocate_exact_size(size_pt, Sense::hover());
            let min = pos2(
                (rect.left() * ppp).round() / ppp,
                (rect.top() * ppp).round() / ppp,
            );
            ui.painter().image(
                tex,
                Rect::from_min_size(min, size_pt),
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                color,
            );
            return response;
        }
    }
    let font = if bold {
        super::theme::semibold(size_pt)
    } else {
        egui::FontId::proportional(size_pt)
    };
    ui.add(egui::Label::new(egui::RichText::new(text).font(font).color(color)).wrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_rtl_text_takes_the_native_path() {
        assert!(!needs_native("report.pdf"));
        assert!(!needs_native("C:\\Users\\Alex\\Documents"));
        assert!(needs_native("گۆرانی کوردی.mp3"));
        assert!(needs_native("D:\\Music\\بەڵگەنامەکان\\track.mp3"));
        assert!(needs_native("תיקייה"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_renders_kurdish_text_with_ink() {
        let r = render("بەڵگەنامەکان ڕاپۆرت", 20, false, 2000, false).expect("rendered");
        assert!(r.width > 40 && r.height >= 18, "{}x{}", r.width, r.height);
        assert!(!r.cut);
        let ink = r
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 128)
            .count();
        assert!(ink > 50, "expected visible letters, got {ink} dark pixels");
    }

    #[cfg(windows)]
    #[test]
    fn long_text_is_cut_to_the_width() {
        let r = render(
            "ڕاپۆرتی ساڵانەی بەڵگەنامەکانی کوردستان",
            20,
            false,
            60,
            false,
        )
        .expect("rendered");
        assert!(r.cut);
        assert_eq!(r.width, 60);
    }

    #[cfg(windows)]
    #[test]
    fn a_sentence_with_a_kurdish_name_wraps_onto_more_lines() {
        let text = "\"بەڵگەنامەکانی ساڵانە.pdf\" will be back in its original place and open to anyone using this Windows account.";
        let one = render(text, 16, false, 4000, false).expect("rendered");
        let many = render(text, 16, false, 220, true).expect("rendered");
        assert!(!many.cut);
        assert!(many.width <= 220);
        assert!(
            many.height >= one.height * 3,
            "{} against {}",
            many.height,
            one.height
        );
    }

    /// Windows joins the letters of a word; kept apart by zero-width
    /// non-joiners, the same letters take more room. A renderer that drew
    /// one glyph per character would give the same width for both.
    #[cfg(windows)]
    #[test]
    fn letters_are_joined_not_drawn_one_by_one() {
        let word = "بەڵگەنامەکان";
        let joined = render(word, 40, false, 4000, false)
            .expect("rendered")
            .width;
        let apart: String = word.chars().flat_map(|c| [c, '\u{200C}']).collect();
        let separate = render(&apart, 40, false, 4000, false)
            .expect("rendered")
            .width;
        assert!(joined < separate, "{joined} against {separate}");
    }
}

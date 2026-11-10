use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use fontdb::{Database, Family, Query, Style, Weight};
use fontdue::{Font, FontSettings, Metrics};

use crate::{
    config::FontConfig,
    terminal::{Cell, Screen, TextStyle},
};

struct Glyph {
    metrics: Metrics,
    bitmap: Vec<u8>,
}

/// Pick a real bold/italic font face when the installed family provides it.
/// If a family has no such face, the draw stage synthesizes the missing
/// attribute rather than silently rendering a regular glyph.
fn load_style_face(
    db: &Database,
    families: &[Family<'_>],
    bold: bool,
    italic: bool,
) -> Option<Font> {
    let query = Query {
        families,
        weight: if bold { Weight::BOLD } else { Weight::NORMAL },
        style: if italic { Style::Italic } else { Style::Normal },
        ..Query::default()
    };
    let id = db.query(&query)?;
    let face = db.face(id)?;
    if bold && face.weight.0 < 600 {
        return None;
    }
    if italic && face.style == Style::Normal {
        return None;
    }
    db.with_face_data(id, |data, index| {
        Font::from_bytes(
            data.to_vec(),
            FontSettings {
                collection_index: index,
                ..FontSettings::default()
            },
        )
        .ok()
    })
    .flatten()
}

pub struct Renderer {
    fonts: Vec<Font>,
    // For each regular fallback face: bold, italic, bold-italic variants.
    styled_fonts: Vec<[Option<Font>; 3]>,
    glyphs: HashMap<(usize, u8, char), Glyph>,
    // Only used when RST_DEBUG_FONTS=1 to avoid repeatedly reporting a codepoint.
    missing_glyphs: HashSet<char>,
    debug_fonts: bool,
    font_size: f32,
    pub cell_width: usize,
    pub cell_height: usize,
    pub padding: usize,
    ascent: i32,
    // Bottom edge of a regular lowercase glyph, relative to a grid row.
    text_ink_bottom: i32,
}

impl Renderer {
    pub fn new(config: &FontConfig, padding: u32, scale: f64) -> Result<Self> {
        let mut db = Database::new();
        db.load_system_fonts();
        let mut fonts = Vec::new();
        let mut styled_fonts = Vec::new();
        let mut loaded = Vec::new();
        let mut seen = HashSet::new();
        let debug_fonts = std::env::var_os("RST_DEBUG_FONTS").is_some();

        for family in std::iter::once(&config.family).chain(config.fallback.iter()) {
            if !seen.insert(family.to_lowercase()) {
                continue;
            }
            let query = Query {
                families: &[Family::Name(family)],
                ..Query::default()
            };
            if let Some(font) = db.query(&query).and_then(|id| {
                db.with_face_data(id, |data, index| {
                    Font::from_bytes(
                        data.to_vec(),
                        FontSettings {
                            collection_index: index,
                            ..FontSettings::default()
                        },
                    )
                    .ok()
                })
                .flatten()
            }) {
                let family_query = [Family::Name(family)];
                styled_fonts.push([
                    load_style_face(&db, &family_query, true, false),
                    load_style_face(&db, &family_query, false, true),
                    load_style_face(&db, &family_query, true, true),
                ]);
                fonts.push(font);
                loaded.push(family.to_string());
            } else if debug_fonts {
                eprintln!("rst: font family not available: {family}");
            }
        }

        // Always include the system monospace face as a final fallback.
        {
            let query = Query {
                families: &[Family::Monospace],
                ..Query::default()
            };
            if let Some(font) = db.query(&query).and_then(|id| {
                db.with_face_data(id, |data, index| {
                    Font::from_bytes(
                        data.to_vec(),
                        FontSettings {
                            collection_index: index,
                            ..FontSettings::default()
                        },
                    )
                    .ok()
                })
                .flatten()
            }) {
                let family_query = [Family::Monospace];
                styled_fonts.push([
                    load_style_face(&db, &family_query, true, false),
                    load_style_face(&db, &family_query, false, true),
                    load_style_face(&db, &family_query, true, true),
                ]);
                fonts.push(font);
                loaded.push("system monospace".to_string());
            }
        }

        if debug_fonts {
            eprintln!("rst: available font chain: {}", loaded.join(" -> "));
        }

        let primary = fonts
            .first()
            .context("no usable monospace font found; install Menlo or a Nerd Font")?;
        let font_size = (config.size as f64 * scale) as f32;
        let (cell_width, cell_height, ascent, text_ink_bottom) =
            font_cell_metrics(primary, font_size);

        Ok(Self {
            fonts,
            styled_fonts,
            glyphs: HashMap::new(),
            missing_glyphs: HashSet::new(),
            debug_fonts,
            font_size,
            cell_width,
            cell_height,
            padding: (padding as f64 * scale) as usize,
            ascent,
            text_ink_bottom,
        })
    }

    /// Resize the already loaded font faces. Rescanning macOS Font Book or
    /// Linux font directories on *every* Cmd+/Cmd- used to stall the UI for
    /// up to a second. Cached rasterized glyphs do need invalidating, but
    /// font discovery and parsing should only happen at startup.
    pub fn set_font_size(&mut self, logical_size: f32, scale: f64) {
        let pixel_size = (logical_size as f64 * scale) as f32;
        if (self.font_size - pixel_size).abs() < f32::EPSILON {
            return;
        }
        let (cell_width, cell_height, ascent, text_ink_bottom) =
            font_cell_metrics(&self.fonts[0], pixel_size);
        self.font_size = pixel_size;
        self.cell_width = cell_width;
        self.cell_height = cell_height;
        self.ascent = ascent;
        self.text_ink_bottom = text_ink_bottom;
        // Previously rasterized pixels belong to a different font size.
        // Keep parsed font faces and fallback order; only discard bitmaps.
        self.glyphs.clear();
    }

    /// Returns bitmap plus whether bold / italic should be synthesized.
    /// Style choice is cached independently to avoid mixing glyph variants.
    fn glyph(&mut self, ch: char, style: TextStyle) -> (&Glyph, bool, bool) {
        let matched = self.fonts.iter().position(|font| font.has_glyph(ch));
        if matched.is_none() && self.debug_fonts && self.missing_glyphs.insert(ch) {
            eprintln!("rst: missing glyph U+{:04X} ({:?}) in configured fonts", ch as u32, ch);
        }
        let index = matched.unwrap_or(0);
        let bold = style.contains(TextStyle::BOLD);
        let italic = style.contains(TextStyle::ITALIC);
        let candidate_variants: &[u8] = match (bold, italic) {
            (true, true) => &[3, 2, 1],
            (true, false) => &[1],
            (false, true) => &[2],
            (false, false) => &[],
        };
        let variant = candidate_variants
            .iter()
            .copied()
            .find(|&variant| {
                self.styled_fonts[index][variant as usize - 1]
                    .as_ref()
                    .is_some_and(|font| font.has_glyph(ch))
            })
            .unwrap_or(0);
        let face = if variant == 0 {
            &self.fonts[index]
        } else {
            self.styled_fonts[index][variant as usize - 1]
                .as_ref()
                .expect("style variant was checked")
        };
        let synth_bold = bold && variant != 1 && variant != 3;
        let synth_italic = italic && variant != 2 && variant != 3;
        let size = self.font_size;
        let glyph = self.glyphs.entry((index, variant, ch)).or_insert_with(|| {
            let (metrics, bitmap) = face.rasterize(ch, size);
            Glyph { metrics, bitmap }
        });
        (glyph, synth_bold, synth_italic)
    }

    /// Draw all backgrounds first, then all glyphs. Painting each cell's
    /// background after its predecessor's glyph used to erase Nerd Font
    /// overhangs and Powerline separators.
    pub fn draw(
        &mut self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        screen: &Screen,
        selection: Option<((usize, usize), (usize, usize))>,
    ) {
        paint_backgrounds(
            pixels,
            width,
            height,
            screen,
            self.cell_width,
            self.cell_height,
            self.padding,
            selection,
        );

        for row in 0..screen.rows {
            let y = self.padding + row * self.cell_height;
            if y >= height {
                break;
            }
            for (col, &cell) in screen.display_row(row).iter().enumerate() {
                if cell.width == 0 {
                    continue;
                }
                let x = self.padding + col * self.cell_width;
                if x >= width {
                    break;
                }
                let (_, bg) = cell_colors(screen, row, col, cell, selection);
                rect(
                    pixels,
                    width,
                    height,
                    x,
                    y,
                    self.cell_width * cell.width as usize,
                    self.cell_height,
                    bg,
                );
            }
        }

        for row in 0..screen.rows {
            let y = self.padding + row * self.cell_height;
            if y >= height {
                break;
            }
            for (col, &cell) in screen.display_row(row).iter().enumerate() {
                if cell.width == 0 {
                    continue;
                }
                let x = self.padding + col * self.cell_width;
                if x >= width {
                    break;
                }
                if !cell.style.visible(screen.text_blink_on) {
                    continue;
                }
                let (fg, bg) = cell_colors(screen, row, col, cell, selection);
                let ink = effective_ink_color(cell.style, fg, bg);
                if cell.ch != ' ' {
                    let spill = icon_spill(screen, row, col, cell, selection, self.cell_width);
                    self.draw_glyph(pixels, width, height, x, y, cell, ink, spill);
                }
                // Underline and strikethrough continue through spaces, just
                // like st. Draw after glyphs so the decoration is visible.
                paint_decorations(
                    pixels, width, height, x, y,
                    self.cell_width * cell.width as usize,
                    self.cell_height, self.ascent, self.font_size, cell.style, ink,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_glyph(
        &mut self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        x: usize,
        y: usize,
        cell: Cell,
        fg: u32,
        spill: (usize, usize),
    ) {
        let baseline = self.ascent
            + (self.cell_height as i32 - self.font_size.ceil() as i32).max(0) / 2;
        let slot_w = self.cell_width * cell.width as usize;
        let slot_h = self.cell_height;
        let text_ink_bottom = self.text_ink_bottom;
        // The four classic Powerline separators are font-independent vector
        // shapes. This avoids the one-pixel seams of font rasterization.
        if matches!(cell.ch as u32, 0xe0b0..=0xe0b3) {
            draw_powerline(pixels, width, height, x, y, slot_w, slot_h, cell.ch, fg);
            return;
        }
        let (glyph, synthetic_bold, synthetic_italic) = self.glyph(cell.ch, cell.style);
        if glyph.metrics.width == 0 || glyph.metrics.height == 0 {
            return;
        }
        // The terminal grid, not the font's reported advance, owns cell width.
        // Oversized Nerd Font symbols are fitted rather than silently cut off.
        let icon = is_nerd_icon(cell.ch);
        let ink = if icon { visible_ink_bounds(glyph) } else { None };
        let placement = if let Some(bounds) = ink {
            place_icon(bounds, x, y, slot_w, slot_h, spill, text_ink_bottom)
        } else {
            place_glyph(
                &glyph.metrics,
                x,
                y,
                slot_w,
                slot_h,
                baseline,
                is_powerline_separator(cell.ch),
            )
        };
        for dy in 0..placement.height {
            let target_y = placement.y + dy;
            if target_y >= height {
                break;
            }
            for dx in 0..placement.width {
                // Prefer true font variants; if the family has no Italic
                // face, lightly shear the bitmap without touching the cell
                // geometry or the fixed-width terminal grid.
                let slant = if synthetic_italic && ink.is_none() {
                    (placement.height.saturating_sub(dy + 1) / 7).min(3)
                } else {
                    0
                };
                let target_x = placement.x + dx + slant;
                if target_x >= width || (synthetic_italic && target_x >= x + slot_w) {
                    continue;
                }
                let alpha = match ink {
                    Some(bounds) => sample_glyph_rect(
                        glyph, bounds, dx, dy, placement.width, placement.height,
                    ),
                    None => sample_glyph(glyph, dx, dy, placement.width, placement.height),
                };
                if alpha == 0 {
                    continue;
                }
                let index = target_y * width + target_x;
                // Blend against the real framebuffer pixel so an overhanging
                // glyph cannot accidentally re-introduce its own background.
                pixels[index] = blend(fg, pixels[index], alpha);
                if synthetic_bold && target_x + 1 < width && target_x + 1 < x + slot_w {
                    // A one-pixel emboldening pass when no real bold face
                    // exists. Keep the extra stroke inside the cell.
                    let next = index + 1;
                    pixels[next] = blend(fg, pixels[next], alpha);
                }
            }
        }
    }
}


/// Faint is drawn by blending foreground into the *effective* background,
/// including selection or cursor highlighting; no opaque dark overlay.
fn effective_ink_color(style: TextStyle, fg: u32, bg: u32) -> u32 {
    if style.contains(TextStyle::FAINT) {
        blend(fg, bg, 128)
    } else {
        fg
    }
}

/// Decorations are independent of the glyph bitmap. In particular underlines
/// must continue across spaces and remain visible on missing-glyph cells.
#[allow(clippy::too_many_arguments)]
fn paint_decorations(
    pixels: &mut [u32],
    stride: usize,
    height: usize,
    x: usize,
    y: usize,
    width: usize,
    cell_height: usize,
    ascent: i32,
    font_size: f32,
    style: TextStyle,
    color: u32,
) {
    if cell_height == 0 || width == 0 || (!style.contains(TextStyle::UNDERLINE)
        && !style.contains(TextStyle::STRUCK))
    {
        return;
    }
    let thickness = ((font_size / 18.0).round() as usize).clamp(1, 3).min(cell_height);
    let vertical_offset = ((cell_height as i32 - font_size.ceil() as i32).max(0)) / 2;
    let baseline = (ascent + vertical_offset).clamp(0, cell_height as i32 - 1);
    let max_y = cell_height - thickness;
    if style.contains(TextStyle::UNDERLINE) {
        let at = (baseline + 1).max(0) as usize;
        rect(pixels, stride, height, x, y + at.min(max_y), width, thickness, color);
    }
    if style.contains(TextStyle::STRUCK) {
        let at = (baseline - ascent / 3).max(0) as usize;
        rect(pixels, stride, height, x, y + at.min(max_y), width, thickness, color);
    }
}

/// Cell geometry derived from the primary face for a chosen pixel size.
/// Used identically at initialization and when changing the font size.
fn font_cell_metrics(font: &Font, font_size: f32) -> (usize, usize, i32, i32) {
    let line = font.horizontal_line_metrics(font_size);
    let cell_width = font.metrics('M', font_size).advance_width.ceil().max(1.0) as usize;
    let cell_height = line
        .map(|line| line.new_line_size.ceil().max(font_size * 1.2) as usize)
        .unwrap_or((font_size * 1.35).ceil() as usize)
        .max(1);
    let ascent = line
        .map(|line| line.ascent.ceil() as i32)
        .unwrap_or(font_size.ceil() as i32);
    // The visual bottom of lowercase 'x' also anchors Nerd Font icons.
    let text_ink_bottom = ascent
        + (cell_height as i32 - font_size.ceil() as i32).max(0) / 2
        - font.metrics('x', font_size).ymin;
    (cell_width, cell_height, ascent, text_ink_bottom)
}

#[derive(Debug, PartialEq, Eq)]
struct Placement {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

/// Standard Nerd Font icons live in Unicode's private-use ranges. Do not
/// apply icon layout to Powerline drawing characters: those need to align
/// with the cell edges, not be optically centered.
fn is_nerd_icon(ch: char) -> bool {
    let point = ch as u32;
    (0xe000..=0xf8ff).contains(&point) && !(0xe0a0..=0xe0d7).contains(&point)
        || (0xf0000..=0xffffd).contains(&point)
        || (0x100000..=0x10fffd).contains(&point)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InkBounds {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

/// Some patched fonts carry large transparent bearings around their icons.
/// Use the visible alpha coverage, not the font's nominal glyph rectangle.
fn visible_ink_bounds(glyph: &Glyph) -> Option<InkBounds> {
    let w = glyph.metrics.width;
    let h = glyph.metrics.height;
    if w == 0 || h == 0 || glyph.bitmap.len() < w * h {
        return None;
    }
    let mut left = w;
    let mut top = h;
    let mut right = 0;
    let mut bottom = 0;
    let mut found = false;
    for y in 0..h {
        for x in 0..w {
            if glyph.bitmap[y * w + x] > 12 {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x);
                bottom = bottom.max(y);
                found = true;
            }
        }
    }
    found.then_some(InkBounds {
        x: left,
        y: top,
        width: right.saturating_sub(left) + 1,
        height: bottom.saturating_sub(top) + 1,
    })
}

/// An icon can use a little of a neighboring empty cell, but never draw
/// over adjacent letters, cursor cells or different background colors.
fn icon_spill(
    screen: &Screen,
    row: usize,
    col: usize,
    cell: Cell,
    selection: Option<((usize, usize), (usize, usize))>,
    cell_width: usize,
) -> (usize, usize) {
    if !is_nerd_icon(cell.ch) {
        return (0, 0);
    }
    let line = screen.display_row(row);
    let source_bg = cell_colors(screen, row, col, cell, selection).1;
    let free = |index: usize| {
        line.get(index).is_some_and(|&neighbor| {
            neighbor.ch == ' '
                && neighbor.width == 1
                && cell_colors(screen, row, index, neighbor, selection).1 == source_bg
        })
    };
    let spill = (cell_width / 6).max(1);
    (
        if col > 0 && free(col - 1) { spill } else { 0 },
        if col + 1 < screen.cols && free(col + 1) { spill } else { 0 },
    )
}

/// Nerd Font icons should sit on the visible lower edge of normal text,
/// rather than in the vertical centre of the full grid cell. Font bearings
/// vary between Nerd Fonts, so anchor only the rasterized ink rectangle.
/// Keep their aspect ratio and allow a modest scale-up, within safe bounds.
fn place_icon(
    ink: InkBounds,
    x: usize,
    y: usize,
    cell_width: usize,
    cell_height: usize,
    spill: (usize, usize),
    text_ink_bottom: i32,
) -> Placement {
    let available_width = cell_width + spill.0 + spill.1;
    let available_height = ((cell_height as f32 * 0.84).round() as usize)
        .clamp(1, cell_height);
    let ratio = (available_width as f64 / ink.width as f64)
        .min(available_height as f64 / ink.height as f64)
        .min(1.6);
    let width = ((ink.width as f64 * ratio).round() as usize).clamp(1, available_width);
    let height = ((ink.height as f64 * ratio).round() as usize).clamp(1, available_height);
    let left_limit = x.saturating_sub(spill.0);
    let right_limit = x.saturating_add(cell_width).saturating_add(spill.1);
    let ideal_x = x as i64 + (cell_width as i64 - width as i64) / 2;
    let actual_x = ideal_x.clamp(left_limit as i64, right_limit.saturating_sub(width) as i64);
    // text_ink_bottom is relative to the row origin and exclusive:
    // regular text and the icon now share a common visible bottom edge.
    // Clamp to the cell to prevent overshooting into other rows.
    let ideal_y = text_ink_bottom - height as i32;
    let actual_y = ideal_y.clamp(0, (cell_height - height) as i32) as usize;
    Placement {
        x: actual_x as usize,
        y: y + actual_y,
        width,
        height,
    }
}

fn sample_glyph_rect(
    glyph: &Glyph,
    ink: InkBounds,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> u8 {
    let sx = ((x as f32 + 0.5) * ink.width as f32 / width as f32 - 0.5)
        .clamp(0.0, (ink.width - 1) as f32) + ink.x as f32;
    let sy = ((y as f32 + 0.5) * ink.height as f32 / height as f32 - 0.5)
        .clamp(0.0, (ink.height - 1) as f32) + ink.y as f32;
    let x0 = sx.floor() as usize;
    let y0 = sy.floor() as usize;
    let x1 = (x0 + 1).min(ink.x + ink.width - 1);
    let y1 = (y0 + 1).min(ink.y + ink.height - 1);
    let tx = sx - x0 as f32;
    let ty = sy - y0 as f32;
    let stride = glyph.metrics.width;
    let row0 = glyph.bitmap[y0 * stride + x0] as f32 * (1.0 - tx)
        + glyph.bitmap[y0 * stride + x1] as f32 * tx;
    let row1 = glyph.bitmap[y1 * stride + x0] as f32 * (1.0 - tx)
        + glyph.bitmap[y1 * stride + x1] as f32 * tx;
    (row0 * (1.0 - ty) + row1 * ty).round() as u8
}

fn is_powerline_separator(ch: char) -> bool {
    matches!(ch as u32, 0xe0b0..=0xe0b3)
}

/// Return a bitmap rectangle that always fits inside its assigned terminal
/// cell(s). Translating a regular glyph is preferred over shrinking it.
/// Wide icons are scaled uniformly and Powerline separators fill their cell.
fn place_glyph(
    metrics: &Metrics,
    x: usize,
    y: usize,
    slot_w: usize,
    slot_h: usize,
    baseline: i32,
    powerline: bool,
) -> Placement {
    let source_w = metrics.width;
    let source_h = metrics.height;
    if powerline {
        return Placement {
            x,
            y,
            width: slot_w,
            height: slot_h,
        };
    }
    if source_w > slot_w || source_h > slot_h {
        let factor = (slot_w as f64 / source_w as f64)
            .min(slot_h as f64 / source_h as f64);
        let fitted_w = ((source_w as f64 * factor).floor() as usize).clamp(1, slot_w);
        let fitted_h = ((source_h as f64 * factor).floor() as usize).clamp(1, slot_h);
        return Placement {
            x: x + (slot_w - fitted_w) / 2,
            y: y + (slot_h - fitted_h) / 2,
            width: fitted_w,
            height: fitted_h,
        };
    }
    let desired_x = x as i64 + metrics.xmin as i64;
    let desired_y = y as i64 + baseline as i64
        - metrics.ymin as i64 - source_h as i64;
    Placement {
        x: desired_x.clamp(x as i64, (x + slot_w - source_w) as i64) as usize,
        y: desired_y.clamp(y as i64, (y + slot_h - source_h) as i64) as usize,
        width: source_w,
        height: source_h,
    }
}

/// Four standard Powerline separators rendered edge-to-edge, independent of
/// font metrics. Four samples per axis provide inexpensive antialiasing.
#[allow(clippy::too_many_arguments)]
fn draw_powerline(
    pixels: &mut [u32],
    stride: usize,
    height: usize,
    x: usize,
    y: usize,
    cell_width: usize,
    cell_height: usize,
    symbol: char,
    color: u32,
) {
    for dy in 0..cell_height {
        if y + dy >= height {
            break;
        }
        for dx in 0..cell_width {
            if x + dx >= stride {
                break;
            }
            let mut filled = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = (dx as f32 + (sx as f32 + 0.5) / 4.0) / cell_width as f32;
                    let fy = (dy as f32 + (sy as f32 + 0.5) / 4.0) / cell_height as f32;
                    let edge = 1.0 - (2.0 * fy - 1.0).abs();
                    let stroke = 0.9 / cell_width as f32;
                    let inside = match symbol as u32 {
                        0xe0b0 => fx <= edge,
                        0xe0b1 => (fx - edge).abs() <= stroke,
                        0xe0b2 => fx >= 1.0 - edge,
                        0xe0b3 => (fx - (1.0 - edge)).abs() <= stroke,
                        _ => false,
                    };
                    if inside {
                        filled += 1;
                    }
                }
            }
            if filled > 0 {
                let idx = (y + dy) * stride + x + dx;
                let alpha = (filled * 255 / 16) as u8;
                pixels[idx] = blend(color, pixels[idx], alpha);
            }
        }
    }
}

fn sample_glyph(glyph: &Glyph, x: usize, y: usize, dest_w: usize, dest_h: usize) -> u8 {
    let src_w = glyph.metrics.width;
    let src_h = glyph.metrics.height;
    if src_w == dest_w && src_h == dest_h {
        return glyph.bitmap[y * src_w + x];
    }
    // Bilinear sampling keeps diagonals smooth when a large icon is fitted
    // to a single cell, and when Powerline arrows fill the entire row height.
    let sx = ((x as f32 + 0.5) * src_w as f32 / dest_w as f32 - 0.5)
        .clamp(0.0, (src_w - 1) as f32);
    let sy = ((y as f32 + 0.5) * src_h as f32 / dest_h as f32 - 0.5)
        .clamp(0.0, (src_h - 1) as f32);
    let x0 = sx.floor() as usize;
    let y0 = sy.floor() as usize;
    let x1 = (x0 + 1).min(src_w - 1);
    let y1 = (y0 + 1).min(src_h - 1);
    let wx = sx - x0 as f32;
    let wy = sy - y0 as f32;
    let top = (glyph.bitmap[y0 * src_w + x0] as f32) * (1.0 - wx)
        + (glyph.bitmap[y0 * src_w + x1] as f32) * wx;
    let bottom = (glyph.bitmap[y1 * src_w + x0] as f32) * (1.0 - wx)
        + (glyph.bitmap[y1 * src_w + x1] as f32) * wx;
    (top * (1.0 - wy) + bottom * wy).round() as u8
}

/// Paint the complete terminal area in grid-cell colors, extending the
/// final column and final row into fractional unused window pixels.
///
/// Font metrics cannot necessarily divide the window size exactly; leaving
/// the remainder at the default palette color creates a visible frame around
/// full-screen TUIs such as Neovim. With padding = 0 there is no gutter:
/// application-provided cell colors reach every window edge.
///
/// An explicit nonzero padding is still respected as a user preference.
#[allow(clippy::too_many_arguments)]
fn paint_backgrounds(
    pixels: &mut [u32],
    width: usize,
    height: usize,
    screen: &Screen,
    cell_width: usize,
    cell_height: usize,
    padding: usize,
    selection: Option<((usize, usize), (usize, usize))>,
) {
    pixels.fill(screen.palette.background);
    if width == 0 || height == 0 || cell_width == 0 || cell_height == 0 {
        return;
    }

    for row in 0..screen.rows {
        let y = padding.saturating_add(row.saturating_mul(cell_height));
        if y >= height {
            break;
        }
        // Extend the first and last rows' backgrounds to the physical
        // edges. Padding moves only *text*, never introduces a separate
        // contrasting stripe around Neovim or other full-screen TUIs.
        let start_y = if row == 0 { 0 } else { y };
        let end_y = if row + 1 == screen.rows {
            height
        } else {
            y.saturating_add(cell_height).min(height)
        };
        for (col, &cell) in screen.display_row(row).iter().enumerate() {
            if cell.width == 0 {
                continue;
            }
            let x = padding.saturating_add(col.saturating_mul(cell_width));
            if x >= width {
                break;
            }
            let (_, background) = cell_colors(screen, row, col, cell, selection);
            let start_x = if col == 0 { 0 } else { x };
            let end_x = if col + cell.width as usize >= screen.cols {
                width
            } else {
                x.saturating_add(cell_width.saturating_mul(cell.width as usize))
                    .min(width)
            };
            rect(
                pixels,
                width,
                height,
                start_x,
                start_y,
                end_x.saturating_sub(start_x),
                end_y.saturating_sub(start_y),
                background,
            );
        }
    }
}

fn cell_colors(
    screen: &Screen,
    row: usize,
    col: usize,
    cell: Cell,
    selection: Option<((usize, usize), (usize, usize))>,
) -> (u32, u32) {
    let selected = selection.is_some_and(|(a, b)| {
        let (start, end) = if (a.1, a.0) <= (b.1, b.0) {
            (a, b)
        } else {
            (b, a)
        };
        (row, col) >= (start.1, start.0) && (row, col) <= (end.1, end.0)
    });
    let cursor = screen.show_cursor
        && screen.cursor_blink_on
        && screen.view_offset == 0
        && row == screen.y
        && col == screen.x.min(screen.cols - 1);
    if cursor {
        (screen.palette.background, screen.palette.cursor)
    } else if selected {
        (cell.fg, screen.palette.selection)
    } else {
        (cell.fg, cell.bg)
    }
}

fn rect(
    pixels: &mut [u32],
    stride: usize,
    height: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    color: u32,
) {
    for py in y..y.saturating_add(h).min(height) {
        let start = py * stride + x.min(stride);
        let end = py * stride + x.saturating_add(w).min(stride);
        pixels[start..end].fill(color);
    }
}

fn blend(fg: u32, bg: u32, alpha: u8) -> u32 {
    let a = alpha as u32;
    let mix = |shift: u32| {
        let f = (fg >> shift) & 0xff;
        let b = (bg >> shift) & 0xff;
        (f * a + b * (255 - a)) / 255
    };
    (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

#[cfg(test)]
mod tests {
    use super::*;


    fn test_screen() -> Screen {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        // Background-only tests must not accidentally paint the initial
        // visible cursor at (0, 0) over the fixture's cell background.
        let mut screen = Screen::new(2, 2, 0, palette);
        screen.show_cursor = false;
        screen
    }

    #[test]
    fn cursor_background_overrides_the_cell_background_when_visible() {
        let mut screen = test_screen();
        screen.show_cursor = true;
        screen.grid[0][0].bg = 0x111111;
        let mut pixels = vec![0; 13 * 10];
        paint_backgrounds(&mut pixels, 13, 10, &screen, 5, 4, 0, None);
        assert_eq!(pixels[0], screen.palette.cursor);
        // Only the actual cursor cell should use the cursor color.
        assert_eq!(pixels[5], screen.palette.background);
    }

    #[test]
    fn full_screen_background_has_no_unpainted_edge_pixels() {
        let mut screen = test_screen();
        screen.grid[0][0].bg = 0x111111;
        screen.grid[0][1].bg = 0x222222;
        screen.grid[1][0].bg = 0x333333;
        screen.grid[1][1].bg = 0x444444;

        // A 2 x 2 grid of 5 x 4 cells, with 3 extra horizontal pixels
        // and 2 extra vertical pixels from the window manager.
        let (width, height) = (13, 10);
        let mut pixels = vec![0; width * height];
        paint_backgrounds(&mut pixels, width, height, &screen, 5, 4, 0, None);

        assert_eq!(pixels[0], 0x111111); // top-left
        assert_eq!(pixels[12], 0x222222); // right edge, top row
        assert_eq!(pixels[9 * width], 0x333333); // bottom edge, left
        assert_eq!(pixels[9 * width + 12], 0x444444); // bottom-right
        assert_eq!(pixels[5 * width + 12], 0x444444); // right edge, bottom row
    }

    #[test]
    fn small_padding_insets_text_but_backgrounds_reach_each_edge() {
        let mut screen = test_screen();
        screen.grid[0][0].bg = 0x111111;
        screen.grid[0][1].bg = 0x222222;
        screen.grid[1][0].bg = 0x333333;
        screen.grid[1][1].bg = 0x444444;
        let (width, height) = (16, 16);
        let mut pixels = vec![0; width * height];
        paint_backgrounds(&mut pixels, width, height, &screen, 5, 4, 2, None);
        assert_eq!(pixels[0], 0x111111); // top-left gutter follows first cell
        assert_eq!(pixels[2 * width + 2], 0x111111);
        assert_eq!(pixels[width - 1], 0x222222); // right edge
        assert_eq!(pixels[15 * width], 0x333333); // bottom left
        assert_eq!(pixels[15 * width + 15], 0x444444); // bottom right
    }

    #[test]
    fn cursor_blink_phase_hides_only_the_cursor_overlay() {
        let mut screen = test_screen();
        screen.grid[0][0].bg = 0x112233;
        screen.cursor_blink_on = false;
        let mut pixels = vec![0; 13 * 10];
        screen.show_cursor = true;
        paint_backgrounds(&mut pixels, 13, 10, &screen, 5, 4, 0, None);
        assert_eq!(pixels[0], 0x112233);
        screen.cursor_blink_on = true;
        paint_backgrounds(&mut pixels, 13, 10, &screen, 5, 4, 0, None);
        assert_eq!(pixels[0], screen.palette.cursor);
        screen.show_cursor = false;
        paint_backgrounds(&mut pixels, 13, 10, &screen, 5, 4, 0, None);
        assert_eq!(pixels[0], 0x112233);
    }

    #[test]
    fn nerd_icons_exclude_powerline_and_regular_text() {
        assert!(is_nerd_icon('\u{f10fe}')); // Nerd Font Kubernetes icon
        assert!(is_nerd_icon('\u{f013}'));
        assert!(!is_nerd_icon('A'));
        assert!(!is_nerd_icon('\u{e0b0}'));
    }

    #[test]
    fn ink_bounds_ignore_transparent_bearings() {
        let mut metrics = Metrics::default();
        metrics.width = 5;
        metrics.height = 4;
        let mut bitmap = vec![0; 20];
        bitmap[1 * 5 + 2] = 255;
        bitmap[2 * 5 + 3] = 255;
        let glyph = Glyph { metrics, bitmap };
        assert_eq!(visible_ink_bounds(&glyph), Some(InkBounds {
            x: 2, y: 1, width: 2, height: 2,
        }));
    }

    #[test]
    fn icons_align_to_text_ink_bottom_not_cell_center() {
        let bounds = InkBounds { x: 2, y: 4, width: 12, height: 12 };
        let at = place_icon(bounds, 50, 100, 20, 30, (3, 3), 26);
        assert_eq!(at.width, 19); // bounded to 1.6x source ink
        assert_eq!(at.height, 19);
        assert_eq!(at.y, 107); // bottom = 126, matching text lower edge
        assert_eq!(at.y + at.height, 126);
        assert!(at.x >= 47 && at.x + at.width <= 73);
    }

    #[test]
    fn icon_baseline_is_clamped_inside_cell() {
        let bounds = InkBounds { x: 0, y: 0, width: 8, height: 8 };
        let too_high = place_icon(bounds, 5, 10, 20, 25, (0, 0), -10);
        assert_eq!(too_high.y, 10);
        let too_low = place_icon(bounds, 5, 10, 20, 25, (0, 0), 50);
        assert_eq!(too_low.y + too_low.height, 35);
    }

    #[test]
    fn nerd_icon_does_not_overlap_adjacent_text() {
        let mut screen = test_screen();
        screen.grid[0][0].ch = '\u{f10fe}';
        screen.grid[0][1].ch = 'X';
        assert_eq!(icon_spill(&screen, 0, 0, screen.grid[0][0], None, 18), (0, 0));
        screen.grid[0][1].ch = ' ';
        assert_eq!(icon_spill(&screen, 0, 0, screen.grid[0][0], None, 18), (0, 3));
    }

    #[test]
    fn glyph_rect_never_overflows_cell() {
        let mut oversized = Metrics::default();
        oversized.width = 28;
        oversized.height = 45;
        oversized.xmin = -7;
        oversized.ymin = -10;
        let rect = place_glyph(&oversized, 10, 20, 12, 22, 16, false);
        assert!(rect.x >= 10 && rect.x + rect.width <= 22);
        assert!(rect.y >= 20 && rect.y + rect.height <= 42);

        let mut overhanging = Metrics::default();
        overhanging.width = 10;
        overhanging.height = 17;
        overhanging.xmin = -3;
        overhanging.ymin = -9;
        let rect = place_glyph(&overhanging, 10, 20, 12, 22, 16, false);
        assert_eq!(rect.width, 10);
        assert_eq!(rect.x, 10);
        assert!(rect.y >= 20 && rect.y + rect.height <= 42);
    }

    #[test]
    fn powerline_triangle_draws_to_full_cell_height() {
        let mut pixels = vec![0; 12 * 20];
        draw_powerline(&mut pixels, 12, 20, 0, 0, 12, 20, '\u{e0b0}', 0xffffff);
        assert_eq!(pixels[10 * 12], 0xffffff); // center, left edge
        assert!(pixels[10 * 12 + 11] > 0); // center, sharp right tip
        assert_eq!(pixels[0 * 12 + 11], 0); // top right stays transparent
    }

    #[test]
    fn powerline_separator_uses_entire_cell() {
        let metrics = Metrics::default();
        let rect = place_glyph(&metrics, 12, 30, 13, 24, 18, true);
        assert_eq!(rect, Placement { x: 12, y: 30, width: 13, height: 24 });
        assert!(is_powerline_separator('\u{e0b0}'));
    }

    #[test]
    fn font_zoom_reuses_loaded_font_faces_and_clears_glyph_cache() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        // Host font availability varies: this test exercises the actual
        // renderer whenever the machine has a suitable monospace font.
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else {
            return;
        };
        let font_count = renderer.fonts.len();
        let original_height = renderer.cell_height;
        let original_size = renderer.font_size;
        renderer.glyph('M', TextStyle::default());
        assert!(!renderer.glyphs.is_empty());

        renderer.set_font_size(config.font.size + 3.0, 1.0);
        assert_eq!(renderer.fonts.len(), font_count);
        assert!(renderer.font_size > original_size);
        assert!(renderer.cell_height >= original_height);
        assert!(renderer.glyphs.is_empty());

        // Same-size calls should not invalidate freshly rasterized glyphs.
        renderer.glyph('M', TextStyle::default());
        renderer.set_font_size(config.font.size + 3.0, 1.0);
        assert!(!renderer.glyphs.is_empty());
    }

    #[test]
    fn blending_bounds() {
        assert_eq!(blend(0xffffff, 0, 0), 0);
        assert_eq!(blend(0xffffff, 0, 255), 0xffffff);
    }
}

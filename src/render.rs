use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use fontdb::{Database, Family, Query, Style, Weight};
use fontdue::{Font, FontSettings, Metrics};
use unicode_normalization::UnicodeNormalization;
use unicode_width::UnicodeWidthChar;

use crate::{
    config::{FontConfig, TabsConfig},
    copy_feedback::{CopyNotice, NoticeKind},
    ime::{self, Preedit},
    help,
    shaping::{self, FontBytes, ShapedText},
    terminal::{Cell, CursorShape, Screen, TextStyle},
    tabs::{self, BarLayout, TabDisplay},
    selection::Selection,
    url_picker::{self, UrlPicker},
};

const MAX_GLYPH_CACHE_ENTRIES: usize = 4096;

struct Glyph {
    metrics: Metrics,
    bitmap: Vec<u8>,
}

/// Pick a real bold/italic font face when the installed family provides it.
/// If a family has no such face, the draw stage synthesizes the missing
/// attribute rather than silently rendering a regular glyph.
fn load_font_face(db: &Database, id: fontdb::ID) -> Option<(Font, FontBytes)> {
    db.with_face_data(id, |data, index| {
        let bytes: std::sync::Arc<[u8]> = data.into();
        let font = Font::from_bytes(
            bytes.clone(),
            FontSettings {
                collection_index: index,
                ..FontSettings::default()
            },
        ).ok()?;
        Some((font, FontBytes { bytes, face_index: index }))
    }).flatten()
}

// Style faces are discovered at startup, but parsed only on first use.
// Most shells initially draw regular text; eagerly parsing every variant
// delays the first window for fonts that may never be used in that session.
struct LazyFont {
    source: FontBytes,
    parsed: std::sync::OnceLock<Option<Font>>,
}

impl LazyFont {
    fn font(&self) -> Option<&Font> {
        self.parsed.get_or_init(|| Font::from_bytes(
            self.source.bytes.clone(),
            FontSettings { collection_index: self.source.face_index, ..FontSettings::default() },
        ).ok()).as_ref()
    }
}

/// Resolve real bold/italic faces *and their original bytes*. Rustybuzz and
/// fontdue must use the same file/collection index for glyph IDs to agree.
fn load_style_face(
    db: &Database,
    families: &[Family<'_>],
    bold: bool,
    italic: bool,
) -> Option<LazyFont> {
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
    db.with_face_data(id, |data, face_index| LazyFont {
        source: FontBytes { bytes: data.into(), face_index },
        parsed: std::sync::OnceLock::new(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorPatch {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

pub struct Renderer {
    fonts: Vec<Font>,
    font_sources: Vec<FontBytes>,
    // For each regular fallback face: bold, italic, bold-italic variants.
    styled_fonts: Vec<[Option<LazyFont>; 3]>,
    styled_sources: Vec<[Option<FontBytes>; 3]>,
    glyphs: HashMap<(usize, u8, char), Glyph>,
    indexed_glyphs: HashMap<(usize, u8, u16), Glyph>,
    shape_cache: HashMap<(usize, u8, bool, String), ShapedText>,
    ligatures: bool,
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
        let mut font_sources = Vec::new();
        let mut styled_fonts = Vec::new();
        let mut styled_sources = Vec::new();
        let mut loaded = Vec::new();
        let mut seen = HashSet::new();
        let mut seen_faces = HashSet::new();
        let debug_fonts = std::env::var_os("RST_DEBUG_FONTS").is_some();

        for family in std::iter::once(&config.family).chain(config.fallback.iter()) {
            if !seen.insert(family.to_lowercase()) {
                continue;
            }
            let query = Query {
                families: &[Family::Name(family)],
                ..Query::default()
            };
            if let Some((font, source)) = db.query(&query)
                .filter(|id| seen_faces.insert(*id))
                .and_then(|id| load_font_face(&db, id))
            {
                let family_query = [Family::Name(family)];
                let variants = [
                    load_style_face(&db, &family_query, true, false),
                    load_style_face(&db, &family_query, false, true),
                    load_style_face(&db, &family_query, true, true),
                ];
                styled_sources.push(std::array::from_fn(|i| {
                    variants[i].as_ref().map(|font| font.source.clone())
                }));
                styled_fonts.push(variants);
                fonts.push(font);
                font_sources.push(source);
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
            if let Some((font, source)) = db.query(&query)
                .filter(|id| seen_faces.insert(*id))
                .and_then(|id| load_font_face(&db, id))
            {
                let family_query = [Family::Monospace];
                let variants = [
                    load_style_face(&db, &family_query, true, false),
                    load_style_face(&db, &family_query, false, true),
                    load_style_face(&db, &family_query, true, true),
                ];
                styled_sources.push(std::array::from_fn(|i| {
                    variants[i].as_ref().map(|font| font.source.clone())
                }));
                styled_fonts.push(variants);
                fonts.push(font);
                font_sources.push(source);
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
            font_sources,
            styled_fonts,
            styled_sources,
            glyphs: HashMap::new(),
            indexed_glyphs: HashMap::new(),
            shape_cache: HashMap::new(),
            ligatures: config.ligatures,
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
        self.indexed_glyphs.clear();
        self.shape_cache.clear();
    }

    /// Use the same fallback/variant decision for shaping as glyph rasterization.
    fn face_choice(&self, ch: char, style: TextStyle) -> (usize, u8) {
        let index = self.fonts.iter().position(|font| font.has_glyph(ch)).unwrap_or(0);
        let bold = style.contains(TextStyle::BOLD);
        let italic = style.contains(TextStyle::ITALIC);
        let preferred: &[u8] = match (bold, italic) {
            (true, true) => &[3, 2, 1],
            (true, false) => &[1],
            (false, true) => &[2],
            _ => &[],
        };
        let variant = preferred.iter().copied().find(|&candidate| {
            self.styled_fonts[index][candidate as usize - 1]
                .as_ref().and_then(LazyFont::font).is_some_and(|font| font.has_glyph(ch))
        }).unwrap_or(0);
        (index, variant)
    }

    fn font_source(&self, face: (usize, u8)) -> Option<&FontBytes> {
        if face.1 == 0 {
            self.font_sources.get(face.0)
        } else {
            self.styled_sources
                .get(face.0)?
                .get(face.1 as usize - 1)?
                .as_ref()
        }
    }

    /// Rasterize Rustybuzz's substituted glyph IDs with fontdue, using
    /// identical font bytes/face index. Fit advances to the terminal grid:
    /// shaping may substitute and join glyphs but cannot move the cursor or
    /// change the reserved width of the caller's cell span.
    #[allow(clippy::too_many_arguments)]
    fn draw_shaped_text(
        &mut self,
        pixels: &mut [u32],
        stride: usize,
        height: usize,
        x: usize,
        y: usize,
        slot_width: usize,
        text: &str,
        fg: u32,
        style: TextStyle,
        face: (usize, u8),
    ) -> bool {
        if slot_width == 0 || text.is_empty() {
            return false;
        }
        let cache_key = (face.0, face.1, self.ligatures, text.to_owned());
        let shaped = if let Some(hit) = self.shape_cache.get(&cache_key) {
            hit.clone()
        } else {
            let Some(source) = self.font_source(face) else { return false };
            let Some(result) = shaping::shape(source, text, self.ligatures) else {
                return false;
            };
            if self.shape_cache.len() >= 1024 {
                self.shape_cache.clear();
            }
            self.shape_cache.insert(cache_key, result.clone());
            result
        };
        if shaped.total_advance == 0 || shaped.units_per_em <= 0.0
            || shaped.glyphs.iter().any(|glyph| glyph.index == 0)
        {
            // Missing-glyph output must use the legacy font fallback rather
            // than silently making a cluster disappear.
            return false;
        }

        let em_scale = self.font_size / shaped.units_per_em;
        let natural = shaped.total_advance.unsigned_abs() as f32 * em_scale;
        let horizontal = slot_width as f32 / natural;
        // Extremely non-monospace fallback faces should not stretch a
        // single glyph beyond recognition; use legacy rendering instead.
        if !(0.4..=3.0).contains(&horizontal) {
            return false;
        }
        let baseline = self.ascent
            + (self.cell_height as i32 - self.font_size.ceil() as i32).max(0) / 2;
        // Borrow the font array and raster cache as disjoint fields.
        let face_font = if face.1 == 0 {
            &self.fonts[face.0]
        } else {
            self.styled_fonts[face.0][face.1 as usize - 1]
                .as_ref().and_then(LazyFont::font).expect("variant selected from available face")
        };
        if shaped.glyphs.iter().any(|glyph| glyph.index >= face_font.glyph_count()) {
            return false;
        }
        // Some RTL shapers express advances as negative design units.
        // Start those runs at the right edge while preserving grid width.
        let mut pen: i64 = if shaped.total_advance < 0 {
            -shaped.total_advance
        } else {
            0
        };
        for item in &shaped.glyphs {
            let gid = item.index;
            let size = self.font_size;
            if self.indexed_glyphs.len() >= MAX_GLYPH_CACHE_ENTRIES
                && !self.indexed_glyphs.contains_key(&(face.0, face.1, gid))
            {
                self.indexed_glyphs.clear();
            }
            let bitmap = self.indexed_glyphs.entry((face.0, face.1, gid))
                .or_insert_with(|| {
                    let (metrics, bitmap) = face_font.rasterize_indexed(gid, size);
                    Glyph { metrics, bitmap }
                });
            let metrics = &bitmap.metrics;
            let offset_x = ((pen + item.x_offset as i64) as f32 * em_scale * horizontal)
                .round() as i64;
            let px = x as i64 + offset_x + metrics.xmin as i64;
            let py = y as i64 + baseline as i64
                - metrics.ymin as i64 - metrics.height as i64
                - (item.y_offset as f32 * em_scale).round() as i64;
            for dy in 0..metrics.height {
                let ty = py + dy as i64;
                if ty < y as i64 || ty >= y.saturating_add(self.cell_height) as i64
                    || ty < 0 || ty >= height as i64 { continue; }
                for dx in 0..metrics.width {
                    let tx = px + dx as i64;
                    if tx < x as i64 || tx >= x.saturating_add(slot_width) as i64
                        || tx < 0 || tx >= stride as i64 { continue; }
                    let alpha = bitmap.bitmap[dy * metrics.width + dx];
                    if alpha == 0 { continue; }
                    let index = ty as usize * stride + tx as usize;
                    pixels[index] = blend(fg, pixels[index], alpha);
                    if style.contains(TextStyle::BOLD)
                        && face.1 != 1 && face.1 != 3
                        && tx + 1 < stride as i64
                        && tx + 1 < x.saturating_add(slot_width) as i64
                    {
                        pixels[index + 1] = blend(fg, pixels[index + 1], alpha);
                    }
                }
            }
            pen += item.x_advance as i64;
        }
        true
    }

    /// Shape only homogeneous runs. Never cross an SGR/selection/cursor,
    /// fallback-font or Powerline boundary: doing so would move glyph ink
    /// across a TUI's colored cells or break column-based selection.
    #[allow(clippy::too_many_arguments)]
    fn draw_shaped_row(
        &mut self,
        pixels: &mut [u32],
        stride: usize,
        height: usize,
        screen: &Screen,
        row: usize,
        selection: Option<Selection>,
        show_cursor: bool,
    ) -> Vec<bool> {
        let cells = screen.display_row(row);
        let mut covered = vec![false; screen.cols];
        // Most rows (including idle blank rows) contain no shaping work.
        // Avoid font fallback lookups and per-cell String allocations there.
        if !cells.iter().any(|cell| !cell.ch.is_ascii() || cell.combining_len != 0)
            && (!self.ligatures || !cells.windows(2).any(|pair| {
                shaping::ascii_pair_needs_shaping(pair[0].ch as u8, pair[1].ch as u8)
            }))
        {
            return covered;
        }
        let mut col = 0usize;
        while col < screen.cols {
            let start = col;
            let cell = cells[start];
            if cell.width == 0 || cell.ch.is_control()
                || is_nerd_icon(cell.ch) || matches!(cell.ch as u32, 0xe0b0..=0xe0b3)
                || !cell.style.visible(screen.text_blink_on)
            {
                col += 1;
                continue;
            }
            let face = self.face_choice(cell.ch, cell.style);
            let colors = cell_colors_with_cursor(screen, row, start, cell, selection, show_cursor);
            let mut text = String::new();
            let mut end = start;
            while end < screen.cols && end - start < 96 {
                let c = cells[end];
                if c.width == 0 {
                    end += 1;
                    continue;
                }
                if c.ch.is_control() || is_nerd_icon(c.ch)
                    || matches!(c.ch as u32, 0xe0b0..=0xe0b3)
                    || c.style != cell.style
                    || !c.style.visible(screen.text_blink_on)
                    || self.face_choice(c.ch, c.style) != face
                    || cell_colors_with_cursor(screen, row, end, c, selection, show_cursor) != colors
                {
                    break;
                }
                text.push_str(&c.text());
                end += c.width as usize;
            }
            if end > start && shaping::needs_shaping(&text, self.ligatures) {
                let y = self.padding.saturating_add(row.saturating_mul(self.cell_height));
                let ink = effective_ink_color(cell.style, colors.0, colors.1);
                if self.draw_shaped_text(
                    pixels, stride, height,
                    self.padding.saturating_add(start.saturating_mul(self.cell_width)),
                    y, (end - start) * self.cell_width, &text, ink, cell.style, face,
                ) {
                    covered[start..end].fill(true);
                    col = end;
                    continue;
                }
            }
            // No ligature candidate anywhere in this homogeneous run:
            // skip the whole span instead of rescanning its suffix at
            // every column (O(n^2) on plain ASCII terminal screens).
            col = end.max(start + 1);
        }
        covered
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
                    .as_ref().and_then(LazyFont::font)
                    .is_some_and(|font| font.has_glyph(ch))
            })
            .unwrap_or(0);
        let face = if variant == 0 {
            &self.fonts[index]
        } else {
            self.styled_fonts[index][variant as usize - 1]
                .as_ref().and_then(LazyFont::font)
                .expect("style variant was checked")
        };
        let synth_bold = bold && variant != 1 && variant != 3;
        let synth_italic = italic && variant != 2 && variant != 3;
        let size = self.font_size;
        if self.glyphs.len() >= MAX_GLYPH_CACHE_ENTRIES
            && !self.glyphs.contains_key(&(index, variant, ch))
        {
            self.glyphs.clear();
        }
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
        selection: Option<Selection>,
        preedit: Option<&Preedit>,
    ) {
        self.draw_rows(pixels, width, height, screen, selection, preedit, 0..screen.rows);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_rows(
        &mut self, pixels: &mut [u32], width: usize, height: usize,
        screen: &Screen, selection: Option<Selection>, preedit: Option<&Preedit>,
        rows: std::ops::Range<usize>,
    ) {
        paint_background_rows(
            pixels,
            width,
            height,
            screen,
            self.cell_width,
            self.cell_height,
            self.padding,
            selection,
            preedit.is_none(),
            rows.clone(),
        );

        for row in rows.clone() {
            let y = self.padding + row * self.cell_height;
            if y >= height {
                break;
            }
            let shaped = self.draw_shaped_row(
                pixels, width, height, screen, row, selection, preedit.is_none(),
            );
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
                let (fg, bg) = cell_colors_with_cursor(
                    screen, row, col, cell, selection, preedit.is_none(),
                );
                let ink = effective_ink_color(cell.style, fg, bg);
                if cell.ch != ' ' && !shaped[col] {
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

        // Unlike the block cursor, underline and bar must be painted over
        // the completed glyphs. Otherwise the text can hide the thin stroke.
        if preedit.is_none() && rows.contains(&screen.y) {
            paint_cursor(pixels, width, height, screen,
                         self.cell_width, self.cell_height, self.padding);
        }
        if let Some(preedit) = preedit {
            self.draw_preedit(pixels, width, height, screen, preedit);
        }
    }

    /// Extract only pixels changed by the cursor, restoring the cursor-free
    /// framebuffer before returning. Includes any ligature reshaping in its row.
    pub fn cursor_patch(
        &mut self, pixels: &mut [u32], width: usize, height: usize,
        screen: &Screen, selection: Option<Selection>,
    ) -> Option<CursorPatch> {
        if !cursor_is_visible(screen) || screen.y >= screen.rows || width == 0 { return None; }
        let y = self.padding.saturating_add(screen.y.saturating_mul(self.cell_height));
        let bottom = y.saturating_add(self.cell_height).min(height);
        if y >= bottom || pixels.len() < width.checked_mul(height)? { return None; }
        let original = pixels[y * width..bottom * width].to_vec();
        self.draw_rows(pixels, width, height, screen, selection, None, screen.y..screen.y + 1);
        let row = &pixels[y * width..bottom * width];
        let mut left = width;
        let mut right = 0;
        let mut top = bottom - y;
        let mut end = 0;
        for (i, (&on, &off)) in row.iter().zip(&original).enumerate() {
            if on != off {
                left = left.min(i % width);
                right = right.max(i % width + 1);
                top = top.min(i / width);
                end = end.max(i / width + 1);
            }
        }
        let patch = if left < right && top < end {
            let mut data = Vec::with_capacity((right - left) * (end - top));
            for dy in top..end { data.extend_from_slice(&row[dy * width + left..dy * width + right]); }
            Some(CursorPatch { x: left, y: y + top, width: right - left, height: end - top, pixels: data })
        } else { None };
        pixels[y * width..bottom * width].copy_from_slice(&original);
        patch
    }

    /// Paint one optional tab row, reusing the terminal's own rasterizer.
    /// No allocations beyond the tiny active tab snapshot passed in by App.
    /// This never modifies the underlying PTY or screen grids.
    #[allow(clippy::too_many_arguments)] // Explicit pixel geometry and tab state.
    pub fn draw_tab_bar(
        &mut self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        screen: &Screen,
        items: &[TabDisplay],
        active_index: usize,
        config: &TabsConfig,
    ) {
        if !config.enabled || width == 0 || height == 0
            || self.cell_width == 0 || self.cell_height == 0
        {
            return;
        }
        let bar_h = tabs::bar_height(self.cell_height, true).min(height);
        let top = if config.position == "bottom" {
            height.saturating_sub(bar_h)
        } else {
            0
        };
        let color = |text: &str| tabs::parse_color(text).unwrap_or(0x282828);
        let background = color(&config.colors.background);
        let inactive_background = color(&config.colors.inactive_background);
        let foreground = color(&config.colors.foreground);
        let active_background = color(&config.colors.active_background);
        let active_foreground = color(&config.colors.active_foreground);
        let accent = color(&config.colors.accent);
        let unread = color(&config.colors.unread);
        rect(pixels, width, height, 0, top, width, bar_h, background);
        if config.position == "bottom" {
            rect(pixels, width, height, 0, top, width, 1.min(bar_h), accent);
        } else {
            rect(pixels, width, height, 0,
                 top + bar_h.saturating_sub(1), width, 1.min(bar_h), accent);
        }

        let columns = (width.saturating_sub(self.padding * 2) / self.cell_width).max(1);
        let layout = BarLayout::for_width(items.len(), active_index, columns);
        for (slot, index) in (layout.first..layout.first + layout.count).enumerate() {
            let item = &items[index];
            let x = self.padding.saturating_add(
                (1 + slot * layout.slot_columns).saturating_mul(self.cell_width),
            );
            let w = layout.slot_columns.saturating_mul(self.cell_width).saturating_sub(2);
            if x >= width { break; }
            let bg = if item.active { active_background } else { inactive_background };
            let ink = if item.active { active_foreground } else { foreground };
            rect(pixels, width, height, x, top.saturating_add(3),
                 w.min(width - x), bar_h.saturating_sub(5), bg);
            if item.unread {
                rect(pixels, width, height, x,
                     top.saturating_add(3), 3.min(w), bar_h.saturating_sub(5), unread);
            }
            let available = layout.slot_columns.saturating_sub(
                if config.close_buttons && layout.slot_columns >= 7 { 5 } else { 2 }
            );
            let title = if item.title.is_empty() { "shell" } else { &item.title };
            let label = format!("{} {}", index + 1, title);
            self.draw_ui_text(
                pixels, width, height,
                x.saturating_add(self.cell_width), top.saturating_add(3),
                &label, ink, available, screen,
            );
            if config.close_buttons && layout.slot_columns >= 7 {
                let close_x = x.saturating_add(
                    (layout.slot_columns - 2).saturating_mul(self.cell_width),
                );
                self.draw_ui_text(pixels, width, height,
                    close_x, top.saturating_add(3), "×", ink, 1, screen);
            }
        }
        let plus_column = 1 + layout.count * layout.slot_columns;
        if plus_column + 1 < columns {
            let plus_x = self.padding.saturating_add(plus_column * self.cell_width);
            self.draw_ui_text(pixels, width, height,
                plus_x, top.saturating_add(3), "+", accent, 1, screen);
        }
    }

    /// Draw a native-looking, keyboard-driven help panel over the terminal.
    /// The terminal grid remains untouched and text does not enter the PTY.
    pub fn draw_help_overlay(
        &mut self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        screen: &Screen,
        scroll: usize,
        macos: bool,
    ) {
        if width == 0 || height == 0 || self.cell_width == 0 || self.cell_height == 0 {
            return;
        }
        let lines = help::lines(macos);
        let cols = screen.cols.saturating_sub(4).clamp(1, 75);
        let visible = screen.rows.saturating_sub(5).clamp(1, 23);
        let panel_w = (cols + 4).saturating_mul(self.cell_width).min(width);
        let panel_h = (visible + 3).saturating_mul(self.cell_height).min(height);
        let left = (width - panel_w) / 2;
        let top = (height - panel_h) / 2;
        let panel = blend(screen.palette.foreground, screen.palette.background, 26);
        rect(pixels, width, height, left, top, panel_w, panel_h, panel);
        let border = screen.palette.cursor;
        rect(pixels, width, height, left, top, panel_w, 2.min(panel_h), border);
        rect(pixels, width, height, left, top, 2.min(panel_w), panel_h, border);
        rect(pixels, width, height, left + panel_w.saturating_sub(2), top, 2.min(panel_w), panel_h, border);
        rect(pixels, width, height, left, top + panel_h.saturating_sub(2), panel_w, 2.min(panel_h), border);

        let text_x = left + 2 * self.cell_width;
        self.draw_ui_text(pixels, width, height, text_x, top, "rst  /  HELP", border, cols, screen);
        for (i, line) in help::viewport(&lines, scroll, visible).iter().enumerate() {
            let y = top + (i + 1) * self.cell_height;
            if y >= top + panel_h.saturating_sub(self.cell_height) { break; }
            let color = if line.starts_with("  ") { screen.palette.foreground } else { border };
            self.draw_ui_text(pixels, width, height, text_x, y, line, color, cols, screen);
        }
        let footer = if scroll + visible < lines.len() {
            "Up/Down scroll   F1 / Esc close    (more below)"
        } else {
            "F1 / Esc close    Up/Down scroll"
        };
        let bottom = top + panel_h.saturating_sub(self.cell_height);
        self.draw_ui_text(pixels, width, height, text_x, bottom, footer, border, cols, screen);
    }

    /// A scrollable, keyboard-only URL palette rendered over the current
    /// terminal surface. No PTY bytes, scrollback or cell contents change.
    pub fn draw_url_picker_overlay(
        &mut self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        screen: &Screen,
        picker: &UrlPicker,
        history_depth: usize,
    ) {
        if width == 0 || height == 0 || self.cell_width == 0 || self.cell_height == 0 {
            return;
        }
        let cols = screen.cols.saturating_sub(4).clamp(1, 88);
        let visible = url_picker::visible_rows(screen.rows);
        let panel_w = (cols + 4).saturating_mul(self.cell_width).min(width);
        let panel_h = (visible + 6).saturating_mul(self.cell_height).min(height);
        let left = width.saturating_sub(panel_w) / 2;
        let top = height.saturating_sub(panel_h) / 2;
        let background = screen.palette.background;
        let accent = screen.palette.ansi[4];
        let panel = blend(screen.palette.foreground, background, 27);
        let secondary = blend(screen.palette.foreground, panel, 173);

        // Dim the underlying terminal so the selected URL is unmistakable.
        blend_rect(pixels, width, height, 0, 0, width, height, 0x000000, 135);
        blend_rect(pixels, width, height, left.saturating_add(4),
                   top.saturating_add(5), panel_w, panel_h, 0x000000, 95);
        rect(pixels, width, height, left, top, panel_w, panel_h, panel);
        rect(pixels, width, height, left, top, panel_w, 2.min(panel_h), accent);
        rect(pixels, width, height, left, top, 2.min(panel_w), panel_h, accent);
        rect(pixels, width, height, left + panel_w.saturating_sub(2),
             top, 2.min(panel_w), panel_h, accent);
        rect(pixels, width, height, left,
             top + panel_h.saturating_sub(2), panel_w, 2.min(panel_h), accent);

        let text_x = left.saturating_add(2 * self.cell_width);
        let header = format!("rst  /  LINKS   {} found", picker.urls.len());
        self.draw_ui_text(pixels, width, height, text_x, top,
                          &header, accent, cols, screen);
        let scope = format!("Last {history_depth} outputs   ·   newest first   ·   HTTP(S) only");
        self.draw_ui_text(pixels, width, height, text_x,
                          top.saturating_add(self.cell_height),
                          &scope, secondary, cols, screen);

        if picker.urls.is_empty() {
            self.draw_ui_text(pixels, width, height, text_x,
                              top.saturating_add(3 * self.cell_height),
                              "No URLs found in recent command output.",
                              screen.palette.foreground, cols, screen);
        } else {
            // A window resize may change the number of visible rows.
            // Re-clamp the viewport without mutating the picker state.
            let first = picker.scroll
                .max(picker.selected.saturating_add(1).saturating_sub(visible))
                .min(picker.selected)
                .min(picker.urls.len().saturating_sub(visible));
            for (line, index) in (first..picker.urls.len().min(first + visible)).enumerate() {
                let y = top.saturating_add((line + 2) * self.cell_height);
                if y >= top.saturating_add(panel_h.saturating_sub(self.cell_height * 2)) {
                    break;
                }
                let selected = index == picker.selected;
                if selected {
                    rect(pixels, width, height,
                         left.saturating_add(3), y,
                         panel_w.saturating_sub(6), self.cell_height,
                         blend(accent, panel, 64));
                    rect(pixels, width, height,
                         left.saturating_add(3), y,
                         3.min(panel_w), self.cell_height, accent);
                }
                let indicator = if selected { "›" } else { " " };
                let label = format!("{indicator} {:>3}  {}", index + 1, picker.urls[index]);
                // Keep full URLs in the model; draw_ui_text only clips the
                // visual representation to the available terminal cells.
                self.draw_ui_text(pixels, width, height, text_x, y,
                    &label, if selected { screen.palette.foreground } else { secondary },
                    cols, screen);
            }
        }

        let position = if picker.urls.is_empty() {
            "0 / 0".to_owned()
        } else {
            format!("{} / {}", picker.selected + 1, picker.urls.len())
        };
        self.draw_ui_text(pixels, width, height, text_x,
            top.saturating_add((visible + 3) * self.cell_height),
            &format!("Selected: {position}"),
            secondary, cols, screen);
        self.draw_ui_text(pixels, width, height, text_x,
            top.saturating_add((visible + 4) * self.cell_height),
            "j/k  ↑↓ move   PgUp/PgDn page   Enter open   Esc close",
            accent, cols, screen);
    }

    /// Briefly tint ONLY screen cells belonging to the OSC 133 command
    /// whose text was successfully copied. The effect is composited over
    /// pixels, leaving cell colors, cursor, scrollback and selection intact.
    pub fn draw_output_copy_highlight(
        &self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        screen: &Screen,
        output_id: u32,
        opacity: u8,
    ) {
        if output_id == 0 || opacity == 0 || self.cell_width == 0
            || self.cell_height == 0 || width == 0 || height == 0
        {
            return;
        }
        let accent = screen.palette.ansi[2]; // current theme's green
        let wash_alpha = (opacity as u16 * 66 / 255) as u8;
        let edge_alpha = (opacity as u16 * 200 / 255) as u8;
        for row in 0..screen.rows {
            let top = self.padding.saturating_add(row.saturating_mul(self.cell_height));
            if top >= height { break; }
            let cells = screen.display_row(row);
            let mut col = 0usize;
            while col < cells.len() {
                if cells[col].output_id != output_id {
                    col += 1;
                    continue;
                }
                let first = col;
                while col < cells.len() && cells[col].output_id == output_id {
                    col += 1;
                }
                let x = self.padding.saturating_add(first.saturating_mul(self.cell_width));
                let segment_w = (col - first).saturating_mul(self.cell_width);
                blend_rect(
                    pixels, width, height, x, top,
                    segment_w, self.cell_height, accent, wash_alpha,
                );
                // A soft accent at the left of each contiguous output run
                // keeps the selection readable without replacing glyph ink.
                blend_rect(
                    pixels, width, height, x, top,
                    2.min(segment_w), self.cell_height, accent, edge_alpha,
                );
            }
        }
    }

    /// Compact native clipboard confirmation with a soft visual hierarchy.
    /// Both the card and the actual copied output share the same fade clock.
    pub fn draw_copy_notice(
        &mut self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        screen: &Screen,
        notice: &CopyNotice,
        opacity: u8,
    ) {
        if opacity == 0 || width == 0 || height == 0
            || self.cell_width == 0 || self.cell_height == 0
        {
            return;
        }

        let accent = match notice.kind {
            NoticeKind::Copied => screen.palette.ansi[2],
            NoticeKind::Warning => screen.palette.ansi[3],
        };
        let max_cols = screen.cols.saturating_sub(5).clamp(1, 54);
        let panel_w = (max_cols + 4).saturating_mul(self.cell_width).min(width);
        let panel_h = (self.cell_height.saturating_mul(2) + 22).min(height);
        let left = width.saturating_sub(panel_w) / 2;
        let top = height.saturating_sub(panel_h.saturating_add(self.cell_height * 2));
        let panel_opacity = (opacity as u16 * 236 / 255) as u8;

        // A tiny offset shadow and 5-pixel stepped corners suggest a soft
        // floating card without an OS toolkit or changes to terminal data.
        blend_rect(
            pixels, width, height,
            left.saturating_add(3), top.saturating_add(4),
            panel_w, panel_h, 0x000000,
            (opacity as u16 * 65 / 255) as u8,
        );
        let body = screen.palette.background;
        if panel_h > 8 && panel_w > 10 {
            blend_rect(pixels, width, height, left + 5, top, panel_w - 10, panel_h, body, panel_opacity);
            blend_rect(pixels, width, height, left + 2, top + 2, panel_w - 4, panel_h - 4, body, panel_opacity);
        }
        blend_rect(pixels, width, height, left, top + 5.min(panel_h),
                   panel_w, panel_h.saturating_sub(10), body, panel_opacity);
        blend_rect(pixels, width, height, left, top + 5.min(panel_h),
                   3.min(panel_w), panel_h.saturating_sub(10), accent,
                   (opacity as u16 * 216 / 255) as u8);

        let text_x = left.saturating_add(self.cell_width.saturating_mul(2));
        let text_y = top.saturating_add(7);
        let icon = match notice.kind {
            NoticeKind::Copied => "✓",
            NoticeKind::Warning => "!",
        };
        let header = format!("{icon}  {}", notice.title);
        let title_ink = blend(accent, body, opacity);
        let sub_ink = blend(screen.palette.foreground, body, opacity);
        self.draw_ui_text(
            pixels, width, height, text_x, text_y,
            &header, title_ink, max_cols, screen,
        );
        self.draw_ui_text(
            pixels, width, height,
            text_x, text_y.saturating_add(self.cell_height + 2),
            &notice.detail, sub_ink, max_cols, screen,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_ui_text(
        &mut self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        x: usize,
        y: usize,
        value: &str,
        color: u32,
        columns: usize,
        screen: &Screen,
    ) {
        let Some(row) = screen.grid.first() else { return };
        let Some(&prototype) = row.first() else { return };
        for (offset, ch) in value.chars().take(columns).enumerate() {
            if ch == ' ' || ch.is_control() { continue; }
            let glyph_x = x.saturating_add(offset.saturating_mul(self.cell_width));
            if glyph_x >= width { break; }
            let mut cell = prototype;
            cell.ch = ch;
            cell.width = 1;
            cell.style = TextStyle::default();
            cell.combining_len = 0;
            self.draw_glyph(pixels, width, height, glyph_x, y, cell, color, (0, 0));
        }
    }

    /// Render the composition as temporary grouped glyph runs. An Arabic or
    /// Indic word must be shaped as a *run*, not as isolated characters.
    /// Split only at a visual row, style/selection, or fallback-font boundary.
    fn draw_preedit(
        &mut self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        screen: &Screen,
        preedit: &Preedit,
    ) {
        if screen.view_offset != 0 {
            return;
        }
        let layout = ime::layout(preedit, screen.cols, screen.rows, (screen.x, screen.y));
        let cell_w = self.cell_width;
        let cell_h = self.cell_height;
        let padding = self.padding;
        let ascent = self.ascent;
        let font_size = self.font_size;
        let style = TextStyle::UNDERLINE;

        // Backdrops are individual grid rectangles even when the font
        // substitutes multiple characters with one ligature glyph.
        for glyph in &layout.glyphs {
            let x = padding.saturating_add(glyph.col.saturating_mul(cell_w));
            let y = padding.saturating_add(glyph.row.saturating_mul(cell_h));
            let bg = if glyph.selected { screen.palette.cursor }
                else { screen.palette.selection };
            rect(pixels, width, height, x, y, glyph.cells * cell_w, cell_h, bg);
        }

        let glyphs = &layout.glyphs;
        let mut i = 0;
        while i < glyphs.len() {
            let start = i;
            let selected = glyphs[i].selected;
            let row = glyphs[i].row;
            let first = glyphs[i].text.chars().next().unwrap_or(' ');
            let face = self.face_choice(first, style);
            let mut text = String::new();
            let mut next_col = glyphs[i].col;
            while i < glyphs.len()
                && glyphs[i].row == row
                && glyphs[i].selected == selected
                && glyphs[i].col == next_col
                && self.face_choice(glyphs[i].text.chars().next().unwrap_or(' '), style) == face
            {
                text.push_str(&glyphs[i].text);
                next_col += glyphs[i].cells;
                i += 1;
            }
            let x = padding.saturating_add(glyphs[start].col.saturating_mul(cell_w));
            let y = padding.saturating_add(row.saturating_mul(cell_h));
            let slot_width = (next_col - glyphs[start].col) * cell_w;
            let fg = if selected { screen.palette.background }
                else { screen.palette.foreground };
            let shaped = self.draw_shaped_text(
                pixels, width, height, x, y,
                slot_width, &text, fg, style, face,
            );
            if !shaped {
                // Malformed/unavailable OpenType data: retain the previous
                // fontdue/NFC preview path, not a blank composition.
                for glyph in &glyphs[start..i] {
                    let Some(&original) = screen.display_row(glyph.row).get(glyph.col) else {
                        continue;
                    };
                    let mut cell = original;
                    let composed: String = glyph.text.nfc().collect();
                    let mut chars = composed.chars();
                    cell.ch = chars.next().unwrap_or(' ');
                    cell.width = glyph.cells as u8;
                    cell.style = style;
                    let gx = padding.saturating_add(glyph.col.saturating_mul(cell_w));
                    let gy = padding.saturating_add(glyph.row.saturating_mul(cell_h));
                    if cell.ch != ' ' && !cell.ch.is_control() {
                        self.draw_glyph(pixels, width, height, gx, gy, cell, fg, (0, 0));
                    }
                    for mark in chars {
                        if UnicodeWidthChar::width(mark) == Some(0)
                            && !matches!(mark, '\u{200d}' | '\u{fe0f}')
                        {
                            cell.ch = mark;
                            self.draw_glyph(pixels, width, height, gx, gy, cell, fg, (0, 0));
                        }
                    }
                }
            }
            for glyph in &glyphs[start..i] {
                let gx = padding.saturating_add(glyph.col.saturating_mul(cell_w));
                let gy = padding.saturating_add(glyph.row.saturating_mul(cell_h));
                paint_decorations(
                    pixels, width, height, gx, gy,
                    glyph.cells * cell_w, cell_h, ascent, font_size, style, fg,
                );
            }
        }
        if let Some((col, row)) = layout.caret {
            let x = padding.saturating_add(col.saturating_mul(cell_w));
            let y = padding.saturating_add(row.saturating_mul(cell_h));
            rect(pixels, width, height, x, y, 2.min(cell_w), cell_h,
                 screen.palette.cursor);
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
    selection: Option<Selection>,
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
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn paint_backgrounds(
    pixels: &mut [u32],
    width: usize,
    height: usize,
    screen: &Screen,
    cell_width: usize,
    cell_height: usize,
    padding: usize,
    selection: Option<Selection>,
) {
    paint_backgrounds_with_cursor(
        pixels, width, height, screen,
        cell_width, cell_height, padding, selection, true,
    );
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn paint_backgrounds_with_cursor(
    pixels: &mut [u32],
    width: usize,
    height: usize,
    screen: &Screen,
    cell_width: usize,
    cell_height: usize,
    padding: usize,
    selection: Option<Selection>,
    show_cursor: bool,
) {
    paint_background_rows(pixels, width, height, screen, cell_width, cell_height,
        padding, selection, show_cursor, 0..screen.rows);
}

#[allow(clippy::too_many_arguments)]
fn paint_background_rows(
    pixels: &mut [u32], width: usize, height: usize, screen: &Screen,
    cell_width: usize, cell_height: usize, padding: usize,
    selection: Option<Selection>, show_cursor: bool, rows: std::ops::Range<usize>,
) {
    let default_background = if screen.reverse_video {
        screen.palette.foreground
    } else {
        screen.palette.background
    };
    if rows.start == 0 && rows.end == screen.rows {
        pixels.fill(default_background);
    } else {
        let y = if rows.start == 0 { 0 } else { padding.saturating_add(rows.start.saturating_mul(cell_height)) };
        let bottom = if rows.end == screen.rows { height } else { padding.saturating_add(rows.end.saturating_mul(cell_height)).min(height) };
        rect(pixels, width, height, 0, y, width, bottom.saturating_sub(y), default_background);
    }
    if width == 0 || height == 0 || cell_width == 0 || cell_height == 0 {
        return;
    }

    for row in rows.clone() {
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
            // Extend only the *underlying* background to unused window
            // pixels. A block cursor is an overlay, not a terminal-cell
            // background: stretching its color into the bottom/right
            // remainder makes it grow after long output or a resize.
            let (_, background) = cell_colors_with_cursor(
                screen, row, col, cell, selection, false,
            );
            // The initial fill already painted default-colored cells and
            // their padding. Only non-default backgrounds need rectangles.
            if background == default_background { continue; }
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

    // Paint the block cursor strictly inside its grid cell after all cell
    // backgrounds, without tinting fractional window padding.
    if show_cursor && rows.contains(&screen.y) && cursor_is_visible(screen)
        && screen.cursor_style.shape == CursorShape::Block
        && screen.x < screen.cols && screen.y < screen.rows
    {
        let row = screen.y;
        let mut col = screen.x;
        let cells = screen.display_row(row);
        if cells[col].width == 0 && col > 0 {
            col -= 1; // leading cell of a double-width character
        }
        let cell = cells[col];
        let (_, color) = cell_colors_with_cursor(
            screen, row, screen.x, cells[screen.x], selection, true,
        );
        let x = padding.saturating_add(col.saturating_mul(cell_width));
        let y = padding.saturating_add(row.saturating_mul(cell_height));
        rect(pixels, width, height, x, y,
             cell_width.saturating_mul(cell.width.max(1) as usize),
             cell_height, color);
    }
}

/// Cursor visibility is governed by DECTCEM, scrollback and the active
/// blink phase. Steady DECSCUSR styles keep cursor_blink_on set to true.
fn cursor_is_visible(screen: &Screen) -> bool {
    screen.show_cursor && screen.cursor_blink_on && screen.view_offset == 0
}

/// Draw narrow cursor styles after the glyph pass. A block cursor uses the
/// existing per-cell background/foreground treatment in cell_colors().
#[allow(clippy::too_many_arguments)]
fn paint_cursor(
    pixels: &mut [u32],
    stride: usize,
    height: usize,
    screen: &Screen,
    cell_width: usize,
    cell_height: usize,
    padding: usize,
) {
    if !cursor_is_visible(screen) || cell_width == 0 || cell_height == 0
        || screen.rows == 0 || screen.cols == 0
    {
        return;
    }

    let x = padding.saturating_add(screen.x.min(screen.cols - 1).saturating_mul(cell_width));
    let y = padding.saturating_add(screen.y.min(screen.rows - 1).saturating_mul(cell_height));
    let thickness = (cell_width.min(cell_height) / 7).clamp(1, 3);
    let color = if screen.reverse_video {
        screen.palette.background
    } else {
        screen.palette.cursor
    };
    match screen.cursor_style.shape {
        CursorShape::Block => {}
        CursorShape::Underline => rect(
            pixels, stride, height,
            x, y.saturating_add(cell_height.saturating_sub(thickness)),
            cell_width, thickness, color,
        ),
        CursorShape::Bar => rect(
            pixels, stride, height, x, y, thickness, cell_height, color,
        ),
    }
}

fn cell_colors(
    screen: &Screen,
    row: usize,
    col: usize,
    cell: Cell,
    selection: Option<Selection>,
) -> (u32, u32) {
    cell_colors_with_cursor(screen, row, col, cell, selection, true)
}

fn cell_colors_with_cursor(
    screen: &Screen,
    row: usize,
    col: usize,
    cell: Cell,
    selection: Option<Selection>,
    show_cursor: bool,
) -> (u32, u32) {
    let selected = selection.is_some_and(|selection| selection.contains(screen, col, row));
    let cursor = show_cursor && cursor_is_visible(screen)
        && screen.cursor_style.shape == CursorShape::Block
        && row == screen.y
        && col == screen.x.min(screen.cols - 1);
    let colors = if cursor {
        (screen.palette.background, screen.palette.cursor)
    } else if selected {
        (cell.fg, screen.palette.selection)
    } else {
        (cell.fg, cell.bg)
    };
    if screen.reverse_video {
        (colors.1, colors.0)
    } else {
        colors
    }
}

#[allow(clippy::too_many_arguments)] // Pixel rectangle geometry.
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

/// Alpha composite a rectangle over the existing frame, preserving the
/// rendered glyph pixels underneath instead of overwriting the terminal.
#[allow(clippy::too_many_arguments)]
fn blend_rect(
    pixels: &mut [u32],
    stride: usize,
    height: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    color: u32,
    alpha: u8,
) {
    if alpha == 0 || stride == 0 { return; }
    let valid_height = height.min(pixels.len() / stride);
    let x_end = x.saturating_add(w).min(stride);
    if x >= x_end { return; }
    for row in y..y.saturating_add(h).min(valid_height) {
        let start = row * stride + x;
        let end = row * stride + x_end;
        for pixel in &mut pixels[start..end] {
            *pixel = blend(color, *pixel, alpha);
        }
    }
}

/// One-pixel focus outline, clipped to the pane, outside normal text padding.
pub fn paint_pane_focus(pixels: &mut [u32], width: usize, height: usize, pane: crate::panes::Rect, color: u32) {
    if pane.width == 0 || pane.height == 0 { return; }
    rect(pixels, width, height, pane.x, pane.y, pane.width, 1, color);
    rect(pixels, width, height, pane.x, pane.y + pane.height - 1, pane.width, 1, color);
    rect(pixels, width, height, pane.x, pane.y, 1, pane.height, color);
    rect(pixels, width, height, pane.x + pane.width - 1, pane.y, 1, pane.height, color);
}

/// Subtle, short visual BEL indicator. Draw *after* the terminal glyphs;
/// no reflow, font changes, or fullscreen inversion. This is also a usable
/// fallback on Wayland systems without an installed libcanberra sound player.
pub fn paint_bell_border(pixels: &mut [u32], width: usize, height: usize, color: u32) {
    if width == 0 || height == 0 {
        return;
    }
    let height = height.min(pixels.len() / width);
    if height == 0 {
        return;
    }
    let thickness = 2usize.min(width).min(height);
    rect(pixels, width, height, 0, 0, width, thickness, color);
    rect(pixels, width, height, 0, height - thickness, width, thickness, color);
    rect(pixels, width, height, 0, 0, thickness, height, color);
    rect(pixels, width, height, width - thickness, 0, thickness, height, color);
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
    fn block_cursor_at_bottom_right_stays_exactly_one_cell() {
        let mut screen = test_screen();
        screen.show_cursor = true;
        screen.x = 1;
        screen.y = 1;
        screen.grid[1][1].bg = 0x334455;
        let (width, height) = (13, 10); // 2x2 grid of 5x4 + 3x2 remainder
        let mut pixels = vec![0; width * height];
        paint_backgrounds(&mut pixels, width, height, &screen, 5, 4, 0, None);
        assert_eq!(pixels[4 * width + 5], screen.palette.cursor);
        assert_eq!(pixels[7 * width + 9], screen.palette.cursor);
        assert_eq!(pixels[7 * width + 10], 0x334455,
                   "right-edge remainder must keep underlying color");
        assert_eq!(pixels[8 * width + 5], 0x334455,
                   "bottom-edge remainder must keep underlying color");
        assert_eq!(pixels[9 * width + 12], 0x334455);
        screen.cursor_blink_on = false;
        paint_backgrounds(&mut pixels, width, height, &screen, 5, 4, 0, None);
        assert_eq!(pixels[4 * width + 5], 0x334455);
    }

    #[test]
    fn cat_like_output_at_bottom_row_never_stretches_block_cursor() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        let mut terminal = crate::terminal::Terminal::new(2, 2, 2, palette);
        // A long file leaves the cursor at the bottom after scrollback.
        terminal.advance(b"a\r\nb\r\nc\r\n");
        terminal.screen.x = 1;
        terminal.screen.y = 1;
        terminal.screen.show_cursor = true;
        terminal.screen.cursor_blink_on = true;
        terminal.screen.grid[1][1].bg = 0x334455;
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else {
            return; // Some CI machines do not have a usable font.
        };
        let width = renderer.cell_width * 2 + renderer.padding * 2 + 3;
        let height = renderer.cell_height * 2 + renderer.padding * 2 + 3;
        let x = renderer.padding + renderer.cell_width;
        let y = renderer.padding + renderer.cell_height;
        let mut pixels = vec![0u32; width * height];
        renderer.draw(&mut pixels, width, height, &terminal.screen, None, None);
        assert_eq!(pixels[y * width + x], terminal.screen.palette.cursor);
        assert_eq!(
            pixels[(y + renderer.cell_height) * width + x],
            0x334455,
            "cursor color must end at the exact bottom of its terminal cell",
        );
        assert_eq!(
            pixels[y * width + x + renderer.cell_width],
            0x334455,
            "cursor color must end at the exact right edge of its terminal cell",
        );
    }

    #[test]
    fn block_cursor_does_not_fill_window_gutters_when_padding_is_nonzero() {
        let mut screen = test_screen();
        screen.show_cursor = true;
        screen.x = 0;
        screen.y = 0;
        screen.grid[0][0].bg = 0x223344;
        let (width, height) = (16, 16);
        let mut pixels = vec![0; width * height];
        paint_backgrounds(&mut pixels, width, height, &screen, 5, 4, 2, None);
        assert_eq!(pixels[0], 0x223344, "outer padding is not part of cursor");
        assert_eq!(pixels[2 * width + 2], screen.palette.cursor);
        assert_eq!(pixels[5 * width + 6], screen.palette.cursor);
        assert_eq!(pixels[5 * width + 7], screen.palette.background);
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
    fn dec_reverse_video_swaps_effective_screen_colors() {
        let mut screen = test_screen();
        screen.grid[0][0].fg = 0xaabbcc;
        screen.grid[0][0].bg = 0x112233;
        assert_eq!(cell_colors(&screen, 0, 0, screen.grid[0][0], None), (0xaabbcc, 0x112233));
        screen.reverse_video = true;
        assert_eq!(cell_colors(&screen, 0, 0, screen.grid[0][0], None), (0x112233, 0xaabbcc));
        // Cursor and selection overlays also reverse, not just stored glyphs.
        screen.show_cursor = true;
        assert_eq!(
            cell_colors(&screen, 0, 0, screen.grid[0][0], None),
            (screen.palette.cursor, screen.palette.background)
        );
        let mut pixels = vec![0u32; 13 * 10];
        screen.show_cursor = false;
        paint_backgrounds(&mut pixels, 13, 10, &screen, 5, 4, 0, None);
        assert_eq!(pixels[0], 0xaabbcc); // reverse video in the first cell
        assert_eq!(pixels[5], screen.palette.foreground); // default cell swapped
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
        let metrics = Metrics { width: 5, height: 4, ..Metrics::default() };
        let mut bitmap = vec![0; 20];
        bitmap[5 + 2] = 255;
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
        let oversized = Metrics { width: 28, height: 45, xmin: -7, ymin: -10, ..Metrics::default() };
        let rect = place_glyph(&oversized, 10, 20, 12, 22, 16, false);
        assert!(rect.x >= 10 && rect.x + rect.width <= 22);
        assert!(rect.y >= 20 && rect.y + rect.height <= 42);

        let overhanging = Metrics { width: 10, height: 17, xmin: -3, ymin: -9, ..Metrics::default() };
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
        assert_eq!(pixels[11], 0); // top right stays transparent
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

    fn style_from_sgr(sequence: &[u8]) -> TextStyle {
        let fixture = test_screen();
        let mut term = crate::terminal::Terminal::new(2, 2, 0, fixture.palette);
        term.advance(sequence);
        term.screen.grid[0][0].style
    }

    #[test]
    fn faint_text_blends_foreground_with_effective_background() {
        let faint = style_from_sgr(b"\x1b[2mX");
        assert_eq!(effective_ink_color(faint, 0xffffff, 0), blend(0xffffff, 0, 128));
        let plain = style_from_sgr(b"X");
        assert_eq!(effective_ink_color(plain, 0xffffff, 0), 0xffffff);
        assert_ne!(
            effective_ink_color(faint, 0xffffff, 0x112233),
            effective_ink_color(faint, 0xffffff, 0),
        );
    }

    #[test]
    fn underline_and_strike_draw_independent_horizontal_rules() {
        let style = style_from_sgr(b"\x1b[4;9mX");
        let mut pixels = vec![0u32; 16 * 16];
        paint_decorations(
            &mut pixels, 16, 16, 0, 0, 8, 12, 9, 12.0, style, 0xabcdef,
        );
        // Baseline at 9; strike at 9 - 9/3 = 6, underline at 10.
        for col in 0..8 {
            assert_eq!(pixels[6 * 16 + col], 0xabcdef);
            assert_eq!(pixels[10 * 16 + col], 0xabcdef);
        }
        assert_eq!(pixels[5 * 16 + 3], 0);
        assert_eq!(pixels[11 * 16 + 3], 0);
        assert_eq!(pixels[6 * 16 + 8], 0);
    }

    #[test]
    fn decorations_are_clipped_to_their_own_cell() {
        let style = style_from_sgr(b"\x1b[4;9mX");
        let mut pixels = vec![0u32; 11 * 7];
        paint_decorations(&mut pixels, 11, 7, 8, 3, 6, 4, 50, 28.0, style, 0xff00ff);
        assert!(pixels.contains(&0xff00ff));
        for row in 0..7 {
            assert!(pixels[row * 11..row * 11 + 8].iter().all(|&pixel| pixel == 0));
        }
    }

    #[test]
    fn hidden_and_blinking_attributes_suppress_glyph_and_decoration() {
        let hidden = style_from_sgr(b"\x1b[8;4mX");
        let blink = style_from_sgr(b"\x1b[5;9mX");
        assert!(!hidden.visible(true));
        assert!(!hidden.visible(false));
        assert!(blink.visible(true));
        assert!(!blink.visible(false));
    }

    #[test]
    fn style_fonts_are_lazy_and_glyph_cache_stays_bounded() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else { return; };
        for face in renderer.styled_fonts.iter().flatten().flatten() {
            assert!(face.parsed.get().is_none());
        }
        if let Some(face) = renderer.styled_fonts.iter().flatten().flatten().next() {
            let first = face.font().map(|font| font as *const Font);
            assert!(face.parsed.get().is_some());
            assert_eq!(first, face.font().map(|font| font as *const Font));
        }
        for ch in (0..MAX_GLYPH_CACHE_ENTRIES as u32).filter_map(char::from_u32) {
            renderer.glyphs.insert((usize::MAX, 0, ch), Glyph {
                metrics: Metrics::default(), bitmap: vec![],
            });
        }
        assert_eq!(renderer.glyphs.len(), MAX_GLYPH_CACHE_ENTRIES);
        renderer.glyph('A', TextStyle::default());
        assert_eq!(renderer.glyphs.len(), 1);
        renderer.glyph('A', TextStyle::default());
        assert_eq!(renderer.glyphs.len(), 1);
    }

    #[test]
    fn font_variant_cache_is_invalidated_on_zoom() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else {
            return; // Host with no discoverable monospace fonts.
        };
        let normal = TextStyle::default();
        let italic = style_from_sgr(b"\x1b[3mM");
        let bold = style_from_sgr(b"\x1b[1mM");
        renderer.glyph('M', normal);
        renderer.glyph('M', italic);
        renderer.glyph('M', bold);
        assert!(!renderer.glyphs.is_empty());
        assert_eq!(renderer.fonts.len(), renderer.styled_fonts.len());
        renderer.set_font_size(config.font.size + 2.0, 1.0);
        assert!(renderer.glyphs.is_empty());
    }


    #[test]
    fn underline_cursor_is_a_thin_bottom_stroke_not_a_full_cell() {
        let mut screen = test_screen();
        screen.show_cursor = true;
        screen.cursor_style = crate::terminal::CursorStyle {
            shape: CursorShape::Underline,
            blink: false,
        };
        screen.grid[0][0].bg = 0x112233;
        let mut pixels = vec![0u32; 10 * 8];
        paint_backgrounds(&mut pixels, 10, 8, &screen, 5, 4, 0, None);
        assert_eq!(pixels[0], 0x112233);
        paint_cursor(&mut pixels, 10, 8, &screen, 5, 4, 0);
        for x in 0..5 {
            assert_eq!(pixels[3 * 10 + x], screen.palette.cursor);
            assert_eq!(pixels[x], 0x112233);
        }
        assert_eq!(pixels[3 * 10 + 5], screen.palette.background);
    }

    #[test]
    fn bar_cursor_is_a_thin_left_stroke_after_glyph_rendering() {
        let mut screen = test_screen();
        screen.show_cursor = true;
        screen.cursor_style = crate::terminal::CursorStyle {
            shape: CursorShape::Bar,
            blink: false,
        };
        let mut pixels = vec![0x112233u32; 10 * 8];
        paint_cursor(&mut pixels, 10, 8, &screen, 5, 4, 0);
        for row in 0..4 {
            assert_eq!(pixels[row * 10], screen.palette.cursor);
            assert_eq!(pixels[row * 10 + 1], 0x112233);
        }
        assert_eq!(pixels[4 * 10], 0x112233);
    }

    #[test]
    fn only_block_cursor_replaces_cell_background() {
        let mut screen = test_screen();
        screen.show_cursor = true;
        screen.grid[0][0].bg = 0x223344;
        for shape in [CursorShape::Underline, CursorShape::Bar] {
            screen.cursor_style = crate::terminal::CursorStyle { shape, blink: false };
            let mut pixels = vec![0u32; 10 * 8];
            paint_backgrounds(&mut pixels, 10, 8, &screen, 5, 4, 0, None);
            assert_eq!(pixels[0], 0x223344);
        }
        screen.cursor_style.shape = CursorShape::Block;
        let mut pixels = vec![0u32; 10 * 8];
        paint_backgrounds(&mut pixels, 10, 8, &screen, 5, 4, 0, None);
        assert_eq!(pixels[0], screen.palette.cursor);
    }

    #[test]
    fn hidden_blink_phase_and_scrollback_suppress_cursor_shapes() {
        let mut screen = test_screen();
        screen.show_cursor = true;
        screen.cursor_style = crate::terminal::CursorStyle {
            shape: CursorShape::Bar,
            blink: true,
        };
        let mut pixels = vec![0x112233u32; 10 * 8];
        screen.cursor_blink_on = false;
        paint_cursor(&mut pixels, 10, 8, &screen, 5, 4, 0);
        assert!(pixels.iter().all(|&pixel| pixel == 0x112233));
        screen.cursor_blink_on = true;
        screen.view_offset = 1;
        paint_cursor(&mut pixels, 10, 8, &screen, 5, 4, 0);
        assert!(pixels.iter().all(|&pixel| pixel == 0x112233));
        screen.view_offset = 0;
        screen.show_cursor = false;
        paint_cursor(&mut pixels, 10, 8, &screen, 5, 4, 0);
        assert!(pixels.iter().all(|&pixel| pixel == 0x112233));
    }

    #[test]
    fn cursor_shapes_clip_safely_at_window_edges_and_small_cells() {
        let mut screen = test_screen();
        screen.show_cursor = true;
        screen.x = 1;
        screen.y = 1;
        let mut pixels = vec![0x112233u32; 7 * 5];
        for shape in [CursorShape::Underline, CursorShape::Bar] {
            screen.cursor_style = crate::terminal::CursorStyle { shape, blink: false };
            paint_cursor(&mut pixels, 7, 5, &screen, 5, 4, 0);
        }
        // Bar at x=5 and y=4 remains in bounds; underline begins at y=7
        // and is correctly clipped beyond the physical surface.
        assert_eq!(pixels[4 * 7 + 5], screen.palette.cursor);
    }


    #[test]
    fn rectangular_highlight_matches_only_selected_columns() {
        let mut screen = test_screen();
        screen.grid[0][0].ch = 'A';
        screen.grid[0][1].ch = 'B';
        screen.grid[1][0].ch = 'C';
        screen.grid[1][1].ch = 'D';
        let mut selected = crate::selection::Selection::new(
            &screen, (1, 0), crate::selection::SelectionSnap::Character, true,
        );
        selected.update(&screen, (1, 1), true);
        assert_eq!(
            cell_colors(&screen, 0, 1, screen.grid[0][1], Some(selected)).1,
            screen.palette.selection
        );
        assert_eq!(
            cell_colors(&screen, 1, 1, screen.grid[1][1], Some(selected)).1,
            screen.palette.selection
        );
        assert_eq!(
            cell_colors(&screen, 1, 0, screen.grid[1][0], Some(selected)).1,
            screen.palette.background
        );
    }

    #[test]
    fn word_selection_highlight_and_clipboard_share_expanded_geometry() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        let mut term = crate::terminal::Terminal::new(12, 2, 0, palette);
        term.advance(b"hello world");
        term.screen.show_cursor = false;
        let selected = crate::selection::Selection::new(
            &term.screen, (2, 0), crate::selection::SelectionSnap::Word, false,
        );
        assert_eq!(selected.text(&term.screen), "hello");
        for x in 0..5 {
            assert_eq!(
                cell_colors(&term.screen, 0, x, term.screen.grid[0][x], Some(selected)).1,
                term.screen.palette.selection
            );
        }
        assert_eq!(
            cell_colors(&term.screen, 0, 5, term.screen.grid[0][5], Some(selected)).1,
            term.screen.palette.background
        );
    }


    #[test]
    fn bell_border_colors_only_two_pixel_outline() {
        let mut pixels = vec![0x112233u32; 10 * 8];
        paint_bell_border(&mut pixels, 10, 8, 0xffcc00);
        for y in 0..8 {
            for x in 0..10 {
                let expected = if !(2..8).contains(&x) || !(2..6).contains(&y) {
                    0xffcc00
                } else {
                    0x112233
                };
                assert_eq!(pixels[y * 10 + x], expected, "pixel ({x}, {y})");
            }
        }
    }

    #[test]
    fn bell_border_is_safe_for_tiny_and_zero_sized_surfaces() {
        let mut empty = Vec::<u32>::new();
        paint_bell_border(&mut empty, 0, 0, 0xffffff);
        paint_bell_border(&mut empty, 1, 1, 0xffffff);
        let mut one = [0u32];
        paint_bell_border(&mut one, 1, 1, 0xabcdef);
        assert_eq!(one, [0xabcdef]);
        let mut short = [0u32; 3];
        paint_bell_border(&mut short, 4, 100, 0xffffff);
        assert_eq!(short, [0, 0, 0]); // incomplete scanline stays intact
    }


    #[test]
    fn composing_text_is_a_visual_overlay_not_mutated_terminal_cells() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else {
            return; // Some CI hosts have no discoverable monospace fonts.
        };
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        let mut terminal = crate::terminal::Terminal::new(12, 3, 0, palette);
        terminal.advance(b"hello\x1b[3G");
        let preedit = crate::ime::Preedit {
            text: "界界".into(),
            cursor: Some((3, 3)),
        };
        let width = renderer.cell_width * terminal.screen.cols;
        let height = renderer.cell_height * terminal.screen.rows;
        let mut base_pixels = vec![0u32; width * height];
        renderer.draw(&mut base_pixels, width, height, &terminal.screen, None, None);
        let mut composing_pixels = vec![0u32; width * height];
        renderer.draw(&mut composing_pixels, width, height, &terminal.screen, None, Some(&preedit));

        assert_ne!(base_pixels, composing_pixels);
        assert_eq!(terminal.screen.grid[0][2].ch, 'l');
        assert_eq!(terminal.screen.grid[0][3].ch, 'l');
        assert!(terminal.screen.history.is_empty());
    }

    #[test]
    fn ime_preview_draws_selection_background_at_grapheme_cell() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else { return };
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        let mut terminal = crate::terminal::Terminal::new(8, 2, 0, palette);
        terminal.advance(b"\x1b[?25l"); // hide ordinary block cursor
        let preedit = crate::ime::Preedit {
            text: "abc".into(),
            cursor: None,
        };
        let width = renderer.cell_width * terminal.screen.cols;
        let height = renderer.cell_height * terminal.screen.rows;
        let mut pixels = vec![0u32; width * height];
        renderer.draw(&mut pixels, width, height, &terminal.screen, None, Some(&preedit));
        assert_eq!(pixels[0], terminal.screen.palette.selection);
        assert_eq!(terminal.screen.grid[0][0].ch, ' ');
    }


    #[test]
    fn ime_composition_hides_the_original_cursor_even_when_preview_wraps() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else { return };
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        let mut terminal = crate::terminal::Terminal::new(4, 3, 0, palette);
        terminal.advance(b"\x1b[3;4H"); // last column of last row
        let preedit = crate::ime::Preedit { text: "界".into(), cursor: Some((3, 3)) };
        let width = renderer.cell_width * terminal.screen.cols;
        let height = renderer.cell_height * terminal.screen.rows;
        let x = 3 * renderer.cell_width;
        let y = 2 * renderer.cell_height;

        let mut ordinary = vec![0u32; width * height];
        renderer.draw(&mut ordinary, width, height, &terminal.screen, None, None);
        let mut composing = vec![0u32; width * height];
        renderer.draw(&mut composing, width, height, &terminal.screen, None, Some(&preedit));

        assert_eq!(ordinary[y * width + x], terminal.screen.palette.cursor);
        assert_eq!(composing[y * width + x], terminal.screen.palette.background);
        assert_eq!(terminal.screen.x, 3);
        assert_eq!(terminal.screen.y, 2);
    }


    #[test]
    fn shaped_glyph_ids_match_same_fontdue_face_and_collection_index() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(renderer) = Renderer::new(&config.font, 0, 1.0) else { return };
        assert_eq!(renderer.font_sources.len(), renderer.fonts.len());
        assert_eq!(renderer.styled_sources.len(), renderer.styled_fonts.len());
        let Some(shaped) = shaping::shape(&renderer.font_sources[0], "A", true) else {
            return; // Some platform-specific system font formats may not parse.
        };
        assert_eq!(shaped.glyphs.len(), 1);
        assert_eq!(shaped.glyphs[0].index, renderer.fonts[0].lookup_glyph_index('A'));
    }

    #[test]
    fn ligature_candidate_shaping_respects_exact_grid_width_and_color() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else { return };
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        let mut term = crate::terminal::Terminal::new(12, 2, 0, palette);
        term.advance(b"fi -> != <>");
        term.screen.show_cursor = false;
        let width = term.screen.cols * renderer.cell_width;
        let height = term.screen.rows * renderer.cell_height;
        let mut pixels = vec![0u32; width * height];
        renderer.draw(&mut pixels, width, height, &term.screen, None, None);
        assert_eq!(term.screen.grid[0][0].ch, 'f');
        assert_eq!(term.screen.grid[0][1].ch, 'i');
        assert_eq!(term.screen.x, 11);
        assert!(pixels.iter().any(|&pixel| pixel != term.screen.palette.background));
        // Shaping never writes back into the terminal grid.
        assert_eq!(term.screen.grid[0][3].ch, '-');
        assert_eq!(term.screen.grid[0][4].ch, '>');
    }

    #[test]
    fn ligatures_can_be_disabled_without_disabling_non_ascii_shaping() {
        let mut config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        config.font.ligatures = false;
        let Ok(renderer) = Renderer::new(&config.font, 0, 1.0) else { return };
        assert!(!renderer.ligatures);
        assert!(!shaping::needs_shaping("fi => ffi", renderer.ligatures));
        assert!(shaping::needs_shaping("مرحبا", renderer.ligatures));
    }

    #[test]
    fn shaping_cache_reuses_run_and_clears_on_font_zoom() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else { return };
        let Some(shaped) = shaping::shape(&renderer.font_sources[0], "fi", true) else {
            return;
        };
        renderer.shape_cache.insert((0, 0, true, "fi".to_owned()), shaped);
        assert_eq!(renderer.shape_cache.len(), 1);
        renderer.set_font_size(config.font.size + 1.0, 1.0);
        assert!(renderer.shape_cache.is_empty());
        assert!(renderer.indexed_glyphs.is_empty());
    }

    #[test]
    fn arabic_preedit_is_shaped_without_mutating_terminal_cells() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else { return };
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        let mut term = crate::terminal::Terminal::new(16, 2, 0, palette);
        term.advance(b"hello");
        let preedit = crate::ime::Preedit {
            text: "مرحبا".to_owned(),
            cursor: Some((10, 10)),
        };
        let width = term.screen.cols * renderer.cell_width;
        let height = term.screen.rows * renderer.cell_height;
        let mut pixels = vec![0u32; width * height];
        renderer.draw(&mut pixels, width, height, &term.screen, None, Some(&preedit));
        assert_eq!(term.screen.grid[0][0].ch, 'h');
        assert_eq!(term.screen.grid[0][1].ch, 'e');
        assert!(term.screen.history.is_empty());
        assert!(pixels.contains(&term.screen.palette.selection));
    }


    #[test]
    fn optional_gruvbox_bar_is_drawn_outside_terminal_cells() {
        let mut config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else { return };
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        let mut term = crate::terminal::Terminal::new(40, 4, 0, palette);
        term.advance(b"shell output");
        let width = term.screen.cols * renderer.cell_width;
        let bar_h = crate::tabs::bar_height(renderer.cell_height, true);
        let height = term.screen.rows * renderer.cell_height + bar_h;
        let items = vec![
            crate::tabs::TabDisplay {
                title: "fish".into(), active: true, unread: false,
            },
            crate::tabs::TabDisplay {
                title: "logs".into(), active: false, unread: true,
            },
        ];
        let mut pixels = vec![0xdeadbeefu32; width * height];
        let original = pixels.clone();
        renderer.draw_tab_bar(&mut pixels, width, height, &term.screen, &items, 0, &config.tabs);
        assert_eq!(pixels, original, "disabled bar should not paint anything");
        config.tabs.enabled = true;
        renderer.draw_tab_bar(&mut pixels, width, height, &term.screen, &items, 0, &config.tabs);
        assert_ne!(pixels, original);
        assert_eq!(pixels[0], crate::tabs::parse_color("#282828").unwrap());
        assert_eq!(term.screen.grid[0][0].ch, 's');
        assert_eq!(term.screen.history.len(), 0);

        config.tabs.position = "bottom".into();
        let mut bottom = original.clone();
        renderer.draw_tab_bar(&mut bottom, width, height, &term.screen, &items, 1, &config.tabs);
        assert_eq!(bottom[0], original[0], "bottom bar must not paint the top row");
        assert_eq!(bottom[(height - 1) * width],
                   crate::tabs::parse_color("#282828").unwrap());
    }

    #[test]
    fn help_popup_draws_over_pixels_without_mutating_terminal_cells() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else { return };
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        let mut term = crate::terminal::Terminal::new(80, 26, 0, palette);
        term.advance(b"normal terminal output");
        let width = term.screen.cols * renderer.cell_width;
        let height = term.screen.rows * renderer.cell_height;
        let mut pixels = vec![0u32; width * height];
        renderer.draw(&mut pixels, width, height, &term.screen, None, None);
        let baseline = pixels.clone();
        renderer.draw_help_overlay(&mut pixels, width, height, &term.screen, 0, true);
        assert_ne!(pixels, baseline);
        assert_eq!(term.screen.grid[0][0].ch, 'n');
        assert_eq!(term.screen.grid[0][1].ch, 'o');
        assert!(term.screen.history.is_empty());
    }

    #[test]
    fn url_picker_highlights_selection_without_modifying_terminal() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else { return };
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        let mut term = crate::terminal::Terminal::new(80, 24, 2, palette);
        term.advance(b"normal terminal output");
        let width = term.screen.cols * renderer.cell_width;
        let height = term.screen.rows * renderer.cell_height;
        let mut pixels = vec![0u32; width * height];
        renderer.draw(&mut pixels, width, height, &term.screen, None, None);
        let baseline = pixels.clone();
        let mut picker = crate::url_picker::UrlPicker::new(
            vec!["https://example.org".into(), "https://rust-lang.org".into()],
        );
        renderer.draw_url_picker_overlay(
            &mut pixels, width, height, &term.screen, &picker, 20,
        );
        assert_ne!(pixels, baseline);
        let first = pixels.clone();
        picker.move_by(1, crate::url_picker::visible_rows(term.screen.rows));
        renderer.draw(&mut pixels, width, height, &term.screen, None, None);
        renderer.draw_url_picker_overlay(
            &mut pixels, width, height, &term.screen, &picker, 20,
        );
        assert_ne!(pixels, first);
        assert_eq!(term.screen.grid[0][0].ch, 'n');
        assert!(term.screen.history.is_empty());
    }

    #[test]
    fn url_picker_clips_safely_on_tiny_windows() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else { return };
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        let term = crate::terminal::Terminal::new(3, 2, 0, palette);
        let width = term.screen.cols * renderer.cell_width;
        let height = term.screen.rows * renderer.cell_height;
        let mut pixels = vec![0u32; width * height];
        renderer.draw_url_picker_overlay(
            &mut pixels, width, height, &term.screen,
            &crate::url_picker::UrlPicker::new(vec![]), 20,
        );
        assert!(pixels.iter().any(|&px| px != 0));
    }

    #[test]
    fn copy_confirmation_overlay_clips_in_small_windows() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else { return };
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        let term = crate::terminal::Terminal::new(4, 2, 0, palette);
        let width = term.screen.cols * renderer.cell_width;
        let height = term.screen.rows * renderer.cell_height;
        let mut pixels = vec![0u32; width * height];
        renderer.draw_help_overlay(&mut pixels, width, height, &term.screen, 200, false);
        let notice = crate::copy_feedback::CopyNotice::warning(
            "No command markers: run rst --install-shell-integration",
            std::time::Instant::now(),
        );
        renderer.draw_copy_notice(
            &mut pixels, width, height, &term.screen, &notice, 255,
        );
        assert!(pixels.iter().any(|&px| px != 0));
    }


    #[test]
    fn copy_fade_tints_only_cells_tagged_as_the_last_command_output() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else { return };
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        let mut term = crate::terminal::Terminal::new(32, 4, 2, palette);
        term.advance(b"prompt> \x1b]133;C\x07hello\x1b]133;D\x07");
        term.screen.show_cursor = false;
        let (_, id) = term.screen.command_capture.latest_output_with_id().unwrap();

        let width = term.screen.cols * renderer.cell_width;
        let height = term.screen.rows * renderer.cell_height;
        let mut before = vec![0u32; width * height];
        renderer.draw(&mut before, width, height, &term.screen, None, None);

        let mut clear = before.clone();
        renderer.draw_output_copy_highlight(
            &mut clear, width, height, &term.screen, id, 0,
        );
        assert_eq!(clear, before);

        let mut highlighted = before.clone();
        renderer.draw_output_copy_highlight(
            &mut highlighted, width, height, &term.screen, id, 255,
        );
        let y_start = renderer.padding;
        let x_prompt_end = renderer.padding + 8 * renderer.cell_width;
        let x_output_end = renderer.padding + 13 * renderer.cell_width;
        for y in y_start..y_start + renderer.cell_height {
            let row = y * width;
            assert_eq!(
                &highlighted[row + renderer.padding..row + x_prompt_end],
                &before[row + renderer.padding..row + x_prompt_end],
                "Prompt pixels must not be highlighted",
            );
        }
        let mut output_changed = false;
        for y in y_start..y_start + renderer.cell_height {
            let row = y * width;
            if highlighted[row + x_prompt_end..row + x_output_end]
                != before[row + x_prompt_end..row + x_output_end]
            {
                output_changed = true;
                break;
            }
        }
        assert!(output_changed, "Copied output must visibly highlight");
        assert_eq!(term.screen.grid[0][8].ch, 'h');
        assert_eq!(term.screen.grid[0][0].ch, 'p');
    }

    #[test]
    fn copied_output_highlight_tracks_scrollback_viewport() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else { return };
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        let mut term = crate::terminal::Terminal::new(16, 2, 4, palette);
        term.advance(b"\x1b]133;C\x07first\r\nsecond\r\nthird\x1b]133;D\x07");
        let (_, output_id) = term.screen.command_capture.latest_output_with_id().unwrap();
        assert_eq!(term.screen.history[0][0].output_id, output_id);
        term.screen.show_cursor = false;
        term.screen.scroll_view(1);
        let width = term.screen.cols * renderer.cell_width;
        let height = term.screen.rows * renderer.cell_height;
        let mut before = vec![0u32; width * height];
        renderer.draw(&mut before, width, height, &term.screen, None, None);
        let mut highlighted = before.clone();
        renderer.draw_output_copy_highlight(
            &mut highlighted, width, height, &term.screen, output_id, 255,
        );
        assert_ne!(before, highlighted, "scrollback copies must be highlighted");
        assert_eq!(term.screen.history[0][0].ch, 'f');
        assert_eq!(term.screen.history[0][0].output_id, output_id);
    }

    #[test]
    fn copy_card_uses_fade_clock_and_does_not_modify_terminal_data() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let Ok(mut renderer) = Renderer::new(&config.font, 0, 1.0) else { return };
        let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
        let mut term = crate::terminal::Terminal::new(48, 8, 0, palette);
        term.advance(b"original text");
        term.screen.show_cursor = false;
        let width = term.screen.cols * renderer.cell_width;
        let height = term.screen.rows * renderer.cell_height;
        let mut before = vec![0u32; width * height];
        renderer.draw(&mut before, width, height, &term.screen, None, None);
        let notice = crate::copy_feedback::CopyNotice::success(3 * 1024, 1, std::time::Instant::now());
        let mut pixels = before.clone();
        renderer.draw_copy_notice(&mut pixels, width, height, &term.screen, &notice, 0);
        assert_eq!(pixels, before);
        renderer.draw_copy_notice(&mut pixels, width, height, &term.screen, &notice, 255);
        assert_ne!(pixels, before);
        assert_eq!(term.screen.grid[0][0].ch, 'o');
        assert_eq!(term.screen.history.len(), 0);
    }

    #[test]
    fn blended_rectangle_clips_to_surface_and_preserves_other_pixels() {
        let mut pixels = vec![0x101010; 8 * 6];
        blend_rect(&mut pixels, 8, 6, 2, 1, 3, 2, 0x00ff00, 255);
        assert_eq!(pixels[0], 0x101010);
        assert_eq!(pixels[8 + 2], 0x00ff00);
        assert_eq!(pixels[2 * 8 + 4], 0x00ff00);
        assert_eq!(pixels[3 * 8 + 3], 0x101010);
        blend_rect(&mut pixels, 8, 6, 100, 100, 30, 30, 0xff0000, 255);
        assert_eq!(pixels[0], 0x101010);
        blend_rect(&mut pixels, 8, 6, 0, 0, 8, 6, 0xffffff, 0);
        assert_eq!(pixels[0], 0x101010);
    }

}

#[cfg(test)]
mod startup_benchmark {
    #[test]
    #[ignore = "manual font startup benchmark; depends on installed fonts"]
    fn font_loading() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        for _ in 0..5 {
            let start = std::time::Instant::now();
            let renderer = super::Renderer::new(&config.font, config.window.padding, 1.0).unwrap();
            eprintln!("font startup: {:?}, {} regular faces", start.elapsed(), renderer.fonts.len());
            std::hint::black_box(renderer);
        }
    }
}

#[cfg(test)]
mod cursor_patch_tests {
    use super::*;

    #[test]
    fn compositor_patch_matches_full_cursor_render_and_restores_base() {
        let config: crate::config::Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
        let mut renderer = Renderer::new(&config.font, 2, 1.0).unwrap();
        for shape in [CursorShape::Block, CursorShape::Bar, CursorShape::Underline] {
            for (text, x, y, reverse, selected) in [
                ("prompt> ", 8, 0, false, false),
                ("fi -> !=", 1, 0, false, true),
                ("\x1b[41;4mcolored spaces   ", 4, 0, false, true),
                ("界e\u{301}👩\u{200d}💻", 1, 0, true, false),
                ("\x1b[41;4mcolored spaces   ", 13, 3, false, true),
            ] {
                let palette = crate::terminal::Palette::try_from(&config.colors).unwrap();
                let mut term = crate::terminal::Terminal::new(14, 4, 8, palette);
                term.advance(text.as_bytes());
                term.screen.x = x;
                term.screen.y = y;
                term.screen.reverse_video = reverse;
                term.screen.cursor_style.shape = shape;
                let selection = selected.then(|| Selection::new(&term.screen, (0, 0), crate::selection::SelectionSnap::Line, false));
                // Include fractional right/bottom padding, as in native windows.
                let width = 2 * renderer.padding + renderer.cell_width * 14 + 3;
                let height = 2 * renderer.padding + renderer.cell_height * 4 + 1;
                let mut base = vec![0; width * height];
                term.screen.cursor_blink_on = false;
                renderer.draw(&mut base, width, height, &term.screen, selection, None);
                let original = base.clone();
                term.screen.cursor_blink_on = true;
                let patch = renderer.cursor_patch(&mut base, width, height, &term.screen, selection).unwrap();
                assert_eq!(base, original, "patch extraction must restore the base frame");
                for dy in 0..patch.height {
                    let target = (patch.y + dy) * width + patch.x;
                    base[target..target + patch.width].copy_from_slice(&patch.pixels[dy * patch.width..(dy + 1) * patch.width]);
                }
                let mut reference = vec![0; width * height];
                renderer.draw(&mut reference, width, height, &term.screen, selection, None);
                assert_eq!(base, reference, "shape {shape:?}, x {x}, y {y}, reverse {reverse}");
            }
        }
    }
}

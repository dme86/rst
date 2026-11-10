use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use fontdb::{Database, Family, Query};
use fontdue::{Font, FontSettings, Metrics};

use crate::{
    config::FontConfig,
    terminal::{Cell, Screen},
};

struct Glyph {
    metrics: Metrics,
    bitmap: Vec<u8>,
}

pub struct Renderer {
    fonts: Vec<Font>,
    glyphs: HashMap<(usize, char), Glyph>,
    font_size: f32,
    pub cell_width: usize,
    pub cell_height: usize,
    pub padding: usize,
    ascent: i32,
}

impl Renderer {
    pub fn new(config: &FontConfig, padding: u32, scale: f64) -> Result<Self> {
        let mut db = Database::new();
        db.load_system_fonts();
        let mut fonts = Vec::new();
        let mut seen = HashSet::new();

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
                fonts.push(font);
            }
        }

        if fonts.is_empty() {
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
                fonts.push(font);
            }
        }

        let primary = fonts
            .first()
            .context("no usable monospace font found; install Menlo or a Nerd Font")?;
        let font_size = (config.size as f64 * scale) as f32;
        let line = primary.horizontal_line_metrics(font_size);
        let cell_width = primary.metrics('M', font_size).advance_width.ceil().max(1.0) as usize;
        let cell_height = line
            .map(|line| line.new_line_size.ceil().max(font_size * 1.2) as usize)
            .unwrap_or((font_size * 1.35).ceil() as usize)
            .max(1);
        let ascent = line
            .map(|line| line.ascent.ceil() as i32)
            .unwrap_or(font_size.ceil() as i32);

        Ok(Self {
            fonts,
            glyphs: HashMap::new(),
            font_size,
            cell_width,
            cell_height,
            padding: (padding as f64 * scale) as usize,
            ascent,
        })
    }

    fn glyph(&mut self, ch: char) -> &Glyph {
        let index = self
            .fonts
            .iter()
            .position(|font| font.has_glyph(ch))
            .unwrap_or(0);
        let font = &self.fonts[index];
        let size = self.font_size;
        self.glyphs.entry((index, ch)).or_insert_with(|| {
            let (metrics, bitmap) = font.rasterize(ch, size);
            Glyph { metrics, bitmap }
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        screen: &Screen,
        selection: Option<((usize, usize), (usize, usize))>,
    ) {
        pixels.fill(screen.palette.background);
        for row in 0..screen.rows {
            let y = self.padding + row * self.cell_height;
            if y >= height {
                break;
            }
            let cells = screen.display_row(row);
            for (col, &cell) in cells.iter().enumerate() {
                if cell.width == 0 {
                    continue;
                }
                let x = self.padding + col * self.cell_width;
                if x >= width {
                    break;
                }
                let selected = selection
                    .map(|(a, b)| {
                        let (start, end) = if (a.1, a.0) <= (b.1, b.0) {
                            (a, b)
                        } else {
                            (b, a)
                        };
                        (row, col) >= (start.1, start.0)
                            && (row, col) <= (end.1, end.0)
                    })
                    .unwrap_or(false);
                let cursor = screen.show_cursor
                    && screen.view_offset == 0
                    && row == screen.y
                    && col == screen.x.min(screen.cols - 1);
                let bg = if cursor {
                    screen.palette.cursor
                } else if selected {
                    screen.palette.selection
                } else {
                    cell.bg
                };
                let fg = if cursor {
                    screen.palette.background
                } else {
                    cell.fg
                };
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
                if cell.ch != ' ' {
                    self.draw_glyph(pixels, width, height, x, y, cell, fg, bg);
                }
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
        bg: u32,
    ) {
        let ascent = self.ascent;
        let line_height = self.cell_height as i32;
        let font_height = self.font_size.ceil() as i32;
        let glyph = self.glyph(cell.ch);
        let top = y as i32 + ascent - glyph.metrics.ymin - glyph.metrics.height as i32
            + (line_height - font_height).max(0) / 2;
        let left = x as i32 + glyph.metrics.xmin;
        for py in 0..glyph.metrics.height {
            let target_y = top + py as i32;
            if target_y < 0 || target_y >= height as i32 {
                continue;
            }
            for px in 0..glyph.metrics.width {
                let target_x = left + px as i32;
                if target_x < 0 || target_x >= width as i32 {
                    continue;
                }
                let alpha = glyph.bitmap[py * glyph.metrics.width + px];
                if alpha == 0 {
                    continue;
                }
                pixels[target_y as usize * width + target_x as usize] = blend(fg, bg, alpha);
            }
        }
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

    #[test]
    fn blending_bounds() {
        assert_eq!(blend(0xffffff, 0, 0), 0);
        assert_eq!(blend(0xffffff, 0, 255), 0xffffff);
    }
}

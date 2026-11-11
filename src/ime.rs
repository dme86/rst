//! Native IME composition and grapheme-aware preedit layout.
//!
//! winit 0.30 exposes macOS input methods / Wayland text-input-v3 through
//! WindowEvent::Ime. Preedit is a *temporary overlay*; only Commit is sent
//! to the PTY, so a shell never sees half-composed text.
//!
//! Preedit cursor offsets are UTF-8 *byte* positions, not character counts.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use winit::event::Ime;

const MAX_PREEDIT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preedit {
    pub text: String,
    /// Start/end byte offsets; None means the IME wants the caret hidden.
    pub cursor: Option<(usize, usize)>,
}

#[derive(Debug, Default)]
pub struct ImeState {
    pub enabled: bool,
    pub preedit: Option<Preedit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreeditGlyph {
    pub text: String,
    pub col: usize,
    pub row: usize,
    pub cells: usize,
    /// True when an IME-selected candidate overlaps this grapheme.
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreeditLayout {
    pub glyphs: Vec<PreeditGlyph>,
    /// The composition caret, if shown by the OS.
    pub caret: Option<(usize, usize)>,
    /// Candidate window anchor, even when the IME hides the caret.
    pub candidate: (usize, usize),
}

impl ImeState {
    pub fn composing(&self) -> bool {
        self.preedit.as_ref().is_some_and(|p| !p.text.is_empty())
    }

    pub fn clear(&mut self) {
        self.preedit = None;
    }

    /// Return committed PTY bytes at most once; never forward preedit bytes.
    /// Keep control-key shortcuts in the ordinary KeyboardInput handler.
    pub fn handle(&mut self, event: Ime, keyboard_locked: bool) -> Option<Vec<u8>> {
        match event {
            Ime::Enabled => {
                self.enabled = true;
                None
            }
            Ime::Disabled => {
                self.enabled = false;
                self.clear();
                None
            }
            Ime::Preedit(text, cursor) => {
                if text.is_empty() || text.len() > MAX_PREEDIT_BYTES {
                    self.clear();
                } else {
                    let cursor = cursor.map(|(start, end)| (
                        floor_char_boundary(&text, start),
                        floor_char_boundary(&text, end),
                    ));
                    self.preedit = Some(Preedit { text, cursor });
                }
                None
            }
            Ime::Commit(text) => {
                self.clear();
                if keyboard_locked || text.is_empty() {
                    return None;
                }
                // The PTY expects CR for Enter and LF from a system input
                // method. Otherwise preserve the original UTF-8 bytes.
                let text = text.replace("\r\n", "\n").replace('\n', "\r");
                Some(text.into_bytes())
            }
        }
    }
}

pub fn floor_char_boundary(text: &str, byte_index: usize) -> usize {
    let mut index = byte_index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn grapheme_width(text: &str) -> usize {
    // A grapheme must not be split between two terminal cells. unicode-width
    // covers CJK, combining marks and most emoji sequences. The renderer
    // itself is still glyph-based, so complex ligature shaping remains
    // outside rst's current rendering engine.
    UnicodeWidthStr::width(text).clamp(1, 2)
}

/// Translate an OS preedit and byte-wise caret position into terminal cells.
/// Long compositions can wrap; when necessary the overlay moves upward so
/// that a composition started on the last screen row remains visible.
pub fn layout(
    preedit: &Preedit,
    cols: usize,
    rows: usize,
    cursor: (usize, usize),
) -> PreeditLayout {
    if cols == 0 || rows == 0 {
        return PreeditLayout {
            glyphs: Vec::new(),
            caret: None,
            candidate: (0, 0),
        };
    }

    let origin_col = cursor.0.min(cols - 1);
    let initial_row = cursor.1.min(rows - 1);
    let graphemes: Vec<(usize, &str)> = preedit.text.grapheme_indices(true).collect();

    // First measure how many visual rows are needed using the *real* cursor
    // column. Shifting the overlay upward does not mutate terminal cells.
    let mut col = origin_col;
    let mut extra_rows = 0usize;
    for (_, grapheme) in &graphemes {
        let width = grapheme_width(grapheme).min(cols);
        if col + width > cols {
            col = 0;
            extra_rows += 1;
        }
        col += width;
    }
    let overflow = initial_row.saturating_add(extra_rows).saturating_sub(rows - 1);
    let first_row = initial_row.saturating_sub(overflow);

    let mut glyphs = Vec::with_capacity(graphemes.len().min(cols.saturating_mul(rows)));
    let mut col = origin_col;
    let mut row = first_row;
    let mut caret = None;
    let range = preedit.cursor.map(|(a, b)| {
        let x = floor_char_boundary(&preedit.text, a);
        let y = floor_char_boundary(&preedit.text, b);
        (x.min(y), x.max(y))
    });

    for (byte, grapheme) in graphemes {
        let width = grapheme_width(grapheme).min(cols);
        if col + width > cols {
            col = 0;
            row = row.saturating_add(1);
        }
        if let Some((start, _)) = range {
            if caret.is_none() && start <= byte {
                caret = Some((col, row.min(rows - 1)));
            }
        }
        let selected = range.is_some_and(|(start, end)| start < byte + grapheme.len() && end > byte);
        if row < rows && glyphs.len() < cols.saturating_mul(rows) {
            glyphs.push(PreeditGlyph {
                text: grapheme.to_owned(),
                col,
                row,
                cells: width,
                selected,
            });
        }
        col += width;
    }
    if range.is_some() && caret.is_none() {
        if col >= cols {
            caret = Some((0, row.saturating_add(1).min(rows - 1)));
        } else {
            caret = Some((col, row.min(rows - 1)));
        }
    }
    let candidate = caret.unwrap_or((origin_col, first_row));
    PreeditLayout { glyphs, caret, candidate }
}

#[cfg(test)]
#[path = "ime_tests.rs"]
mod tests;

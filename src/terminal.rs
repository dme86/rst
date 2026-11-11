use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;
use unicode_segmentation::UnicodeSegmentation;
use vte::{Params, Parser, Perform};

use crate::{charset::CharacterSet, command_capture::CommandCapture, config::Colors, mouse::MouseTracking, osc, selection::Selection, tab_naming};

pub const DEFAULT_TITLE: &str = "rst — Rust Simple Terminal";

/// Compact, per-cell SGR attributes. Like st's ATTR_* bitmask, these must
/// travel with each glyph through scrollback, insert/delete, and alternate
/// screen swaps instead of living only in the current cursor state.
/// Cursor geometry is independent from the glyph's SGR attributes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorStyle {
    pub shape: CursorShape,
    pub blink: bool,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self { shape: CursorShape::Block, blink: true }
    }
}

impl CursorStyle {
    /// DECSCUSR: CSI Ps SP q. Unknown values leave the cursor unchanged.
    /// Ps=0 restores the user's configured default.
    pub fn from_decscusr(param: u16, default: Self) -> Option<Self> {
        let (shape, blink) = match param {
            0 => return Some(default),
            1 => (CursorShape::Block, true),
            2 => (CursorShape::Block, false),
            3 => (CursorShape::Underline, true),
            4 => (CursorShape::Underline, false),
            5 => (CursorShape::Bar, true),
            6 => (CursorShape::Bar, false),
            _ => return None,
        };
        Some(Self { shape, blink })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextStyle(u8);

impl TextStyle {
    pub const BOLD: Self = Self(1 << 0);
    pub const FAINT: Self = Self(1 << 1);
    pub const ITALIC: Self = Self(1 << 2);
    pub const UNDERLINE: Self = Self(1 << 3);
    pub const BLINK: Self = Self(1 << 4);
    pub const REVERSE: Self = Self(1 << 5);
    pub const INVISIBLE: Self = Self(1 << 6);
    pub const STRUCK: Self = Self(1 << 7);

    pub fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 != 0
    }

    fn set(&mut self, flag: Self, enabled: bool) {
        if enabled {
            self.0 |= flag.0;
        } else {
            self.0 &= !flag.0;
        }
    }

    pub fn visible(self, blink_on: bool) -> bool {
        !self.contains(Self::INVISIBLE)
            && (!self.contains(Self::BLINK) || blink_on)
    }
}

/// Palette provenance lets OSC recolor existing cells without changing
/// literal RGB cells, including saved alternate screens and scrollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorSource {
    DefaultForeground,
    DefaultBackground,
    Indexed(u8),
    Direct,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub width: u8,
    pub fg: u32,
    pub bg: u32,
    pub fg_source: ColorSource,
    pub bg_source: ColorSource,
    pub style: TextStyle,
    /// Set on the final display cell when the next printable glyph soft-wraps.
    pub wrapped: bool,
    /// Extra Unicode scalars in the same grapheme cluster (combining marks,
    /// variation selectors, and ZWJ emoji parts). Fixed storage keeps Cell
    /// Copy and makes history/insert/delete efficient and allocation-free.
    pub combining: [char; 8],
    pub combining_len: u8,
    /// OSC 133 output that most recently wrote this screen cell (0 = none).
    pub output_id: u32,
}

impl Cell {
    pub fn text(self) -> String {
        let mut out = String::from(self.ch);
        for &ch in &self.combining[..self.combining_len as usize] {
            out.push(ch);
        }
        out
    }

    pub fn append_combining(&mut self, ch: char) {
        let count = self.combining_len as usize;
        if count < self.combining.len() {
            self.combining[count] = ch;
            self.combining_len += 1;
        }
    }

}

#[derive(Clone)]
pub struct Palette {
    pub foreground: u32,
    pub background: u32,
    pub cursor: u32,
    pub selection: u32,
    pub ansi: [u32; 16],
    pub extended: [u32; 240],
}

impl TryFrom<&Colors> for Palette {
    type Error = anyhow::Error;

    fn try_from(colors: &Colors) -> anyhow::Result<Self> {
        fn parse(hex: &str) -> anyhow::Result<u32> {
            let digits = hex
                .strip_prefix('#')
                .ok_or_else(|| anyhow::anyhow!("expected #RRGGBB, got {hex}"))?;
            anyhow::ensure!(digits.len() == 6, "expected #RRGGBB, got {hex}");
            Ok(u32::from_str_radix(digits, 16)?)
        }

        anyhow::ensure!(colors.palette.len() == 16, "colors.palette needs 16 colors");
        let parsed = colors
            .palette
            .iter()
            .map(|s| parse(s))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            foreground: parse(&colors.foreground)?,
            background: parse(&colors.background)?,
            cursor: parse(&colors.cursor)?,
            selection: parse(&colors.selection)?,
            ansi: parsed.try_into().expect("length validated"),
            extended: std::array::from_fn(|i| Palette::xterm_indexed((i + 16) as u8)),
        })
    }
}

impl Palette {
    fn xterm_indexed(index: u8) -> u32 {
        match index {
            16..=231 => {
                let i = index as u32 - 16;
                let channel = |v: u32| if v == 0 { 0 } else { 55 + 40 * v };
                (channel(i / 36) << 16) | (channel((i / 6) % 6) << 8) | channel(i % 6)
            }
            232..=255 => {
                let grey = 8 + 10 * (index as u32 - 232);
                (grey << 16) | (grey << 8) | grey
            }
            _ => 0,
        }
    }

    pub fn indexed(&self, index: u8) -> u32 {
        if index < 16 { self.ansi[index as usize] }
        else { self.extended[index as usize - 16] }
    }

    fn set_indexed(&mut self, index: u8, rgb: u32) {
        if index < 16 { self.ansi[index as usize] = rgb; }
        else { self.extended[index as usize - 16] = rgb; }
    }

    fn resolve(&self, source: ColorSource, literal: u32) -> u32 {
        match source {
            ColorSource::DefaultForeground => self.foreground,
            ColorSource::DefaultBackground => self.background,
            ColorSource::Indexed(index) => self.indexed(index),
            ColorSource::Direct => literal,
        }
    }
}

#[derive(Clone)]
struct SavedScreen {
    grid: Vec<Vec<Cell>>,
    x: usize,
    y: usize,
    wrap_pending: bool,
}

#[derive(Clone, Copy, Debug)]
struct CursorSnapshot {
    x: usize,
    y: usize,
    wrap_pending: bool,
    fg: u32,
    bg: u32,
    fg_source: ColorSource,
    bg_source: ColorSource,
    style: TextStyle,
    origin_mode: bool,
}

pub struct Screen {
    pub cols: usize,
    pub rows: usize,
    pub grid: Vec<Vec<Cell>>,
    pub history: VecDeque<Vec<Cell>>,
    pub view_offset: usize,
    pub x: usize,
    pub y: usize,
    pub show_cursor: bool,
    // UI blink phase, independent of DECTCEM visibility (CSI ?25 h/l).
    pub cursor_blink_on: bool,
    pub cursor_style: CursorStyle,
    default_cursor_style: CursorStyle,
    // Blink attribute (SGR 5/6) animates independently of the cursor.
    pub text_blink_on: bool,
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub mouse_tracking: MouseTracking,
    pub mouse_sgr: bool,
    pub bracketed_paste: bool,
    pub focus_reporting: bool,
    /// At least one OSC 133 B (prompt complete) marker was received.
    pub prompt_seen: bool,
    pub keyboard_lock: bool,
    pub reverse_video: bool,
    pub origin_mode: bool,
    pub auto_wrap: bool,
    pub insert_mode: bool,
    pub newline_mode: bool,
    // Four G0..G3 designation registers and the currently invoked GL set.
    // Kept globally across primary/alternate-screen swaps, as in st.
    charsets: [CharacterSet; 4],
    active_charset: usize,
    pub palette: Palette,
    initial_palette: Palette,
    pub title: String,
    pub icon_title: String,
    /// Event-driven shell metadata for native tab labels; never persisted.
    pub cwd: Option<String>,
    pub foreground_program: Option<String>,
    pub tab_label_revision: u64,
    pub osc52_write_allowed: bool,
    clipboard_writes: Vec<String>,
    pub command_capture: CommandCapture,
    fg: u32,
    bg: u32,
    fg_source: ColorSource,
    bg_source: ColorSource,
    style: TextStyle,
    history_limit: usize,
    scroll_top: usize,
    scroll_bottom: usize,
    tab_stops: Vec<bool>,
    wrap_pending: bool,
    saved_cursor: CursorSnapshot,
    saved_primary_cursor: Option<CursorSnapshot>,
    alternate: Option<SavedScreen>,
    replies: Vec<u8>,
    pending_bells: u8,
}

impl Screen {
    /// Configure the initial cursor; DECSCUSR 0 and RIS restore this style.
    pub fn set_default_cursor_style(&mut self, style: CursorStyle) {
        self.default_cursor_style = style;
        self.cursor_style = style;
        self.cursor_blink_on = true;
    }

    fn set_decscusr(&mut self, param: u16) {
        if let Some(style) = CursorStyle::from_decscusr(param, self.default_cursor_style) {
            self.cursor_style = style;
            // A newly selected shape must be visible immediately.
            self.cursor_blink_on = true;
        }
    }

    pub fn new(cols: usize, rows: usize, history_limit: usize, palette: Palette) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let blank = Cell {
            ch: ' ',
            width: 1,
            fg: palette.foreground,
            bg: palette.background,
            fg_source: ColorSource::DefaultForeground,
            bg_source: ColorSource::DefaultBackground,
            style: TextStyle::default(),
            wrapped: false,
            combining: ['\0'; 8],
            combining_len: 0,
            output_id: 0,
        };
        Self {
            cols,
            rows,
            grid: vec![vec![blank; cols]; rows],
            history: VecDeque::new(),
            view_offset: 0,
            x: 0,
            y: 0,
            show_cursor: true,
            cursor_blink_on: true,
            cursor_style: CursorStyle::default(),
            default_cursor_style: CursorStyle::default(),
            text_blink_on: true,
            app_cursor: false,
            app_keypad: false,
            mouse_tracking: MouseTracking::Off,
            mouse_sgr: false,
            bracketed_paste: false,
            focus_reporting: false,
            prompt_seen: false,
            keyboard_lock: false,
            reverse_video: false,
            origin_mode: false,
            auto_wrap: true,
            insert_mode: false,
            newline_mode: false,
            charsets: [CharacterSet::Ascii; 4],
            active_charset: 0,
            fg: palette.foreground,
            bg: palette.background,
            fg_source: ColorSource::DefaultForeground,
            bg_source: ColorSource::DefaultBackground,
            style: TextStyle::default(),
            initial_palette: palette.clone(),
            palette,
            title: DEFAULT_TITLE.to_string(),
            icon_title: DEFAULT_TITLE.to_string(),
            cwd: None,
            foreground_program: None,
            tab_label_revision: 0,
            osc52_write_allowed: false,
            clipboard_writes: Vec::new(),
            command_capture: CommandCapture::default(),
            history_limit,
            scroll_top: 0,
            scroll_bottom: rows,
            tab_stops: (0..cols).map(|col| col != 0 && col % 8 == 0).collect(),
            wrap_pending: false,
            saved_cursor: CursorSnapshot {
                x: 0,
                y: 0,
                wrap_pending: false,
                fg: blank.fg,
                bg: blank.bg,
                fg_source: blank.fg_source,
                bg_source: blank.bg_source,
                style: TextStyle::default(),
                origin_mode: false,
            },
            saved_primary_cursor: None,
            alternate: None,
            replies: Vec::new(),
            pending_bells: 0,
        }
    }

    fn snapshot_cursor(&self) -> CursorSnapshot {
        CursorSnapshot {
            x: self.x.min(self.cols - 1),
            y: self.y.min(self.rows - 1),
            wrap_pending: self.wrap_pending,
            fg: self.fg,
            bg: self.bg,
            fg_source: self.fg_source,
            bg_source: self.bg_source,
            style: self.style,
            origin_mode: self.origin_mode,
        }
    }

    fn save_cursor(&mut self) {
        self.saved_cursor = self.snapshot_cursor();
    }

    fn restore_cursor(&mut self) {
        let saved = self.saved_cursor;
        self.restore_snapshot(saved);
    }

    fn restore_snapshot(&mut self, saved: CursorSnapshot) {
        self.origin_mode = saved.origin_mode;
        self.x = saved.x.min(self.cols - 1);
        self.y = saved.y.min(self.rows - 1);
        self.fg = self.palette.resolve(saved.fg_source, saved.fg);
        self.bg = self.palette.resolve(saved.bg_source, saved.bg);
        self.fg_source = saved.fg_source;
        self.bg_source = saved.bg_source;
        self.style = saved.style;
        self.wrap_pending = self.auto_wrap && saved.wrap_pending && self.x + 1 == self.cols;
    }

    /// Absolute cursor movement clamps to DECOM's scrolling region.
    fn move_to(&mut self, x: usize, y: usize) {
        self.x = x.min(self.cols - 1);
        let (top, bottom) = if self.origin_mode {
            (self.scroll_top, self.scroll_bottom)
        } else {
            (0, self.rows)
        };
        self.y = y.clamp(top, bottom - 1);
        self.wrap_pending = false;
    }

    /// CUP/HVP and VPA are relative to the top margin in origin mode.
    fn move_to_origin(&mut self, x: usize, y: usize) {
        let absolute_y = if self.origin_mode {
            self.scroll_top.saturating_add(y)
        } else {
            y
        };
        self.move_to(x, absolute_y);
    }

    fn home_cursor(&mut self) {
        self.move_to_origin(0, 0);
    }

    fn relative_y(&mut self, lines: i32) {
        let y = if lines >= 0 {
            self.y.saturating_add(lines as usize)
        } else {
            self.y.saturating_sub(lines.unsigned_abs() as usize)
        };
        self.move_to(self.x, y);
    }

    fn tab(&mut self, steps: i32) {
        self.wrap_pending = false;
        let count = steps.unsigned_abs().min(self.cols as u32);
        if steps > 0 {
            for _ in 0..count {
                let next = (self.x + 1..self.cols).find(|&col| self.tab_stops[col]);
                self.x = next.unwrap_or(self.cols - 1);
            }
        } else {
            for _ in 0..count {
                let prev = (0..self.x).rev().find(|&col| self.tab_stops[col]);
                self.x = prev.unwrap_or(0);
            }
        }
    }

    // Background Color Erase (BCE): an erased cell uses the *current* SGR
    // background, not the initial palette background. tmux relies on this
    // for coloured status lines and clear-to-end-of-line sequences.
    fn blank(&self) -> Cell {
        let (fg, bg, fg_source, bg_source) = self.ink_colors_with_sources();
        Cell {
            ch: ' ',
            width: 1,
            fg,
            bg,
            fg_source,
            bg_source,
            // Erased cells retain BCE colors, not the formatting of the
            // glyph that was previously at this location.
            style: TextStyle::default(),
            wrapped: false,
            combining: ['\0'; 8],
            combining_len: 0,
            output_id: 0,
        }
    }

    fn ink_colors_with_sources(&self) -> (u32, u32, ColorSource, ColorSource) {
        // st brightens ANSI colors 0..7 when Bold is active; preserving
        // the palette index also makes later OSC 4 changes repaint correctly.
        let foreground_source = match self.fg_source {
            ColorSource::Indexed(index) if index < 8 && self.style.contains(TextStyle::BOLD) => {
                ColorSource::Indexed(index + 8)
            }
            source => source,
        };
        let foreground = self.palette.resolve(foreground_source, self.fg);
        let background = self.palette.resolve(self.bg_source, self.bg);
        if self.style.contains(TextStyle::REVERSE) {
            (background, foreground, self.bg_source, foreground_source)
        } else {
            (foreground, background, foreground_source, self.bg_source)
        }
    }

    fn empty_row(&self) -> Vec<Cell> {
        vec![self.blank(); self.cols]
    }

    fn linefeed(&mut self) {
        self.wrap_pending = false;
        if self.y >= self.scroll_top && self.y + 1 == self.scroll_bottom {
            self.scroll_up(1);
        } else {
            self.y = (self.y + 1).min(self.rows - 1);
        }
    }

    fn scroll_up(&mut self, lines: usize) {
        for _ in 0..lines {
            let removed = self.grid.remove(self.scroll_top);
            if self.scroll_top == 0 && self.scroll_bottom == self.rows && self.alternate.is_none()
                && self.history_limit > 0
            {
                self.history.push_back(removed);
                while self.history.len() > self.history_limit {
                    self.history.pop_front();
                }
            }
            let blank = self.empty_row();
            self.grid.insert(self.scroll_bottom - 1, blank);
        }
        self.view_offset = 0;
    }

    fn scroll_down(&mut self, lines: usize) {
        for _ in 0..lines {
            self.grid.remove(self.scroll_bottom - 1);
            let blank = self.empty_row();
            self.grid.insert(self.scroll_top, blank);
        }
        self.view_offset = 0;
    }

    /// Whether any displayed cell needs SGR blinking. Avoid a wakeup timer
    /// for ordinary terminal output that does not contain blinking glyphs.
    pub fn has_blinking_text(&self) -> bool {
        (0..self.rows).any(|row| {
            self.display_row(row)
                .iter()
                .any(|cell| cell.style.contains(TextStyle::BLINK))
        })
    }

    pub fn in_alternate_screen(&self) -> bool {
        self.alternate.is_some()
    }

    pub fn scroll_view(&mut self, lines: i32) {
        if self.alternate.is_some() {
            return;
        }
        let value = self.view_offset as i64 + lines as i64;
        self.view_offset = value.clamp(0, self.history.len() as i64) as usize;
    }

    pub fn display_row(&self, row: usize) -> &[Cell] {
        if self.alternate.is_some() || self.view_offset == 0 {
            return &self.grid[row.min(self.rows - 1)];
        }
        let index = self.history.len() - self.view_offset + row.min(self.rows - 1);
        if index < self.history.len() {
            &self.history[index]
        } else {
            &self.grid[index - self.history.len()]
        }
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        let blank = self.blank();
        let width_changed = cols != self.cols;
        for row in &mut self.grid {
            row.resize(cols, blank);
            if width_changed {
                for cell in row { cell.wrapped = false; }
            }
        }
        // Scrollback rows are also addressable by display_row(). Keep them
        // exactly as wide as the viewport to avoid out-of-bounds selection
        // and rendering after a terminal resize.
        for row in &mut self.history {
            row.resize(cols, blank);
            if width_changed {
                for cell in row { cell.wrapped = false; }
            }
        }
        if rows > self.rows {
            self.grid.extend(vec![vec![blank; cols]; rows - self.rows]);
        } else {
            self.grid.truncate(rows);
        }
        if let Some(saved) = &mut self.alternate {
            for row in &mut saved.grid {
                row.resize(cols, blank);
                if width_changed {
                    for cell in row { cell.wrapped = false; }
                }
            }
            saved.grid.resize(rows, vec![blank; cols]);
            saved.x = saved.x.min(cols - 1);
            saved.y = saved.y.min(rows - 1);
        }
        let previous_tab_width = self.tab_stops.len();
        self.tab_stops.resize_with(cols, || false);
        // New columns get conventional 8-cell tab stops; existing user
        // customizations survive a resize just like st.
        for col in previous_tab_width..cols {
            self.tab_stops[col] = col != 0 && col % 8 == 0;
        }
        self.cols = cols;
        self.rows = rows;
        self.x = self.x.min(cols - 1);
        self.y = self.y.min(rows - 1);
        self.scroll_top = 0;
        self.scroll_bottom = rows;
        self.saved_cursor.x = self.saved_cursor.x.min(cols - 1);
        self.saved_cursor.y = self.saved_cursor.y.min(rows - 1);
        if let Some(snapshot) = self.saved_primary_cursor.as_mut() {
            snapshot.x = snapshot.x.min(cols - 1);
            snapshot.y = snapshot.y.min(rows - 1);
        }
        self.wrap_pending = false;
        self.view_offset = 0;
    }

    fn put_char(&mut self, ch: char) {
        // Apply the currently invoked DEC character set before measuring
        // glyph width or writing the cell. Selection and clipboard then see
        // the real Unicode line-drawing character instead of an ASCII alias.
        let ch = self.charsets[self.active_charset].map(ch);
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        // Associate zero-width marks, variation selectors and ZWJ-linked
        // scalars with the preceding base cell, including a wide glyph's
        // leading cell. This runs before DECAWM so a combining mark at the
        // right edge never triggers an unwanted line wrap.
        let previous = if self.wrap_pending {
            Some((self.x, self.y))
        } else if self.x > 0 {
            Some((self.x - 1, self.y))
        } else if self.y > 0 && self.grid[self.y - 1][self.cols - 1].wrapped {
            Some((self.cols - 1, self.y - 1))
        } else {
            None
        };
        // Ordinary ASCII cannot extend a preceding grapheme. Avoid a
        // String allocation and Unicode segmentation for every character
        // in a high-throughput shell/tmux output stream.
        if width == 0 || !ch.is_ascii() {
            if let Some((mut x, y)) = previous {
                if self.grid[y][x].width == 0 && x > 0 {
                    x -= 1;
                }
                let cell = &self.grid[y][x];
                let same_grapheme = {
                    let mut candidate = cell.text();
                    candidate.push(ch);
                    UnicodeSegmentation::graphemes(candidate.as_str(), true).count() == 1
                };
                if width == 0 || same_grapheme {
                    self.grid[y][x].append_combining(ch);
                    return;
                }
            }
        }
        if width == 0 || width > 2 {
            // Orphan marks and unsupported widths do not consume a cell.
            return;
        }
        // A one-column terminal cannot hold a double-width glyph. Using a
        // single-cell replacement prevents the cursor leaving the grid.
        let (ch, width) = if width > self.cols { ('?', 1) } else { (ch, width) };
        // DECAWM wraps on the next printable glyph, not immediately after
        // painting the last column (the delayed-wrap behavior from st).
        if self.wrap_pending {
            if self.auto_wrap {
                self.grid[self.y][self.cols - 1].wrapped = true;
                self.x = 0;
                self.linefeed();
            }
            self.wrap_pending = false;
        }
        if self.x + width > self.cols {
            if self.auto_wrap {
                self.grid[self.y][self.cols - 1].wrapped = true;
                self.x = 0;
                self.linefeed();
            } else {
                // Avoid writing half a double-width glyph at the right edge.
                self.grid[self.y][self.cols - 1] = self.blank();
                return;
            }
        }
        if self.insert_mode {
            let count = width.min(self.cols - self.x);
            let blank = self.blank();
            let row = &mut self.grid[self.y];
            row.copy_within(self.x..self.cols - count, self.x + count);
            row[self.x..self.x + count].fill(blank);
        }
        let (fg, bg, fg_source, bg_source) = self.ink_colors_with_sources();
        let output_id = self.command_capture.active_output_id().unwrap_or(0);
        self.grid[self.y][self.x] = Cell {
            ch,
            width: width as u8,
            fg,
            bg,
            fg_source,
            bg_source,
            style: self.style,
            wrapped: false,
            combining: ['\0'; 8],
            combining_len: 0,
            output_id,
        };
        if width == 2 && self.x + 1 < self.cols {
            self.grid[self.y][self.x + 1] = Cell {
                ch: ' ',
                width: 0,
                fg,
                bg,
                fg_source,
                bg_source,
                style: self.style,
                wrapped: false,
            combining: ['\0'; 8],
            combining_len: 0,
            output_id,
            };
        }
        if self.x + width == self.cols {
            self.x = self.cols - 1;
            self.wrap_pending = self.auto_wrap;
        } else {
            self.x += width;
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let cursor = self.x.min(self.cols - 1);
        let (start, end) = match mode {
            1 => (0, cursor + 1),
            2 => (0, self.cols),
            _ => (cursor, self.cols),
        };
        let blank = self.blank();
        for cell in &mut self.grid[self.y][start..end] {
            *cell = blank;
        }
    }

    fn erase_display(&mut self, mode: u16) {
        match mode {
            2 => {
                let blank = self.blank();
                for row in &mut self.grid {
                    row.fill(blank);
                }
            }
            3 => {
                self.history.clear();
                self.view_offset = 0;
            }
            1 => {
                let blank = self.blank();
                for row in &mut self.grid[..self.y] {
                    row.fill(blank);
                }
                self.erase_line(1);
            }
            _ => {
                self.erase_line(0);
                let blank = self.blank();
                for row in &mut self.grid[self.y + 1..] {
                    row.fill(blank);
                }
            }
        }
    }

    fn sgr(&mut self, params: &[u16]) {
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => {
                    self.fg = self.palette.foreground;
                    self.bg = self.palette.background;
                    self.fg_source = ColorSource::DefaultForeground;
                    self.bg_source = ColorSource::DefaultBackground;
                    self.style = TextStyle::default();
                }
                1 => self.style.set(TextStyle::BOLD, true),
                2 => self.style.set(TextStyle::FAINT, true),
                3 => self.style.set(TextStyle::ITALIC, true),
                4 => self.style.set(TextStyle::UNDERLINE, true),
                5 | 6 => self.style.set(TextStyle::BLINK, true),
                7 => self.style.set(TextStyle::REVERSE, true),
                8 => self.style.set(TextStyle::INVISIBLE, true),
                9 => self.style.set(TextStyle::STRUCK, true),
                22 => {
                    self.style.set(TextStyle::BOLD, false);
                    self.style.set(TextStyle::FAINT, false);
                }
                23 => self.style.set(TextStyle::ITALIC, false),
                24 => self.style.set(TextStyle::UNDERLINE, false),
                25 => self.style.set(TextStyle::BLINK, false),
                27 => self.style.set(TextStyle::REVERSE, false),
                28 => self.style.set(TextStyle::INVISIBLE, false),
                29 => self.style.set(TextStyle::STRUCK, false),
                30..=37 => {
                    let index = (params[i] - 30) as u8;
                    self.fg_source = ColorSource::Indexed(index);
                    self.fg = self.indexed_color(index);
                },
                40..=47 => {
                    let index = (params[i] - 40) as u8;
                    self.bg_source = ColorSource::Indexed(index);
                    self.bg = self.indexed_color(index);
                },
                90..=97 => {
                    let index = (params[i] - 90 + 8) as u8;
                    self.fg_source = ColorSource::Indexed(index);
                    self.fg = self.indexed_color(index);
                },
                100..=107 => {
                    let index = (params[i] - 100 + 8) as u8;
                    self.bg_source = ColorSource::Indexed(index);
                    self.bg = self.indexed_color(index);
                },
                39 => {
                    self.fg = self.palette.foreground;
                    self.fg_source = ColorSource::DefaultForeground;
                },
                49 => {
                    self.bg = self.palette.background;
                    self.bg_source = ColorSource::DefaultBackground;
                },
                38 | 48 => {
                    let is_fg = params[i] == 38;
                    let value = if params.get(i + 1) == Some(&2) && i + 4 < params.len() {
                        i += 4;
                        Some((
                            ((params[i - 2].min(255) as u32) << 16)
                                | ((params[i - 1].min(255) as u32) << 8)
                                | params[i].min(255) as u32,
                            ColorSource::Direct,
                        ))
                    } else if params.get(i + 1) == Some(&5) && i + 2 < params.len() {
                        i += 2;
                        let index = params[i].min(255) as u8;
                        Some((self.indexed_color(index), ColorSource::Indexed(index)))
                    } else {
                        None
                    };
                    if let Some((color, source)) = value {
                        if is_fg {
                            self.fg = color;
                            self.fg_source = source;
                        } else {
                            self.bg = color;
                            self.bg_source = source;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn indexed_color(&self, index: u8) -> u32 {
        self.palette.indexed(index)
    }

    /// Re-evaluate only palette-based cells after OSC color changes. Direct
    /// RGB colors and decorations are unaffected. Keep saved screens/history
    /// consistent so switching alternate screen cannot resurrect stale colors.
    fn refresh_palette(&mut self) {
        fn refresh_cell(cell: &mut Cell, palette: &Palette) {
            cell.fg = palette.resolve(cell.fg_source, cell.fg);
            cell.bg = palette.resolve(cell.bg_source, cell.bg);
        }
        let palette = &self.palette;
        for row in &mut self.grid {
            for cell in row { refresh_cell(cell, palette); }
        }
        for row in &mut self.history {
            for cell in row { refresh_cell(cell, palette); }
        }
        if let Some(saved) = self.alternate.as_mut() {
            for row in &mut saved.grid {
                for cell in row { refresh_cell(cell, palette); }
            }
        }
        self.fg = palette.resolve(self.fg_source, self.fg);
        self.bg = palette.resolve(self.bg_source, self.bg);
        self.saved_cursor.fg = palette.resolve(self.saved_cursor.fg_source, self.saved_cursor.fg);
        self.saved_cursor.bg = palette.resolve(self.saved_cursor.bg_source, self.saved_cursor.bg);
        if let Some(saved) = &mut self.saved_primary_cursor {
            saved.fg = palette.resolve(saved.fg_source, saved.fg);
            saved.bg = palette.resolve(saved.bg_source, saved.bg);
        }
    }

    fn set_alternate(&mut self, enable: bool) {
        if enable && self.alternate.is_none() {
            let empty = vec![self.empty_row(); self.rows];
            self.alternate = Some(SavedScreen {
                grid: std::mem::replace(&mut self.grid, empty),
                x: self.x,
                y: self.y,
                wrap_pending: self.wrap_pending,
            });
            self.x = 0;
            self.y = 0;
            self.wrap_pending = false;
            self.view_offset = 0;
        } else if !enable {
            if let Some(saved) = self.alternate.take() {
                self.grid = saved.grid;
                self.x = saved.x;
                self.y = saved.y;
                self.wrap_pending = saved.wrap_pending;
                self.view_offset = 0;
            }
        }
    }

    /// Drain trusted clipboard-write requests at the UI boundary, where
    /// native clipboard access is available. Never expose clipboard reads.
    pub fn take_clipboard_writes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.clipboard_writes)
    }

    fn set_osc_color(&mut self, code: u16, value: u32) {
        match code {
            10 => self.palette.foreground = value,
            11 => self.palette.background = value,
            12 => self.palette.cursor = value,
            _ => return,
        }
        self.refresh_palette();
    }

    fn reset_osc_color(&mut self, code: u16) {
        let value = match code {
            110 => self.initial_palette.foreground,
            111 => self.initial_palette.background,
            112 => self.initial_palette.cursor,
            _ => return,
        };
        self.set_osc_color(code - 100, value);
    }

    fn handle_osc(&mut self, params: &[&[u8]]) {
        // vte truncates at capacity. Reject the entire command rather than
        // applying a partial title, URL or clipboard write.
        if params.iter().map(|part| part.len()).sum::<usize>() >= MAX_OSC_BYTES {
            return;
        }
        let Some(code) = params.first().and_then(|p| std::str::from_utf8(p).ok())
            .and_then(|p| p.parse::<u16>().ok()) else {
            return;
        };
        match code {
            0..=2 => {
                if params.len() < 2 {
                    return;
                }
                // OSC titles may themselves contain semicolons; vte splits
                // those into arguments, so restore them before validation.
                let mut value = Vec::new();
                for (i, part) in params[1..].iter().enumerate() {
                    if i != 0 { value.push(b';'); }
                    if value.len().saturating_add(part.len()) > 4096 {
                        return;
                    }
                    value.extend_from_slice(part);
                }
                if let Some(title) = osc::title(&value) {
                    let title = if title.is_empty() { DEFAULT_TITLE.to_string() } else { title };
                    if (code == 0 || code == 2)
                        && self.title != title
                    {
                        self.title = title.clone();
                        self.tab_label_revision = self.tab_label_revision.wrapping_add(1);
                    }
                    if code == 0 || code == 1 { self.icon_title = title; }
                }
            }
            7 => {
                // OSC 7;file://host/absolute/path, emitted at shell prompts.
                // Restore semicolons in paths split by the VTE OSC parser.
                let mut raw = Vec::new();
                for (i, segment) in params[1..].iter().enumerate() {
                    if i != 0 { raw.push(b';'); }
                    if raw.len().saturating_add(segment.len()) > 4096 { return; }
                    raw.extend_from_slice(segment);
                }
                if let Some(cwd) = tab_naming::cwd_from_osc7(&raw) {
                    if self.cwd.as_ref() != Some(&cwd) {
                        self.cwd = Some(cwd);
                        self.tab_label_revision = self.tab_label_revision.wrapping_add(1);
                    }
                }
            }
            4 => {
                // OSC 4 accepts multiple index;spec pairs in one string.
                let mut updated = false;
                for pair in params[1..].as_chunks::<2>().0 {
                    let Some(index) = std::str::from_utf8(pair[0]).ok()
                        .and_then(|v| v.parse::<u8>().ok()) else { continue };
                    if pair[1] == b"?" {
                        self.replies.extend(osc::color_reply(4, Some(index), self.palette.indexed(index)));
                    } else if let Some(color) = osc::parse_color(pair[1]) {
                        self.palette.set_indexed(index, color);
                        updated = true;
                    }
                }
                if updated { self.refresh_palette(); }
            }
            10..=12 => {
                let Some(arg) = params.get(1) else { return };
                if *arg == b"?" {
                    let color = match code {
                        10 => self.palette.foreground,
                        11 => self.palette.background,
                        _ => self.palette.cursor,
                    };
                    self.replies.extend(osc::color_reply(code, None, color));
                } else if let Some(color) = osc::parse_color(arg) {
                    self.set_osc_color(code, color);
                }
            }
            52 => {
                if self.osc52_write_allowed && self.clipboard_writes.len() < 8 {
                    if let (Some(selection), Some(payload)) = (params.get(1), params.get(2)) {
                        if let Some(text) = osc::clipboard_write(selection, payload) {
                            self.clipboard_writes.push(text);
                        }
                    }
                }
            }
            104 => {
                // Without indices, OSC 104 resets the complete 256-color
                // palette to the configured initial values.
                if params.len() == 1 || params[1..].iter().all(|p| p.is_empty()) {
                    self.palette.ansi = self.initial_palette.ansi;
                    self.palette.extended = self.initial_palette.extended;
                    self.refresh_palette();
                } else {
                    let mut updated = false;
                    for arg in &params[1..] {
                        if let Some(index) = std::str::from_utf8(arg).ok()
                            .and_then(|v| v.parse::<u8>().ok()) {
                            self.palette.set_indexed(index, self.initial_palette.indexed(index));
                            updated = true;
                        }
                    }
                    if updated { self.refresh_palette(); }
                }
            }
            110..=112 => self.reset_osc_color(code),
            133 => {
                // OSC 133;E;<program> provides a validated executable name.
                // Existing A/B/C/D capture markers retain their semantics.
                if params.get(1).is_some_and(|part| part.len() == 1 && part[0] == b'E') {
                    // Require exactly one metadata parameter: no injected
                    // semicolons or unexpected escape-sequence fragments.
                    if params.len() == 3 {
                        if let Some(program) = tab_naming::program_from_osc133(params[2]) {
                            if self.foreground_program.as_deref() != Some(program.as_str()) {
                                self.foreground_program = Some(program);
                                self.tab_label_revision = self.tab_label_revision.wrapping_add(1);
                            }
                        }
                    }
                } else if let Some(marker) = params.get(1).and_then(|part| part.first()) {
                    if matches!(*marker, b'A' | b'D') && self.foreground_program.take().is_some() {
                        self.tab_label_revision = self.tab_label_revision.wrapping_add(1);
                    }
                    if *marker == b'B' { self.prompt_seen = true; }
                    self.command_capture.marker(*marker);
                }
            },
            _ => {} // Never run unknown OSC commands or window operations.
        }
    }

    /// Drain pending BEL (0x07) notifications. Keep parser state separate
    /// from UI sound and urgency handling, and bound flood output.
    pub fn take_bells(&mut self) -> u8 {
        std::mem::take(&mut self.pending_bells)
    }

    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    pub fn selected_text(&self, selection: &Selection) -> String {
        selection.text(self)
    }

    /// DECRQM (CSI ? Ps $ p / CSI Ps $ p): report the current state
    /// of recognized modes without accidentally modifying the terminal.
    /// 1 = set, 2 = reset, 0 = unknown/unsupported.
    fn report_mode(&mut self, mode: u16, private: bool) {
        let value = if private {
            match mode {
                1 => Some(self.app_cursor),
                5 => Some(self.reverse_video),
                6 => Some(self.origin_mode),
                7 => Some(self.auto_wrap),
                25 => Some(self.show_cursor),
                9 => Some(self.mouse_tracking == MouseTracking::X10),
                1000 => Some(self.mouse_tracking == MouseTracking::Button),
                1002 => Some(self.mouse_tracking == MouseTracking::ButtonMotion),
                1003 => Some(self.mouse_tracking == MouseTracking::AnyMotion),
                1004 => Some(self.focus_reporting),
                1006 => Some(self.mouse_sgr),
                47 | 1047 | 1049 => Some(self.in_alternate_screen()),
                2004 => Some(self.bracketed_paste),
                _ => None,
            }
        } else {
            match mode {
                2 => Some(self.keyboard_lock),
                4 => Some(self.insert_mode),
                20 => Some(self.newline_mode),
                _ => None,
            }
        };
        let state = match value {
            Some(true) => 1,
            Some(false) => 2,
            None => 0,
        };
        let prefix = if private { "?" } else { "" };
        self.replies.extend_from_slice(
            format!("\x1b[{prefix}{mode};{state}$y").as_bytes(),
        );
    }

    /// DECSTR (CSI ! p) resets cursor modes and attributes without clearing
    /// the grid, scrollback, or an application's alternate-screen content.
    fn soft_reset(&mut self) {
        self.origin_mode = false;
        self.auto_wrap = true;
        self.insert_mode = false;
        self.newline_mode = false;
        self.keyboard_lock = false;
        self.app_cursor = false;
        self.app_keypad = false;
        self.reverse_video = false;
        self.show_cursor = true;
        self.cursor_style = self.default_cursor_style;
        self.cursor_blink_on = true;
        self.mouse_tracking = MouseTracking::Off;
        self.mouse_sgr = false;
        self.focus_reporting = false;
        self.bracketed_paste = false;
        self.style = TextStyle::default();
        // DECSTR restores the initial designations and invokes ASCII G0.
        self.charsets = [CharacterSet::Ascii; 4];
        self.active_charset = 0;
        self.fg = self.palette.foreground;
        self.bg = self.palette.background;
        self.fg_source = ColorSource::DefaultForeground;
        self.bg_source = ColorSource::DefaultBackground;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows;
        self.wrap_pending = false;
        self.home_cursor();
        self.save_cursor();
    }

    fn set_standard_mode(&mut self, mode: u16, enabled: bool) {
        match mode {
            2 => self.keyboard_lock = enabled, // KAM
            4 => self.insert_mode = enabled,  // IRM
            20 => self.newline_mode = enabled, // LNM
            _ => {}
        }
    }

    fn set_private_mode(&mut self, mode: u16, enabled: bool) {
        match mode {
            1 => self.app_cursor = enabled, // DECCKM
            5 => self.reverse_video = enabled, // DECSCNM
            6 => { // DECOM
                self.origin_mode = enabled;
                self.home_cursor();
            }
            7 => { // DECAWM
                self.auto_wrap = enabled;
                if !enabled {
                    self.wrap_pending = false;
                }
            }
            25 => self.show_cursor = enabled,
            1004 => self.focus_reporting = enabled,
            // Existing exclusive mouse mode selection.
            9 | 1000 | 1002 | 1003 => {
                self.mouse_tracking = if enabled {
                    match mode {
                        9 => MouseTracking::X10,
                        1000 => MouseTracking::Button,
                        1002 => MouseTracking::ButtonMotion,
                        1003 => MouseTracking::AnyMotion,
                        _ => unreachable!(),
                    }
                } else {
                    MouseTracking::Off
                };
            }
            1006 => self.mouse_sgr = enabled,
            47 | 1047 => self.set_alternate(enabled),
            1048 => {
                if enabled {
                    self.save_cursor();
                } else {
                    self.restore_cursor();
                }
            }
            1049 => {
                if enabled {
                    if !self.in_alternate_screen() {
                        self.saved_primary_cursor = Some(self.snapshot_cursor());
                        self.set_alternate(true);
                        self.home_cursor();
                    }
                } else if self.in_alternate_screen() {
                    self.set_alternate(false);
                    if let Some(snapshot) = self.saved_primary_cursor.take() {
                        self.restore_snapshot(snapshot);
                    }
                }
            }
            2004 => self.bracketed_paste = enabled,
            _ => {} // Unsupported DEC modes must not change unrelated state.
        }
    }

}

impl Perform for Screen {
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        self.handle_osc(params);
    }

    fn print(&mut self, c: char) {
        // Capture displayed Unicode, including DEC line-drawing mappings,
        // not the original ASCII aliases or raw VT escape sequences.
        let visible = self.charsets[self.active_charset].map(c);
        self.command_capture.print(visible);
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        self.command_capture.control(byte);
        match byte {
            0x07 => self.pending_bells = self.pending_bells.saturating_add(1),
            0x0e => self.active_charset = 1, // SO / LS1: invoke G1
            0x0f => self.active_charset = 0, // SI / LS0: invoke G0
            b'\r' => {
                self.x = 0;
                self.wrap_pending = false;
            }
            b'\n' | 0x0b | 0x0c => {
                if self.newline_mode {
                    self.x = 0;
                }
                self.linefeed();
            }
            0x08 => {
                self.x = self.x.saturating_sub(1);
                self.wrap_pending = false;
            }
            b'\t' => self.tab(1),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let values: Vec<u16> = params
            .iter()
            .map(|value| value.first().copied().unwrap_or(0))
            .collect();
        let n = |index: usize| values.get(index).copied().unwrap_or(0).max(1) as usize;
        // DECSCUSR has a literal SPACE intermediate (0x20) followed by q.
        // Do not mistake a plain CSI q or an unrelated CSI intermediate for it.
        if intermediates == b" " && action == 'q' {
            self.set_decscusr(values.first().copied().unwrap_or(0));
            return;
        }
        let private = intermediates.contains(&b'?');
        if action == 'p' && intermediates.contains(&b'$') {
            for mode in values.iter().copied() {
                self.report_mode(mode, private);
            }
            return;
        }
        if action == 'p' && intermediates.contains(&b'!') && !private {
            self.soft_reset();
            return;
        }
        if private {
            if action == 'h' || action == 'l' {
                for mode in values.iter().copied() {
                    self.set_private_mode(mode, action == 'h');
                }
            }
            return;
        }
        if action == 'h' || action == 'l' {
            for mode in values.iter().copied() {
                self.set_standard_mode(mode, action == 'h');
            }
            return;
        }
        match action {
            'A' => self.relative_y(-(n(0) as i32)),
            'B' | 'e' => self.relative_y(n(0) as i32),
            'C' | 'a' => self.move_to(self.x.saturating_add(n(0)), self.y),
            'D' => self.move_to(self.x.saturating_sub(n(0)), self.y),
            'E' => {
                self.move_to(0, self.y.saturating_add(n(0)));
            }
            'F' => {
                self.move_to(0, self.y.saturating_sub(n(0)));
            }
            'G' | '\u{60}' => self.move_to(n(0) - 1, self.y),
            'd' => self.move_to_origin(self.x, n(0) - 1),
            'H' | 'f' => self.move_to_origin(n(1) - 1, n(0) - 1),
            'I' => self.tab(n(0) as i32), // CHT
            'Z' => self.tab(-(n(0) as i32)), // CBT
            'g' => match values.first().copied().unwrap_or(0) {
                0 => self.tab_stops[self.x] = false,
                3 => self.tab_stops.fill(false),
                _ => {}
            },
            'J' => self.erase_display(values.first().copied().unwrap_or(0)),
            'K' => {
                let mode = values.first().copied().unwrap_or(0);
                self.command_capture.erase_line(mode);
                self.erase_line(mode);
            },
            'm' => self.sgr(if values.is_empty() { &[0] } else { &values }),
            'S' => self.scroll_up(n(0).min(self.rows)),
            'T' => self.scroll_down(n(0).min(self.rows)),
            'r' => {
                let top = (n(0) - 1).min(self.rows - 1);
                let bottom = if values.get(1).copied().unwrap_or(0) == 0 {
                    self.rows
                } else {
                    n(1).min(self.rows)
                };
                if top < bottom {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom;
                    self.home_cursor();
                }
            }
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            'P' => {
                self.wrap_pending = false;
                let x = self.x.min(self.cols - 1);
                let blank = self.blank();
                let count = n(0).min(self.cols - x);
                let line = &mut self.grid[self.y];
                line.copy_within(x + count..self.cols, x);
                line[self.cols - count..].fill(blank);
            }
            '@' => {
                self.wrap_pending = false;
                let x = self.x.min(self.cols - 1);
                let blank = self.blank();
                let count = n(0).min(self.cols - x);
                let line = &mut self.grid[self.y];
                line.copy_within(x..self.cols - count, x + count);
                line[x..x + count].fill(blank);
            }
            'X' => {
                self.wrap_pending = false;
                let blank = self.blank();
                let x = self.x.min(self.cols - 1);
                let end = (x + n(0)).min(self.cols);
                self.grid[self.y][x..end].fill(blank);
            }
            'L' => {
                self.wrap_pending = false;
                if (self.scroll_top..self.scroll_bottom).contains(&self.y) {
                    for _ in 0..n(0).min(self.scroll_bottom - self.y) {
                        self.grid.remove(self.scroll_bottom - 1);
                        let row = self.empty_row();
                        self.grid.insert(self.y, row);
                    }
                }
            }
            'M' => {
                self.wrap_pending = false;
                if (self.scroll_top..self.scroll_bottom).contains(&self.y) {
                    for _ in 0..n(0).min(self.scroll_bottom - self.y) {
                        self.grid.remove(self.y);
                        let row = self.empty_row();
                        self.grid.insert(self.scroll_bottom - 1, row);
                    }
                }
            }
            'n' => match values.first().copied().unwrap_or(0) {
                5 => self.replies.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let reported_y = if self.origin_mode {
                        self.y - self.scroll_top + 1
                    } else {
                        self.y + 1
                    };
                    self.replies.extend_from_slice(
                        format!("\x1b[{};{}R", reported_y, self.x + 1).as_bytes()
                    );
                }
                _ => {}
            },
            'c' => self.replies.extend_from_slice(b"\x1b[?1;2c"),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, action: u8) {
        if ignore {
            return;
        }
        if intermediates == b"#" {
            if action == b'8' {
                // DECALN: fill the current screen with E, matching st's
                // screen alignment test, without changing cursor/margins.
                let (fg, bg, fg_source, bg_source) = self.ink_colors_with_sources();
                let cell = Cell {
                    ch: 'E',
                    width: 1,
                    fg,
                    bg,
                    fg_source,
                    bg_source,
                    style: self.style,
                    wrapped: false,
            combining: ['\0'; 8],
            combining_len: 0,
            output_id: 0,
                };
                for row in &mut self.grid {
                    row.fill(cell);
                }
                self.view_offset = 0;
            }
            return;
        }
        // ESC ( / ) / * / + designates a set into G0/G1/G2/G3.
        // vte delivers the designator as one escape intermediate and the
        // following 0 or B as the final byte, even across PTY read chunks.
        if intermediates.len() == 1 {
            let slot = match intermediates[0] {
                b'(' => Some(0),
                b')' => Some(1),
                b'*' => Some(2),
                b'+' => Some(3),
                _ => None,
            };
            if let Some(slot) = slot {
                if let Some(charset) = CharacterSet::designate(action) {
                    self.charsets[slot] = charset;
                }
                return;
            }
        }
        if !intermediates.is_empty() {
            return;
        }
        match action {
            b'n' => self.active_charset = 2, // LS2: invoke G2
            b'o' => self.active_charset = 3, // LS3: invoke G3
            b'=' => self.app_keypad = true,  // DECKPAM
            b'>' => self.app_keypad = false, // DECKPNM
            b'D' => self.linefeed(),
            b'E' => {
                self.x = 0;
                self.linefeed();
            }
            b'M' => {
                self.wrap_pending = false;
                if self.y == self.scroll_top {
                    self.scroll_down(1);
                } else {
                    self.y = self.y.saturating_sub(1);
                }
            }
            b'H' => { // HTS
                self.tab_stops[self.x] = true;
            }
            b'7' => self.save_cursor(), // DECSC
            b'8' => self.restore_cursor(), // DECRC
            b'c' => {
                let cols = self.cols;
                let rows = self.rows;
                let limit = self.history_limit;
                let palette = self.initial_palette.clone();
                let osc52_allowed = self.osc52_write_allowed;
                let default_cursor_style = self.default_cursor_style;
                // A BEL before RIS in the same PTY read still rings; the
                // reset must not discard unconsumed UI notifications.
                let pending_bells = self.pending_bells;
                let command_capture = std::mem::take(&mut self.command_capture);
                *self = Self::new(cols, rows, limit, palette);
                self.command_capture = command_capture;
                self.pending_bells = pending_bells;
                self.osc52_write_allowed = osc52_allowed;
                self.set_default_cursor_style(default_cursor_style);
            }
            _ => {}
        }
    }
}

// Enough for a base64-encoded 64 KiB OSC 52 clipboard payload.
const MAX_OSC_BYTES: usize = 96 * 1024;

pub struct Terminal {
    parser: Parser<MAX_OSC_BYTES>,
    pub screen: Screen,
}

impl Terminal {
    pub fn new(cols: usize, rows: usize, history_limit: usize, palette: Palette) -> Self {
        Self {
            parser: Parser::new_with_size(),
            screen: Screen::new(cols, rows, history_limit, palette),
        }
    }

    pub fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.screen, bytes);
    }
}

#[cfg(test)]
#[path = "vt_modes_tests.rs"]
mod vt_modes_tests;

#[cfg(test)]
#[path = "osc_tests.rs"]
mod osc_tests;

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod cursor_tests;

#[cfg(test)]
#[path = "charset_tests.rs"]
mod charset_tests;

#[cfg(test)]
#[path = "attention_tests.rs"]
mod attention_tests;

#[cfg(test)]
#[path = "grapheme_tests.rs"]
mod grapheme_tests;

#[cfg(test)]
#[path = "command_capture_tests.rs"]
mod command_capture_tests;

#[cfg(test)]
#[path = "copy_feedback_tests.rs"]
mod copy_feedback_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(cols: usize, rows: usize) -> Terminal {
        let colors: crate::config::Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        Terminal::new(cols, rows, 10, Palette::try_from(&colors.colors).unwrap())
    }

    #[test]
    fn cr_and_csi_cursor_movement() {
        let mut term = terminal(10, 3);
        term.advance(b"hello\rX\x1b[2;4H!");
        assert_eq!(term.screen.grid[0][0].ch, 'X');
        assert_eq!(term.screen.grid[1][3].ch, '!');
    }

    #[test]
    fn background_color_erase_preserves_active_background() {
        let mut term = terminal(10, 3);
        term.advance(b"\x1b[48;2;12;34;56m\x1b[2K");
        assert_eq!(term.screen.grid[0][0].bg, 0x0c2238);
        assert_eq!(term.screen.grid[0][9].bg, 0x0c2238);
        term.advance(b"\x1b[0m\x1b[2;1H\x1b[2K");
        assert_eq!(term.screen.grid[1][0].bg, term.screen.palette.background);
    }

    #[test]
    fn truecolor_sgr() {
        let mut term = terminal(10, 3);
        term.advance(b"\x1b[38;2;12;34;56mR");
        assert_eq!(term.screen.grid[0][0].fg, 0x0c2238);
    }

    #[test]
    fn scrollback_keeps_scrolled_lines() {
        let mut term = terminal(4, 2);
        term.advance(b"a\r\nb\r\nc");
        assert_eq!(term.screen.history.len(), 1);
        assert_eq!(term.screen.history[0][0].ch, 'a');
        assert_eq!(term.screen.grid[0][0].ch, 'b');
    }

    #[test]
    fn deckpam_deckpnm_and_reset_restore_numeric_keypad() {
        let mut term = terminal(8, 3);
        assert!(!term.screen.app_keypad);
        term.advance(b"\x1b=");
        assert!(term.screen.app_keypad);
        term.advance(b"\x1b>");
        assert!(!term.screen.app_keypad);
        term.advance(b"\x1b=\x1bc");
        assert!(!term.screen.app_keypad);
    }

    #[test]
    fn alternate_screen_state_is_visible_to_keyboard_shortcuts() {
        let mut term = terminal(8, 3);
        assert!(!term.screen.in_alternate_screen());
        term.advance(b"\x1b[?1049h");
        assert!(term.screen.in_alternate_screen());
        term.advance(b"\x1b[?1049l");
        assert!(!term.screen.in_alternate_screen());
    }

    #[test]
    fn dec_mouse_modes_are_exclusive_and_sgr_is_independent() {
        let mut term = terminal(80, 24);
        assert_eq!(term.screen.mouse_tracking, MouseTracking::Off);
        assert!(!term.screen.mouse_sgr);

        term.advance(b"\x1b[?1000;1006h");
        assert_eq!(term.screen.mouse_tracking, MouseTracking::Button);
        assert!(term.screen.mouse_sgr);

        term.advance(b"\x1b[?1002h");
        assert_eq!(term.screen.mouse_tracking, MouseTracking::ButtonMotion);
        term.advance(b"\x1b[?1003h");
        assert_eq!(term.screen.mouse_tracking, MouseTracking::AnyMotion);
        term.advance(b"\x1b[?1003l");
        assert_eq!(term.screen.mouse_tracking, MouseTracking::Off);
        assert!(term.screen.mouse_sgr);

        term.advance(b"\x1b[?9h");
        assert_eq!(term.screen.mouse_tracking, MouseTracking::X10);
        term.advance(b"\x1b[?1006l");
        assert!(!term.screen.mouse_sgr);
        assert_eq!(term.screen.mouse_tracking, MouseTracking::X10);
        term.advance(b"\x1b[?9l");
        assert_eq!(term.screen.mouse_tracking, MouseTracking::Off);
    }

    #[test]
    fn mouse_mode_reset_and_alternate_screen_switch() {
        let mut term = terminal(80, 24);
        term.advance(b"\x1b[?1002;1006h\x1b[?1049h");
        assert_eq!(term.screen.mouse_tracking, MouseTracking::ButtonMotion);
        assert!(term.screen.mouse_sgr);
        term.advance(b"\x1b[?1049l");
        assert_eq!(term.screen.mouse_tracking, MouseTracking::ButtonMotion);

        term.advance(b"\x1bc");
        assert_eq!(term.screen.mouse_tracking, MouseTracking::Off);
        assert!(!term.screen.mouse_sgr);
    }

    #[test]
    fn alternate_screen_restores_primary() {
        let mut term = terminal(8, 3);
        term.advance(b"hello\x1b[?1049hworld\x1b[?1049l");
        assert_eq!(term.screen.grid[0][0].ch, 'h');
    }

    #[test]
    fn sgr_attributes_are_persisted_per_cell() {
        let mut term = terminal(20, 3);
        term.advance(b"\x1b[1;2;3;4;5;7;8;9mA\x1b[23;24;28;29mB");
        let first = term.screen.grid[0][0];
        let second = term.screen.grid[0][1];
        for flag in [
            TextStyle::BOLD, TextStyle::FAINT, TextStyle::ITALIC,
            TextStyle::UNDERLINE, TextStyle::BLINK, TextStyle::REVERSE,
            TextStyle::INVISIBLE, TextStyle::STRUCK,
        ] {
            assert!(first.style.contains(flag), "missing SGR flag {:?}", flag);
        }
        for flag in [
            TextStyle::ITALIC, TextStyle::UNDERLINE,
            TextStyle::INVISIBLE, TextStyle::STRUCK,
        ] {
            assert!(!second.style.contains(flag), "SGR reset must clear {:?}", flag);
        }
        assert!(second.style.contains(TextStyle::BOLD));
        assert!(second.style.contains(TextStyle::FAINT));
        assert!(second.style.contains(TextStyle::BLINK));
        assert!(second.style.contains(TextStyle::REVERSE));
    }

    #[test]
    fn sgr_22_clears_bold_and_faint_without_touching_other_styles() {
        let mut term = terminal(20, 3);
        term.advance(b"\x1b[1;2;3mA\x1b[22mB");
        let before = term.screen.grid[0][0].style;
        let after = term.screen.grid[0][1].style;
        assert!(before.contains(TextStyle::BOLD));
        assert!(before.contains(TextStyle::FAINT));
        assert!(!after.contains(TextStyle::BOLD));
        assert!(!after.contains(TextStyle::FAINT));
        assert!(after.contains(TextStyle::ITALIC));
    }

    #[test]
    fn sgr_25_and_29_clear_blink_and_strikethrough() {
        let mut term = terminal(20, 3);
        term.advance(b"\x1b[6;9mA\x1b[25mB\x1b[29mC");
        assert!(term.screen.grid[0][0].style.contains(TextStyle::BLINK));
        assert!(term.screen.grid[0][0].style.contains(TextStyle::STRUCK));
        assert!(!term.screen.grid[0][1].style.contains(TextStyle::BLINK));
        assert!(term.screen.grid[0][1].style.contains(TextStyle::STRUCK));
        assert!(!term.screen.grid[0][2].style.contains(TextStyle::STRUCK));
    }

    #[test]
    fn sgr_zero_and_empty_sgr_reset_all_attributes_and_colors() {
        let mut term = terminal(20, 3);
        term.advance(b"\x1b[3;4;8;38;2;12;34;56;48;5;21mA\x1b[mB");
        let changed = term.screen.grid[0][0];
        let reset = term.screen.grid[0][1];
        assert_eq!(changed.fg, 0x0c2238);
        assert!(changed.style.contains(TextStyle::ITALIC));
        assert!(changed.style.contains(TextStyle::UNDERLINE));
        assert!(changed.style.contains(TextStyle::INVISIBLE));
        assert_eq!(reset.fg, term.screen.palette.foreground);
        assert_eq!(reset.bg, term.screen.palette.background);
        assert_eq!(reset.style, TextStyle::default());
        term.advance(b"\x1b[1;5m\x1b[0mC");
        assert_eq!(term.screen.grid[0][2].style, TextStyle::default());
    }

    #[test]
    fn bold_ansi_brightening_reverse_colors_and_reset() {
        let mut term = terminal(20, 3);
        term.advance(b"\x1b[31;1mA\x1b[7mB\x1b[27;22mC");
        let first = term.screen.grid[0][0];
        let reversed = term.screen.grid[0][1];
        let reset = term.screen.grid[0][2];
        assert_eq!(first.fg, term.screen.palette.ansi[9]);
        assert!(first.style.contains(TextStyle::BOLD));
        assert_eq!(reversed.fg, term.screen.palette.background);
        assert_eq!(reversed.bg, term.screen.palette.ansi[9]);
        assert!(reversed.style.contains(TextStyle::REVERSE));
        assert!(!reset.style.contains(TextStyle::BOLD));
        assert!(!reset.style.contains(TextStyle::REVERSE));
        assert_eq!(reset.fg, term.screen.palette.ansi[1]);
    }

    #[test]
    fn erased_cells_preserve_bce_but_drop_decorations() {
        let mut term = terminal(8, 3);
        term.advance(b"\x1b[4;9;48;2;12;34;56mA\x1b[2K");
        for cell in &term.screen.grid[0] {
            assert_eq!(cell.bg, 0x0c2238);
            assert_eq!(cell.style, TextStyle::default());
        }
        term.advance(b"B");
        assert!(term.screen.grid[0][1].style.contains(TextStyle::UNDERLINE));
        assert!(term.screen.grid[0][1].style.contains(TextStyle::STRUCK));
    }

    #[test]
    fn sgr_styles_survive_scrollback_and_alternate_screen() {
        let mut term = terminal(8, 2);
        term.advance(b"\x1b[3;4mA\r\nB\r\nC");
        assert!(term.screen.history[0][0].style.contains(TextStyle::ITALIC));
        assert!(term.screen.history[0][0].style.contains(TextStyle::UNDERLINE));
        term.advance(b"\x1b[?1049h\x1b[9mZ\x1b[?1049l");
        assert!(term.screen.grid[0][0].style.contains(TextStyle::ITALIC));
        assert!(!term.screen.grid[0][0].style.contains(TextStyle::STRUCK));
        term.advance(b"\x1bcD");
        assert_eq!(term.screen.grid[0][0].style, TextStyle::default());
    }

    #[test]
    fn text_blink_visibility_is_independent_from_cursor_and_only_when_needed() {
        let mut term = terminal(8, 2);
        assert!(!term.screen.has_blinking_text());
        term.advance(b"\x1b[5mX\x1b[25mY");
        assert!(term.screen.has_blinking_text());
        term.screen.show_cursor = false;
        term.screen.text_blink_on = false;
        assert!(!term.screen.grid[0][0].style.visible(term.screen.text_blink_on));
        assert!(term.screen.grid[0][1].style.visible(term.screen.text_blink_on));
        term.screen.text_blink_on = true;
        assert!(term.screen.grid[0][0].style.visible(term.screen.text_blink_on));
        term.advance(b"\x1b[2J");
        assert!(!term.screen.has_blinking_text());
    }

}

#[cfg(test)]
#[path = "robustness_tests.rs"]
mod robustness_tests;

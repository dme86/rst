use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;
use vte::{Params, Parser, Perform};

use crate::{config::Colors, mouse::MouseTracking};

/// Compact, per-cell SGR attributes. Like st's ATTR_* bitmask, these must
/// travel with each glyph through scrollback, insert/delete, and alternate
/// screen swaps instead of living only in the current cursor state.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub width: u8,
    pub fg: u32,
    pub bg: u32,
    pub style: TextStyle,
}

#[derive(Clone)]
pub struct Palette {
    pub foreground: u32,
    pub background: u32,
    pub cursor: u32,
    pub selection: u32,
    pub ansi: [u32; 16],
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
        })
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
    // Blink attribute (SGR 5/6) animates independently of the cursor.
    pub text_blink_on: bool,
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub mouse_tracking: MouseTracking,
    pub mouse_sgr: bool,
    pub bracketed_paste: bool,
    pub focus_reporting: bool,
    pub keyboard_lock: bool,
    pub reverse_video: bool,
    pub origin_mode: bool,
    pub auto_wrap: bool,
    pub insert_mode: bool,
    pub newline_mode: bool,
    pub palette: Palette,
    fg: u32,
    bg: u32,
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
}

impl Screen {
    pub fn new(cols: usize, rows: usize, history_limit: usize, palette: Palette) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let blank = Cell {
            ch: ' ',
            width: 1,
            fg: palette.foreground,
            bg: palette.background,
            style: TextStyle::default(),
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
            text_blink_on: true,
            app_cursor: false,
            app_keypad: false,
            mouse_tracking: MouseTracking::Off,
            mouse_sgr: false,
            bracketed_paste: false,
            focus_reporting: false,
            keyboard_lock: false,
            reverse_video: false,
            origin_mode: false,
            auto_wrap: true,
            insert_mode: false,
            newline_mode: false,
            fg: palette.foreground,
            bg: palette.background,
            style: TextStyle::default(),
            palette,
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
                style: TextStyle::default(),
                origin_mode: false,
            },
            saved_primary_cursor: None,
            alternate: None,
            replies: Vec::new(),
        }
    }

    fn snapshot_cursor(&self) -> CursorSnapshot {
        CursorSnapshot {
            x: self.x.min(self.cols - 1),
            y: self.y.min(self.rows - 1),
            wrap_pending: self.wrap_pending,
            fg: self.fg,
            bg: self.bg,
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
        self.fg = saved.fg;
        self.bg = saved.bg;
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
        let (fg, bg) = self.ink_colors();
        Cell {
            ch: ' ',
            width: 1,
            fg,
            bg,
            // Erased cells retain BCE colors, not the formatting of the
            // glyph that was previously at this location.
            style: TextStyle::default(),
        }
    }

    fn ink_colors(&self) -> (u32, u32) {
        let bright_fg = if self.style.contains(TextStyle::BOLD) {
            self.palette.ansi[..8]
                .iter()
                .position(|&color| color == self.fg)
                .map(|index| self.palette.ansi[index + 8])
                .unwrap_or(self.fg)
        } else {
            self.fg
        };
        if self.style.contains(TextStyle::REVERSE) {
            (self.bg, bright_fg)
        } else {
            (bright_fg, self.bg)
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
            if self.scroll_top == 0 && self.scroll_bottom == self.rows && self.alternate.is_none() {
                if self.history_limit > 0 {
                    self.history.push_back(removed);
                    while self.history.len() > self.history_limit {
                        self.history.pop_front();
                    }
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
        for row in &mut self.grid {
            row.resize(cols, blank);
        }
        if rows > self.rows {
            self.grid.extend(vec![vec![blank; cols]; rows - self.rows]);
        } else {
            self.grid.truncate(rows);
        }
        if let Some(saved) = &mut self.alternate {
            for row in &mut saved.grid {
                row.resize(cols, blank);
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
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width == 0 || width > 2 {
            // TODO: merge combining marks into grapheme clusters.
            return;
        }
        // A one-column terminal cannot hold a double-width glyph. Using a
        // single-cell replacement prevents the cursor leaving the grid.
        let (ch, width) = if width > self.cols { ('?', 1) } else { (ch, width) };
        // DECAWM wraps on the next printable glyph, not immediately after
        // painting the last column (the delayed-wrap behavior from st).
        if self.wrap_pending {
            if self.auto_wrap {
                self.x = 0;
                self.linefeed();
            }
            self.wrap_pending = false;
        }
        if self.x + width > self.cols {
            if self.auto_wrap {
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
        let (fg, bg) = self.ink_colors();
        self.grid[self.y][self.x] = Cell {
            ch,
            width: width as u8,
            fg,
            bg,
            style: self.style,
        };
        if width == 2 && self.x + 1 < self.cols {
            self.grid[self.y][self.x + 1] = Cell {
                ch: ' ',
                width: 0,
                fg,
                bg,
                style: self.style,
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
            3 => self.history.clear(),
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
                30..=37 => self.fg = self.palette.ansi[(params[i] - 30) as usize],
                40..=47 => self.bg = self.palette.ansi[(params[i] - 40) as usize],
                90..=97 => self.fg = self.palette.ansi[(params[i] - 90 + 8) as usize],
                100..=107 => self.bg = self.palette.ansi[(params[i] - 100 + 8) as usize],
                39 => self.fg = self.palette.foreground,
                49 => self.bg = self.palette.background,
                38 | 48 => {
                    let is_fg = params[i] == 38;
                    let value = if params.get(i + 1) == Some(&2) && i + 4 < params.len() {
                        i += 4;
                        Some(
                            ((params[i - 2].min(255) as u32) << 16)
                                | ((params[i - 1].min(255) as u32) << 8)
                                | params[i].min(255) as u32,
                        )
                    } else if params.get(i + 1) == Some(&5) && i + 2 < params.len() {
                        i += 2;
                        Some(self.indexed_color(params[i] as u8))
                    } else {
                        None
                    };
                    if let Some(color) = value {
                        if is_fg {
                            self.fg = color;
                        } else {
                            self.bg = color;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn indexed_color(&self, index: u8) -> u32 {
        match index {
            0..=15 => self.palette.ansi[index as usize],
            16..=231 => {
                let i = index as u32 - 16;
                let channel = |v: u32| if v == 0 { 0 } else { 55 + 40 * v };
                (channel(i / 36) << 16)
                    | (channel((i / 6) % 6) << 8)
                    | channel(i % 6)
            }
            _ => {
                let gray = 8 + 10 * (index as u32 - 232);
                (gray << 16) | (gray << 8) | gray
            }
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

    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    pub fn selected_text(&self, start: (usize, usize), end: (usize, usize)) -> String {
        let (mut start, mut end) = (start, end);
        if (start.1, start.0) > (end.1, end.0) {
            std::mem::swap(&mut start, &mut end);
        }
        let mut out = String::new();
        for y in start.1..=end.1.min(self.rows - 1) {
            if y >= self.rows {
                break;
            }
            let first = if y == start.1 { start.0 } else { 0 };
            let last = if y == end.1 { end.0 + 1 } else { self.cols };
            let line = self.display_row(y);
            let value = line[first.min(self.cols)..last.min(self.cols)]
                .iter()
                .filter(|cell| cell.width != 0)
                .map(|cell| cell.ch)
                .collect::<String>();
            out.push_str(value.trim_end());
            if y != end.1 {
                out.push('\n');
            }
        }
        out
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
        self.mouse_tracking = MouseTracking::Off;
        self.mouse_sgr = false;
        self.focus_reporting = false;
        self.bracketed_paste = false;
        self.style = TextStyle::default();
        self.fg = self.palette.foreground;
        self.bg = self.palette.background;
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
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
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
            'K' => self.erase_line(values.first().copied().unwrap_or(0)),
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
                let (fg, bg) = self.ink_colors();
                let cell = Cell {
                    ch: 'E',
                    width: 1,
                    fg,
                    bg,
                    style: self.style,
                };
                for row in &mut self.grid {
                    row.fill(cell);
                }
                self.view_offset = 0;
            }
            return;
        }
        if !intermediates.is_empty() {
            return;
        }
        match action {
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
                let palette = self.palette.clone();
                *self = Self::new(cols, rows, limit, palette);
            }
            _ => {}
        }
    }
}

pub struct Terminal {
    parser: Parser,
    pub screen: Screen,
}

impl Terminal {
    pub fn new(cols: usize, rows: usize, history_limit: usize, palette: Palette) -> Self {
        Self {
            parser: Parser::new(),
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

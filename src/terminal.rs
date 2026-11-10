use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;
use vte::{Params, Parser, Perform};

use crate::config::Colors;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub width: u8,
    pub fg: u32,
    pub bg: u32,
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
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub palette: Palette,
    fg: u32,
    bg: u32,
    bold: bool,
    inverse: bool,
    history_limit: usize,
    scroll_top: usize,
    scroll_bottom: usize,
    saved_cursor: (usize, usize),
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
            app_cursor: false,
            bracketed_paste: false,
            fg: palette.foreground,
            bg: palette.background,
            bold: false,
            inverse: false,
            palette,
            history_limit,
            scroll_top: 0,
            scroll_bottom: rows,
            saved_cursor: (0, 0),
            alternate: None,
            replies: Vec::new(),
        }
    }

    fn blank(&self) -> Cell {
        Cell {
            ch: ' ',
            width: 1,
            fg: self.palette.foreground,
            bg: self.palette.background,
        }
    }

    fn empty_row(&self) -> Vec<Cell> {
        vec![self.blank(); self.cols]
    }

    fn linefeed(&mut self) {
        if self.y + 1 == self.scroll_bottom {
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
        self.cols = cols;
        self.rows = rows;
        self.x = self.x.min(cols - 1);
        self.y = self.y.min(rows - 1);
        self.scroll_top = 0;
        self.scroll_bottom = rows;
        self.view_offset = 0;
    }

    fn put_char(&mut self, ch: char) {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width == 0 || width > 2 {
            // TODO: merge combining marks into a grapheme cluster.
            return;
        }
        if self.x + width > self.cols {
            self.x = 0;
            self.linefeed();
        }
        let (fg, bg) = if self.inverse {
            (self.bg, self.fg)
        } else {
            (self.fg, self.bg)
        };
        self.grid[self.y][self.x] = Cell {
            ch,
            width: width as u8,
            fg,
            bg,
        };
        if width == 2 && self.x + 1 < self.cols {
            self.grid[self.y][self.x + 1] = Cell {
                ch: ' ',
                width: 0,
                fg,
                bg,
            };
        }
        self.x += width;
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
                    self.bold = false;
                    self.inverse = false;
                }
                1 => self.bold = true,
                22 => self.bold = false,
                7 => self.inverse = true,
                27 => self.inverse = false,
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
            });
            self.x = 0;
            self.y = 0;
            self.view_offset = 0;
        } else if !enable {
            if let Some(saved) = self.alternate.take() {
                self.grid = saved.grid;
                self.x = saved.x;
                self.y = saved.y;
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

    fn set_private_mode(&mut self, mode: u16, enabled: bool) {
        match mode {
            1 => self.app_cursor = enabled,
            25 => self.show_cursor = enabled,
            47 | 1047 | 1049 => self.set_alternate(enabled),
            2004 => self.bracketed_paste = enabled,
            _ => {}
        }
    }
}

impl Perform for Screen {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => self.x = 0,
            b'\n' | 0x0b | 0x0c => self.linefeed(),
            0x08 => self.x = self.x.saturating_sub(1),
            b'\t' => {
                let next = (self.x / 8 + 1) * 8;
                self.x = next.min(self.cols - 1);
            }
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
        if private {
            if action == 'h' || action == 'l' {
                for mode in values.iter().copied() {
                    self.set_private_mode(mode, action == 'h');
                }
            }
            return;
        }
        match action {
            'A' => self.y = self.y.saturating_sub(n(0)),
            'B' => self.y = (self.y + n(0)).min(self.rows - 1),
            'C' => self.x = (self.x + n(0)).min(self.cols - 1),
            'D' => self.x = self.x.saturating_sub(n(0)),
            'E' => {
                self.y = (self.y + n(0)).min(self.rows - 1);
                self.x = 0;
            }
            'F' => {
                self.y = self.y.saturating_sub(n(0));
                self.x = 0;
            }
            'G' => self.x = (n(0) - 1).min(self.cols - 1),
            'd' => self.y = (n(0) - 1).min(self.rows - 1),
            'H' | 'f' => {
                self.y = (n(0) - 1).min(self.rows - 1);
                self.x = (n(1) - 1).min(self.cols - 1);
            }
            'J' => self.erase_display(values.first().copied().unwrap_or(0)),
            'K' => self.erase_line(values.first().copied().unwrap_or(0)),
            'm' => self.sgr(&values),
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
                    self.x = 0;
                    self.y = 0;
                }
            }
            's' => self.saved_cursor = (self.x, self.y),
            'u' => {
                self.x = self.saved_cursor.0.min(self.cols - 1);
                self.y = self.saved_cursor.1.min(self.rows - 1);
            }
            'P' => {
                let x = self.x.min(self.cols - 1);
                let blank = self.blank();
                let count = n(0).min(self.cols - x);
                let line = &mut self.grid[self.y];
                line.copy_within(x + count..self.cols, x);
                line[self.cols - count..].fill(blank);
            }
            '@' => {
                let x = self.x.min(self.cols - 1);
                let blank = self.blank();
                let count = n(0).min(self.cols - x);
                let line = &mut self.grid[self.y];
                line.copy_within(x..self.cols - count, x + count);
                line[x..x + count].fill(blank);
            }
            'X' => {
                let blank = self.blank();
                let x = self.x.min(self.cols - 1);
                let end = (x + n(0)).min(self.cols);
                self.grid[self.y][x..end].fill(blank);
            }
            'L' => {
                if (self.scroll_top..self.scroll_bottom).contains(&self.y) {
                    for _ in 0..n(0).min(self.scroll_bottom - self.y) {
                        self.grid.remove(self.scroll_bottom - 1);
                        let row = self.empty_row();
                        self.grid.insert(self.y, row);
                    }
                }
            }
            'M' => {
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
                6 => self
                    .replies
                    .extend_from_slice(format!("\x1b[{};{}R", self.y + 1, self.x + 1).as_bytes()),
                _ => {}
            },
            'c' => self.replies.extend_from_slice(b"\x1b[?1;2c"),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], ignore: bool, action: u8) {
        if ignore {
            return;
        }
        match action {
            b'D' => self.linefeed(),
            b'E' => {
                self.x = 0;
                self.linefeed();
            }
            b'M' => {
                if self.y == self.scroll_top {
                    self.scroll_down(1);
                } else {
                    self.y = self.y.saturating_sub(1);
                }
            }
            b'7' => self.saved_cursor = (self.x, self.y),
            b'8' => {
                self.x = self.saved_cursor.0.min(self.cols - 1);
                self.y = self.saved_cursor.1.min(self.rows - 1);
            }
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
    fn alternate_screen_restores_primary() {
        let mut term = terminal(8, 3);
        term.advance(b"hello\x1b[?1049hworld\x1b[?1049l");
        assert_eq!(term.screen.grid[0][0].ch, 'h');
    }
}

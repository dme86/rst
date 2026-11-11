//! st-inspired mouse selections (snap-to-word, snap-to-line, Alt rectangle).
//! Selection geometry and text extraction share a single source of truth,
//! so the clipboard receives exactly the highlighted cells.

use std::time::{Duration, Instant};

use crate::terminal::Screen;

pub type Point = (usize, usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    Linear,
    Rectangular,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionSnap {
    Character,
    Word,
    Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Point,
    pub head: Point,
    pub kind: SelectionKind,
    pub snap: SelectionSnap,
    alternate: bool,
    view_offset: usize,
    dragged: bool,
}

fn order(a: Point, b: Point) -> (Point, Point) {
    if (a.1, a.0) <= (b.1, b.0) {
        (a, b)
    } else {
        (b, a)
    }
}

fn canonical(screen: &Screen, point: Point) -> Point {
    let y = point.1.min(screen.rows - 1);
    let row = screen.display_row(y);
    let mut x = point.0.min(screen.cols - 1);
    // A double-width glyph owns the following dummy cell.
    while x > 0 && row[x].width == 0 {
        x -= 1;
    }
    (x, y)
}

fn next_glyph(screen: &Screen, point: Point) -> Option<Point> {
    let (mut x, mut y) = point;
    loop {
        if x + 1 < screen.cols {
            x += 1;
        } else {
            if y + 1 >= screen.rows || !screen.display_row(y)[screen.cols - 1].wrapped {
                return None;
            }
            x = 0;
            y += 1;
        }
        if screen.display_row(y)[x].width != 0 {
            return Some((x, y));
        }
    }
}

fn previous_glyph(screen: &Screen, point: Point) -> Option<Point> {
    let (mut x, mut y) = point;
    loop {
        if x > 0 {
            x -= 1;
        } else {
            if y == 0 || !screen.display_row(y - 1)[screen.cols - 1].wrapped {
                return None;
            }
            x = screen.cols - 1;
            y -= 1;
        }
        if screen.display_row(y)[x].width != 0 {
            return Some((x, y));
        }
    }
}

fn word_bounds(screen: &Screen, at: Point) -> (Point, Point) {
    let at = canonical(screen, at);
    let ch = screen.display_row(at.1)[at.0].ch;
    // Vanilla st's default worddelimiters is a single space. Paths and
    // punctuation therefore remain in the same word unless configured
    // otherwise; only identical delimiter characters form a run.
    let delimiter = ch == ' ';
    let mut left = at;
    let mut right = at;
    while let Some(prev) = previous_glyph(screen, left) {
        let next_ch = screen.display_row(prev.1)[prev.0].ch;
        if (next_ch == ' ') != delimiter || (delimiter && next_ch != ch) {
            break;
        }
        left = prev;
    }
    while let Some(next) = next_glyph(screen, right) {
        let next_ch = screen.display_row(next.1)[next.0].ch;
        if (next_ch == ' ') != delimiter || (delimiter && next_ch != ch) {
            break;
        }
        right = next;
    }
    // Include both display cells of a wide glyph, not only its lead cell.
    let row = screen.display_row(right.1);
    if row[right.0].width == 2 && right.0 + 1 < screen.cols {
        right.0 += 1;
    }
    (left, right)
}

fn line_bounds(screen: &Screen, at: Point) -> (Point, Point) {
    let mut top = at.1.min(screen.rows - 1);
    let mut bottom = top;
    // A physical row with a soft-wrap marker belongs to the same logical
    // line, including when it is visible in scrollback.
    while top > 0 && screen.display_row(top - 1)[screen.cols - 1].wrapped {
        top -= 1;
    }
    while bottom + 1 < screen.rows && screen.display_row(bottom)[screen.cols - 1].wrapped {
        bottom += 1;
    }
    ((0, top), (screen.cols - 1, bottom))
}

impl Selection {
    pub fn new(screen: &Screen, at: Point, snap: SelectionSnap, rectangular: bool) -> Self {
        let at = canonical(screen, at);
        Self {
            anchor: at,
            head: at,
            kind: if rectangular { SelectionKind::Rectangular } else { SelectionKind::Linear },
            snap,
            alternate: screen.in_alternate_screen(),
            view_offset: screen.view_offset,
            dragged: false,
        }
    }

    pub fn update(&mut self, screen: &Screen, at: Point, rectangular: bool) {
        let at = canonical(screen, at);
        if at != self.anchor {
            self.dragged = true;
        }
        self.head = at;
        self.kind = if rectangular { SelectionKind::Rectangular } else { SelectionKind::Linear };
    }

    fn bounds(&self, screen: &Screen) -> Option<(Point, Point)> {
        if self.alternate != screen.in_alternate_screen() || self.view_offset != screen.view_offset {
            return None;
        }
        if self.kind == SelectionKind::Rectangular {
            let a = canonical(screen, self.anchor);
            let b = canonical(screen, self.head);
            if !self.dragged && self.snap == SelectionSnap::Character {
                return None;
            }
            return Some(((a.0.min(b.0), a.1.min(b.1)),
                         (a.0.max(b.0), a.1.max(b.1))));
        }
        let (start, end) = order(canonical(screen, self.anchor), canonical(screen, self.head));
        match self.snap {
            SelectionSnap::Character => {
                if !self.dragged {
                    None // A single click places the selection anchor only.
                } else {
                    Some((start, end))
                }
            }
            SelectionSnap::Word => {
                let (first, _) = word_bounds(screen, start);
                let (_, last) = word_bounds(screen, end);
                Some((first, last))
            }
            SelectionSnap::Line => {
                let (first, _) = line_bounds(screen, start);
                let (_, last) = line_bounds(screen, end);
                Some((first, last))
            }
        }
    }

    pub fn contains(&self, screen: &Screen, x: usize, y: usize) -> bool {
        let Some((start, end)) = self.bounds(screen) else { return false };
        if y < start.1 || y > end.1 {
            return false;
        }
        if self.kind == SelectionKind::Rectangular {
            // Match either column of a double-wide glyph so the visible
            // character remains highlighted if a rectangle intersects it.
            let width = screen.display_row(y)[x].width;
            let right = if width == 2 { x.saturating_add(1) } else { x };
            return right >= start.0 && x <= end.0;
        }
        (y, x) >= (start.1, start.0) && (y, x) <= (end.1, end.0)
    }

    pub fn text(&self, screen: &Screen) -> String {
        let Some((start, end)) = self.bounds(screen) else { return String::new() };
        let mut result = String::new();
        for y in start.1..=end.1 {
            let row = screen.display_row(y);
            let (first, last) = if self.kind == SelectionKind::Rectangular {
                (start.0, end.0)
            } else {
                (if y == start.1 { start.0 } else { 0 },
                 if y == end.1 { end.0 } else { screen.cols - 1 })
            };
            let mut text = String::new();
            // A rectangle can intersect the trailing cell of a wide glyph.
            // Scan one column to the left and include its owner when needed.
            let scan_from = if self.kind == SelectionKind::Rectangular {
                first.saturating_sub(1)
            } else {
                first
            };
            for x in scan_from..=last {
                let cell = row[x];
                if cell.width == 0 {
                    continue;
                }
                if x < first && !(self.kind == SelectionKind::Rectangular
                    && cell.width == 2 && x + 1 >= first)
                {
                    continue;
                }
                text.push_str(&cell.text());
            }
            result.push_str(text.trim_end_matches(' '));
            let wrapped = row[screen.cols - 1].wrapped;
            if y != end.1 {
                if self.kind == SelectionKind::Rectangular || !wrapped {
                    result.push('\n');
                }
            } else if self.kind == SelectionKind::Linear
                && (self.snap == SelectionSnap::Line || last == screen.cols - 1)
                && !wrapped
            {
                // st includes the terminating newline for fully selected
                // logical lines, but not for a soft-wrapped continuation.
                result.push('\n');
            }
        }
        result
    }
}

#[derive(Debug, Default)]
pub struct ClickTracker {
    last: Option<(Instant, Point, bool)>,
    previous: Option<(Instant, Point, bool)>,
}

impl ClickTracker {
    pub fn reset(&mut self) {
        self.last = None;
        self.previous = None;
    }

    pub fn press(&mut self, now: Instant, at: Point, rectangular: bool) -> SelectionSnap {
        const DOUBLE: Duration = Duration::from_millis(300);
        const TRIPLE: Duration = Duration::from_millis(600);
        let same_position = |other: &(Instant, Point, bool)| {
            other.1.1 == at.1
                && other.1.0.abs_diff(at.0) <= 1
                && other.2 == rectangular
        };
        let near_last = self.last.as_ref().is_some_and(|old| {
            same_position(old) && now.saturating_duration_since(old.0) <= DOUBLE
        });
        let triple = near_last && self.previous.as_ref().is_some_and(|old| {
            same_position(old) && now.saturating_duration_since(old.0) <= TRIPLE
        });
        if triple {
            self.reset();
            return SelectionSnap::Line;
        }
        self.previous = if near_last { self.last } else { None };
        self.last = Some((now, at, rectangular));
        if near_last { SelectionSnap::Word } else { SelectionSnap::Character }
    }
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;

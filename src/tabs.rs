//! Layout decisions for the optional native tab bar.
//! Painting belongs to render.rs; PTY/session management belongs to App.

pub const TAB_COLUMNS: usize = 20;

#[derive(Clone, Debug)]
pub struct TabDisplay {
    pub title: String,
    pub active: bool,
    pub unread: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabAction {
    Select(usize),
    Close(usize),
    New,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BarLayout {
    pub first: usize,
    pub count: usize,
    pub slot_columns: usize,
    pub widths: Vec<usize>,
}

impl BarLayout {
    pub fn for_width(total: usize, active_index: usize, columns: usize) -> Self {
        // Keep a small right margin for +; when the window is too narrow,
        // keyboard shortcuts remain usable and the active tab stays visible.
        let slot_columns = TAB_COLUMNS.min(columns.saturating_sub(3).max(1));
        let capacity = columns.saturating_sub(4).checked_div(slot_columns)
            .unwrap_or(0).max(1);
        let count = total.min(capacity);
        let first = active_index.saturating_sub(count.saturating_sub(1))
            .min(total.saturating_sub(count));
        Self { first, count, slot_columns, widths: vec![slot_columns; count] }
    }

    pub fn for_items(items: &[TabDisplay], active: usize, columns: usize, close: bool, powerline: bool) -> Self {
        use unicode_width::UnicodeWidthStr;
        let budget = columns.saturating_sub(4).max(1);
        let desired = |index: usize| {
            let title = if items[index].title.is_empty() { "shell" } else { &items[index].title };
            (title.width() + (index + 1).to_string().len() + 3
                + if close { 3 } else { 0 } + if powerline { 2 } else { 0 }).min(40)
        };
        if items.is_empty() { return Self::for_width(0, 0, columns); }
        let active = active.min(items.len() - 1);
        let mut first = 0;
        loop {
            let mut widths = Vec::new();
            let mut remaining = budget;
            for index in first..items.len() {
                let width = desired(index).min(budget);
                if width > remaining { break; }
                widths.push(width);
                remaining -= width;
            }
            if active < first + widths.len() {
                return Self { first, count: widths.len(), slot_columns: 0, widths };
            }
            first += 1;
        }
    }

    pub fn hit_test(
        &self,
        column: usize,
        columns: usize,
        close_buttons: bool,
    ) -> Option<TabAction> {
        if column == 0 || column >= columns {
            return None;
        }
        let relative = column - 1;
        let mut start = 0;
        for (slot, &width) in self.widths.iter().enumerate() {
            if relative < start + width {
                return Some(if close_buttons && width >= 7 && relative - start == width - 2 {
                    TabAction::Close(self.first + slot)
                } else { TabAction::Select(self.first + slot) });
            }
            start += width;
        }
        if relative == start && column + 1 < columns { Some(TabAction::New) } else { None }
    }
}

/// Neighbor selection is independent of session storage order. Tabs in
/// the inactive-session Vec may be swap_removed without affecting the UI.
pub fn relative_tab(order: &[u64], active: u64, step: isize) -> Option<u64> {
    if order.len() < 2 { return None; }
    let current = order.iter().position(|id| *id == active)?;
    let next = (current as isize + step).rem_euclid(order.len() as isize) as usize;
    order.get(next).copied()
}

pub fn successor_after_close(order: &[u64], active: u64) -> Option<u64> {
    if order.len() < 2 { return None; }
    let current = order.iter().position(|id| *id == active)?;
    let next = if current + 1 < order.len() { current + 1 } else { current - 1 };
    order.get(next).copied()
}

pub fn bar_height(cell_height: usize, enabled: bool) -> usize {
    if enabled { cell_height.saturating_add(6) } else { 0 }
}

/// Validate configurable theme colors without involving system libraries.
pub fn parse_color(value: &str) -> Option<u32> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

/// Cached native status: no shell commands and at most one clock wakeup per minute.
pub struct TabStatus {
    pub date: String,
    pub time: String,
    pub hostname: String,
    pub next_refresh: std::time::Instant,
}

impl TabStatus {
    pub fn new() -> Self {
        let mut bytes = [0_u8; 256];
        // SAFETY: gethostname writes only within this initialized buffer.
        let hostname = if unsafe { libc::gethostname(bytes.as_mut_ptr().cast(), bytes.len()) } == 0 {
            let length = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
            String::from_utf8_lossy(&bytes[..length]).chars()
                .filter(|ch| !ch.is_control()).take(48).collect()
        } else { "localhost".into() };
        let mut status = Self { date: String::new(), time: String::new(), hostname,
            next_refresh: std::time::Instant::now() };
        status.refresh(std::time::Instant::now());
        status
    }

    pub fn refresh(&mut self, now: std::time::Instant) -> bool {
        if now < self.next_refresh { return false; }
        let epoch = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        self.next_refresh = now + std::time::Duration::from_secs(60 - epoch.as_secs() % 60)
            - std::time::Duration::from_nanos(u64::from(epoch.subsec_nanos()));
        let seconds = epoch.as_secs() as libc::time_t;
        let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
        // SAFETY: localtime_r receives valid input and output pointers; its return
        // value is checked before reading the initialized output.
        if unsafe { libc::localtime_r(&seconds, local.as_mut_ptr()) }.is_null() { return false; }
        let local = unsafe { local.assume_init() };
        let date = format!("{:04}-{:02}-{:02}", local.tm_year + 1900, local.tm_mon + 1, local.tm_mday);
        let time = format!("{:02}:{:02}", local.tm_hour, local.tm_min);
        let changed = self.date != date || self.time != time;
        self.date = date; self.time = time;
        changed
    }

    /// Keep room for at least one tab and +; hide status first on narrow windows.
    pub fn segments(&self, columns: usize) -> Vec<&str> {
        use unicode_width::UnicodeWidthStr;
        let mut parts = vec![self.date.as_str(), self.time.as_str(), self.hostname.as_str()];
        while !parts.is_empty() && parts.iter().map(|part| part.width() + 3).sum::<usize>()
            + TAB_COLUMNS + 4 > columns {
            parts.remove(0);
        }
        parts
    }

    pub fn columns(&self, columns: usize) -> usize {
        use unicode_width::UnicodeWidthStr;
        self.segments(columns).iter().map(|part| part.width() + 3).sum()
    }
}

impl Default for TabStatus { fn default() -> Self { Self::new() } }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reserves_space_and_ignores_clicks() {
        let status = TabStatus {date: "2026-11-12".into(), time: "12:10".into(),
            hostname: "workstation".into(), next_refresh: std::time::Instant::now()};
        assert!(status.segments(20).is_empty());
        assert_eq!(status.segments(80).len(), 3);
        let columns = 80 - status.columns(80);
        let layout = BarLayout::for_width(5, 4, columns);
        assert!(layout.first <= 4 && 4 < layout.first + layout.count);
        for column in columns..80 { assert_eq!(layout.hit_test(column, columns, true), None); }
        let mut native = TabStatus::new();
        assert!(!native.time.is_empty());
        assert!(!native.refresh(std::time::Instant::now()));
    }

    #[test]
    fn dynamic_tabs_fit_titles_and_keep_click_targets_aligned() {
        let items: Vec<_> = ["~", "long-directory-name", "fish"].iter().map(|title|
            TabDisplay { title: (*title).into(), active: false, unread: false }).collect();
        let layout = BarLayout::for_items(&items, 0, 100, true, true);
        assert!(layout.widths[0] < layout.widths[1]);
        let mut column = 1;
        for (slot, width) in layout.widths.iter().enumerate() {
            assert_eq!(layout.hit_test(column, 100, true), Some(TabAction::Select(slot)));
            assert_eq!(layout.hit_test(column + width - 2, 100, true), Some(TabAction::Close(slot)));
            column += width;
        }
        assert_eq!(layout.hit_test(column, 100, true), Some(TabAction::New));
        for columns in 1..100 {
            let layout = BarLayout::for_items(&items, 2, columns, true, true);
            assert!(layout.first <= 2 && 2 < layout.first + layout.count);
            assert!(layout.widths.iter().sum::<usize>() <= columns.saturating_sub(4).max(1));
        }
    }

    #[test]
    fn active_tab_remains_visible_when_list_overflows() {
        let layout = BarLayout::for_width(10, 8, 70);
        assert_eq!(layout.count, 3);
        assert!(layout.first <= 8);
        assert!(8 < layout.first + layout.count);
        assert_eq!(layout.hit_test(1, 70, true), Some(TabAction::Select(layout.first)));
        assert_eq!(layout.hit_test(19, 70, true), Some(TabAction::Close(layout.first)));
        assert_eq!(layout.hit_test(61, 70, true), Some(TabAction::New));
    }

    #[test]
    fn hit_testing_works_at_narrow_widths_and_outside_bar() {
        let layout = BarLayout::for_width(3, 2, 5);
        assert_eq!(layout.count, 1);
        assert_eq!(layout.first, 2);
        assert_eq!(layout.hit_test(0, 5, true), None);
        assert_eq!(layout.hit_test(1, 5, true), Some(TabAction::Select(2)));
        assert_eq!(layout.hit_test(2, 5, true), Some(TabAction::Select(2)));
        assert_eq!(layout.hit_test(4, 5, true), None);
        assert_eq!(bar_height(18, false), 0);
        assert_eq!(bar_height(18, true), 24);
    }

    #[test]
    fn tab_order_survives_switch_and_close_and_wraps() {
        let order = [12, 3, 27, 2];
        assert_eq!(relative_tab(&order, 3, 1), Some(27));
        assert_eq!(relative_tab(&order, 12, -1), Some(2));
        assert_eq!(relative_tab(&order, 2, 1), Some(12));
        assert_eq!(relative_tab(&order, 27, 0), Some(27));
        assert_eq!(relative_tab(&order, 99, 1), None);
        assert_eq!(successor_after_close(&order, 3), Some(27));
        assert_eq!(successor_after_close(&order, 2), Some(27));
        assert_eq!(successor_after_close(&[12], 12), None);
        assert_eq!(successor_after_close(&order, 99), None);
    }

    #[test]
    fn configurable_colors_are_strict_hex() {
        assert_eq!(parse_color("#fabd2f"), Some(0xfabd2f));
        assert_eq!(parse_color("#282828"), Some(0x282828));
        assert_eq!(parse_color("fabd2f"), None);
        assert_eq!(parse_color("#123"), None);
        assert_eq!(parse_color("#zzzzzz"), None);
    }
}

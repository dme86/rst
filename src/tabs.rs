//! Minimal, allocation-free layout decisions for the optional native tab bar.
//! Painting belongs to render.rs; PTY/session management belongs to App.

pub const TAB_COLUMNS: usize = 16;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BarLayout {
    pub first: usize,
    pub count: usize,
    pub slot_columns: usize,
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
        Self { first, count, slot_columns }
    }

    pub fn hit_test(
        self,
        column: usize,
        columns: usize,
        close_buttons: bool,
    ) -> Option<TabAction> {
        if column == 0 {
            return None;
        }
        let relative = column - 1;
        let bar_slots = self.count.saturating_mul(self.slot_columns);
        if relative < bar_slots {
            let index = self.first + relative / self.slot_columns;
            let within = relative % self.slot_columns;
            if close_buttons && self.slot_columns >= 7 && within == self.slot_columns - 2 {
                Some(TabAction::Close(index))
            } else {
                Some(TabAction::Select(index))
            }
        } else if relative == bar_slots && column + 1 < columns {
            Some(TabAction::New)
        } else {
            None
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_tab_remains_visible_when_list_overflows() {
        let layout = BarLayout::for_width(10, 8, 70);
        assert_eq!(layout.count, 4);
        assert!(layout.first <= 8);
        assert!(8 < layout.first + layout.count);
        assert_eq!(layout.hit_test(1, 70, true), Some(TabAction::Select(layout.first)));
        assert_eq!(layout.hit_test(15, 70, true), Some(TabAction::Close(layout.first)));
        assert_eq!(layout.hit_test(65, 70, true), Some(TabAction::New));
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

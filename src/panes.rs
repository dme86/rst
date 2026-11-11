//! Per-tab split trees and physical geometry, independent of PTYs/window APIs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}
impl Rect {
    pub fn contains(self, x: usize, y: usize) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x.saturating_add(self.width)
            && y < self.y.saturating_add(self.height)
    }
    pub fn grid(self, padding: usize, cell_width: usize, cell_height: usize) -> (usize, usize) {
        (
            (self.width.saturating_sub(padding.saturating_mul(2)) / cell_width.max(1)).max(1),
            (self.height.saturating_sub(padding.saturating_mul(2)) / cell_height.max(1)).max(1),
        )
    }
}

pub const MAX_PANES: usize = 8;
pub const DIVIDER: usize = 2;

#[derive(Clone, Debug)]
pub enum Node {
    Leaf(u64),
    Split {
        axis: Axis,
        first: Box<Node>,
        second: Box<Node>,
    },
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub root: Node,
    pub focused: u64,
    pub zoomed: bool,
}
impl Layout {
    pub fn new(id: u64) -> Self {
        Self {
            root: Node::Leaf(id),
            focused: id,
            zoomed: false,
        }
    }
    pub fn panes(&self, rect: Rect) -> Vec<(u64, Rect)> {
        let mut panes = Vec::new();
        self.root.place(rect, &mut panes);
        panes
    }
    pub fn toggle_zoom(&mut self) -> bool {
        if self.ids().len() < 2 {
            return false;
        }
        self.zoomed = !self.zoomed;
        true
    }
    pub fn rail_rect(&self, rect: Rect, rail_width: usize) -> Option<Rect> {
        if !self.zoomed || self.ids().len() < 2 {
            return None;
        }
        let width = rail_width.min(rect.width / 3);
        if width == 0 {
            return None;
        }
        Some(Rect {
            x: rect.x + rect.width - width,
            width,
            ..rect
        })
    }
    pub fn visible_panes(&self, rect: Rect, rail_width: usize) -> Vec<(u64, Rect)> {
        if !self.zoomed {
            return self.panes(rect);
        }
        let focus = if let Some(rail) = self.rail_rect(rect, rail_width) {
            let gap = DIVIDER.min(rect.width.saturating_sub(rail.width + 1));
            Rect {
                width: rect.width - rail.width - gap,
                ..rect
            }
        } else {
            rect
        };
        vec![(self.focused, focus)]
    }
    /// Collapsed PTYs keep their normal geometry; they are never resized
    /// into the tiny sidebar. Only the enlarged pane changes grid size.
    pub fn resize_panes(&self, rect: Rect, rail_width: usize) -> Vec<(u64, Rect)> {
        let mut panes = self.panes(rect);
        if self.zoomed {
            let expanded = self.visible_panes(rect, rail_width)[0].1;
            for (id, pane) in &mut panes {
                if *id == self.focused {
                    *pane = expanded;
                }
            }
        }
        panes
    }
    /// None is the restore button; remaining entries select a collapsed pane.
    pub fn rail_entries(
        &self,
        rect: Rect,
        rail_width: usize,
        row_height: usize,
    ) -> Vec<(Option<u64>, Rect)> {
        let Some(rail) = self.rail_rect(rect, rail_width) else {
            return Vec::new();
        };
        let mut ids = vec![None];
        ids.extend(
            self.ids()
                .into_iter()
                .filter(|id| *id != self.focused)
                .map(Some),
        );
        let height = row_height.min(rail.height / ids.len());
        ids.into_iter()
            .enumerate()
            .map(|(index, id)| {
                (
                    id,
                    Rect {
                        y: rail.y + index * height,
                        height,
                        ..rail
                    },
                )
            })
            .collect()
    }
    pub fn ids(&self) -> Vec<u64> {
        self.panes(Rect::default())
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }
    pub fn contains(&self, id: u64) -> bool {
        self.root.contains(id)
    }
    pub fn split(&mut self, id: u64, new_id: u64, axis: Axis) -> bool {
        if self.ids().len() >= MAX_PANES || self.contains(new_id) {
            return false;
        }
        if self.root.split(id, new_id, axis) {
            self.focused = new_id;
            self.zoomed = false;
            true
        } else {
            false
        }
    }
    pub fn remove(&mut self, id: u64) -> bool {
        let ids = self.ids();
        if ids.len() == 1 || !ids.contains(&id) {
            return false;
        }
        self.root.remove(id);
        if ids.len() == 2 {
            self.zoomed = false;
        }
        if self.focused == id {
            let at = ids.iter().position(|item| *item == id).unwrap();
            self.focused = if at + 1 < ids.len() {
                ids[at + 1]
            } else {
                ids[at - 1]
            };
        }
        true
    }
    pub fn neighbor(&self, id: u64, direction: Direction, rect: Rect) -> Option<u64> {
        let panes = self.panes(rect);
        let current = panes.iter().find(|(pane, _)| *pane == id)?.1;
        let cx = current.x as i64 * 2 + current.width as i64;
        let cy = current.y as i64 * 2 + current.height as i64;
        panes
            .iter()
            .filter(|(candidate, r)| *candidate != id && r.width > 0 && r.height > 0)
            .filter_map(|(candidate, r)| {
                let dx = r.x as i64 * 2 + r.width as i64 - cx;
                let dy = r.y as i64 * 2 + r.height as i64 - cy;
                let (forward, lateral) = match direction {
                    Direction::Left => (-dx, dy.abs()),
                    Direction::Right => (dx, dy.abs()),
                    Direction::Up => (-dy, dx.abs()),
                    Direction::Down => (dy, dx.abs()),
                };
                (forward > 0).then_some((*candidate, forward + lateral * 2))
            })
            .min_by_key(|(_, distance)| *distance)
            .map(|(candidate, _)| candidate)
    }
}
impl Node {
    fn contains(&self, id: u64) -> bool {
        match self {
            Self::Leaf(pane) => *pane == id,
            Self::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }
    fn split(&mut self, id: u64, new_id: u64, axis: Axis) -> bool {
        match self {
            Self::Leaf(pane) if *pane == id => {
                *self = Self::Split {
                    axis,
                    first: Box::new(Self::Leaf(id)),
                    second: Box::new(Self::Leaf(new_id)),
                };
                true
            }
            Self::Split { first, second, .. } => {
                first.split(id, new_id, axis) || second.split(id, new_id, axis)
            }
            _ => false,
        }
    }
    fn remove(&mut self, id: u64) {
        if let Self::Split { first, second, .. } = self {
            if matches!(first.as_ref(), Self::Leaf(pane) if *pane == id) {
                *self = *second.clone();
            } else if matches!(second.as_ref(), Self::Leaf(pane) if *pane == id) {
                *self = *first.clone();
            } else {
                first.remove(id);
                second.remove(id);
            }
        }
    }
    fn place(&self, rect: Rect, panes: &mut Vec<(u64, Rect)>) {
        match self {
            Self::Leaf(id) => panes.push((*id, rect)),
            Self::Split {
                axis,
                first,
                second,
            } => {
                let length = match axis {
                    Axis::Vertical => rect.width,
                    Axis::Horizontal => rect.height,
                };
                let gap = DIVIDER.min(length);
                let available = length - gap;
                let a = available / 2;
                let b = available - a;
                let (one, two) = match axis {
                    Axis::Vertical => (
                        Rect { width: a, ..rect },
                        Rect {
                            x: rect.x + a + gap,
                            width: b,
                            ..rect
                        },
                    ),
                    Axis::Horizontal => (
                        Rect { height: a, ..rect },
                        Rect {
                            y: rect.y + a + gap,
                            height: b,
                            ..rect
                        },
                    ),
                };
                first.place(one, panes);
                second.place(two, panes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixed_splits_cover_no_overlapping_pixels_and_map_mouse_coordinates() {
        let mut layout = Layout::new(1);
        assert!(layout.split(1, 2, Axis::Vertical));
        assert!(layout.split(2, 3, Axis::Horizontal));
        let rect = Rect {
            x: 3,
            y: 30,
            width: 101,
            height: 72,
        };
        let panes = layout.panes(rect);
        assert_eq!(
            panes.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        for (i, (_, pane)) in panes.iter().enumerate() {
            assert!(pane.width > 0 && pane.height > 0);
            assert!(rect.contains(pane.x, pane.y));
            assert!(rect.contains(pane.x + pane.width - 1, pane.y + pane.height - 1));
            for (_, other) in &panes[i + 1..] {
                assert!(!pane.contains(other.x, other.y));
                assert!(!other.contains(pane.x, pane.y));
            }
        }
        assert_eq!(layout.neighbor(1, Direction::Right, rect), Some(2));
        assert_eq!(layout.neighbor(2, Direction::Down, rect), Some(3));
        assert_eq!(layout.neighbor(3, Direction::Up, rect), Some(2));
        assert_eq!(layout.neighbor(3, Direction::Left, rect), Some(1));
    }
    #[test]
    fn closing_panes_collapses_branches_and_preserves_focus() {
        let mut layout = Layout::new(1);
        layout.split(1, 2, Axis::Vertical);
        layout.split(2, 3, Axis::Horizontal);
        assert!(layout.remove(2));
        assert_eq!(layout.ids(), vec![1, 3]);
        assert_eq!(layout.focused, 3);
        assert!(layout.remove(3));
        assert_eq!(layout.focused, 1);
        assert_eq!(layout.ids(), vec![1]);
        assert!(!layout.remove(1));
        assert!(!layout.remove(99));
    }
    #[test]
    fn tiny_windows_and_repeated_resize_never_underflow() {
        let mut layout = Layout::new(1);
        for id in 2..=8 {
            assert!(layout.split(
                id - 1,
                id,
                if id % 2 == 0 {
                    Axis::Vertical
                } else {
                    Axis::Horizontal
                }
            ));
        }
        assert!(!layout.split(8, 9, Axis::Vertical));
        for width in 0..30 {
            for height in 0..30 {
                for (_, pane) in layout.panes(Rect {
                    width,
                    height,
                    ..Rect::default()
                }) {
                    assert!(pane.x + pane.width <= width && pane.y + pane.height <= height);
                    let (cols, rows) = pane.grid(6, 10, 20);
                    assert!(cols > 0 && rows > 0);
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Split(Axis),
    Focus(Direction),
    Close,
    ToggleZoom,
}

pub fn shortcut(
    key: &winit::keyboard::Key<&str>,
    mods: winit::keyboard::ModifiersState,
    macos: bool,
) -> Option<Action> {
    use winit::keyboard::{Key, ModifiersState as M, NamedKey};
    let zoom = if macos {
        M::SUPER | M::SHIFT
    } else {
        M::CONTROL | M::SHIFT
    };
    if mods == zoom && matches!(key, Key::Character(text) if text.eq_ignore_ascii_case("f")) {
        return Some(Action::ToggleZoom);
    }
    let primary = if macos {
        M::SUPER
    } else {
        M::CONTROL | M::SHIFT
    };
    if mods == primary {
        if let Key::Character(text) = key {
            if text.eq_ignore_ascii_case("d") {
                return Some(Action::Split(Axis::Vertical));
            }
            if !macos && text.eq_ignore_ascii_case("e") {
                return Some(Action::Split(Axis::Horizontal));
            }
            if text.eq_ignore_ascii_case("w") {
                return Some(Action::Close);
            }
        }
    }
    if macos
        && mods == (M::SUPER | M::SHIFT)
        && matches!(key, Key::Character(text) if text.eq_ignore_ascii_case("d"))
    {
        return Some(Action::Split(Axis::Horizontal));
    }
    let navigation = if macos {
        M::SUPER | M::ALT
    } else {
        M::CONTROL | M::SHIFT
    };
    if mods == navigation {
        return match key {
            Key::Named(NamedKey::ArrowLeft) => Some(Action::Focus(Direction::Left)),
            Key::Named(NamedKey::ArrowRight) => Some(Action::Focus(Direction::Right)),
            Key::Named(NamedKey::ArrowUp) => Some(Action::Focus(Direction::Up)),
            Key::Named(NamedKey::ArrowDown) => Some(Action::Focus(Direction::Down)),
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod shortcut_tests {
    use super::*;
    use winit::keyboard::{Key, ModifiersState as M, NamedKey};
    #[test]
    fn split_and_focus_shortcuts_require_exact_modifiers() {
        assert_eq!(
            shortcut(&Key::Character("d"), M::SUPER, true),
            Some(Action::Split(Axis::Vertical))
        );
        assert_eq!(
            shortcut(&Key::Character("D"), M::SUPER | M::SHIFT, true),
            Some(Action::Split(Axis::Horizontal))
        );
        assert_eq!(
            shortcut(&Key::Character("D"), M::CONTROL | M::SHIFT, false),
            Some(Action::Split(Axis::Vertical))
        );
        assert_eq!(
            shortcut(&Key::Character("E"), M::CONTROL | M::SHIFT, false),
            Some(Action::Split(Axis::Horizontal))
        );
        assert_eq!(
            shortcut(&Key::Named(NamedKey::ArrowLeft), M::SUPER | M::ALT, true),
            Some(Action::Focus(Direction::Left))
        );
        assert_eq!(
            shortcut(
                &Key::Named(NamedKey::ArrowDown),
                M::CONTROL | M::SHIFT,
                false
            ),
            Some(Action::Focus(Direction::Down))
        );
        assert_eq!(
            shortcut(&Key::Character("w"), M::SUPER, true),
            Some(Action::Close)
        );
        assert_eq!(shortcut(&Key::Character("d"), M::empty(), true), None);
        assert_eq!(shortcut(&Key::Character("d"), M::CONTROL, false), None);
        assert_eq!(
            shortcut(&Key::Character("d"), M::SUPER | M::ALT, true),
            None
        );
        assert_eq!(
            shortcut(&Key::Named(NamedKey::ArrowLeft), M::ALT, true),
            None
        );
    }
    #[test]
    fn stable_tab_identity_survives_closing_original_pane() {
        let mut tabs = std::collections::BTreeMap::from([(1, Layout::new(1)), (3, Layout::new(3))]);
        tabs.get_mut(&1).unwrap().split(1, 2, Axis::Vertical);
        assert!(tabs.get_mut(&1).unwrap().remove(1));
        assert_eq!(tabs[&1].focused, 2);
        assert_eq!(tabs[&1].ids(), vec![2]);
        assert!(tabs[&3].contains(3));
        assert_eq!(
            tabs.iter()
                .find(|(_, layout)| layout.contains(2))
                .map(|(id, _)| *id),
            Some(1)
        );
    }
}

#[cfg(test)]
mod stress_tests {
    use super::*;
    #[test]
    fn repeated_split_close_and_resize_preserve_unique_ids_focus_and_bounds() {
        for seed in 1..=32u64 {
            let mut state = seed;
            let mut layout = Layout::new(1);
            let mut next_id = 2;
            for step in 0..1000 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let ids = layout.ids();
                let id = ids[state as usize % ids.len()];
                if state & 1 == 0 {
                    if layout.split(
                        id,
                        next_id,
                        if state & 2 == 0 {
                            Axis::Vertical
                        } else {
                            Axis::Horizontal
                        },
                    ) {
                        next_id += 1;
                    }
                } else {
                    layout.remove(id);
                }
                let ids = layout.ids();
                assert!(!ids.is_empty() && ids.len() <= MAX_PANES);
                assert!(ids.contains(&layout.focused));
                let mut distinct = ids.clone();
                distinct.sort_unstable();
                distinct.dedup();
                assert_eq!(distinct.len(), ids.len());
                let rect = Rect {
                    x: 5,
                    y: 23,
                    width: (state % 800) as usize,
                    height: (state % 600) as usize,
                };
                for (_, pane) in layout.panes(rect) {
                    assert!(pane.x >= rect.x && pane.y >= rect.y);
                    assert!(
                        pane.x + pane.width <= rect.x + rect.width
                            && pane.y + pane.height <= rect.y + rect.height,
                        "seed {seed}, step {step}"
                    );
                }
            }
        }
    }
}

/// Full-width panes draw straight into the window buffer. Narrow panes reuse
/// one staging allocation because the rasterizer expects contiguous scanlines.
pub struct Frame<'a> {
    pub pixels: &'a mut [u32],
    destination: Option<(&'a mut [u32], Rect, usize)>,
}
impl<'a> Frame<'a> {
    pub fn new(
        window: &'a mut [u32],
        staging: &'a mut Vec<u32>,
        stride: usize,
        rect: Rect,
    ) -> Self {
        let len = rect.width * rect.height;
        if rect.x == 0 && rect.width == stride {
            let start = rect.y * stride;
            Self {
                pixels: &mut window[start..start + len],
                destination: None,
            }
        } else {
            staging.resize(len, 0);
            Self {
                pixels: &mut staging[..len],
                destination: Some((window, rect, stride)),
            }
        }
    }
    pub fn commit(self) {
        if let Some((window, rect, stride)) = self.destination {
            for row in 0..rect.height {
                let target = (rect.y + row) * stride + rect.x;
                window[target..target + rect.width]
                    .copy_from_slice(&self.pixels[row * rect.width..(row + 1) * rect.width]);
            }
        }
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;
    #[test]
    fn full_width_pane_draws_without_a_staging_allocation() {
        let mut window = vec![1; 60];
        let mut staging = Vec::new();
        let frame = Frame::new(
            &mut window,
            &mut staging,
            10,
            Rect {
                x: 0,
                y: 2,
                width: 10,
                height: 2,
            },
        );
        frame.pixels.fill(2);
        frame.commit();
        assert!(staging.is_empty() && staging.capacity() == 0);
        assert!(window[..20].iter().all(|p| *p == 1));
        assert!(window[20..40].iter().all(|p| *p == 2));
        assert!(window[40..].iter().all(|p| *p == 1));
    }
    #[test]
    fn narrow_pane_copy_preserves_neighbors_and_tab_bar() {
        let mut window = vec![1; 60];
        let mut staging = Vec::new();
        let frame = Frame::new(
            &mut window,
            &mut staging,
            10,
            Rect {
                x: 4,
                y: 2,
                width: 3,
                height: 2,
            },
        );
        frame.pixels.fill(2);
        frame.commit();
        for y in 0..6 {
            for x in 0..10 {
                assert_eq!(
                    window[y * 10 + x],
                    if (2..4).contains(&y) && (4..7).contains(&x) {
                        2
                    } else {
                        1
                    }
                );
            }
        }
    }
}

#[cfg(test)]
mod zoom_tests {
    use super::*;
    use winit::keyboard::{Key, ModifiersState as M};
    fn layout() -> Layout {
        let mut layout = Layout::new(1);
        layout.split(1, 2, Axis::Vertical);
        layout.split(2, 3, Axis::Horizontal);
        layout
    }
    #[test]
    fn zoom_preserves_tree_and_collapsed_pty_sizes_and_restores_exact_layout() {
        let mut layout = layout();
        let rect = Rect {
            x: 5,
            y: 32,
            width: 1000,
            height: 600,
        };
        let original = layout.panes(rect);
        assert!(layout.toggle_zoom());
        assert_eq!(layout.visible_panes(rect, 160).len(), 1);
        let expanded = layout.visible_panes(rect, 160)[0];
        assert_eq!(expanded.0, 3);
        assert!(expanded.1.width > original[2].1.width);
        let resized = layout.resize_panes(rect, 160);
        assert_eq!(resized[0], original[0]);
        assert_eq!(resized[1], original[1]);
        assert_eq!(resized[2], expanded);
        assert_eq!(layout.ids(), vec![1, 2, 3]);
        layout.focused = 1;
        assert_eq!(layout.visible_panes(rect, 160)[0].0, 1);
        assert!(layout.toggle_zoom());
        assert_eq!(layout.visible_panes(rect, 160), original);
        assert_eq!(layout.resize_panes(rect, 160), original);
        assert!(layout.rail_entries(rect, 160, 32).is_empty());
    }
    #[test]
    fn sidebar_and_zoomed_pane_stay_in_bounds_when_window_is_tiny() {
        let mut layout = layout();
        layout.toggle_zoom();
        for width in 0..60 {
            for height in 0..60 {
                let rect = Rect {
                    x: 7,
                    y: 15,
                    width,
                    height,
                };
                let focused = layout.visible_panes(rect, 200)[0].1;
                assert!(focused.x + focused.width <= rect.x + width);
                assert!(focused.y + focused.height <= rect.y + height);
                if width > 0 {
                    assert!(focused.width > 0);
                }
                for (_, entry) in layout.rail_entries(rect, 200, 40) {
                    assert!(entry.x + entry.width <= rect.x + width);
                    assert!(entry.y + entry.height <= rect.y + height);
                    assert!(!focused.contains(entry.x, entry.y));
                }
            }
        }
    }
    #[test]
    fn zoom_handles_session_exit_new_splits_and_independent_tabs() {
        let mut first = layout();
        let second = layout();
        first.toggle_zoom();
        assert!(!second.zoomed);
        first.remove(3);
        assert!(first.zoomed && first.contains(first.focused));
        first.remove(2);
        assert!(!first.zoomed);
        assert!(!first.toggle_zoom());
        first.split(1, 4, Axis::Vertical);
        first.toggle_zoom();
        first.split(4, 5, Axis::Horizontal);
        assert!(!first.zoomed);
        assert_eq!(first.ids(), vec![1, 4, 5]);
    }
    #[test]
    fn zoom_uses_f_without_capturing_find_or_native_fullscreen() {
        assert_eq!(
            shortcut(&Key::Character("F"), M::SUPER | M::SHIFT, true),
            Some(Action::ToggleZoom)
        );
        assert_eq!(
            shortcut(&Key::Character("f"), M::CONTROL | M::SHIFT, false),
            Some(Action::ToggleZoom)
        );
        assert_eq!(shortcut(&Key::Character("f"), M::SUPER, true), None);
        assert_eq!(
            shortcut(&Key::Character("f"), M::SUPER | M::CONTROL, true),
            None
        );
        assert_eq!(shortcut(&Key::Character("f"), M::empty(), false), None);
    }
}

//! Legacy X10, VT200 and SGR mouse reporting, based on suckless st x.c.
//! The window backend provides cell coordinates; this module owns the
//! button state, motion filtering and exact PTY wire representation.

use winit::{
    event::{ElementState, MouseButton, MouseScrollDelta},
    keyboard::ModifiersState,
};

/// The mutually exclusive DEC tracking modes supported by upstream st.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseTracking {
    #[default]
    Off,
    X10,          // CSI ? 9 h: presses only, no modifiers
    Button,       // CSI ? 1000 h: presses, releases, wheel
    ButtonMotion, // CSI ? 1002 h: plus drag motion
    AnyMotion,    // CSI ? 1003 h: plus motion without a held button
}

impl MouseTracking {
    pub fn enabled(self) -> bool {
        self != Self::Off
    }
}

#[derive(Debug, Default)]
pub struct MouseReporter {
    // X11-style button IDs 1..=11, lowest numbered pressed first (st).
    pressed: u16,
    last_reported: Option<(usize, usize)>,
    wheel_x: f64,
    wheel_y: f64,
}

fn button_number(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(1),
        MouseButton::Middle => Some(2),
        MouseButton::Right => Some(3),
        MouseButton::Back => Some(8),
        MouseButton::Forward => Some(9),
        // Winit reports scroll wheels separately. Other numbered buttons
        // map directly to the corresponding X11 extensions when available.
        MouseButton::Other(n @ 6..=11) => Some(n as u8),
        _ => None,
    }
}

/// The low bits of Cb, exactly as calculated by st's mousereport().
fn button_code(number: u8) -> u16 {
    match number {
        1..=3 => (number - 1) as u16,
        4..=7 => 64 + (number - 4) as u16,
        8..=11 => 128 + (number - 8) as u16,
        _ => 3,
    }
}

fn modifier_bits(mods: ModifiersState, tracking: MouseTracking) -> u16 {
    if tracking == MouseTracking::X10 {
        return 0;
    }
    u16::from(mods.shift_key()) * 4
        + u16::from(mods.alt_key()) * 8
        + u16::from(mods.control_key()) * 16
}

fn encode(code: u16, at: (usize, usize), sgr: bool, released: bool) -> Vec<u8> {
    let (x, y) = at;
    if sgr {
        let suffix = if released { 'm' } else { 'M' };
        return format!("\x1b[<{code};{};{}{suffix}", x + 1, y + 1).into_bytes();
    }

    // The original X10 wire format has one byte per coordinate. As in st,
    // never silently wrap values beyond the 223rd cell.
    if x >= 223 || y >= 223 {
        return Vec::new();
    }
    vec![0x1b, b'[', b'M', (code + 32) as u8, (x + 33) as u8, (y + 33) as u8]
}

impl MouseReporter {
    pub fn dragging(&self) -> bool { self.pressed != 0 }

    /// Forget held buttons on focus loss (and any fractional scroll state).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Discard a partial wheel gesture when reporting is bypassed by Shift.
    pub fn reset_scroll(&mut self) {
        self.wheel_x = 0.0;
        self.wheel_y = 0.0;
    }

    pub fn button(
        &mut self,
        button: MouseButton,
        state: ElementState,
        at: (usize, usize),
        tracking: MouseTracking,
        sgr: bool,
        mods: ModifiersState,
    ) -> Vec<u8> {
        let Some(number) = button_number(button) else {
            return Vec::new();
        };
        let mask = 1u16 << (number - 1);
        let released = state == ElementState::Released;

        // Always account for press/release, even while Shift forces local
        // selection: a subsequent drag must not leave a button stuck.
        if released {
            self.pressed &= !mask;
        } else {
            self.pressed |= mask;
        }
        if !tracking.enabled() || mods.shift_key() {
            return Vec::new();
        }

        if released && tracking == MouseTracking::X10 {
            return Vec::new();
        }
        self.last_reported = Some(at);

        // Legacy release reports button=3; SGR preserves the released button.
        let base = if released && !sgr { 3 } else { button_code(number) };
        encode(base + modifier_bits(mods, tracking), at, sgr, released)
    }

    pub fn motion(
        &mut self,
        at: (usize, usize),
        tracking: MouseTracking,
        sgr: bool,
        mods: ModifiersState,
    ) -> Vec<u8> {
        if mods.shift_key()
            || !matches!(tracking, MouseTracking::ButtonMotion | MouseTracking::AnyMotion)
            || self.last_reported == Some(at)
        {
            return Vec::new();
        }
        if tracking == MouseTracking::ButtonMotion && self.pressed == 0 {
            return Vec::new();
        }

        self.last_reported = Some(at);
        let button = if self.pressed == 0 {
            3 // motion with no held button in 1003
        } else {
            button_code(self.pressed.trailing_zeros() as u8 + 1)
        };
        encode(32 + button + modifier_bits(mods, tracking), at, sgr, false)
    }

    pub fn scroll(
        &mut self,
        delta: MouseScrollDelta,
        cell_size: (usize, usize),
        at: (usize, usize),
        tracking: MouseTracking,
        sgr: bool,
        mods: ModifiersState,
    ) -> Vec<u8> {
        if !tracking.enabled() || mods.shift_key() {
            self.reset_scroll();
            return Vec::new();
        }
        let (x, y) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (f64::from(x), f64::from(y)),
            MouseScrollDelta::PixelDelta(pos) => (
                pos.x / cell_size.0.max(1) as f64,
                pos.y / cell_size.1.max(1) as f64,
            ),
        };
        let vertical = accumulate_steps(&mut self.wheel_y, y);
        let horizontal = accumulate_steps(&mut self.wheel_x, x);
        let modifier = modifier_bits(mods, tracking);

        // Wheel events are presses only; no release and no motion bit.
        // xterm: 64=up, 65=down, 66=left, 67=right.
        let mut out = Vec::new();
        for _ in 0..vertical.unsigned_abs() {
            out.extend(encode(if vertical > 0 { 64 } else { 65 } + modifier, at, sgr, false));
        }
        for _ in 0..horizontal.unsigned_abs() {
            out.extend(encode(if horizontal > 0 { 67 } else { 66 } + modifier, at, sgr, false));
        }
        if !out.is_empty() {
            self.last_reported = Some(at);
        }
        out
    }
}

/// Touchpads send fractional pixel deltas. Convert to whole cell-sized wheel
/// steps without losing the remainder, while capping bursts per event.
fn accumulate_steps(remainder: &mut f64, incoming: f64) -> i32 {
    if !incoming.is_finite() {
        *remainder = 0.0;
        return 0;
    }
    *remainder += incoming;
    if !remainder.is_finite() {
        *remainder = 0.0;
        return 0;
    }
    let whole = remainder.trunc().clamp(-64.0, 64.0);
    *remainder -= whole;
    whole as i32
}

#[cfg(test)]
#[path = "mouse_tests.rs"]
mod tests;

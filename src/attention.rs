//! Terminal focus and audible bell policy.
//!
//! Keep the parser/PTY side independent from native window APIs. Focus
//! transitions are filtered here, bell events are rate-limited before any
//! platform sound player is invoked, and audible output is optional.

use std::time::{Duration, Instant};

/// Xterm/st focus reporting: CSI I on entry, CSI O on leave.
pub const FOCUS_IN: &[u8] = b"\x1b[I";
pub const FOCUS_OUT: &[u8] = b"\x1b[O";

#[derive(Debug, Default)]
pub struct FocusTracker {
    last: Option<bool>,
}

impl FocusTracker {
    /// Return the PTY report only for a *real* focus transition.
    /// Changes while DECSET 1004 is off still update the tracked state.
    pub fn change(&mut self, focused: bool, reporting: bool) -> Option<&'static [u8]> {
        if self.last.replace(focused) == Some(focused) || !reporting {
            return None;
        }
        Some(if focused { FOCUS_IN } else { FOCUS_OUT })
    }

    /// On startup the window has not yet reported focus. Treat this as
    /// focused for urgency purposes, avoiding spurious Dock/compositor pings.
    pub fn is_focused(&self) -> bool {
        self.last.unwrap_or(true)
    }
}

pub const BELL_COOLDOWN: Duration = Duration::from_millis(150);
pub const VISUAL_BELL_DURATION: Duration = Duration::from_millis(160);

/// Multiple BEL bytes in one PTY read, or an application ringing in a
/// tight loop, should not overwhelm speakers or spawn unbounded processes.
#[derive(Debug, Default)]
pub struct BellLimiter {
    next_allowed: Option<Instant>,
}

impl BellLimiter {
    pub fn allow(&mut self, now: Instant) -> bool {
        if self.next_allowed.is_some_and(|deadline| now < deadline) {
            return false;
        }
        self.next_allowed = Some(now + BELL_COOLDOWN);
        true
    }
}

/// Play the system alert without introducing audio-engine dependencies.
#[cfg(target_os = "macos")]
pub fn audible_bell() {
    #[link(name = "AppKit", kind = "framework")]
    extern "C" {
        fn NSBeep();
    }
    // AppKit's built-in user-configured system alert sound, called on the
    // winit/main thread. No process launch, no embedded sound asset.
    unsafe { NSBeep() };
}

/// Wayland has no standardized bell API. Use the freedesktop sound theme
/// event via libcanberra's optional command-line player, without a shell.
/// The process is reaped off the UI thread. The visual bell works even when
/// the player is not installed or no sound theme is configured.
#[cfg(target_os = "linux")]
pub fn audible_bell() {
    std::thread::spawn(|| {
        let _ = std::process::Command::new("canberra-gtk-play")
            .args(["-i", "bell-terminal"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    });
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn audible_bell() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_reporting_only_emits_on_actual_transitions_when_enabled() {
        let mut tracker = FocusTracker::default();
        assert!(tracker.is_focused());
        assert_eq!(tracker.change(true, true), Some(FOCUS_IN));
        assert_eq!(tracker.change(true, true), None);
        assert_eq!(tracker.change(false, true), Some(FOCUS_OUT));
        assert!(!tracker.is_focused());
        assert_eq!(tracker.change(false, true), None);
        assert_eq!(tracker.change(true, true), Some(FOCUS_IN));
    }

    #[test]
    fn disabled_focus_reporting_never_emits_but_tracks_actual_focus() {
        let mut tracker = FocusTracker::default();
        assert_eq!(tracker.change(false, false), None);
        assert!(!tracker.is_focused());
        assert_eq!(tracker.change(false, true), None);
        assert_eq!(tracker.change(true, true), Some(FOCUS_IN));
        assert_eq!(tracker.change(false, false), None);
        assert_eq!(tracker.change(true, false), None);
        assert_eq!(tracker.change(true, true), None);
    }

    #[test]
    fn throttling_suppresses_burst_without_deferring_stale_bells() {
        let mut limiter = BellLimiter::default();
        let now = Instant::now();
        assert!(limiter.allow(now));
        assert!(!limiter.allow(now));
        assert!(!limiter.allow(now + Duration::from_millis(149)));
        assert!(limiter.allow(now + Duration::from_millis(150)));
        assert!(!limiter.allow(now + Duration::from_millis(200)));
        assert!(limiter.allow(now + Duration::from_millis(301)));
    }
}

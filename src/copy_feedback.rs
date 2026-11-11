//! Short, non-invasive copy feedback used by the terminal renderer.
//! The same easing drives both the on-screen output highlight and the
//! clipboard confirmation, without mutating PTY text or selection state.

use std::time::{Duration, Instant};

const FADE_IN: Duration = Duration::from_millis(180);
const HOLD_UNTIL: Duration = Duration::from_millis(1_250);
const TOTAL: Duration = Duration::from_millis(2_100);
const FRAME: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    Copied,
    Warning,
}

#[derive(Clone, Debug)]
pub struct CopyNotice {
    pub kind: NoticeKind,
    pub title: String,
    pub detail: String,
    pub output_id: Option<u32>,
    started: Instant,
}

fn human_size(size: usize) -> String {
    if size < 1024 {
        format!("{size} B")
    } else if size < 1024 * 1024 {
        format!("{:.1} KiB", size as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", size as f64 / (1024.0 * 1024.0))
    }
}

impl CopyNotice {
    pub fn success(bytes: usize, output_id: u32, started: Instant) -> Self {
        Self {
            kind: NoticeKind::Copied,
            title: "COPIED TO CLIPBOARD".into(),
            detail: format!("Last command output  ·  {}", human_size(bytes)),
            output_id: Some(output_id),
            started,
        }
    }

    pub fn warning(message: impl Into<String>, started: Instant) -> Self {
        Self {
            kind: NoticeKind::Warning,
            title: "COULD NOT COPY".into(),
            detail: message.into(),
            output_id: None,
            started,
        }
    }

    pub fn finished(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= TOTAL
    }

    /// 0..=255 alpha using linear ease-in and ease-out with a quiet hold.
    pub fn opacity(&self, now: Instant) -> u8 {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed >= TOTAL {
            0
        } else if elapsed < FADE_IN {
            (elapsed.as_millis() * 255 / FADE_IN.as_millis()) as u8
        } else if elapsed <= HOLD_UNTIL {
            255
        } else {
            ((TOTAL - elapsed).as_millis() * 255
                / (TOTAL - HOLD_UNTIL).as_millis()) as u8
        }
    }

    /// Draw at ~60 FPS only while fading. During hold, sleep until fade-out.
    pub fn next_wakeup(&self, now: Instant) -> Option<Instant> {
        if self.finished(now) {
            return None;
        }
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed < FADE_IN || elapsed >= HOLD_UNTIL {
            Some((now + FRAME).min(self.started + TOTAL))
        } else {
            Some(self.started + HOLD_UNTIL)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_is_smooth_with_full_hold_and_final_transparency() {
        let start = Instant::now();
        let notice = CopyNotice::success(2048, 7, start);
        assert_eq!(notice.opacity(start), 0);
        assert!(notice.opacity(start + Duration::from_millis(80)) > 0);
        assert_eq!(notice.opacity(start + FADE_IN), 255);
        assert_eq!(notice.opacity(start + HOLD_UNTIL), 255);
        assert!(notice.opacity(start + Duration::from_millis(1800)) < 255);
        assert_eq!(notice.opacity(start + TOTAL), 0);
        assert!(notice.finished(start + TOTAL));
        assert_eq!(notice.next_wakeup(start + TOTAL), None);
    }

    #[test]
    fn notification_matches_captured_output_and_size() {
        let notice = CopyNotice::success(1048576, 23, Instant::now());
        assert_eq!(notice.kind, NoticeKind::Copied);
        assert_eq!(notice.output_id, Some(23));
        assert!(notice.detail.contains("1.0 MiB"));
        let warning = CopyNotice::warning("Clipboard unavailable", Instant::now());
        assert_eq!(warning.kind, NoticeKind::Warning);
        assert_eq!(warning.output_id, None);
    }

    #[test]
    fn hold_does_not_animate_unnecessarily() {
        let start = Instant::now();
        let notice = CopyNotice::success(10, 1, start);
        assert_eq!(
            notice.next_wakeup(start + Duration::from_millis(300)),
            Some(start + HOLD_UNTIL),
        );
        assert!(notice.next_wakeup(start + Duration::from_millis(30)).is_some());
    }
}

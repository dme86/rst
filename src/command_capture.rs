//! OSC 133 shell-integration command output capture.
//!
//! A/B label prompt/input; C begins command output; D ends it. Capture
//! happens after ANSI parsing, so SGR and escape sequences never enter the
//! clipboard. Unlike screen scraping, this survives scrollback eviction.
//! Keep only the latest finished command and the active command, each capped
//! independently to avoid unbounded output from logs.

use std::collections::{HashSet, VecDeque};

use crate::url_picker::{extract_urls, DEFAULT_OUTPUT_HISTORY, MAX_TOTAL_URLS, MAX_URLS_PER_OUTPUT};

pub const DEFAULT_MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyError {
    NoIntegration,
    NoOutput,
    TooLarge,
}

impl CopyError {
    pub fn message(self) -> &'static str {
        match self {
            Self::NoIntegration => "No command markers: run rst --install-shell-integration",
            Self::NoOutput => "No command output available yet",
            Self::TooLarge => "Command output exceeded the configured capture limit",
        }
    }
}

#[derive(Clone, Debug)]
struct OutputBuffer {
    id: u32,
    previous_lines: String,
    line: Vec<char>,
    column: usize,
    max_bytes: usize,
    too_large: bool,
}

impl OutputBuffer {
    fn new(max_bytes: usize, id: u32) -> Self {
        Self {
            id,
            previous_lines: String::new(),
            line: Vec::new(),
            column: 0,
            max_bytes,
            too_large: false,
        }
    }

    fn can_append(&mut self, extra: usize) -> bool {
        if self.too_large {
            return false;
        }
        // A Vec<char> uses four bytes per scalar; the conservative check
        // also caps memory when a remote process emits combining sequences.
        if self.previous_lines.len()
            .saturating_add(self.line.len().saturating_mul(4))
            .saturating_add(extra) > self.max_bytes
        {
            self.too_large = true;
            return false;
        }
        true
    }

    fn put(&mut self, ch: char) {
        if !self.can_append(4) {
            return;
        }
        if self.column < self.line.len() {
            self.line[self.column] = ch;
        } else {
            // Carriage return may leave the insertion column unchanged
            // after CSI K, but never synthesize more than bounded spaces.
            while self.line.len() < self.column {
                if !self.can_append(4) {
                    return;
                }
                self.line.push(' ');
            }
            self.line.push(ch);
        }
        self.column += 1;
    }

    fn control(&mut self, byte: u8) {
        if self.too_large {
            return;
        }
        match byte {
            b'\r' => self.column = 0,
            b'\n' | 0x0b | 0x0c => self.newline(),
            0x08 => self.column = self.column.saturating_sub(1),
            b'\t' => {
                let n = 8 - self.column % 8;
                for _ in 0..n {
                    self.put(' ');
                }
            }
            _ => {}
        }
    }

    fn newline(&mut self) {
        if !self.can_append(1) {
            return;
        }
        for ch in self.line.drain(..) {
            self.previous_lines.push(ch);
        }
        self.previous_lines.push('\n');
        self.column = 0;
    }

    fn erase_line(&mut self, mode: u16) {
        if self.too_large {
            return;
        }
        match mode {
            0 => self.line.truncate(self.column),
            1 => {
                let end = self.column.min(self.line.len().saturating_sub(1));
                if !self.line.is_empty() {
                    self.line[..=end].fill(' ');
                }
            }
            2 => self.line.clear(),
            _ => {}
        }
    }

    fn content(&self) -> Result<String, CopyError> {
        if self.too_large {
            return Err(CopyError::TooLarge);
        }
        let mut text = String::with_capacity(self.previous_lines.len() + self.line.len() * 4);
        text.push_str(&self.previous_lines);
        text.extend(self.line.iter().copied());
        if text.is_empty() {
            Err(CopyError::NoOutput)
        } else {
            Ok(text)
        }
    }
}

#[derive(Debug)]
pub struct CommandCapture {
    active: Option<OutputBuffer>,
    completed: Option<OutputBuffer>,
    max_bytes: usize,
    saw_markers: bool,
    next_id: u32,
    // One compact URL list per recent completed command, not its full text.
    url_history: VecDeque<Vec<String>>,
    url_history_limit: usize,
}

impl Default for CommandCapture {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_BYTES)
    }
}

impl CommandCapture {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            active: None,
            completed: None,
            max_bytes: max_bytes.max(1),
            saw_markers: false,
            next_id: 0,
            url_history: VecDeque::new(),
            url_history_limit: DEFAULT_OUTPUT_HISTORY,
        }
    }

    pub fn with_url_history(max_bytes: usize, outputs: usize) -> Self {
        let mut capture = Self::new(max_bytes);
        capture.url_history_limit = outputs.max(1);
        capture
    }

    pub fn saw_markers(&self) -> bool {
        self.saw_markers
    }

    pub fn running(&self) -> bool {
        self.active.is_some()
    }

    /// The ID of the command currently emitting PTY output, if any.
    pub fn active_output_id(&self) -> Option<u32> {
        self.active.as_ref().map(|output| output.id)
    }

    pub fn marker(&mut self, marker: u8) {
        match marker {
            b'A' | b'B' => {
                self.saw_markers = true;
                if marker == b'A' {
                    self.finish();
                }
            }
            b'C' => {
                self.saw_markers = true;
                self.finish();
                // IDs uniquely tag visible cells, including those moved to
                // scrollback; zero is reserved for unmarked prompt text.
                self.next_id = self.next_id.wrapping_add(1).max(1);
                self.active = Some(OutputBuffer::new(self.max_bytes, self.next_id));
            }
            b'D' => {
                self.saw_markers = true;
                self.finish();
            }
            _ => {}
        }
    }

    fn finish(&mut self) {
        if let Some(active) = self.active.take() {
            // The command may be 32 MiB; only keep its bounded URL list.
            // Empty outputs still count toward the last N commands.
            let links = active.content().map(|text| {
                extract_urls(&text, MAX_URLS_PER_OUTPUT)
            }).unwrap_or_default();
            self.url_history.push_back(links);
            while self.url_history.len() > self.url_history_limit {
                self.url_history.pop_front();
            }
            self.completed = Some(active);
        }
    }

    pub fn print(&mut self, ch: char) {
        if let Some(active) = self.active.as_mut() {
            active.put(ch);
        }
    }

    pub fn control(&mut self, byte: u8) {
        if let Some(active) = self.active.as_mut() {
            active.control(byte);
        }
    }

    pub fn erase_line(&mut self, mode: u16) {
        if let Some(active) = self.active.as_mut() {
            active.erase_line(mode);
        }
    }

    /// Copy an active command if it already produced output, otherwise
    /// prefer the most recent completed command. Never silently truncate.
    /// Unique links, newest first, from the currently running command
    /// and the last N completed command outputs. Does not depend on scrollback.
    pub fn recent_urls(&self) -> Vec<String> {
        let mut results = Vec::new();
        let mut seen = HashSet::new();
        let active_links = self.active.as_ref()
            .and_then(|active| active.content().ok())
            .map(|text| extract_urls(&text, MAX_URLS_PER_OUTPUT))
            .unwrap_or_default();
        for links in std::iter::once(&active_links).chain(self.url_history.iter().rev()) {
            for url in links.iter().rev() {
                if seen.insert(url.clone()) {
                    results.push(url.clone());
                    if results.len() == MAX_TOTAL_URLS {
                        return results;
                    }
                }
            }
        }
        results
    }

    #[cfg(test)]
    pub fn latest_output(&self) -> Result<String, CopyError> {
        self.latest_output_with_id().map(|(text, _)| text)
    }

    /// Return exactly the output chosen for copying plus its visible-cell
    /// identifier. An empty running command falls back to the last complete
    /// command, matching the existing copy shortcut's semantics.
    pub fn latest_output_with_id(&self) -> Result<(String, u32), CopyError> {
        if !self.saw_markers {
            return Err(CopyError::NoIntegration);
        }
        if let Some(active) = &self.active {
            match active.content() {
                Ok(text) => return Ok((text, active.id)),
                Err(CopyError::TooLarge) => return Err(CopyError::TooLarge),
                Err(_) => {}
            }
        }
        let completed = self.completed.as_ref().ok_or(CopyError::NoOutput)?;
        completed.content().map(|text| (text, completed.id))
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_deduplicated_newest_first_across_recent_commands() {
        let mut cap = CommandCapture::with_url_history(DEFAULT_MAX_BYTES, 2);
        cap.marker(b'C');
        for ch in "https://old.example https://same.example".chars() { cap.print(ch); }
        cap.marker(b'D');
        cap.marker(b'C');
        for ch in "https://new.example https://same.example".chars() { cap.print(ch); }
        cap.marker(b'D');
        assert_eq!(cap.recent_urls(), vec![
            "https://same.example", "https://new.example", "https://old.example",
        ]);
        cap.marker(b'C');
        for ch in "no links".chars() { cap.print(ch); }
        cap.marker(b'D');
        assert_eq!(cap.recent_urls(), vec![
            "https://same.example", "https://new.example",
        ]);
    }

    #[test]
    fn running_links_are_visible_and_scrollback_is_not_required() {
        let mut cap = CommandCapture::default();
        cap.marker(b'C');
        for ch in "https://live.example/now".chars() { cap.print(ch); }
        assert_eq!(cap.recent_urls(), vec!["https://live.example/now"]);
        cap.marker(b'D');
        assert_eq!(cap.recent_urls(), vec!["https://live.example/now"]);
    }

    #[test]
    fn overlarge_outputs_do_not_store_partial_urls() {
        let mut cap = CommandCapture::with_url_history(32, 3);
        cap.marker(b'C');
        for ch in "https://a.example/12345678900000000".chars() { cap.print(ch); }
        cap.marker(b'D');
        assert!(cap.recent_urls().is_empty());
    }


    #[test]
    fn markers_separate_prompt_input_and_command_output() {
        let mut cap = CommandCapture::default();
        for ch in "prompt> echo hello".chars() { cap.print(ch); }
        assert_eq!(cap.latest_output(), Err(CopyError::NoIntegration));
        cap.marker(b'A');
        cap.marker(b'B');
        for ch in "echo hello".chars() { cap.print(ch); }
        cap.marker(b'C');
        for ch in "hello\n".chars() {
            if ch == '\n' { cap.control(b'\n') } else { cap.print(ch) }
        }
        cap.marker(b'D');
        for ch in "prompt> ".chars() { cap.print(ch); }
        assert_eq!(cap.latest_output(), Ok("hello\n".to_owned()));
    }

    #[test]
    fn running_command_can_be_copied_before_d_marker() {
        let mut cap = CommandCapture::default();
        cap.marker(b'C');
        assert_eq!(cap.latest_output(), Err(CopyError::NoOutput));
        for ch in "log line".chars() { cap.print(ch); }
        assert!(cap.running());
        assert_eq!(cap.latest_output(), Ok("log line".to_owned()));
    }

    #[test]
    fn carriage_return_erase_and_backspace_replace_progress_text() {
        let mut cap = CommandCapture::default();
        cap.marker(b'C');
        for ch in "loading 100%".chars() { cap.print(ch); }
        cap.control(b'\r');
        cap.erase_line(2);
        for ch in "done".chars() { cap.print(ch); }
        cap.control(b'\n');
        cap.print('X');
        cap.print('x');
        cap.control(0x08);
        cap.print('!');
        cap.marker(b'D');
        assert_eq!(cap.latest_output(), Ok("done\nX!".into()));
    }

    #[test]
    fn tabs_and_unicode_remain_readable() {
        let mut cap = CommandCapture::default();
        cap.marker(b'C');
        cap.print('界');
        cap.control(b'\t');
        cap.print('é');
        cap.marker(b'D');
        assert_eq!(cap.latest_output(), Ok("界       é".to_owned()));
    }

    #[test]
    fn overflow_does_not_leak_partially_captured_logs() {
        let mut cap = CommandCapture::new(20);
        cap.marker(b'C');
        for ch in "123456789".chars() { cap.print(ch); }
        assert_eq!(cap.latest_output(), Err(CopyError::TooLarge));
        cap.marker(b'D');
        assert_eq!(cap.latest_output(), Err(CopyError::TooLarge));
        cap.marker(b'C');
        cap.print('a');
        cap.marker(b'D');
        assert_eq!(cap.latest_output(), Ok("a".to_owned()));
    }

    #[test]
    fn prompt_marker_recovers_when_shell_omits_d() {
        let mut cap = CommandCapture::default();
        cap.marker(b'C');
        cap.print('z');
        cap.marker(b'A');
        assert_eq!(cap.latest_output(), Ok("z".to_owned()));
    }

    #[test]
    fn missing_or_empty_output_reports_clear_error() {
        let mut cap = CommandCapture::default();
        assert_eq!(cap.latest_output(), Err(CopyError::NoIntegration));
        cap.marker(b'A');
        assert_eq!(cap.latest_output(), Err(CopyError::NoOutput));
        cap.marker(b'C');
        cap.marker(b'D');
        assert_eq!(cap.latest_output(), Err(CopyError::NoOutput));
    }

    #[test]
    fn output_ids_follow_active_and_completed_capture() {
        let mut cap = CommandCapture::default();
        assert_eq!(cap.active_output_id(), None);
        cap.marker(b'C');
        let first = cap.active_output_id().unwrap();
        assert_ne!(first, 0);
        cap.print('a');
        cap.marker(b'D');
        assert_eq!(cap.active_output_id(), None);
        assert_eq!(cap.latest_output_with_id(), Ok(("a".to_owned(), first)));
        cap.marker(b'C');
        assert_eq!(cap.latest_output_with_id(), Ok(("a".to_owned(), first)));
        let second = cap.active_output_id().unwrap();
        assert_ne!(second, first);
        cap.print('b');
        assert_eq!(cap.latest_output_with_id(), Ok(("b".to_owned(), second)));
    }

}

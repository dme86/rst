//! URL extraction and keyboard navigation for the recent-output link palette.
//! No shell is involved: only explicit http(s) links can be opened.

pub const DEFAULT_OUTPUT_HISTORY: usize = 20;
pub const MAX_URLS_PER_OUTPUT: usize = 64;
pub const MAX_TOTAL_URLS: usize = 256;
const MAX_URL_BYTES: usize = 2048;

/// Extract links from OSC 133 command output, in the order they appeared.
/// Retain only the latest distinct links when an output contains many URLs.
pub fn extract_urls(output: &str, limit: usize) -> Vec<String> {
    let mut urls = Vec::new();
    if limit == 0 {
        return urls;
    }
    for (index, _) in output.match_indices("http") {
        let remainder = &output[index..];
        let prefix = if remainder.starts_with("https://") {
            "https://"
        } else if remainder.starts_with("http://") {
            "http://"
        } else {
            continue;
        };
        // Avoid extracting an embedded scheme from a larger token.
        if index > 0 && output[..index].chars().next_back().is_some_and(|c| {
            c.is_alphanumeric() || matches!(c, '/' | '.' | '@' | '_' | '-')
        }) {
            continue;
        }
        let mut end = 0;
        for (offset, ch) in remainder.char_indices() {
            if ch.is_whitespace() || ch.is_control()
                || matches!(ch, '<' | '>' | '"' | '\'' | '\\' | '|' | '\u{60}')
                || offset + ch.len_utf8() > MAX_URL_BYTES
            {
                break;
            }
            end = offset + ch.len_utf8();
        }
        if end <= prefix.len() {
            continue;
        }
        let mut url = &remainder[..end];
        url = url.trim_end_matches(&['.', ',', ';', ':', '!', '?', '*'][..]);
        // Markdown/prose often surround URLs with parentheses or brackets,
        // but balanced parentheses in an actual URL must be preserved.
        for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
            while url.ends_with(close)
                && url.chars().filter(|c| *c == close).count()
                    > url.chars().filter(|c| *c == open).count()
            {
                url = &url[..url.len() - close.len_utf8()];
            }
        }
        // Re-trim punctuation exposed by removing a surrounding bracket.
        url = url.trim_end_matches(&['.', ',', ';', ':', '!', '?', '*'][..]);
        let authority = url.strip_prefix(prefix).unwrap_or("")
            .split(&['/', '?', '#'][..]).next().unwrap_or("");
        if authority.is_empty() || authority.starts_with(':') {
            continue;
        }
        if let Some(pos) = urls.iter().position(|item| item == url) {
            urls.remove(pos);
        }
        urls.push(url.to_owned());
        if urls.len() > limit {
            urls.remove(0);
        }
    }
    urls
}

#[derive(Clone, Debug)]
pub struct UrlPicker {
    pub urls: Vec<String>,
    pub selected: usize,
    pub scroll: usize,
}

impl UrlPicker {
    pub fn new(urls: Vec<String>) -> Self {
        Self { urls, selected: 0, scroll: 0 }
    }

    pub fn selected_url(&self) -> Option<&str> {
        self.urls.get(self.selected).map(String::as_str)
    }

    pub fn move_by(&mut self, amount: i32, visible: usize) {
        if self.urls.is_empty() {
            return;
        }
        self.selected = if amount < 0 {
            self.selected.saturating_sub(amount.unsigned_abs() as usize)
        } else {
            self.selected.saturating_add(amount as usize)
                .min(self.urls.len() - 1)
        };
        self.keep_visible(visible);
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn select_last(&mut self, visible: usize) {
        self.selected = self.urls.len().saturating_sub(1);
        self.keep_visible(visible);
    }

    fn keep_visible(&mut self, visible: usize) {
        let visible = visible.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll.saturating_add(visible) {
            self.scroll = self.selected + 1 - visible;
        }
    }
}

/// Rows available for URLs, excluding the title, separator, explanation
/// and keyboard hints. Works even in a very small terminal window.
pub fn visible_rows(screen_rows: usize) -> usize {
    screen_rows.saturating_sub(7).min(17).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_multiline_urls_and_trims_prose() {
        let urls = extract_urls(
            "See (https://example.org/path(foo)).\nAnother: https://rust-lang.org/docs, and [http://localhost:8080/x?q=1].",
            10,
        );
        assert_eq!(urls, vec![
            "https://example.org/path(foo)",
            "https://rust-lang.org/docs",
            "http://localhost:8080/x?q=1",
        ]);
    }

    #[test]
    fn rejects_non_http_schemes_and_incomplete_authorities() {
        assert_eq!(
            extract_urls("file:///etc/passwd javascript:alert(1) https:// http:///bad https://:123", 20),
            Vec::<String>::new()
        );
    }

    #[test]
    fn only_latest_occurrences_and_last_n_urls_are_kept() {
        assert_eq!(
            extract_urls("https://a.test https://b.test https://a.test https://c.test", 2),
            vec!["https://a.test", "https://c.test"]
        );
    }

    #[test]
    fn does_not_extract_schemes_in_larger_tokens() {
        assert_eq!(extract_urls("abchttps://bad.test /https://bad.test https://ok.test", 10),
                   vec!["https://ok.test"]);
    }

    #[test]
    fn palette_clamps_navigation_and_keeps_selection_visible() {
        let mut picker = UrlPicker::new((0..30).map(|i| format!("https://example.org/{i}")).collect());
        picker.move_by(20, 5);
        assert_eq!((picker.selected, picker.scroll), (20, 16));
        picker.move_by(-18, 5);
        assert_eq!((picker.selected, picker.scroll), (2, 2));
        picker.move_by(-200, 5);
        assert_eq!((picker.selected, picker.scroll), (0, 0));
        picker.select_last(5);
        assert_eq!((picker.selected, picker.scroll), (29, 25));
        assert_eq!(picker.selected_url(), Some("https://example.org/29"));
        picker.select_first();
        assert_eq!(picker.scroll, 0);
    }

    #[test]
    fn empty_palette_and_small_windows_are_safe() {
        let mut picker = UrlPicker::new(vec![]);
        picker.move_by(1, visible_rows(0));
        picker.select_last(visible_rows(0));
        assert_eq!(picker.selected_url(), None);
        assert_eq!(visible_rows(0), 1);
    }
}

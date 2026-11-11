//! OSC helpers for color specifications, xterm-compatible replies and OSC 52.
//! Terminal state and GUI/clipboard side effects remain in terminal.rs/main.rs.

use base64::Engine;

pub const MAX_OSC52_BYTES: usize = 64 * 1024;
const MAX_OSC_TITLE_BYTES: usize = 4096;
const MAX_TITLE_CHARS: usize = 256;

fn component(part: &str) -> Option<u32> {
    if part.is_empty() || part.len() > 4 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let n = u32::from_str_radix(part, 16).ok()?;
    let max = (1u32 << (part.len() * 4)) - 1;
    Some((n * 255 + max / 2) / max)
}

/// X11 color forms supported by st: #RGB/#RRGGBB/#RRRGGGBBB/#RRRRGGGGBBBB
/// and rgb:R/G/B with 1..4 hexadecimal digits per component.
/// Named X11 colors are deliberately excluded (not portable to macOS).
pub fn parse_color(input: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(input).ok()?;
    let (r, g, b) = if let Some(hex) = text.strip_prefix('#') {
        if ![3, 6, 9, 12].contains(&hex.len()) {
            return None;
        }
        let len = hex.len() / 3;
        (
            component(&hex[..len])?,
            component(&hex[len..2 * len])?,
            component(&hex[2 * len..])?,
        )
    } else {
        let rgb = text.strip_prefix("rgb:")?;
        let mut parts = rgb.split('/');
        let r = component(parts.next()?)?;
        let g = component(parts.next()?)?;
        let b = component(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        (r, g, b)
    };
    Some((r << 16) | (g << 8) | b)
}

/// st/xterm return 16-bit RGB components, with BEL as the OSC terminator.
pub fn color_reply(code: u16, index: Option<u8>, color: u32) -> Vec<u8> {
    let r = (color >> 16) & 0xff;
    let g = (color >> 8) & 0xff;
    let b = color & 0xff;
    let prefix = index.map_or_else(
        || format!("{code};"),
        |index| format!("{code};{index};"),
    );
    format!("\x1b]{prefix}rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x07")
        .into_bytes()
}

/// Prevent arbitrary terminal output from injecting control chars or creating
/// unbounded native window titles. Empty OSC 0/2 resets to the default title.
pub fn title(data: &[u8]) -> Option<String> {
    if data.len() > MAX_OSC_TITLE_BYTES {
        return None;
    }
    let raw = std::str::from_utf8(data).ok()?;
    Some(raw.chars().filter(|ch| !ch.is_control()).take(MAX_TITLE_CHARS).collect())
}

/// Decode clipboard *writes* only. OSC 52 queries ("?") must never
/// disclose host clipboard contents to a child shell or remote program.
pub fn clipboard_write(selection: &[u8], payload: &[u8]) -> Option<String> {
    if selection.is_empty() || !selection.iter().all(|b| matches!(*b, b'c' | b'p' | b's' | b'0'..=b'7')) {
        return None;
    }
    if payload.is_empty() || payload == b"?" || payload.len() > MAX_OSC52_BYTES.div_ceil(3) * 4 {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD.decode(payload).ok()?;
    if decoded.len() > MAX_OSC52_BYTES {
        return None;
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_x11_color_forms_and_reject_invalid() {
        assert_eq!(parse_color(b"#f0a"), Some(0xff00aa));
        assert_eq!(parse_color(b"#12ab34"), Some(0x12ab34));
        assert_eq!(parse_color(b"#fff000abc"), Some(0xff00ab));
        assert_eq!(parse_color(b"#ffff00008080"), Some(0xff0080));
        assert_eq!(parse_color(b"rgb:f/0/a"), Some(0xff00aa));
        assert_eq!(parse_color(b"rgb:12/ab/34"), Some(0x12ab34));
        assert_eq!(parse_color(b"rgb:ffff/0000/8080"), Some(0xff0080));
        assert_eq!(parse_color(b"rgb:ff/ab"), None);
        assert_eq!(parse_color(b"#fffff"), None);
        assert_eq!(parse_color(b"#ghj"), None);
        assert_eq!(parse_color(b"red"), None);
    }

    #[test]
    fn xterm_reply_uses_four_digit_components_and_bel() {
        assert_eq!(color_reply(4, Some(2), 0x12abfe), b"\x1b]4;2;rgb:1212/abab/fefe\x07");
        assert_eq!(color_reply(11, None, 0x282828), b"\x1b]11;rgb:2828/2828/2828\x07");
    }

    #[test]
    fn titles_are_bounded_and_control_characters_stripped() {
        assert_eq!(title("Neovim 🦀".as_bytes()), Some("Neovim 🦀".into()));
        assert_eq!(title(b"hello\x1b[31m"), Some("hello[31m".into()));
        assert_eq!(title(&vec![b'a'; 4097]), None);
        assert_eq!(title(&vec![b'a'; 300]).unwrap().len(), 256);
    }

    #[test]
    fn osc52_rejects_reads_large_invalid_and_non_utf8_payloads() {
        assert_eq!(clipboard_write(b"c", b"aGVsbG8="), Some("hello".into()));
        assert_eq!(clipboard_write(b"p", b"aGVsbG8="), Some("hello".into()));
        assert_eq!(clipboard_write(b"c", b"?"), None);
        assert_eq!(clipboard_write(b"x", b"aGVsbG8="), None);
        assert_eq!(clipboard_write(b"c", b"!"), None);
        assert_eq!(clipboard_write(b"c", b"/w=="), None);
        assert_eq!(clipboard_write(b"c", &vec![b'A'; 100_000]), None);
    }
}

//! Pure-Rust OpenType shaping (HarfBuzz-compatible via rustybuzz).
//!
//! The terminal grid owns geometry: shaping may substitute/position glyphs,
//! but must never change where a cell, cursor, background or selection lives.

use rustybuzz::{Face, UnicodeBuffer};

#[derive(Clone)]
pub struct FontBytes {
    pub bytes: std::sync::Arc<[u8]>,
    pub face_index: u32,
}

#[derive(Clone, Debug)]
pub struct ShapedGlyph {
    pub index: u16,
    /// Positions in face design units (scaled to pixels by the renderer).
    pub x_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,
}

#[derive(Clone, Debug)]
pub struct ShapedText {
    pub glyphs: Vec<ShapedGlyph>,
    pub units_per_em: f32,
    pub total_advance: i64,
}

/// Skip expensive shaper setup for ordinary plain ASCII terminal output.
/// Keep ligature candidates and non-ASCII (Arabic/Indic, combining, etc.).
pub fn needs_shaping(text: &str, ligatures: bool) -> bool {
    if !text.is_ascii() {
        return true;
    }
    ligatures
        && text
            .as_bytes()
            .windows(2)
            .any(|pair| ascii_pair_needs_shaping(pair[0], pair[1]))
}

/// All ASCII candidates are pairs; ffi/ffl/=== also contain fi/fl/==.
pub fn ascii_pair_needs_shaping(a: u8, b: u8) -> bool {
    matches!(
        (a, b),
        (b'=', b'>')
            | (b'-', b'>')
            | (b'=', b'=')
            | (b'!', b'=')
            | (b'<', b'=')
            | (b'>', b'=')
            | (b'&', b'&')
            | (b'|', b'|')
            | (b':', b':')
            | (b'f', b'i')
            | (b'f', b'l')
            | (b'<', b'-')
            | (b'+', b'+')
            | (b'*', b'*')
    )
}

pub fn shape(source: &FontBytes, text: &str, ligatures: bool) -> Option<ShapedText> {
    if text.is_empty() || text.len() > 4096 {
        return None;
    }
    let face = Face::from_slice(&source.bytes, source.face_index)?;
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    let features = if ligatures {
        Vec::new()
    } else {
        use std::str::FromStr;
        ["liga=0", "clig=0", "dlig=0", "calt=0"]
            .iter()
            .filter_map(|feature| rustybuzz::Feature::from_str(feature).ok())
            .collect()
    };
    let shaped = rustybuzz::shape(&face, &features, buffer);
    let mut total_advance = 0i64;
    let glyphs = shaped
        .glyph_infos()
        .iter()
        .zip(shaped.glyph_positions().iter())
        .map(|(info, pos)| {
            total_advance += pos.x_advance as i64;
            ShapedGlyph {
                index: info.glyph_id as u16,
                x_advance: pos.x_advance,
                x_offset: pos.x_offset,
                y_offset: pos.y_offset,
            }
        })
        .collect::<Vec<_>>();
    if glyphs.is_empty() || total_advance == 0 {
        return None;
    }
    Some(ShapedText {
        glyphs,
        units_per_em: face.units_per_em() as f32,
        total_advance,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_ascii_avoids_shaping_work() {
        assert!(!needs_shaping("cargo test --all", true));
        assert!(needs_shaping("a != b", true));
        assert!(needs_shaping("fi", true));
        assert!(!needs_shaping("fi", false));
        assert!(needs_shaping("مرحبا", false));
        assert!(needs_shaping("e\u{301}", false));
        assert!(needs_shaping("क्‍ष", false));
    }

    #[test]
    fn corrupt_or_empty_fonts_do_not_crash_the_renderer() {
        let font = FontBytes {
            bytes: vec![1, 2, 3].into(),
            face_index: 0,
        };
        assert!(shape(&font, "fi", true).is_none());
        assert!(shape(&font, "", true).is_none());
    }

    #[test]
    fn very_large_shaping_input_is_refused() {
        let font = FontBytes {
            bytes: vec![].into(),
            face_index: 0,
        };
        assert!(shape(&font, &"a".repeat(5000), true).is_none());
    }
}

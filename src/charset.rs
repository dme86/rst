//! VT100 DEC Special Graphics character translation.
//!
//! st's tsetchar() translates ASCII 0x41..=0x7e through its vt100_0 table
//! only when the currently invoked G0/G1/G2/G3 register is designated '0'.
//! Store the mapped Unicode character in the grid, not a font-dependent
//! attribute; render, selection and clipboard then share the same result.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CharacterSet {
    #[default]
    Ascii,
    DecSpecialGraphics,
}

impl CharacterSet {
    /// Character-set designation final byte after ESC (, ), * or +.
    /// Like upstream st's tdeftran(), only 0 (graphics) and B (ASCII)
    /// are implemented; unsupported sets leave the register unchanged.
    pub fn designate(final_byte: u8) -> Option<Self> {
        match final_byte {
            b'0' => Some(Self::DecSpecialGraphics),
            b'B' => Some(Self::Ascii),
            _ => None,
        }
    }

    /// Map one printed character using upstream st's vt100_0 table.
    /// Already-decoded non-ASCII UTF-8 text is never remapped.
    pub fn map(self, ch: char) -> char {
        if self != Self::DecSpecialGraphics {
            return ch;
        }
        match ch {
            'A' => '↑',
            'B' => '↓',
            'C' => '→',
            'D' => '←',
            'E' => '█',
            'F' => '▚',
            'G' => '☃',
            '_' => ' ',
            '\u{60}' => '◆',
            'a' => '▒',
            'b' => '␉',
            'c' => '␌',
            'd' => '␍',
            'e' => '␊',
            'f' => '°',
            'g' => '±',
            'h' => '␤',
            'i' => '␋',
            'j' => '┘',
            'k' => '┐',
            'l' => '┌',
            'm' => '└',
            'n' => '┼',
            'o' => '⎺',
            'p' => '⎻',
            'q' => '─',
            'r' => '⎼',
            's' => '⎽',
            't' => '├',
            'u' => '┤',
            'v' => '┴',
            'w' => '┬',
            'x' => '│',
            'y' => '≤',
            'z' => '≥',
            '{' => 'π',
            '|' => '≠',
            '}' => '£',
            '~' => '·',
            _ => ch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_upstream_st_vt100_graphics_table() {
        let input: String = "ABCDEFG_".chars()
            .chain(std::iter::once('\u{60}'))
            .chain("abcdefghijklmnopqrstuvwxyz{|}~".chars())
            .collect();
        let expected = "↑↓→←█▚☃ ◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·";
        let actual: String = input.chars()
            .map(|ch| CharacterSet::DecSpecialGraphics.map(ch))
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn other_ascii_and_utf8_remain_unchanged() {
        for ch in "0123456789 !?#$%&()HIZ[\\]^ÄΩ界🦀".chars() {
            assert_eq!(CharacterSet::DecSpecialGraphics.map(ch), ch);
            assert_eq!(CharacterSet::Ascii.map(ch), ch);
        }
    }

    #[test]
    fn designation_accepts_only_original_st_graphics_and_ascii() {
        assert_eq!(CharacterSet::designate(b'0'), Some(CharacterSet::DecSpecialGraphics));
        assert_eq!(CharacterSet::designate(b'B'), Some(CharacterSet::Ascii));
        for other in *b"A129CZ" {
            assert_eq!(CharacterSet::designate(other), None);
        }
    }
}

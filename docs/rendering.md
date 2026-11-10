# Text rendering notes

rst uses a fixed terminal-cell grid. Font glyph dimensions do **not** decide
terminal cursor movement. The renderer now maintains these invariants:

1. Paint the background of **every** cell before painting any glyph in the
   frame. A later cell's background must not erase an earlier glyph.
2. Honor the pixel bounds returned by fontdue's `Metrics` (not only the
   configured font size). If ink fits inside its cell(s), retain the normal
   baseline and position, adjusting only an overflow. If it is too large,
   downscale it proportionally to the available cell bounds with bilinear
   sampling. This avoids truncating oversize Nerd Font symbols.
3. Render the four common Powerline separators (U+E0B0..U+E0B3) as
   antialiased, full-cell vector geometry. Their correctness is independent
   of font patching, glyph margins and ascent/descent.
4. Blend glyph coverage with the **actual** pixel behind it, not with a
   separately assumed background color.
5. Use the currently selected SGR background color for erasing cells (BCE).
   This matters for tmux status lines.

The placement and Powerline geometry have unit tests in `src/render.rs`.
Background Color Erase has a regression test in `src/terminal.rs`.

The default 6 px text inset is intentionally **not a separate colored border**:
the first and last rows and columns extend their actual background colors to
the window edges. Full-screen TUIs continue to fill the content area.
Cursor visibility has two separate dimensions: application-controlled
DECTCEM visibility, and the UI's 700 ms blink phase. The event loop sleeps
between changes, and typing resets the blink to visible.

## Optical alignment of Nerd Font icons

Nerd Font icons are Unicode private-use glyphs, not ordinary letterforms.
Their rasterizer metrics can have different bearings and baselines from the
primary text font. rst therefore calculates the non-transparent *ink bounds*
of each private-use icon, aligns its visible lower edge to the normal
text baseline (measured with the primary font's lowercase 'x'), and scales
it proportionally to the available space. A conservative amount of
horizontal overflow is permitted only into immediately neighboring **blank
cells of the same background color**. Regular letters are unchanged, and
Powerline separators keep their full-cell vector rendering.

This is a rendering/layout heuristic, not a general Unicode shaping solution.
Tests cover supplemental private-use icons (including U+F10FE), visible-alpha
bounds, baseline alignment, vertical clamping and avoidance of collisions
with neighboring text.

## Known limits

This is still an early terminal emulator, not a full Unicode shaping engine.
The grid currently stores one scalar value per cell; combining marks, emoji
ZWJ sequences, grapheme clusters and shaped ligatures need a separate
grapheme-aware cell model and text shaper. Cell-fitting prevents ink clipping
but may make exceptionally wide symbols smaller. A renderer cannot guarantee
visually ideal appearance for all fonts and Unicode sequences without further
work. Tmux mode/cursor/protocol coverage is also incomplete.

To inspect missing glyphs or font fallbacks, use
`RST_DEBUG_FONTS=1 make run`.

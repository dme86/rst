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

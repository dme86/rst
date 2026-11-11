# st as a functional reference

Upstream: https://git.suckless.org/st/files.html

rst is an original Rust implementation inspired by st's minimalism and
separation of terminal state from the native window frontend. It does not
vendor or translate upstream C source.

| st source | Responsibility | rst |
| --- | --- | --- |
| st.c, st.h | VT parser, terminal grid, cursor, character styles | src/terminal.rs |
| x.c, win.h | X11 surface, drawing, input, selection | src/main.rs, src/render.rs |
| config.def.h | hard-coded defaults | config/default.toml, src/config.rs |
| st.info | terminfo capabilities | future terminfo compatibility work |

## Priorities
- Keep one PTY process per window, no tabs or embedded shell.
- Implement VT commands as explicit state transitions and unit tests.
- Keep windowing/graphics independent from the terminal model.
- macOS AppKit and Linux/Wayland are equally important.
- 24-bit ANSI color, bounded scrollback and font fallback by default.

## Keyboard compatibility (implemented)

- st's legacy F1-F12, F13-F35, and modified cursor/function-key sequences are
  encoded in `src/keyboard.rs`, with byte-for-byte test vectors in
  `src/keyboard_tests.rs`.
- CSI ? 1 h/l selects application cursor keys; ESC = / ESC > selects DEC
  application keypad mode. Physical keypad identity is used only in that mode.
- Regular text uses the logical keyboard layout; AltGr-generated printable
  characters take precedence over Ctrl+Alt escape encoding.
- Standard navigation, Ctrl+Space, Shift+Tab, Meta-prefix and modified
  Home/End/Insert/Delete/PageUp/Down are covered.
- Native shortcuts (including Cmd copy/paste and text zoom) still take
  precedence. On the primary screen Shift+PageUp/Down scrolls history;
  alternate-screen apps receive these keys.

## Text selection (implemented)

- From upstream st's `selstart`, `selextend`, `selsnap`, `getsel` and x.c's
  multi-click handling: 300 ms word snap, 600 ms line snap, Alt-drag
  rectangular geometry, and Shift-forced local selection during reporting.
- Dedicated `src/selection.rs` stores selection geometry and click timing;
  `src/render.rs` and clipboard extraction use the same normalized bounds.
- Soft-wrap flags on cells let word/line selection span logical lines and
  suppress unwanted clipboard newlines. Wide Unicode dummy cells are
  normalized to their visible glyph, including rectangle intersections.
- Scrollback history is kept at the current viewport width on resize.
- Protocol-independent regression vectors: `src/selection_tests.rs`,
  with additional pixel-color selection tests in `src/render.rs`.

## Mouse reporting (implemented)

- DEC 9 (X10), 1000 (press/release), 1002 (pressed-button motion),
  1003 (all motion), and SGR 1006 are parsed in `src/terminal.rs`.
- `src/mouse.rs` follows upstream st's `x.c::mousereport`: button code,
  lowest-held-button motion, modifier bits, legacy 223-cell limit, SGR
  M/m release suffix, and wheel press codes (64-67).
- Mouse tracking suppresses local selection/scrollback; holding Shift forces
  local selection and scrolling (st's `forcemousemod` behavior).
- Mouse/trackpad events are handled through winit for both macOS and
  Wayland, with fractional pixel-wheel deltas accumulated into events.
- Protocol byte vectors and behavior checks live in `src/mouse_tests.rs`
  and the terminal unit tests.

## SGR text styling (implemented)

- Match upstream st's `st.c::tsetattr` flags: bold, faint, italic, underline,
  blink, reverse, invisible, struck and their selective reset codes.
- Store `TextStyle` in each `Cell` so history, erase, insert/delete and
  alternate-screen operations preserve the right properties. BCE erasures
  retain the effective background without inheriting text decorations.
- Use real bold/italic font faces where available; synthesize missing styles
  without adding font dependencies. Draw underline and strike across spaces
  and apply faint ink with background-aware blending.
- SGR 5/6 text blinking has its own event-loop deadline (800 ms), independent
  of the cursor and without a continuous redraw loop.
- Unit tests cover state transitions, selective resets, BCE, color interaction,
  pixel decorations and blink suppression.

## Core VT/DEC modes (implemented)

- DECOM (origin), DECAWM (deferred right-margin wrap), IRM, LNM, KAM,
  DECSCNM (whole-screen reverse), DEC focus tracking (?1004) and existing
  DECCKM / DECTCEM / mouse / bracketed-paste support
- DECSTBM scrolling-region bounds and cursor home semantics, cursor
  addressing relative to margins, DSR cursor-position replies
- State-driven HTS / CHT / CBT / TBC tab stops; custom stops survive resize
- DECSC/DECRC and ANSI CSI s/u save/restore full cursor attributes, separate
  DECSET ?1048 cursor save and ?1049 alternate-screen save/restore
- Byte-stream parser behavior verified with regression vectors in
  `src/vt_modes_tests.rs` and reverse-video render tests

## VT/DEC device control extension

- DECRQM reports set/reset/unsupported mode status for recognized standard
  and private modes without mutating terminal state.
- DECSTR resets cursor modes, SGR, margins and pointer reporting while
  preserving the grid and scrollback, unlike RIS.
- DECALN implements upstream st's `tdectest` alignment fill (ESC # 8);
  the escape dispatcher rejects unknown intermediates instead of running
  a superficially similar plain ESC command.
- Byte-stream regression tests are in `src/vt_modes_tests.rs`.
- This milestone does not imply complete VT/DEC parity: advanced DCS,
  DECSLRM, protection attributes and many device status queries remain.

## OSC controls (implemented)

- st-style OSC 0/1/2 support for UTF-8 window and icon titles with bounded,
  sanitized input and empty-title reset.
- OSC 4/104 and 10/11/12 with 110/111/112 manage all 256 indexed colors
  and defaults, supporting set, query and reset with 16-bit RGB responses.
- Cell color provenance enables repainting indexed/default-colored cells,
  including history and alternate screen, without changing truecolor cells.
- OSC 52 write-only clipboard integration is disabled by default; opt in using
  [clipboard].osc52_write. UTF-8 writes are limited to 64 KiB; read queries
  are always denied, even when clipboard writes are enabled.
- Parser and state tests live in src/osc.rs and src/osc_tests.rs.

## Cursor shape (DECSCUSR) implemented

- Implements CSI Ps SP q with xterm/st shapes: blinking or steady block
  (1/2), underline (3/4), and bar (5/6). Ps=0 or omitted restores the
  configured default; unsupported values are ignored.
- Configuration supports cursor.style = block, underline or bar and
  cursor.blink true/false. Cursor style selection and DECTCEM visibility
  remain independent, as do glyph SGR blink and cursor blink.
- Block keeps the existing cell-background overlay; underline and bar
  draw thin pixel-clipped strokes after the text pass. Scrollback, hidden
  cursor and blink-off frames suppress the overlay.
- Soft reset (DECSTR) and full reset (RIS) restore the configured cursor.
- Regression vectors in src/cursor_tests.rs plus pixel tests in src/render.rs.

## DEC character sets (implemented)

- Match `st.c::tsetchar`'s complete `vt100_0` ASCII-to-Unicode mapping:
  box drawing, arrows, shading and legacy symbols, preserving ASCII
  characters and non-ASCII UTF-8 outside the translation table.
- Like `st.c::tdeftran`, designate only `0` (DEC graphics) and `B` (ASCII)
  into G0/G1/G2/G3 with `ESC (`, `ESC )`, `ESC *` and `ESC +`.
- Invoke G1/G0 with SO/SI (0x0e/0x0f); invoke G2/G3 with ESC n/o.
  Designations are global across primary/alternate screen changes.
- SGR does not reset charset invocation. DECSTR and RIS restore all
  registers to ASCII and invoke G0. Unknown designators are ignored.
- Translation happens before glyph-width measurement, so the grid,
  selection and clipboard all contain the visible Unicode character.
- Table tests in src/charset.rs; 14 streamed-PTY cases in
  src/charset_tests.rs for registration, shifts, Unicode, wrap and reset.

## Focus events and terminal BEL (implemented)

- Retain existing DECSET 1004 / DECRST 1004 parser state. The winit event
  bridge sends exact `CSI I` / `CSI O` byte sequences on real focus
  transitions and ignores duplicate events; focus state is tracked
  even while reporting is disabled.
- Capture ASCII BEL (0x07) via `Perform::execute` as a bounded pending
  event. OSC's BEL terminator is not a separate terminal bell, and
  pending events survive reset within the same PTY buffer.
- UI sound uses native `NSBeep()` on macOS and the freedesktop
  `bell-terminal` sound through optional `canberra-gtk-play` on Wayland.
  The Linux process is spawned off the UI thread and reaped.
- A 150 ms limiter prevents output floods; a 160 ms border overlay
  provides a backend-independent visual alert with no busy-loop.
- Like st's `xbell`, a bell from an unfocused terminal requests attention
  using winit's API (subject to Wayland compositor capabilities).
- `[bell]` settings enable/disable audible and visual feedback, while
  preserving compatibility with older user TOML configs.
- Tests: `src/attention.rs`, `src/attention_tests.rs`, and pixel assertions
  in `src/render.rs`.

## Known gaps (prototype)
- ANSI/VT support is intentionally incomplete, especially DECSLRM,
  character sets, DCS, status lines and complex terminal queries.
- Full Unicode grapheme shaping, zero-width joining and ligatures.
- Modern negotiated keyboard protocols (CSI-u/kitty) and advanced mouse extensions outside upstream st.
- Native Wayland clipboard without compositor data-control extensions.
- IME/preedit integration, hyperlinks, synchronized output, and clipboard read prompts.
- Automated comparison tests against a pinned upstream st revision.

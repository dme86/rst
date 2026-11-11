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

## Known gaps (prototype)
- ANSI/VT support is intentionally incomplete, especially DECSLRM,
  character sets, DCS, status lines and complex terminal queries.
- Full Unicode grapheme shaping, zero-width joining and ligatures.
- Modern negotiated keyboard protocols (CSI-u/kitty) and advanced mouse extensions outside upstream st.
- Native Wayland clipboard without compositor data-control extensions.
- IME/preedit integration, hyperlinks, synchronized output, OSC 52 prompts.
- Automated comparison tests against a pinned upstream st revision.

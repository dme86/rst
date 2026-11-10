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

## Known gaps (prototype)
- ANSI/VT support is intentionally incomplete, especially DECSLRM,
  character sets, DCS, status lines and complex terminal queries.
- Full Unicode grapheme shaping, zero-width joining and ligatures.
- Mouse reporting and modern negotiated keyboard protocols (CSI-u/kitty).
- Native Wayland clipboard without compositor data-control extensions.
- IME/preedit integration, hyperlinks, synchronized output, OSC 52 prompts.
- Automated comparison tests against a pinned upstream st revision.

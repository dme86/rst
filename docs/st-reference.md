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

## Known gaps (prototype)
- ANSI/VT support is intentionally incomplete, especially DECSLRM,
  character sets, DCS, status lines and complex terminal queries.
- Full Unicode grapheme shaping, zero-width joining and ligatures.
- Full DEC keyboard/mouse protocol modes and tracking.
- Native Wayland clipboard without compositor data-control extensions.
- IME/preedit integration, hyperlinks, synchronized output, OSC 52 prompts.
- Automated comparison tests against a pinned upstream st revision.

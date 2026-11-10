# rst — Rust Simple Terminal

A small, native terminal emulator for **macOS** and **Linux/Wayland**, inspired by the design philosophy of [suckless st](https://git.suckless.org/st/).

**Status:** early prototype. The initial implementation is being built in Rust without X11 or a heavyweight GUI framework. It is not yet a complete st/VT replacement.

## Quick start

Install Rust using [rustup](https://rustup.rs/), then:

```sh
git clone https://github.com/dme86/rst.git
cd rst
make run
```

`make run` builds the debug executable and opens a native window running your `$SHELL` (default `/bin/sh`). Other targets: `make build`, `make release`, `make test`, `make fmt`, `make lint`, `make clean` and `make help`.

The optional configuration file is `~/.config/rst/config.toml` (or `$XDG_CONFIG_HOME/rst/config.toml`). Defaults are embedded in the executable; see [`config/default.toml`](config/default.toml).

### Default colors

The bundled default terminal background is `#282828`. If your existing
`~/.config/rst/config.toml` overrides `[colors] background`, that user value
continues to take precedence; change or remove it to use the new default.

### Keyboard font zoom

Press **Cmd++** or **Cmd+-** on macOS to enlarge or shrink terminal text.
Use **Cmd+0** to reset to the font size from the configuration you launched
with. On Linux/Wayland the corresponding shortcuts are **Ctrl++**,
**Ctrl+-** and **Ctrl+0**. Both `+` and `=` are accepted for zooming in,
so keyboards that require Shift for a plus sign are supported.

Zoom is temporary for the current window: `config.toml` is unchanged.
Each step changes the logical font size by 1 point (limits: 6–72).
The renderer reuses the installed font faces already loaded at startup; only
size-dependent metrics and rasterized glyphs are refreshed. This prevents
the roughly one-second pause previously caused by scanning all system fonts
on every zoom keypress. The existing window dimensions are retained, while
the grid is recalculated and the PTY receives a resize notification so
tmux and Neovim adapt.

### Comfortable spacing without a visible TUI border

rst now defaults to `[window] padding = 6` (logical pixels): text and the
cursor are inset slightly from the left and top, so the shell doesn't press
against the window edge. **Full-screen applications remain visually
edge-to-edge:** the renderer extends the surrounding cell's background colors
through the inset and any unused pixels, rather than painting a contrasting
frame around Neovim or tmux.

If you already have `~/.config/rst/config.toml`, that file overrides the
bundled defaults. Set `[window] padding = 6` there to adopt the new spacing;
use `0` if you prefer no inset. This affects the terminal's content area,
not macOS window decorations.

### Cursor blink

The default block cursor blinks continuously with 700 ms visible / 700 ms
hidden, using a deadline-based event-loop timer rather than continuous
redraws. Typing immediately restores the cursor and restarts its timer.
Applications can still hide the cursor using the standard terminal escape
sequence.

```toml
[window]
padding = 6

[cursor]
style = "block"
blink = true
blink_interval_ms = 700
```

Set `blink = false` to keep the cursor steady, or adjust
`blink_interval_ms` between 100 and 3000.

### Fonts and Nerd Font icons

The default primary face is **0xProto Nerd Font Regular** (when installed), followed by `0xProto Nerd Font Mono`, JetBrainsMono Nerd Font, Symbols Nerd Font Mono, Menlo and other system monospace fonts. The font selector requests the normal/regular face by default. Fonts are not embedded: they must be installed on the host.

If you already created a user config, that file overrides new defaults. Set the family explicitly:

```toml
[font]
family = "0xProto Nerd Font"
size = 15.0
```

To inspect which font families rst found and identify missing glyph codepoints, launch with `RST_DEBUG_FONTS=1 make run`. Missing icons can also originate in the current renderer's incomplete Unicode/grapheme support.


## Scope

One window and one PTY, with an owned terminal grid, ANSI parser, basic VT operations, 24-bit color, font fallback, bounded scrollback and native copy/paste. No tabs, shell integration, Electron, GTK, X11, or XWayland. On Linux, the windowing backend is **Wayland-only**.

This is a work-in-progress: extended VT modes, IME, grapheme shaping, complete alt-screen compatibility, font ligatures, full mouse reporting and OSC 52 security prompts remain to be implemented/tested. Linux clipboard integration currently relies on compositor data-control support; the macOS native clipboard is the primary first-run target.

## Inspiration and attribution

The architecture follows st's separation of terminal state (`st.c`) from platform rendering/input (`x.c`), but the Rust implementation is original. Read [`docs/st-reference.md`](docs/st-reference.md) for the source references and compatibility backlog. No C source from st is vendored.

MIT licensed; see [`LICENSE`](LICENSE).

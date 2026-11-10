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

### Borderless terminal / full-screen TUIs

rst defaults to `[window] padding = 0`: Neovim and other full-screen terminal
applications fill the entire **content area** without a contrasting outer
gutter. The renderer extends the last row/column's background colors through
any fractional leftover pixels from cell sizing, including statusline colors.

If you previously ran `make config`, your
`~/.config/rst/config.toml` overrides bundled defaults. Change its
`[window] padding` to `0` as well; a nonzero value intentionally preserves
the extra border. This setting controls the terminal's **inner** padding,
not the native macOS window title bar.

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

<p align="center">
  <img src="assets/logo.png" alt="rst — Rust Simple Terminal" width="190">
</p>

<h1 align="center">rst</h1>
<p align="center"><strong>Rust Simple Terminal</strong></p>

A small, native terminal emulator for **macOS** and **Linux/Wayland**, inspired by the design philosophy of [suckless st](https://git.suckless.org/st/).

**Status:** early prototype. The initial implementation is being built in Rust without X11 or a heavyweight GUI framework. It is not yet a complete st/VT replacement.

## Quick start

Install Rust using [rustup](https://rustup.rs/), then:

```sh
git clone https://github.com/dme86/rst.git
cd rst
make run
```

On macOS, `make run` builds a native `target/debug/rst.app` with the Dock icon and opens it through Launch Services. On Linux/Wayland, it runs the executable directly. Both use `$SHELL` (default: `/bin/sh`).

For macOS packaging: `make macos-app` (debug), `make macos-release` (optimized) and `make install-macos` (copies to `~/Applications`). For direct console logs run `make run-cli`. The locally built macOS app is unsigned; public distribution requires signing and notarization. The generated app icon uses the checked-in `assets/logo.png`.

Other targets: `make build`, `make release`, `make test`, `make fmt`, `make lint`, `make clean`, `make help`.

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

### Keyboard compatibility

rst sends the traditional st/xterm keyboard sequences for F1–F12 (plus
extended F13–F35), Insert/Delete, Home/End, PageUp/Down and arrow keys.
Shift, Alt and Ctrl combinations use the standard xterm modifier parameters;
Shift+Tab sends reverse-tab (CSI Z), and Ctrl+Space sends NUL. Left/Right
Alt on international keyboard layouts preserve composed printable text
(including AltGr) instead of turning it into unwanted control bytes.

Application cursor mode (DECCKM / CSI ? 1 h/l) and DEC application keypad
mode (DECKPAM/DECKPNM / ESC = and ESC >) are honored. The numeric keypad
is identified by physical key code in application mode, while regular
text input remains layout-aware. On the primary screen, Shift+PageUp/Down
scrolls the local history; full-screen TUI apps receive those keys.

See the table-driven protocol checks in `src/keyboard_tests.rs` and DEC-mode
tests in `src/terminal.rs`. Shell shortcuts handled by the desktop
environment may never reach the terminal. These changes do not implement
Kitty keyboard protocol or CSI-u negotiation.

### Mouse reporting

rst implements upstream st's legacy X10 (CSI ? 9 h), button tracking
(CSI ? 1000 h), drag tracking (CSI ? 1002 h), and all-motion tracking
(CSI ? 1003 h). SGR extended coordinates (CSI ? 1006 h) use the modern
CSI < button ; column ; row M/m format. Applications such as tmux,
Neovim and text-based editors activate and deactivate these modes themselves.

Clicks (including middle/right and extra mouse buttons), button releases,
dragging, cursor motion and vertical/horizontal wheel events are forwarded
to the application only when tracking is active. Normal rst selection and
scrollback remain unchanged when tracking is off. Hold **Shift** to
temporarily select text or scroll local history while a TUI is tracking
the mouse, matching the original st's forced-selection modifier.

Wheel events use cell-aware accumulation for smooth trackpad pixel deltas
on macOS/Wayland. Mouse reports are tested byte-for-byte against the
legacy/SGR protocol in `src/mouse_tests.rs`. OSC or Kitty-specific mouse
extensions (not provided by vanilla st) are not part of this implementation.

### Fonts and Nerd Font icons

The default primary face is **0xProto Nerd Font Regular** (when installed), followed by `0xProto Nerd Font Mono`, JetBrainsMono Nerd Font, Symbols Nerd Font Mono, Menlo and other system monospace fonts. The font selector requests the normal/regular face by default. Fonts are not embedded: they must be installed on the host.

If you already created a user config, that file overrides new defaults. Set the family explicitly:

```toml
[font]
family = "0xProto Nerd Font"
size = 16.5
```

To inspect which font families rst found and identify missing glyph codepoints, launch with `RST_DEBUG_FONTS=1 make run`. Missing icons can also originate in the current renderer's incomplete Unicode/grapheme support.


## Scope

One window and one PTY, with an owned terminal grid, ANSI parser, basic VT operations, 24-bit color, font fallback, bounded scrollback and native copy/paste. No tabs, shell integration, Electron, GTK, X11, or XWayland. On Linux, the windowing backend is **Wayland-only**.

This is a work-in-progress: extended VT modes, IME, grapheme shaping, complete alt-screen compatibility, font ligatures and OSC 52 security prompts remain to be implemented/tested. Linux clipboard integration currently relies on compositor data-control support; the macOS native clipboard is the primary first-run target.

## Inspiration and attribution

The architecture follows st's separation of terminal state (`st.c`) from platform rendering/input (`x.c`), but the Rust implementation is original. Read [`docs/st-reference.md`](docs/st-reference.md) for the source references and compatibility backlog. No C source from st is vendored.

MIT licensed; see [`LICENSE`](LICENSE).

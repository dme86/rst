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

### Cursor shape and blinking (DECSCUSR)

rst supports **block, underline, and bar** cursor shapes on macOS and Wayland.
The default is a blinking block with a 700 ms interval. Choose a different
default in `~/.config/rst/config.toml`:

```toml
[cursor]
style = "bar" # "block", "underline", or "bar"
blink = true
blink_interval_ms = 700
```

Programs such as Neovim can override both the shape and blink behavior using
`DECSCUSR` (`CSI Ps SP q`). The space before `q` is required:

| Ps | Cursor |
| --- | --- |
| 0 (or omitted) | Restore your configured cursor default |
| 1 / 2 | Blinking / steady block |
| 3 / 4 | Blinking / steady underline |
| 5 / 6 | Blinking / steady vertical bar |

For a quick manual check, use `printf '\033[5 q'` (blinking bar) and
`printf '\033[0 q'` (restore your default). With a steady cursor the
event loop does not schedule cursor-blink frames. Text blinking is independent.
`CSI ? 25 l` still hides the cursor regardless of its shape.

### VT/DEC modes (compatibility milestone)

rst implements the core ANSI/VT100 and DEC mode behavior expected by tmux
and Neovim. Terminal modes are enabled or disabled with CSI ... h/l.

| Mode / sequence | Behavior |
| --- | --- |
| CSI ? 6 h/l | DECOM origin mode; cursor home and row addressing relative to scroll margins |
| CSI ? 7 h/l | DECAWM deferred auto-wrap or right-margin overwrite |
| CSI 4 h/l | IRM insert/replace text |
| CSI 20 h/l | LNM: LF returns to column 1 when enabled |
| CSI ? 5 h/l | DECSCNM screen-wide reverse video |
| CSI 2 h/l | KAM keyboard input lock |
| CSI ? 1004 h/l | Focus in/out reporting (CSI I / CSI O) |
| CSI ? 1048 h/l | Save and restore cursor position and SGR attributes |
| CSI ? 47, 1047, 1049 h/l | Alternate screen switching; 1049 additionally saves and restores the primary cursor |
| ESC 7 / ESC 8, CSI s / CSI u | Cursor save and restore |
| ESC H, CSI I, CSI Z, CSI g | Set, advance, reverse and clear tab stops |

Tab stops default to every 8 columns and remain configurable during the
session; resizing preserves custom stops. Scroll margins (CSI top;bottom r)
interact with origin mode and cursor movement. Delayed auto-wrap ensures text
does not scroll until a *new* printable character reaches the end of a full row.

See the parser and screen regressions in `src/vt_modes_tests.rs`. This is a
**core VT/DEC milestone**, not a claim to implement every historical DEC
extension: DECSLRM and specialized status-line or reporting extensions
are still pending.

### DEC character sets (VT100 line drawing)

`rst` supports the same DEC Special Graphics translation table as suckless
`st`: designate a register with `ESC ( 0` (G0), `ESC ) 0` (G1),
`ESC * 0` (G2) or `ESC + 0` (G3). `0` chooses DEC line drawing;
`B` restores US ASCII for that register. Use `SO`/`SI` (bytes 0x0e/0x0f)
to switch between G1 and G0, or `ESC n`/`ESC o` to invoke G2/G3.

Draw a box directly in a shell:

```sh
printf '\033(0lqqk\r\nx  x\r\nmqqj\033(B\n'
```

The sequence produces `┌──┐`, `│  │` and `└──┘` using plain ASCII
characters `l`, `q`, `k`, `x`, `m` and `j`. It also includes the complete
st VT100 symbol table (arrows, shading, mathematical and control symbols).
The translated Unicode glyphs are stored in terminal cells, so rendering,
scrollback and copying select the same visible text. ASCII/UTF-8 not
covered by the DEC table remains unchanged; SGR and alternate-screen
switches do not discard the designation. DECSTR and RIS restore ASCII.

Regression tests: `src/charset.rs` and `src/charset_tests.rs`. Like
upstream `st`, unsupported national/legacy charset designations are ignored.

### Focus reporting and terminal bell

- `DECSET 1004` (`CSI ? 1004 h/l`) enables/disables xterm/st focus
  reporting. On a real focus transition, rst sends `CSI I` (focused) or
  `CSI O` (unfocused) to the PTY. Duplicate window focus notifications
  are suppressed; focus state is tracked even when the mode is disabled.
- `BEL` (ASCII `0x07`, `printf '\a'`) plays a native system alert and
  briefly highlights a two-pixel border in the configured cursor color.
  The 150 ms rate limit prevents bell floods from overwhelming sound or
  launching excessive processes. BEL that merely terminates an OSC
  sequence does not ring.
- Like st, an unfocused window requests user attention. On macOS this
  can bounce the Dock icon; Wayland attention depends on compositor
  support for `xdg_activation_v1`.

Configure bell behavior in `~/.config/rst/config.toml`:

```toml
[bell]
enabled = true  # Master BEL switch
audible = true  # macOS system beep or libcanberra on Linux
visual = true   # Brief border flash; works without audio packages
```

On macOS the sound uses AppKit `NSBeep()` and respects the system alert
sound. On Linux/Wayland, the optional `canberra-gtk-play` utility plays
the freedesktop `bell-terminal` sound event. Install the libcanberra
player and a sound theme if you want audible bells; the visual alert
still works without them. There is no shell invocation.

To check focus events, run `printf '\033[?1004h'` followed by `cat -v`,
switch to another app and back, and look for `^[[O` / `^[[I`. After
interrupting `cat`, run `printf '\033[?1004l'`. Protocol and blink
lifecycle tests are in `src/attention.rs`, `src/attention_tests.rs`
and `src/render.rs`.

### SGR text attributes (st compatibility)

rst now stores SGR styling **per terminal cell** (including scrollback and
alternate-screen content) and renders the text attributes supported by vanilla
st. Multiple SGR parameters can be combined in one escape sequence.

| SGR | Text attribute | Reset |
| --- | --- | --- |
| 1 | Bold (font face + bright ANSI colors where applicable) | 22 |
| 2 | Faint/dim text | 22 |
| 3 | Italic | 23 |
| 4 | Underline (including spaces) | 24 |
| 5 / 6 | Slow / rapid blink (both use st-style text blink) | 25 |
| 7 | Reverse foreground/background | 27 |
| 8 | Invisible/hidden glyphs | 28 |
| 9 | Strikethrough (including spaces) | 29 |
| 0 or empty | Reset all attributes and default colors | — |

Truecolor, indexed colors and background-color erase are retained. Installed
Bold, Italic and Bold Italic font faces are preferred; if unavailable, rst
applies a lightweight synthetic fallback without changing terminal cell
dimensions. The blink timer is independent of cursor blinking and sleeps when
no visible text requires animation.

For a quick check: `printf '\\033[1;3;4mBold italic underlined\\033[0m\\n'`.
The corresponding state and rendering tests are in `src/terminal.rs` and
`src/render.rs`. Extended SGR underline styles/colors and font ligatures are
outside vanilla st parity and remain future work.

### Native IME and composition preview (macOS / Wayland)

`rst` enables winit's native input method support when creating its window.
On macOS this integrates with the system's text input sources, including
dead-key accents and Japanese/Chinese/Korean input methods; on Wayland it
uses the compositor's text-input support (availability depends on compositor
and input method). `XIM` itself is an X11-specific API and is not used by
rst's macOS/Wayland-only backends.

- `Ime::Preedit` is displayed as a temporary, underlined overlay at the
  terminal cursor. It does **not** modify the PTY or the screen buffer.
- `Ime::Commit` sends the final UTF-8 text exactly once to the child.
  An IME cancel, loss of focus, empty composition or `Ime::Disabled` clears
  the preview without inserting text.
- Native candidate popups follow the preedit caret through
  `Window::set_ime_cursor_area`, including cursor moves and window resizing.
- UTF-8 byte positions are converted to grapheme-aware terminal cells.
  Full-width CJK text, combined accents and emoji sequences are not split
  across cells during preview layout. Long compositions can wrap.
- Ordinary function keys, Ctrl/Alt combinations and window shortcuts
  still use the existing keyboard encoder outside composition.

**Current renderer limitation:** `fontdue` rasterizes individual glyphs;
complex-script shaping, advanced ligatures and joined emoji appearance are
not yet equivalent to a HarfBuzz-based text renderer. The OS handles
candidate selection and text composition; only the final committed text
is written to the terminal application's PTY.

Regression tests: `src/ime_tests.rs` and preedit overlay tests in
`src/render.rs`. Manual validation should cover macOS Japanese/Korean/
Chinese input and a Wayland compositor with text-input-v3 support.

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

### VT/DEC device-control extensions

The existing origin mode (DECOM), delayed auto-wrap (DECAWM), insert/newline
modes, tab stops, scrolling margins and cursor save/restore are now extended
with DEC device-control sequences:

- **DECRQM**: query the current state of supported ANSI/DEC modes using
  `CSI Ps $ p` or `CSI ? Ps $ p`. The response is `CSI [ ? ] Ps ; Pm $ y`,
  where `Pm` is 1 (set), 2 (reset), or 0 (unknown).
- **DECSTR** (`CSI ! p`): reset cursor attributes and modes without erasing
  the screen or scrollback, unlike the full `ESC c` reset.
- **DECALN** (`ESC # 8`): display an `E` in every screen cell, matching
  the traditional st alignment test.

All operations have PTY-byte-stream regression tests in
`src/vt_modes_tests.rs`. These extensions are a compatibility milestone,
not a claim of complete VT500 emulation; advanced DCS and left/right margins
are still out of scope.

### OSC: titles, dynamic colors, clipboard

rst now supports st-compatible OSC commands with BEL and ST (ESC backslash)
terminators, including split PTY reads:

- **OSC 0/1/2:** Window and icon titles. OSC 0 sets both, 1 sets the icon
  title and 2 sets the native window title. Empty titles reset to the default.
- **OSC 4/104:** Change/query and selectively or globally reset all 256
  palette entries. Multiple index/color pairs are supported.
- **OSC 10/11/12:** Change or query default foreground, background and cursor
  colors. **OSC 110/111/112** resets each to its configured initial value.
- Queries (`?`) return xterm's 16-bit RGB response format.
- Colors accept #RGB, #RRGGBB, #RRRGGGBBB, #RRRRGGGGBBBB and rgb:R/G/B.
  X11 named color strings are not supported across macOS and Wayland.
- Existing indexed and default-colored cells in the screen, scrollback and
  saved alternate screen are recolored; explicit RGB cells stay unchanged.

**OSC 52** clipboard *writes* are implemented, but **disabled by default**.
Enable only for trusted shells by adding this configuration:

```toml
[clipboard]
osc52_write = true
```

The existing `clipboard.enabled = false` also prevents OSC 52 writes.
OSC 52 clipboard *reads* are never supported. Writes are limited to valid
UTF-8 of at most 64 KiB, and the queue is bounded. A remote shell can
overwrite your host clipboard if this option is enabled.
Protocol tests: `src/osc_tests.rs` and `src/osc.rs`.

### Text selection (st-style)

- **Left-drag:** Select arbitrary text across rows.
- **Double-click (within 300 ms):** Snap to a whole word. Like vanilla st,
  space separates words, while paths such as `/tmp/file.rs` stay intact.
- **Triple-click (within 600 ms):** Select the whole logical line, including
  any soft-wrapped continuation rows.
- **Alt/Option + left-drag:** Select a rectangular column block. Alt can
  also be pressed or released during a drag to change its geometry.
- **Shift + mouse** temporarily bypasses mouse reporting when a TUI such
  as tmux or Neovim owns the mouse. Use Shift+Alt to force rectangle selection.

Selections handle double-width Unicode cells, soft wraps and scrollback.
Normal selection joins soft-wrapped rows without inserting extra newlines;
rectangular selection copies the chosen column range from each row.
Resizing, changing the scrollback viewport or switching alternate screens
invalidates stale selections. Press **Cmd+C** on macOS (or the platform's
copy shortcut) to copy, or set `[clipboard] copy_on_select = true`.

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
temporarily select text while a TUI is tracking the mouse, matching
the original st's forced-selection modifier. Shift+wheel uses local scrollback
on the primary screen; alternate-screen applications do not expose that history.

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

This is a work-in-progress: extended VT modes, complex grapheme shaping, complete alt-screen compatibility, font ligatures and richer OSC security prompts remain to be implemented/tested. Linux clipboard integration currently relies on compositor data-control support; the macOS native clipboard is the primary first-run target.

## Inspiration and attribution

The architecture follows st's separation of terminal state (`st.c`) from platform rendering/input (`x.c`), but the Rust implementation is original. Read [`docs/st-reference.md`](docs/st-reference.md) for the source references and compatibility backlog. No C source from st is vendored.

MIT licensed; see [`LICENSE`](LICENSE).

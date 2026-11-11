mod attention;
mod charset;
mod command_capture;
mod copy_feedback;
mod config;
mod help;
mod ime;
mod keyboard;
mod mouse;
#[cfg(target_os = "macos")]
mod macos_icon;
mod osc;
mod pty;
mod render;
mod selection;
mod shell_integration;
mod shaping;
mod terminal;
mod url_picker;

use std::{num::NonZeroU32, sync::Arc, time::{Duration, Instant}};

use anyhow::Result;
use softbuffer::{Context, Surface};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{UserAttentionType, Window, WindowId},
};

use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use crate::{
    attention::{BellLimiter, FocusTracker, VISUAL_BELL_DURATION},
    command_capture::CommandCapture,
    copy_feedback::CopyNotice,
    config::Config,
    keyboard::{encode_key, KeyboardModes},
    ime::ImeState,
    mouse::MouseReporter,
    pty::{PtySession, UserEvent},
    render::Renderer,
    selection::{ClickTracker, Selection},
    terminal::{CursorShape, CursorStyle, Palette, Terminal},
    url_picker::UrlPicker,
};

struct App {
    config: Config,
    original_font_size: f32,
    terminal: Terminal,
    pty: PtySession,
    context: Context<OwnedDisplayHandle>,
    window: Option<Arc<Window>>,
    surface: Option<Surface<OwnedDisplayHandle, Arc<Window>>>,
    renderer: Option<Renderer>,
    modifiers: ModifiersState,
    ime: ImeState,
    mouse: (f64, f64),
    mouse_reporter: MouseReporter,
    selection: Option<Selection>,
    clicks: ClickTracker,
    selecting: bool,
    next_cursor_blink: Instant,
    next_text_blink: Instant,
    focus: FocusTracker,
    bell_limiter: BellLimiter,
    bell_flash_until: Option<Instant>,
    help_open: bool,
    help_scroll: usize,
    url_picker: Option<UrlPicker>,
    toast: Option<CopyNotice>,
    next_toast_frame: Option<Instant>,
}

impl App {
    fn resize_terminal(&mut self) {
        let (Some(window), Some(renderer)) = (&self.window, &self.renderer) else {
            return;
        };
        let size = window.inner_size();
        let cols = ((size.width as usize).saturating_sub(2 * renderer.padding)
            / renderer.cell_width)
            .max(1);
        let rows = ((size.height as usize).saturating_sub(2 * renderer.padding)
            / renderer.cell_height)
            .max(1);
        if (cols, rows) != (self.terminal.screen.cols, self.terminal.screen.rows) {
            self.terminal.screen.resize(cols, rows);
            self.selection = None;
            self.selecting = false;
            self.clicks.reset();
            self.pty
                .resize(cols, rows, renderer.cell_width, renderer.cell_height);
        }
        window.request_redraw();
        self.update_ime_cursor_area();
    }

    /// Keep native candidate popups adjacent to the composition caret.
    /// winit consumes physical, window-local pixel coordinates.
    fn update_ime_cursor_area(&self) {
        let (Some(window), Some(renderer)) = (&self.window, &self.renderer) else {
            return;
        };
        let screen = &self.terminal.screen;
        let (col, row) = if let Some(ref preedit) = self.ime.preedit {
            ime::layout(preedit, screen.cols, screen.rows, (screen.x, screen.y)).candidate
        } else {
            (screen.x.min(screen.cols.saturating_sub(1)),
             screen.y.min(screen.rows.saturating_sub(1)))
        };
        let x = renderer.padding.saturating_add(col.saturating_mul(renderer.cell_width));
        let y = renderer.padding.saturating_add(row.saturating_mul(renderer.cell_height));
        window.set_ime_cursor_area(
            PhysicalPosition::new(x.min(i32::MAX as usize) as i32,
                                  y.min(i32::MAX as usize) as i32),
            PhysicalSize::new(renderer.cell_width.min(u32::MAX as usize) as u32,
                              renderer.cell_height.min(u32::MAX as usize) as u32),
        );
    }

    fn redraw(&mut self) {
        let (Some(window), Some(surface), Some(renderer)) =
            (&self.window, &mut self.surface, &mut self.renderer)
        else {
            return;
        };
        let size = window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        if let Err(err) = surface.resize(width, height) {
            eprintln!("rst: resize surface: {err}");
            return;
        }
        let mut buffer = match surface.buffer_mut() {
            Ok(buffer) => buffer,
            Err(err) => {
                eprintln!("rst: acquire buffer: {err}");
                return;
            }
        };
        renderer.draw(
            &mut buffer,
            size.width as usize,
            size.height as usize,
            &self.terminal.screen,
            self.selection,
            self.ime.preedit.as_ref(),
        );
        if self.bell_flash_until.is_some_and(|until| Instant::now() < until) {
            render::paint_bell_border(
                &mut buffer,
                size.width as usize,
                size.height as usize,
                self.terminal.screen.palette.cursor,
            );
        }
        if let Some(notice) = &self.toast {
            if let Some(output_id) = notice.output_id {
                renderer.draw_output_copy_highlight(
                    &mut buffer, size.width as usize, size.height as usize,
                    &self.terminal.screen, output_id, notice.opacity(Instant::now()),
                );
            }
        }
        if self.help_open {
            renderer.draw_help_overlay(
                &mut buffer, size.width as usize, size.height as usize,
                &self.terminal.screen, self.help_scroll, cfg!(target_os = "macos"),
            );
        }
        if let Some(picker) = &self.url_picker {
            renderer.draw_url_picker_overlay(
                &mut buffer, size.width as usize, size.height as usize,
                &self.terminal.screen, picker, self.config.url_picker.recent_outputs,
            );
        }
        if let Some(notice) = &self.toast {
            let opacity = notice.opacity(Instant::now());
            if opacity != 0 {
                renderer.draw_copy_notice(
                    &mut buffer, size.width as usize, size.height as usize,
                    &self.terminal.screen, notice, opacity,
                );
            }
        }
        window.pre_present_notify();
        if let Err(err) = buffer.present() {
            eprintln!("rst: present: {err}");
        }
    }

    fn handle_bell(&mut self) {
        if !self.config.bell.enabled || !self.bell_limiter.allow(Instant::now()) {
            return;
        }
        if self.config.bell.audible {
            attention::audible_bell();
        }
        if self.config.bell.visual {
            self.bell_flash_until = Some(Instant::now() + VISUAL_BELL_DURATION);
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
        // st marks an unfocused window as urgent. Winit maps this onto
        // Dock attention on macOS and xdg-activation where Wayland supports it.
        if !self.focus.is_focused() {
            if let Some(window) = &self.window {
                window.request_user_attention(Some(UserAttentionType::Informational));
            }
        }
    }

    fn scroll_scrollback(&mut self, lines: i32) {
        let old_offset = self.terminal.screen.view_offset;
        self.terminal.screen.scroll_view(lines);
        if self.terminal.screen.view_offset != old_offset {
            self.selection = None;
            self.selecting = false;
            self.clicks.reset();
        }
    }

    fn cell_at_mouse(&self) -> (usize, usize) {
        let Some(renderer) = &self.renderer else {
            return (0, 0);
        };
        let x = (self.mouse.0.max(0.0) as usize).saturating_sub(renderer.padding)
            / renderer.cell_width;
        let y = (self.mouse.1.max(0.0) as usize).saturating_sub(renderer.padding)
            / renderer.cell_height;
        (
            x.min(self.terminal.screen.cols - 1),
            y.min(self.terminal.screen.rows - 1),
        )
    }

    fn copy_selection(&self) {
        if !self.config.clipboard.enabled {
            return;
        }
        if let Some(selection) = self.selection {
            let value = self.terminal.screen.selected_text(&selection);
            if !value.is_empty() {
                match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(value)) {
                    Ok(()) => {}
                    Err(err) => eprintln!("rst: copy: {err}"),
                }
            }
        }
    }

    fn paste_clipboard(&mut self) {
        if !self.config.clipboard.enabled {
            return;
        }
        let result = arboard::Clipboard::new().and_then(|mut cb| cb.get_text());
        match result {
            Ok(value) => {
                // The PTY wants CR for newlines; bracketed paste protects shell editing.
                let text = value.replace("\r\n", "\n").replace('\n', "\r");
                if self.terminal.screen.bracketed_paste {
                    self.pty.write(b"\x1b[200~");
                    self.pty.write(text.as_bytes());
                    self.pty.write(b"\x1b[201~");
                } else {
                    self.pty.write(text.as_bytes());
                }
            }
            Err(err) => eprintln!("rst: paste: {err}"),
        }
    }

    fn zoom_font(&mut self, action: ZoomAction) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let next_size = zoomed_font_size(
            self.config.font.size,
            self.original_font_size,
            action,
        );
        if (next_size - self.config.font.size).abs() < f32::EPSILON {
            return;
        }

        // Fonts were discovered and parsed when the window was created.
        // Zoom changes only their metrics and cached glyph bitmaps: no new
        // fontdb::load_system_fonts() scan on the UI event loop.
        let scale = window.scale_factor();
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        renderer.set_font_size(next_size, scale);
        self.config.font.size = next_size;
        self.selection = None;
        self.selecting = false;
        self.resize_terminal();
    }

    fn present_notice(&mut self, notice: CopyNotice) {
        self.next_toast_frame = notice.next_wakeup(Instant::now());
        self.toast = Some(notice);
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn notify(&mut self, message: impl Into<String>) {
        self.present_notice(CopyNotice::warning(message, Instant::now()));
    }

    fn copy_last_output(&mut self) {
        if !self.config.clipboard.enabled {
            self.notify("Clipboard access is disabled in config.toml");
            return;
        }
        let (text, output_id) = match self.terminal.screen.command_capture.latest_output_with_id() {
            Ok(pair) => pair,
            Err(err) => {
                self.notify(err.message());
                return;
            }
        };
        let length = text.len();
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text)) {
            Ok(()) => self.present_notice(CopyNotice::success(
                length, output_id, Instant::now(),
            )),
            Err(err) => {
                eprintln!("rst: command output clipboard: {err}");
                self.notify("Clipboard unavailable: could not copy command output");
            }
        }
    }

    fn toggle_url_picker(&mut self) {
        if self.url_picker.is_some() {
            self.url_picker = None;
        } else if !self.terminal.screen.command_capture.saw_markers() {
            self.notify("No shell markers: run rst --install-shell-integration");
            return;
        } else {
            self.url_picker = Some(UrlPicker::new(
                self.terminal.screen.command_capture.recent_urls(),
            ));
        }
        self.ime.clear();
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn handle_url_picker_key(&mut self, key: &Key<&str>) {
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.toggle_url_picker();
            return;
        }
        if matches!(key, Key::Named(NamedKey::Enter)) {
            if let Some(url) = self.url_picker.as_ref()
                .and_then(|picker| picker.selected_url()).map(str::to_owned)
            {
                // Never involve a shell; do not auto-open untrusted log data.
                // Only the user explicitly pressing Enter launches a link.
                let program = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
                match std::process::Command::new(program)
                    .arg(&url)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(_) => self.toggle_url_picker(),
                    Err(err) => {
                        eprintln!("rst: open link: {err}");
                        self.notify(format!("Could not launch browser: {err}"));
                    }
                }
            }
            return;
        }
        let visible = url_picker::visible_rows(self.terminal.screen.rows);
        if let Some(picker) = self.url_picker.as_mut() {
            match key {
                Key::Character(value) if value.eq_ignore_ascii_case("j") =>
                    picker.move_by(1, visible),
                Key::Character(value) if value.eq_ignore_ascii_case("k") =>
                    picker.move_by(-1, visible),
                Key::Named(NamedKey::ArrowDown) => picker.move_by(1, visible),
                Key::Named(NamedKey::ArrowUp) => picker.move_by(-1, visible),
                Key::Named(NamedKey::PageDown) => picker.move_by(visible as i32, visible),
                Key::Named(NamedKey::PageUp) => picker.move_by(-(visible as i32), visible),
                Key::Named(NamedKey::Home) => picker.select_first(),
                Key::Named(NamedKey::End) => picker.select_last(visible),
                _ => {}
            }
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn toggle_help(&mut self) {
        self.help_open = !self.help_open;
        self.help_scroll = 0;
        self.ime.clear();
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn handle_help_key(&mut self, key: &Key<&str>) {
        let lines = help::lines(cfg!(target_os = "macos")).len();
        let visible = self.terminal.screen.rows.saturating_sub(5).min(23).max(1);
        let limit = lines.saturating_sub(visible);
        match key {
            Key::Named(NamedKey::Escape) => self.toggle_help(),
            Key::Named(NamedKey::ArrowDown) => {
                self.help_scroll = (self.help_scroll + 1).min(limit);
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.help_scroll = self.help_scroll.saturating_sub(1);
            }
            Key::Named(NamedKey::PageDown) => {
                self.help_scroll = self.help_scroll.saturating_add(visible).min(limit);
            }
            Key::Named(NamedKey::PageUp) => {
                self.help_scroll = self.help_scroll.saturating_sub(visible);
            }
            _ => {}
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn reset_cursor_blink(&mut self) {
        if !self.terminal.screen.cursor_style.blink {
            return;
        }
        self.next_cursor_blink =
            Instant::now() + Duration::from_millis(self.config.cursor.blink_interval_ms);
        if !self.terminal.screen.cursor_blink_on {
            self.terminal.screen.cursor_blink_on = true;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }

    fn handle_key(&mut self, event: KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let key = event.logical_key.as_ref();
        if self.url_picker.is_some() {
            if matches!(key, Key::Named(NamedKey::F1))
                || command_shortcut(&key, self.modifiers, cfg!(target_os = "macos"), "h")
            {
                self.url_picker = None;
                self.toggle_help();
            } else if command_shortcut(&key, self.modifiers, cfg!(target_os = "macos"), "u") {
                self.toggle_url_picker();
            } else {
                self.handle_url_picker_key(&key);
            }
            return;
        }
        if matches!(key, Key::Named(NamedKey::F1))
            || command_shortcut(&key, self.modifiers, cfg!(target_os = "macos"), "h")
        {
            self.toggle_help();
            return;
        }
        if self.help_open {
            self.handle_help_key(&key);
            return;
        }
        if self.ime.composing() {
            // Preedit is never forwarded as ordinary keyboard input.
            return;
        }
        if command_shortcut(&key, self.modifiers, cfg!(target_os = "macos"), "o") {
            self.copy_last_output();
            return;
        }
        if command_shortcut(&key, self.modifiers, cfg!(target_os = "macos"), "u") {
            self.toggle_url_picker();
            return;
        }
        // Typing wakes the cursor and restarts the blink interval.
        self.reset_cursor_blink();
        if let Some(action) = zoom_shortcut(
            &key,
            self.modifiers,
            cfg!(target_os = "macos"),
        ) {
            self.zoom_font(action);
            return;
        }
        let primary = if cfg!(target_os = "macos") {
            self.modifiers.super_key()
        } else {
            self.modifiers.control_key() && self.modifiers.shift_key()
        };
        if primary {
            if let Key::Character(text) = key {
                if text.eq_ignore_ascii_case("c") {
                    self.copy_selection();
                    return;
                }
                if text.eq_ignore_ascii_case("v") {
                    self.paste_clipboard();
                    return;
                }
            }
        }

        // Shift+PageUp/Down remains the rst scrollback shortcut on the
        // primary screen. In full-screen TUIs it must reach the child.
        if self.modifiers == ModifiersState::SHIFT
            && !self.terminal.screen.in_alternate_screen()
        {
            let scroll = match key {
                Key::Named(NamedKey::PageUp) => Some(self.terminal.screen.rows as i32),
                Key::Named(NamedKey::PageDown) => Some(-(self.terminal.screen.rows as i32)),
                _ => None,
            };
            if let Some(lines) = scroll {
                self.scroll_scrollback(lines);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
                return;
            }
        }

        if self.terminal.screen.keyboard_lock {
            return; // ANSI KAM (CSI 2 h): suppress PTY keyboard input.
        }

        let bytes = encode_key(
            &key,
            event.physical_key,
            event.location,
            event.text_with_all_modifiers(),
            self.modifiers,
            KeyboardModes {
                application_cursor: self.terminal.screen.app_cursor,
                application_keypad: self.terminal.screen.app_keypad,
            },
        );
        if !bytes.is_empty() {
            self.selection = None;
            self.terminal.screen.view_offset = 0;
            self.pty.write(&bytes);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZoomAction {
    In,
    Out,
    Reset,
}

fn command_shortcut(
    key: &Key<&str>,
    modifiers: ModifiersState,
    macos: bool,
    letter: &str,
) -> bool {
    let primary = if macos {
        modifiers.super_key() && !modifiers.control_key()
    } else {
        modifiers.control_key() && !modifiers.super_key()
    };
    primary
        && modifiers.shift_key()
        && !modifiers.alt_key()
        && matches!(key, Key::Character(value) if value.eq_ignore_ascii_case(letter))
}

fn zoomed_font_size(current: f32, initial: f32, action: ZoomAction) -> f32 {
    let size = match action {
        ZoomAction::In => current + 1.0,
        ZoomAction::Out => current - 1.0,
        ZoomAction::Reset => initial,
    };
    size.clamp(6.0, 72.0)
}

/// macOS: Cmd++ / Cmd+- / Cmd+0. Wayland: Ctrl++ / Ctrl+- / Ctrl+0.
/// '+' and '=' are both accepted because layouts differ in whether '+' needs
/// Shift. Never send a recognized zoom shortcut to the child PTY.
fn zoom_shortcut(
    key: &Key<&str>,
    modifiers: ModifiersState,
    is_macos: bool,
) -> Option<ZoomAction> {
    let pressed = if is_macos {
        modifiers.super_key() && !modifiers.control_key()
    } else {
        modifiers.control_key() && !modifiers.super_key()
    };
    if !pressed || modifiers.alt_key() {
        return None;
    }
    match key {
        Key::Character(text) => match *text {
            "+" | "=" => Some(ZoomAction::In),
            "-" | "_" => Some(ZoomAction::Out),
            "0" => Some(ZoomAction::Reset),
            _ => None,
        },
        _ => None,
    }
}

/// Calculate the next blink frame without relying on timers or a busy-loop.
fn blink_step(
    visible: bool,
    next: Instant,
    now: Instant,
    interval: Duration,
) -> (bool, Instant) {
    if now >= next {
        (!visible, now + interval)
    } else {
        (visible, next)
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let mut next_deadline: Option<Instant> = None;

        // The cursor and SGR 5/6 text blink on *independent* clocks.
        // Hiding the cursor with DECTCEM must not freeze blinking text.
        if self.terminal.screen.cursor_style.blink
            && self.terminal.screen.show_cursor
            && self.window.is_some()
        {
            let interval = Duration::from_millis(self.config.cursor.blink_interval_ms);
            let (visible, next) = blink_step(
                self.terminal.screen.cursor_blink_on,
                self.next_cursor_blink,
                now,
                interval,
            );
            if visible != self.terminal.screen.cursor_blink_on {
                self.terminal.screen.cursor_blink_on = visible;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            self.next_cursor_blink = next;
            next_deadline = Some(next);
        } else {
            self.terminal.screen.cursor_blink_on = true;
            self.next_cursor_blink =
                now + Duration::from_millis(self.config.cursor.blink_interval_ms);
        }

        if self.window.is_some() && self.terminal.screen.has_blinking_text() {
            // st's SGR blinktimeout defaults to 800 ms.
            let interval = Duration::from_millis(800);
            let (visible, next) = blink_step(
                self.terminal.screen.text_blink_on,
                self.next_text_blink,
                now,
                interval,
            );
            if visible != self.terminal.screen.text_blink_on {
                self.terminal.screen.text_blink_on = visible;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            self.next_text_blink = next;
            next_deadline = Some(next_deadline.map_or(next, |other| other.min(next)));
        } else {
            self.terminal.screen.text_blink_on = true;
            self.next_text_blink = now + Duration::from_millis(800);
        }
        // Expire a visual bell even if the PTY produces no further output.
        if let Some(until) = self.bell_flash_until {
            if now >= until {
                self.bell_flash_until = None;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            } else {
                next_deadline = Some(next_deadline.map_or(until, |other| other.min(until)));
            }
        }
        if let Some(notice) = &self.toast {
            if notice.finished(now) {
                self.toast = None;
                self.next_toast_frame = None;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            } else {
                let wakeup = self.next_toast_frame
                    .or_else(|| notice.next_wakeup(now));
                if let Some(at) = wakeup {
                    if now >= at {
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                        self.next_toast_frame = notice.next_wakeup(now);
                    }
                    if let Some(next) = self.next_toast_frame {
                        next_deadline = Some(next_deadline.map_or(next, |old| old.min(next)));
                    }
                }
            }
        }
        // No busy redraw when neither blinking nor a bell flash is active.
        event_loop.set_control_flow(
            next_deadline.map_or(ControlFlow::Wait, ControlFlow::WaitUntil),
        );
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let logical_width = self.config.window.columns as f64 * self.config.font.size as f64 * 0.65
            + 2.0 * self.config.window.padding as f64;
        let logical_height = self.config.window.rows as f64 * self.config.font.size as f64 * 1.40
            + 2.0 * self.config.window.padding as f64;
        let attributes = Window::default_attributes()
            .with_title(&self.terminal.screen.title)
            .with_inner_size(LogicalSize::new(logical_width, logical_height));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                eprintln!("rst: cannot create window: {err}");
                event_loop.exit();
                return;
            }
        };
        // Make the Dock and Cmd+Tab use the checked-in logo even if this
        // binary was started with cargo run (no .app / Launch Services).
        #[cfg(target_os = "macos")]
        macos_icon::install();
        let renderer =
            match Renderer::new(&self.config.font, self.config.window.padding, window.scale_factor()) {
                Ok(renderer) => renderer,
                Err(err) => {
                    eprintln!("rst: cannot initialize font: {err:#}");
                    event_loop.exit();
                    return;
                }
            };
        let surface = match Surface::new(&self.context, window.clone()) {
            Ok(surface) => surface,
            Err(err) => {
                eprintln!("rst: cannot create surface: {err}");
                event_loop.exit();
                return;
            }
        };
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.surface = Some(surface);
        // Not enabled by default in winit. This activates native macOS
        // text services and Wayland text-input where supported.
        if let Some(window) = &self.window {
            window.set_ime_allowed(true);
        }
        self.resize_terminal();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.window.as_ref().map(|w| w.id()) != Some(window_id) {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) => self.resize_terminal(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(renderer) = self.renderer.as_mut() {
                    // Retina/HiDPI changes also reuse the font faces.
                    renderer.set_font_size(self.config.font.size, scale_factor);
                    renderer.padding =
                        (self.config.window.padding as f64 * scale_factor) as usize;
                    self.resize_terminal();
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } => self.handle_key(event),
            WindowEvent::Ime(event) => {
                if self.help_open || self.url_picker.is_some() {
                    self.ime.clear();
                    return;
                }
                let committed = self.ime.handle(event, self.terminal.screen.keyboard_lock);
                if let Some(bytes) = committed {
                    if !bytes.is_empty() {
                        self.pty.write(&bytes);
                        self.reset_cursor_blink();
                    }
                }
                self.update_ime_cursor_area();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::Focused(focused) => {
                // DECSET 1004: only real focus transitions reach the PTY.
                // Remember focus even while reporting is disabled, so
                // enabling 1004 mid-session does not invent an extra event.
                if let Some(report) = self.focus.change(
                    focused, self.terminal.screen.focus_reporting,
                ) {
                    self.pty.write(report);
                }
                if focused {
                    if let Some(window) = &self.window {
                        window.request_user_attention(None);
                    }
                } else {
                    // Uncommitted composition is never sent to the child.
                    self.ime.clear();
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    // A mouse release may be lost when the window loses focus.
                    self.mouse_reporter.reset();
                    self.selecting = false;
                    self.clicks.reset();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = (position.x, position.y);
                if self.help_open || self.url_picker.is_some() { return; }
                let tracking = self.terminal.screen.mouse_tracking;
                if tracking.enabled() && !self.modifiers.shift_key() && !self.selecting {
                    let at = self.cell_at_mouse();
                    let report = self.mouse_reporter.motion(
                        at,
                        tracking,
                        self.terminal.screen.mouse_sgr,
                        self.modifiers,
                    );
                    if !report.is_empty() {
                        self.pty.write(&report);
                    }
                    return;
                }

                // Shift bypasses application reporting, as in st, allowing
                // ordinary local text selection even when tmux tracks the mouse.
                if self.selecting {
                    let at = self.cell_at_mouse();
                    if let Some(ref mut selection) = self.selection {
                        selection.update(&self.terminal.screen, at, self.modifiers.alt_key());
                    }
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput { button, state, .. } => {
                if self.help_open || self.url_picker.is_some() { return; }
                let tracking = self.terminal.screen.mouse_tracking;
                let at = self.cell_at_mouse();
                // Once Shift begins a local selection inside a TUI, finish
                // that gesture locally even if Shift is released before the
                // mouse button. Otherwise a stray mouse-release escapes to
                // the child application with no matching button press.
                let finishing_local = self.selecting
                    && button == MouseButton::Left
                    && state == ElementState::Released;
                let report_mods = if finishing_local {
                    self.modifiers | ModifiersState::SHIFT
                } else {
                    self.modifiers
                };
                let report = self.mouse_reporter.button(
                    button,
                    state,
                    at,
                    tracking,
                    self.terminal.screen.mouse_sgr,
                    report_mods,
                );
                if !report.is_empty() {
                    self.pty.write(&report);
                }

                if tracking.enabled() && !self.modifiers.shift_key() && !finishing_local {
                    // In reporting mode the child owns the click, not rst's
                    // clipboard selection. A Shift-forced selection can
                    // still be finished after the modifier is released.
                    if self.selecting && button == MouseButton::Left && state == ElementState::Released {
                        self.selecting = false;
                        if self.config.clipboard.copy_on_select {
                            self.copy_selection();
                        }
                    }
                    return;
                }

                if button == MouseButton::Left {
                    if state == ElementState::Pressed {
                        let snap = self.clicks.press(
                            Instant::now(), at, self.modifiers.alt_key(),
                        );
                        self.selection = Some(Selection::new(
                            &self.terminal.screen,
                            at,
                            snap,
                            self.modifiers.alt_key(),
                        ));
                        self.selecting = true;
                    } else if self.selecting {
                        if let Some(ref mut selection) = self.selection {
                            selection.update(
                                &self.terminal.screen, at, self.modifiers.alt_key(),
                            );
                        }
                        self.selecting = false;
                        if self.config.clipboard.copy_on_select {
                            self.copy_selection();
                        }
                    }
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(picker) = self.url_picker.as_mut() {
                    let amount = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y.round() as i32,
                        MouseScrollDelta::PixelDelta(pos) => (pos.y / 24.0).round() as i32,
                    };
                    picker.move_by(-amount, url_picker::visible_rows(self.terminal.screen.rows));
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    return;
                }
                if self.help_open {
                    let amount = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y.round() as i32,
                        MouseScrollDelta::PixelDelta(pos) => (pos.y / 24.0).round() as i32,
                    };
                    let visible = self.terminal.screen.rows.saturating_sub(5).min(23).max(1);
                    let limit = help::lines(cfg!(target_os = "macos")).len()
                        .saturating_sub(visible);
                    if amount > 0 {
                        self.help_scroll = self.help_scroll.saturating_sub(amount as usize);
                    } else {
                        self.help_scroll = self.help_scroll.saturating_add((-amount) as usize)
                            .min(limit);
                    }
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    return;
                }
                let tracking = self.terminal.screen.mouse_tracking;
                if tracking.enabled() && !self.modifiers.shift_key() {
                    let cell_size = self.renderer.as_ref().map_or(
                        (8, 18),
                        |r| (r.cell_width, r.cell_height),
                    );
                    let at = self.cell_at_mouse();
                    let report = self.mouse_reporter.scroll(
                        delta,
                        cell_size,
                        at,
                        tracking,
                        self.terminal.screen.mouse_sgr,
                        self.modifiers,
                    );
                    if !report.is_empty() {
                        self.pty.write(&report);
                    }
                    return;
                }
                // Switching to local scrollback must not leave a partial
                // trackpad wheel delta queued for the child.
                self.mouse_reporter.reset_scroll();
                // Mouse reporting disabled or Shift held: retain rst's
                // scrollback behavior on the primary screen.
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * 3.0) as i32,
                    MouseScrollDelta::PixelDelta(pos) => {
                        let height = self.renderer.as_ref().map_or(18.0, |r| r.cell_height as f64);
                        (pos.y / height).round() as i32
                    }
                };
                self.scroll_scrollback(lines);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Output(bytes) => {
                let previous_mouse_mode = self.terminal.screen.mouse_tracking;
                let previous_alt_screen = self.terminal.screen.in_alternate_screen();
                let previous_view_offset = self.terminal.screen.view_offset;
                let previous_title = self.terminal.screen.title.clone();
                let previous_cursor_style = self.terminal.screen.cursor_style;
                self.terminal.advance(&bytes);
                if self.terminal.screen.take_bells() != 0 {
                    self.handle_bell();
                }
                if previous_alt_screen != self.terminal.screen.in_alternate_screen()
                    || previous_view_offset != self.terminal.screen.view_offset
                {
                    self.selection = None;
                    self.selecting = false;
                    self.clicks.reset();
                }
                if self.terminal.screen.cursor_style != previous_cursor_style {
                    self.next_cursor_blink = Instant::now()
                        + Duration::from_millis(self.config.cursor.blink_interval_ms);
                }
                if self.terminal.screen.title != previous_title {
                    if let Some(window) = &self.window {
                        window.set_title(&self.terminal.screen.title);
                    }
                }
                // OSC 52 is opt-in, write-only, and size-limited. Native
                // clipboard APIs must be accessed on the window/UI thread.
                for text in self.terminal.screen.take_clipboard_writes() {
                    if self.config.clipboard.enabled && self.config.clipboard.osc52_write {
                        if let Err(err) =
                            arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text))
                        {
                            eprintln!("rst: OSC 52 clipboard write: {err}");
                        }
                    }
                }
                if self.terminal.screen.mouse_tracking != previous_mouse_mode {
                    // Never carry held buttons or stale movement across a
                    // TUI enabling/disabling DEC mouse reporting.
                    self.mouse_reporter.reset();
                    self.selecting = false;
                    self.clicks.reset();
                    if self.terminal.screen.mouse_tracking.enabled() {
                        // An old local selection must not overlay a TUI that
                        // now owns the mouse.
                        self.selection = None;
                    }
                }
                let replies = self.terminal.screen.take_replies();
                if !replies.is_empty() {
                    self.pty.write(&replies);
                }
                self.update_ime_cursor_area();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            UserEvent::Closed => event_loop.exit(),
        }
    }
}

fn main() -> Result<()> {
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new("--install-shell-integration"))
    {
        let changes = shell_integration::install()?;
        if changes.is_empty() {
            println!("rst: shell integration already installed");
        } else {
            for line in changes { println!("rst: {line}"); }
        }
        println!("rst: open a new rst window to activate shell integration");
        return Ok(());
    }
    let config = config::load()?;
    let palette = Palette::try_from(&config.colors)?;
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    // softbuffer's platform error holds non-Send raw window handles.
    // Convert it into an owned diagnostic before passing through anyhow.
    let context = Context::new(event_loop.owned_display_handle())
        .map_err(|err| anyhow::anyhow!("cannot initialize display: {err}"))?;
    let pty = PtySession::start(
        config.window.columns,
        config.window.rows,
        &config.terminal.term,
        event_loop.create_proxy(),
    )?;
    let mut terminal = Terminal::new(
        config.window.columns,
        config.window.rows,
        config.scrollback.lines,
        palette,
    );
    terminal.screen.osc52_write_allowed = config.clipboard.enabled && config.clipboard.osc52_write;
    terminal.screen.command_capture = CommandCapture::with_url_history(
        config.command_output.max_megabytes.saturating_mul(1024 * 1024),
        config.url_picker.recent_outputs,
    );
    let shape = match config.cursor.style.as_str() {
        "block" => CursorShape::Block,
        "underline" => CursorShape::Underline,
        "bar" => CursorShape::Bar,
        _ => unreachable!("validated cursor style"),
    };
    terminal.screen.set_default_cursor_style(CursorStyle {
        shape,
        blink: config.cursor.blink,
    });
    let original_font_size = config.font.size;
    let config_blink_ms = config.cursor.blink_interval_ms;
    let mut app = App {
        config,
        original_font_size,
        terminal,
        pty,
        context,
        window: None,
        surface: None,
        renderer: None,
        modifiers: ModifiersState::empty(),
        ime: ImeState::default(),
        mouse: (0.0, 0.0),
        mouse_reporter: MouseReporter::default(),
        selection: None,
        clicks: ClickTracker::default(),
        selecting: false,
        next_cursor_blink: Instant::now()
            + Duration::from_millis(config_blink_ms),
        next_text_blink: Instant::now() + Duration::from_millis(800),
        focus: FocusTracker::default(),
        bell_limiter: BellLimiter::default(),
        bell_flash_until: None,
        help_open: false,
        help_scroll: 0,
        url_picker: None,
        toast: None,
        next_toast_frame: None,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;

    #[test]
    fn cursor_blink_steps_only_after_deadline() {
        let now = Instant::now();
        let interval = Duration::from_millis(700);
        let due = now + interval;
        let (visible, deadline) = blink_step(true, due, now, interval);
        assert!(visible);
        assert_eq!(deadline, due);
        let (visible, deadline) = blink_step(true, due, due, interval);
        assert!(!visible);
        assert_eq!(deadline, due + interval);
        let (visible, _) = blink_step(false, deadline, deadline, interval);
        assert!(visible);
    }

    #[test]
    fn macos_command_plus_and_minus_are_zoom_shortcuts() {
        let cmd = ModifiersState::SUPER;
        for key in ["+", "="] {
            assert_eq!(
                zoom_shortcut(&Key::Character(key), cmd, true),
                Some(ZoomAction::In)
            );
        }
        for key in ["-", "_"] {
            assert_eq!(
                zoom_shortcut(&Key::Character(key), cmd, true),
                Some(ZoomAction::Out)
            );
        }
        assert_eq!(
            zoom_shortcut(&Key::Character("0"), cmd, true),
            Some(ZoomAction::Reset)
        );
    }

    #[test]
    fn zoom_shortcuts_only_handle_the_platform_modifier() {
        let ctrl = ModifiersState::CONTROL;
        let cmd = ModifiersState::SUPER;
        assert_eq!(
            zoom_shortcut(&Key::Character("+"), ctrl, false),
            Some(ZoomAction::In)
        );
        assert_eq!(zoom_shortcut(&Key::Character("+"), ctrl, true), None);
        assert_eq!(zoom_shortcut(&Key::Character("+"), cmd, false), None);
        assert_eq!(
            zoom_shortcut(
                &Key::Character("+"),
                cmd | ModifiersState::ALT,
                true
            ),
            None
        );
        assert_eq!(
            zoom_shortcut(&Key::Character("x"), cmd, true),
            None
        );
    }

    #[test]
    fn copy_output_and_help_shortcuts_are_platform_specific() {
        let cmd_shift = ModifiersState::SUPER | ModifiersState::SHIFT;
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert!(command_shortcut(&Key::Character("O"), cmd_shift, true, "o"));
        assert!(command_shortcut(&Key::Character("o"), ctrl_shift, false, "o"));
        assert!(command_shortcut(&Key::Character("H"), cmd_shift, true, "h"));
        assert!(command_shortcut(&Key::Character("U"), cmd_shift, true, "u"));
        assert!(command_shortcut(&Key::Character("u"), ctrl_shift, false, "u"));
        assert!(command_shortcut(&Key::Character("h"), ctrl_shift, false, "h"));
        assert!(!command_shortcut(&Key::Character("o"), ModifiersState::SUPER, true, "o"));
        assert!(!command_shortcut(&Key::Character("o"), ctrl_shift, true, "o"));
        assert!(!command_shortcut(&Key::Character("o"), cmd_shift, false, "o"));
        assert!(!command_shortcut(
            &Key::Character("o"), cmd_shift | ModifiersState::ALT, true, "o",
        ));
    }

    #[test]
    fn font_zoom_changes_size_and_can_reset_to_configured_value() {
        assert_eq!(zoomed_font_size(15.0, 15.0, ZoomAction::In), 16.0);
        assert_eq!(zoomed_font_size(16.0, 15.0, ZoomAction::Out), 15.0);
        assert_eq!(zoomed_font_size(23.0, 16.5, ZoomAction::Reset), 16.5);
        assert_eq!(zoomed_font_size(72.0, 15.0, ZoomAction::In), 72.0);
        assert_eq!(zoomed_font_size(6.0, 15.0, ZoomAction::Out), 6.0);
    }


}

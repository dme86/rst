mod config;
mod keyboard;
mod pty;
mod render;
mod terminal;

use std::{num::NonZeroU32, sync::Arc, time::{Duration, Instant}};

use anyhow::Result;
use softbuffer::{Context, Surface};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Window, WindowId},
};

use crate::{
    config::Config,
    keyboard::{encode_key, KeyboardModes},
    pty::{PtySession, UserEvent},
    render::Renderer,
    terminal::{Palette, Terminal},
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
    mouse: (f64, f64),
    selection: Option<((usize, usize), (usize, usize))>,
    selecting: bool,
    next_cursor_blink: Instant,
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
            self.pty
                .resize(cols, rows, renderer.cell_width, renderer.cell_height);
        }
        window.request_redraw();
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
        );
        window.pre_present_notify();
        if let Err(err) = buffer.present() {
            eprintln!("rst: present: {err}");
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
        if let Some((start, end)) = self.selection {
            let value = self.terminal.screen.selected_text(start, end);
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

    fn reset_cursor_blink(&mut self) {
        if !self.config.cursor.blink {
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
        // Typing wakes the cursor and restarts the blink interval.
        self.reset_cursor_blink();
        let key = event.logical_key.as_ref();
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
                self.terminal.screen.scroll_view(lines);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
                return;
            }
        }

        let bytes = encode_key(
            &key,
            event.physical_key,
            event.location,
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
        if self.config.cursor.blink
            && self.terminal.screen.show_cursor
            && self.window.is_some()
        {
            let now = Instant::now();
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
            // Sleep between blink events. No continuous frame loop.
            event_loop.set_control_flow(ControlFlow::WaitUntil(next));
        } else {
            // If a TUI hides the cursor (DECTCEM), don't animate it.
            self.terminal.screen.cursor_blink_on = true;
            self.next_cursor_blink = Instant::now()
                + Duration::from_millis(self.config.cursor.blink_interval_ms);
            event_loop.set_control_flow(ControlFlow::Wait);
        }
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
            .with_title("rst — Rust Simple Terminal")
            .with_inner_size(LogicalSize::new(logical_width, logical_height));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                eprintln!("rst: cannot create window: {err}");
                event_loop.exit();
                return;
            }
        };
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
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = (position.x, position.y);
                if self.selecting {
                    let at = self.cell_at_mouse();
                    if let Some((start, _)) = self.selection {
                        self.selection = Some((start, at));
                    }
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput {
                button: MouseButton::Left,
                state,
                ..
            } => {
                if state == ElementState::Pressed {
                    let pos = self.cell_at_mouse();
                    self.selection = Some((pos, pos));
                    self.selecting = true;
                } else if self.selecting {
                    self.selecting = false;
                    if self.config.clipboard.copy_on_select {
                        self.copy_selection();
                    }
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * 3.0) as i32,
                    MouseScrollDelta::PixelDelta(pos) => {
                        let height = self.renderer.as_ref().map_or(18.0, |r| r.cell_height as f64);
                        (pos.y / height).round() as i32
                    }
                };
                self.terminal.screen.scroll_view(lines);
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
                self.terminal.advance(&bytes);
                let replies = self.terminal.screen.take_replies();
                if !replies.is_empty() {
                    self.pty.write(&replies);
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            UserEvent::Closed => event_loop.exit(),
        }
    }
}

fn main() -> Result<()> {
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
    let terminal = Terminal::new(
        config.window.columns,
        config.window.rows,
        config.scrollback.lines,
        palette,
    );
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
        mouse: (0.0, 0.0),
        selection: None,
        selecting: false,
        next_cursor_blink: Instant::now()
            + Duration::from_millis(config_blink_ms),
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
    fn font_zoom_changes_size_and_can_reset_to_configured_value() {
        assert_eq!(zoomed_font_size(15.0, 15.0, ZoomAction::In), 16.0);
        assert_eq!(zoomed_font_size(16.0, 15.0, ZoomAction::Out), 15.0);
        assert_eq!(zoomed_font_size(23.0, 16.5, ZoomAction::Reset), 16.5);
        assert_eq!(zoomed_font_size(72.0, 15.0, ZoomAction::In), 72.0);
        assert_eq!(zoomed_font_size(6.0, 15.0, ZoomAction::Out), 6.0);
    }


}

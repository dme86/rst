mod config;
mod pty;
mod render;
mod terminal;

use std::{num::NonZeroU32, sync::Arc};

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

        // Keep the previous font and renderer if loading the new font fails.
        let mut next_font = self.config.font.clone();
        next_font.size = next_size;
        match Renderer::new(
            &next_font,
            self.config.window.padding,
            window.scale_factor(),
        ) {
            Ok(renderer) => {
                self.renderer = Some(renderer);
                self.config.font = next_font;
                // Selection coordinates refer to the old grid.
                self.selection = None;
                self.selecting = false;
                // A new font changes the character-cell dimensions; propagate
                // the resulting terminal geometry to the PTY/tmux/Neovim.
                self.resize_terminal();
            }
            Err(err) => eprintln!("rst: cannot change font size: {err:#}"),
        }
    }

    fn handle_key(&mut self, event: KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
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

        let mut bytes: Vec<u8> = Vec::new();
        match key {
            // Space is NamedKey::Space on macOS and Wayland, not Key::Character(" ").
            Key::Named(NamedKey::Space) => bytes.extend_from_slice(space_bytes(
                self.modifiers.control_key(),
                self.modifiers.alt_key(),
                self.modifiers.super_key(),
            )),
            Key::Named(NamedKey::Enter) => bytes.push(b'\r'),
            Key::Named(NamedKey::Tab) => bytes.push(b'\t'),
            Key::Named(NamedKey::Backspace) => bytes.push(0x7f),
            Key::Named(NamedKey::Escape) => bytes.push(0x1b),
            Key::Named(NamedKey::ArrowUp) => bytes.extend_from_slice(
                if self.terminal.screen.app_cursor { b"\x1bOA" } else { b"\x1b[A" },
            ),
            Key::Named(NamedKey::ArrowDown) => bytes.extend_from_slice(
                if self.terminal.screen.app_cursor { b"\x1bOB" } else { b"\x1b[B" },
            ),
            Key::Named(NamedKey::ArrowRight) => bytes.extend_from_slice(
                if self.terminal.screen.app_cursor { b"\x1bOC" } else { b"\x1b[C" },
            ),
            Key::Named(NamedKey::ArrowLeft) => bytes.extend_from_slice(
                if self.terminal.screen.app_cursor { b"\x1bOD" } else { b"\x1b[D" },
            ),
            Key::Named(NamedKey::Home) => bytes.extend_from_slice(b"\x1b[H"),
            Key::Named(NamedKey::End) => bytes.extend_from_slice(b"\x1b[F"),
            Key::Named(NamedKey::Delete) => bytes.extend_from_slice(b"\x1b[3~"),
            Key::Named(NamedKey::PageUp) => {
                if self.modifiers.shift_key() {
                    self.terminal.screen.scroll_view(self.terminal.screen.rows as i32);
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    return;
                }
                bytes.extend_from_slice(b"\x1b[5~");
            }
            Key::Named(NamedKey::PageDown) => {
                if self.modifiers.shift_key() {
                    self.terminal.screen.scroll_view(-(self.terminal.screen.rows as i32));
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    return;
                }
                bytes.extend_from_slice(b"\x1b[6~");
            }
            Key::Character(text) if self.modifiers.control_key() => {
                if let Some(c) = text.chars().next() {
                    let c = c.to_ascii_lowercase();
                    match c {
                        'a'..='z' => bytes.push(c as u8 - b'a' + 1),
                        ' ' | '@' => bytes.push(0),
                        '[' => bytes.push(27),
                        '\\' => bytes.push(28),
                        ']' => bytes.push(29),
                        '^' => bytes.push(30),
                        '_' => bytes.push(31),
                        '?' => bytes.push(127),
                        _ => {}
                    }
                }
            }
            Key::Character(text) if !self.modifiers.super_key() => {
                if self.modifiers.alt_key() {
                    bytes.push(0x1b);
                }
                bytes.extend_from_slice(text.as_bytes());
            }
            _ => {}
        }
        if !bytes.is_empty() {
            self.selection = None;
            self.terminal.screen.view_offset = 0;
            self.pty.write(&bytes);
        }
    }
}

// winit reports a space as a named key. Generate the same PTY bytes as a
// printable character, including Ctrl+Space (NUL) and Alt+Space (ESC + space).
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

fn space_bytes(control: bool, alt: bool, command: bool) -> &'static [u8] {
    if command {
        b""
    } else if control {
        b"\x00"
    } else if alt {
        b"\x1b "
    } else {
        b" "
    }
}

impl ApplicationHandler<UserEvent> for App {
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
                match Renderer::new(
                    &self.config.font,
                    self.config.window.padding,
                    scale_factor,
                ) {
                    Ok(renderer) => {
                        self.renderer = Some(renderer);
                        self.resize_terminal();
                    }
                    Err(err) => eprintln!("rst: font reinitialization failed: {err:#}"),
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
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;

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

    #[test]
    fn space_sends_ascii_space_to_pty() {
        assert_eq!(space_bytes(false, false, false), b" ");
    }

    #[test]
    fn ctrl_space_sends_nul() {
        assert_eq!(space_bytes(true, false, false), b"\x00");
    }

    #[test]
    fn alt_space_sends_escape_prefixed_space() {
        assert_eq!(space_bytes(false, true, false), b"\x1b ");
    }

    #[test]
    fn command_space_is_not_sent_to_shell() {
        assert_eq!(space_bytes(false, false, true), b"");
    }
}

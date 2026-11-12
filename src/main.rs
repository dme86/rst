mod attention;
mod charset;
mod command_capture;
mod copy_feedback;
mod config;
mod cursor_layer;
mod help;
mod ime;
mod keyboard;
mod mouse;
#[cfg(target_os = "macos")]
mod macos_icon;
mod osc;
mod pty;
mod pty_input;
mod profile;
mod process_name;
mod render;
mod selection;
mod shell_integration;
mod shaping;
mod terminal;
mod url_picker;
mod tabs;
mod panes;
mod tab_naming;

use std::{num::NonZeroU32, sync::Arc, time::{Duration, Instant}};

use anyhow::Result;
use softbuffer::{Context, Surface};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy, OwnedDisplayHandle},
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

/// Sessions outside the keyboard focus retain their PTY and complete VT state.
/// Visible split siblings render without a cursor; hidden tabs do not render.
/// Only the keyboard-focused session runs cursor/text blink clocks.
struct StoredSession {
    id: u64,
    terminal: Terminal,
    pty: PtySession,
    unread: bool,
}

struct App {
    profile: Option<profile::Profile>,
    config: Config,
    original_font_size: f32,
    terminal: Terminal,
    pty: PtySession,
    active_session_id: u64,
    next_session_id: u64,
    tab_order: Vec<u64>,
    layouts: std::collections::BTreeMap<u64, panes::Layout>,
    pane_buffer: Vec<u32>,
    inactive_sessions: Vec<StoredSession>,
    proxy: EventLoopProxy<UserEvent>,
    shell_label: String,
    home_dir: Option<String>,
    context: Context<OwnedDisplayHandle>,
    window: Option<Arc<Window>>,
    surface: Option<Surface<OwnedDisplayHandle, Arc<Window>>>,
    renderer: Option<Renderer>,
    native_cursor: Option<cursor_layer::CursorLayer>,
    modifiers: ModifiersState,
    ime: ImeState,
    mouse: (f64, f64),
    mouse_reporter: MouseReporter,
    selection: Option<Selection>,
    clicks: ClickTracker,
    selecting: bool,
    rail_mouse_grab: bool,
    next_cursor_blink: Instant,
    next_text_blink: Instant,
    next_process_probe: Instant,
    tab_status: tabs::TabStatus,
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
    /// A PTY with a running command or an alternate-screen TUI can have a
    /// foreground job other than its interactive shell. Idle tabs never
    /// cause process queries or extra wakeups.
    fn tab_may_have_foreground_job(screen: &terminal::Screen) -> bool {
        screen.command_capture.running() || screen.in_alternate_screen()
    }

    /// Called at most once per probe interval from about_to_wait, never
    /// from rendering or the PTY reader threads. Only name changes repaint.
    fn refresh_foreground_process_names(&mut self) -> bool {
        let mut changed = false;
        if Self::tab_may_have_foreground_job(&self.terminal.screen) {
            changed |= self.pty.refresh_foreground_name();
        } else {
            changed |= self.pty.clear_foreground_name();
        }
        for tab in &mut self.inactive_sessions {
            if Self::tab_may_have_foreground_job(&tab.terminal.screen) {
                changed |= tab.pty.refresh_foreground_name();
            } else {
                changed |= tab.pty.clear_foreground_name();
            }
        }
        changed
    }

    fn has_foreground_job(&self) -> bool {
        Self::tab_may_have_foreground_job(&self.terminal.screen)
            || self.inactive_sessions.iter().any(|tab| {
                Self::tab_may_have_foreground_job(&tab.terminal.screen)
            })
    }

    fn owner_tab(&self, pane: u64) -> Option<u64> {
        self.layouts.iter().find(|(_, layout)| layout.contains(pane)).map(|(id, _)| *id)
    }

    fn current_tab(&self) -> u64 {
        self.owner_tab(self.active_session_id).expect("focused pane belongs to a tab")
    }

    fn active_tab_index(&self) -> usize {
        self.tab_order.iter().position(|id| *id == self.current_tab()).unwrap_or(0)
    }

    fn content_rect(&self) -> panes::Rect {
        let (Some(window), Some(renderer)) = (&self.window, &self.renderer) else { return panes::Rect::default(); };
        let size = window.inner_size();
        let bar = tabs::bar_height(renderer.cell_height, self.config.tabs.enabled).min(size.height as usize);
        panes::Rect { x: 0, y: if self.config.tabs.position == "top" { bar } else { 0 },
            width: size.width as usize, height: (size.height as usize).saturating_sub(bar) }
    }

    fn visible_panes(&self) -> Vec<(u64, panes::Rect)> {
        self.layouts[&self.current_tab()].visible_panes(self.content_rect(), self.pane_rail_width())
    }

    fn pane_rail_width(&self) -> usize {
        self.layouts[&self.current_tab()].ids().len().saturating_sub(1) * 5
    }

    fn pane_rail_entries(&self) -> Vec<(u64, panes::Rect)> {
        self.layouts[&self.current_tab()].rail_entries(self.content_rect(), self.pane_rail_width())
    }

    fn toggle_pane_zoom(&mut self) {
        let owner = self.current_tab();
        if self.layouts.get_mut(&owner).expect("current tab").toggle_zoom() {
            if !self.layouts[&owner].zoomed {
                for session in &mut self.inactive_sessions {
                    if self.layouts[&owner].contains(session.id) { session.unread = false; }
                }
            }
            self.clear_tab_interaction();
            self.resize_terminal();
        }
    }

    fn active_pane_rect(&self) -> panes::Rect {
        self.visible_panes().into_iter().find(|(id, _)| *id == self.active_session_id)
            .map(|(_, rect)| rect).unwrap_or_default()
    }

    fn grid_size(&self, rect: panes::Rect) -> (usize, usize) {
        self.renderer.as_ref().map_or((self.config.window.columns, self.config.window.rows),
            |r| rect.grid(r.padding, r.cell_width, r.cell_height))
    }

    fn tab_displays(&self) -> Vec<tabs::TabDisplay> {
        let max_chars = tab_naming::label_columns(
            tabs::TAB_COLUMNS, self.config.tabs.close_buttons,
        );
        self.tab_order.iter().map(|id| {
            let focused = self.layouts[id].focused;
            let (screen, process_name, unread) = if focused == self.active_session_id {
                (&self.terminal.screen, self.pty.foreground_name(), false)
            } else {
                let tab = self.inactive_sessions.iter().find(|tab| tab.id == focused)
                    .expect("all inactive IDs remain stored");
                (&tab.terminal.screen, tab.pty.foreground_name(), tab.unread)
            };
            let title = tab_naming::label_with_process(
                process_name,
                screen.cwd.as_deref(),
                screen.foreground_program.as_deref(),
                &screen.title,
                screen.command_capture.running() || screen.in_alternate_screen(),
                &self.shell_label,
                &self.config.tabs.name_mode,
                self.home_dir.as_deref(),
                max_chars,
            );
            tabs::TabDisplay {
                title,
                active: *id == self.current_tab(),
                unread: unread || self.inactive_sessions.iter().any(|session|
                    self.layouts[id].contains(session.id) && session.unread),
            }
        }).collect()
    }

    /// The tab bar lives outside the terminal VT grid, not in scrollback.
    fn mouse_in_bar(&self) -> bool {
        if !self.config.tabs.enabled { return false; }
        let (Some(renderer), Some(window)) = (&self.renderer, &self.window) else {
            return false;
        };
        let height = window.inner_size().height as usize;
        let bar_h = tabs::bar_height(renderer.cell_height, true).min(height);
        let top = if self.config.tabs.position == "bottom" {
            height.saturating_sub(bar_h)
        } else { 0 };
        let y = self.mouse.1.max(0.0) as usize;
        y >= top && y < top.saturating_add(bar_h)
    }

    fn tab_bar_action(&self) -> Option<tabs::TabAction> {
        if !self.mouse_in_bar() { return None; }
        let renderer = self.renderer.as_ref()?;
        let col = self.mouse.0.max(0.0) as usize / renderer.cell_width.max(1);
        let columns = self.window.as_ref()?.inner_size().width as usize / renderer.cell_width.max(1);
        let columns = columns.saturating_sub(if self.config.tabs.show_status {
            self.tab_status.columns(columns)
        } else { 0 });
        let layout = tabs::BarLayout::for_items(
            &self.tab_displays(), self.active_tab_index(), columns,
            self.config.tabs.close_buttons, self.config.tabs.powerline,
        );
        if col >= columns { return None; }
        layout.hit_test(col + 1, columns, self.config.tabs.close_buttons)
    }

    fn clear_tab_interaction(&mut self) {
        self.selection = None;
        self.selecting = false;
        self.clicks.reset();
        self.mouse_reporter.reset();
        self.ime.clear();
        self.help_open = false;
        self.url_picker = None;
        self.toast = None;
        self.next_toast_frame = None;
        self.bell_flash_until = None;
        self.next_cursor_blink =
            Instant::now() + Duration::from_millis(self.config.cursor.blink_interval_ms);
        self.next_text_blink = Instant::now() + Duration::from_millis(800);
        self.terminal.screen.cursor_blink_on = true;
        self.terminal.screen.text_blink_on = true;
        if let Some(window) = &self.window {
            window.set_title(&self.terminal.screen.title);
            window.request_redraw();
        }
        self.update_ime_cursor_area();
    }

    fn switch_tab(&mut self, target_id: u64) {
        if !self.config.tabs.enabled { return; }
        if let Some(layout) = self.layouts.get(&target_id) {
            self.focus_session(layout.focused);
        }
    }

    fn focus_session(&mut self, target_id: u64) {
        if target_id == self.active_session_id {
            return;
        }
        let Some(tab) = self.inactive_sessions.iter_mut().find(|tab| tab.id == target_id) else {
            return;
        };
        if self.focus.is_focused() && self.terminal.screen.focus_reporting {
            self.pty.write(b"\x1b[O");
        }
        std::mem::swap(&mut self.terminal, &mut tab.terminal);
        std::mem::swap(&mut self.pty, &mut tab.pty);
        std::mem::swap(&mut self.active_session_id, &mut tab.id);
        tab.unread = false;
        if self.focus.is_focused() && self.terminal.screen.focus_reporting {
            self.pty.write(b"\x1b[I");
        }
        if let Some(owner) = self.owner_tab(self.active_session_id) {
            self.layouts.get_mut(&owner).expect("owned pane").focused = self.active_session_id;
            for session in &mut self.inactive_sessions {
                if !self.layouts[&owner].zoomed && self.layouts[&owner].contains(session.id) { session.unread = false; }
            }
        }
        self.clear_tab_interaction();
        if self.layouts[&self.current_tab()].zoomed { self.resize_terminal(); }
    }

    fn switch_relative_tab(&mut self, step: isize) {
        if !self.config.tabs.enabled { return; }
        if let Some(next) = tabs::relative_tab(&self.tab_order, self.current_tab(), step) {
            self.switch_tab(next);
        }
    }

    fn new_tab(&mut self) {
        if !self.config.tabs.enabled { return; }
        if self.tab_order.len() >= self.config.tabs.max_tabs {
            self.notify(format!("Maximum of {} tabs reached", self.config.tabs.max_tabs));
            return;
        }
        let new_id = self.next_session_id;
        let (cols, rows) = self.grid_size(self.content_rect());
        let next_terminal = match configured_terminal(&self.config, cols, rows) {
            Ok(terminal) => terminal,
            Err(err) => {
                self.notify(format!("Cannot initialize tab: {err}"));
                return;
            }
        };
        let mut next_pty = match PtySession::start(
            new_id, cols, rows, &self.config.terminal.term, self.proxy.clone(),
        ) {
            Ok(pty) => pty,
            Err(err) => {
                self.notify(format!("Cannot start shell: {err}"));
                return;
            }
        };
        if let Some(renderer) = &self.renderer {
            next_pty.resize(cols, rows, renderer.cell_width, renderer.cell_height);
        }
        let previous = StoredSession {
            id: self.active_session_id,
            terminal: std::mem::replace(&mut self.terminal, next_terminal),
            pty: std::mem::replace(&mut self.pty, next_pty),
            unread: false,
        };
        self.inactive_sessions.push(previous);
        let insert_at = self.active_tab_index() + 1;
        self.tab_order.insert(insert_at, new_id);
        self.layouts.insert(new_id, panes::Layout::new(new_id));
        self.active_session_id = new_id;
        self.next_session_id = self.next_session_id.saturating_add(1);
        self.clear_tab_interaction();
        self.resize_terminal();
    }

    fn split_pane(&mut self, axis: panes::Axis) {
        let owner = self.current_tab();
        let new_id = self.next_session_id;
        let mut layout = self.layouts[&owner].clone();
        if !layout.split(self.active_session_id, new_id, axis) {
            self.notify(format!("Maximum of {} panes per tab reached", panes::MAX_PANES));
            return;
        }
        let rects = layout.panes(self.content_rect());
        if rects.iter().any(|(_, rect)| { let (cols, rows) = self.grid_size(*rect); cols < 8 || rows < 3 }) {
            self.notify("Not enough space to split this pane");
            return;
        }
        let rect = rects.iter().find(|(id, _)| *id == new_id).expect("new split pane").1;
        let (cols, rows) = self.grid_size(rect);
        let next_terminal = match configured_terminal(&self.config, cols, rows) {
            Ok(terminal) => terminal, Err(err) => { self.notify(format!("Cannot initialize split: {err}")); return; }
        };
        let cwd = self.terminal.screen.cwd.as_deref().filter(|cwd| std::path::Path::new(cwd).is_dir());
        let next_pty = match PtySession::start_in_dir(new_id, cols, rows, &self.config.terminal.term, self.proxy.clone(), cwd) {
            Ok(pty) => pty, Err(err) => { self.notify(format!("Cannot start split shell: {err}")); return; }
        };
        if self.focus.is_focused() && self.terminal.screen.focus_reporting { self.pty.write(b"\x1b[O"); }
        self.inactive_sessions.push(StoredSession { id: self.active_session_id,
            terminal: std::mem::replace(&mut self.terminal, next_terminal),
            pty: std::mem::replace(&mut self.pty, next_pty), unread: false });
        self.layouts.insert(owner, layout);
        self.active_session_id = new_id;
        self.next_session_id = self.next_session_id.saturating_add(1);
        self.clear_tab_interaction();
        self.resize_terminal();
    }

    fn focus_direction(&mut self, direction: panes::Direction) {
        if let Some(id) = self.layouts[&self.current_tab()].neighbor(self.active_session_id, direction, self.content_rect()) {
            self.focus_session(id);
        }
    }

    fn close_session(&mut self, id: u64, kill: bool) -> bool {
        let Some(owner) = self.owner_tab(id) else { return false; };
        if self.layouts[&owner].ids().len() == 1 { return self.close_tab(owner, kill); }
        self.layouts.get_mut(&owner).expect("pane owner").remove(id);
        if self.active_session_id == id {
            self.focus_session(self.layouts[&owner].focused);
        }
        if let Some(position) = self.inactive_sessions.iter().position(|session| session.id == id) {
            let mut old = self.inactive_sessions.swap_remove(position);
            if kill { old.pty.terminate(); }
        }
        self.resize_terminal();
        false
    }

    /// Returns true if the last tab was closed and rst should quit.
    fn close_tab(&mut self, id: u64, kill: bool) -> bool {
        let Some(layout) = self.layouts.get(&id) else { return false; };
        let sessions = layout.ids();
        if self.tab_order.len() == 1 {
            if kill { self.terminate_all_tabs(); }
            return true;
        }
        if id == self.current_tab() {
            let next = tabs::successor_after_close(&self.tab_order, id).expect("another tab");
            self.focus_session(self.layouts[&next].focused);
        }
        self.inactive_sessions.retain_mut(|session| {
            if sessions.contains(&session.id) {
                if kill { session.pty.terminate(); }
                false
            } else { true }
        });
        self.tab_order.retain(|tab| *tab != id);
        self.layouts.remove(&id);
        if let Some(window) = &self.window { window.request_redraw(); }
        false
    }

    fn terminate_all_tabs(&mut self) {
        self.pty.terminate();
        for tab in &mut self.inactive_sessions {
            tab.pty.terminate();
        }
    }

    fn resize_terminal(&mut self) {
        let content = self.content_rect();
        let rects: Vec<_> = self.layouts.values().flat_map(|layout| layout.resize_panes(content, self.pane_rail_width())).collect();
        let Some(renderer) = &self.renderer else { return; };
        let mut active_changed = false;
        for (id, rect) in rects {
            let (cols, rows) = rect.grid(renderer.padding, renderer.cell_width, renderer.cell_height);
            let (terminal, pty) = if id == self.active_session_id {
                (&mut self.terminal, &mut self.pty)
            } else if let Some(session) = self.inactive_sessions.iter_mut().find(|session| session.id == id) {
                (&mut session.terminal, &mut session.pty)
            } else { continue; };
            if (terminal.screen.cols, terminal.screen.rows) != (cols, rows) {
                terminal.screen.resize(cols, rows);
                if id == self.active_session_id { active_changed = true; }
            }
            pty.resize(cols, rows, renderer.cell_width, renderer.cell_height);
        }
        if active_changed { self.selection = None; self.selecting = false; self.clicks.reset(); }
        if let Some(window) = &self.window { window.request_redraw(); }
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
        let pane = self.active_pane_rect();
        let x = pane.x.saturating_add(renderer.padding).saturating_add(col.saturating_mul(renderer.cell_width));
        let y = renderer.padding.saturating_add(pane.y)
            .saturating_add(row.saturating_mul(renderer.cell_height));
        window.set_ime_cursor_area(
            PhysicalPosition::new(x.min(i32::MAX as usize) as i32,
                                  y.min(i32::MAX as usize) as i32),
            PhysicalSize::new(renderer.cell_width.min(u32::MAX as usize) as u32,
                              renderer.cell_height.min(u32::MAX as usize) as u32),
        );
    }

    fn native_cursor_enabled(&self) -> bool {
        self.focus.is_focused() && self.native_cursor.is_some() && self.terminal.screen.cursor_style.blink
            && self.terminal.screen.show_cursor && self.terminal.screen.view_offset == 0
            && self.ime.preedit.is_none() && !self.help_open && self.url_picker.is_none()
            && self.toast.is_none() && self.bell_flash_until.is_none()
    }

    fn redraw(&mut self) {
        let draw_started = self.profile.as_ref().map(|_| Instant::now());
        let panes = self.visible_panes();
        let rail_rect = self.layouts[&self.current_tab()].rail_rect(self.content_rect(), self.pane_rail_width());
        let rail_entries = self.pane_rail_entries();
        let rail_items: Vec<_> = rail_entries.iter().map(|(id, rect)| {
            let unread = self.inactive_sessions.iter().find(|session| session.id == *id)
                .is_some_and(|session| session.unread);
            (*rect, unread)
        }).collect();
        let active_rect = self.active_pane_rect();
        let native = self.native_cursor_enabled() && active_rect.width > 0 && active_rect.height > 0;
        if !native { if let Some(layer) = &mut self.native_cursor { layer.hide(); } }
        let items = if self.config.tabs.enabled { self.tab_displays() } else { Vec::new() };
        let active_index = self.active_tab_index();
        let (Some(window), Some(surface), Some(renderer)) =
            (&self.window, &mut self.surface, &mut self.renderer) else { return; };
        let size = window.inner_size();
        let (Some(width), Some(height)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return; };
        if let Err(err) = surface.resize(width, height) { eprintln!("rst: resize surface: {err}"); return; }
        let mut buffer = match surface.buffer_mut() {
            Ok(buffer) => buffer, Err(err) => { eprintln!("rst: acquire buffer: {err}"); return; }
        };
        let stride = size.width as usize;
        if panes.len() > 1 || rail_rect.is_some() { buffer.fill(tabs::parse_color(&self.config.tabs.colors.inactive_background).unwrap_or(0x504945)); }
        for (id, rect) in &panes {
            if rect.width == 0 || rect.height == 0 { continue; }
            let active = *id == self.active_session_id;
            let screen = if active { &mut self.terminal.screen }
                else if let Some(session) = self.inactive_sessions.iter_mut().find(|session| session.id == *id) {
                    &mut session.terminal.screen
                } else { continue; };
            let saved_cursor = (screen.show_cursor, screen.cursor_blink_on, screen.text_blink_on);
            if !active { screen.show_cursor = false; screen.text_blink_on = true; }
            if active && native { screen.cursor_blink_on = false; }
            let frame = panes::Frame::new(&mut buffer, &mut self.pane_buffer, stride, *rect);
            let pixels = &mut *frame.pixels;
            renderer.draw(pixels, rect.width, rect.height, screen,
                if active { self.selection } else { None },
                if active { self.ime.preedit.as_ref() } else { None });
            if active && native {
                screen.cursor_blink_on = true;
                if let Some(mut patch) = renderer.cursor_patch(pixels, rect.width, rect.height, screen, self.selection) {
                    patch.x += rect.x;
                    patch.y += rect.y;
                    if !self.native_cursor.as_mut().expect("native cursor selected").update(
                        patch, window.scale_factor(), self.config.cursor.blink_interval_ms)
                    {
                        self.native_cursor = None;
                        screen.cursor_blink_on = saved_cursor.1;
                        renderer.draw(pixels, rect.width, rect.height, screen, self.selection, self.ime.preedit.as_ref());
                    }
                } else if let Some(layer) = &mut self.native_cursor { layer.hide(); }
            }
            if active {
                if self.bell_flash_until.is_some_and(|until| Instant::now() < until) {
                    render::paint_bell_border(pixels, rect.width, rect.height, screen.palette.cursor);
                }
                if let Some(notice) = &self.toast {
                    if let Some(output_id) = notice.output_id {
                        renderer.draw_output_copy_highlight(pixels, rect.width, rect.height, screen, output_id, notice.opacity(Instant::now()));
                    }
                }
                if self.help_open { renderer.draw_help_overlay(pixels, rect.width, rect.height, screen, self.help_scroll, cfg!(target_os = "macos")); }
                if let Some(picker) = &self.url_picker { renderer.draw_url_picker_overlay(pixels, rect.width, rect.height, screen, picker, self.config.url_picker.recent_outputs); }
                if let Some(notice) = &self.toast {
                    if notice.opacity(Instant::now()) != 0 { renderer.draw_copy_notice(pixels, rect.width, rect.height, screen, notice, notice.opacity(Instant::now())); }
                }
            }
            screen.show_cursor = saved_cursor.0;
            screen.cursor_blink_on = saved_cursor.1;
            screen.text_blink_on = saved_cursor.2;
            frame.commit();
            if active && panes.len() > 1 {
                render::paint_pane_focus(&mut buffer, stride, size.height as usize, *rect,
                    tabs::parse_color(&self.config.tabs.colors.accent).unwrap_or(0x83a598));
            }
        }
        if let Some(rail) = rail_rect {
            renderer.draw_collapsed_panes(&mut buffer, stride, size.height as usize,
                &self.terminal.screen, rail, &rail_items);
        }
        if self.config.tabs.enabled {
            renderer.draw_tab_bar(&mut buffer, stride, size.height as usize, &self.terminal.screen,
                &items, active_index, &self.config.tabs, self.config.tabs.show_status.then_some(&self.tab_status));
        }
        window.pre_present_notify();
        if let Err(err) = buffer.present() { eprintln!("rst: present: {err}"); }
        else if let (Some(profile), Some(started)) = (&mut self.profile, draw_started) {
            profile.frame(started, self.terminal.screen.prompt_seen);
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
        let pane = self.active_pane_rect();
        let x = (self.mouse.0.max(0.0) as usize).saturating_sub(pane.x.saturating_add(renderer.padding)) / renderer.cell_width;
        let y = (self.mouse.1.max(0.0) as usize).saturating_sub(pane.y.saturating_add(renderer.padding)) / renderer.cell_height;
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
                    self.pty.write_parts(&[b"\x1b[200~", text.as_bytes(), b"\x1b[201~"]);
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
        let mut notice = CopyNotice::warning(message, Instant::now());
        notice.title = "RST".into();
        self.present_notice(notice);
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
        let visible = self.terminal.screen.rows.saturating_sub(5).clamp(1, 23);
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

    fn handle_key(&mut self, event: KeyEvent, event_loop: &ActiveEventLoop) {
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
        if let Some(action) = panes::shortcut(&key, self.modifiers, cfg!(target_os = "macos")) {
            let multiple = self.layouts[&self.current_tab()].ids().len() > 1;
            if !matches!(action, panes::Action::Focus(_)) || multiple {
                match action {
                    panes::Action::Split(axis) if !event.repeat => self.split_pane(axis),
                    panes::Action::Focus(direction) => self.focus_direction(direction),
                    panes::Action::ToggleZoom if !event.repeat => self.toggle_pane_zoom(),
                    panes::Action::Close if !event.repeat && self.close_session(self.active_session_id, true) => event_loop.exit(),
                    _ => {}
                }
                return;
            }
        }
        if self.config.tabs.enabled {
            let macos = cfg!(target_os = "macos");
            if tab_shortcut(&key, self.modifiers, macos, "t") {
                if !event.repeat { self.new_tab(); }
                return;
            }
            if matches!(key, Key::Named(NamedKey::Tab))
                && (self.modifiers == ModifiersState::CONTROL
                    || self.modifiers == (ModifiersState::CONTROL | ModifiersState::SHIFT))
            {
                if !event.repeat {
                    self.switch_relative_tab(if self.modifiers.shift_key() { -1 } else { 1 });
                }
                return;
            }
            let digit_modifier = if macos {
                ModifiersState::SUPER
            } else {
                ModifiersState::ALT
            };
            if self.modifiers == digit_modifier {
                if let Key::Character(value) = key {
                    if let Some(digit) = value.chars().next()
                        .filter(|_| value.len() == 1)
                        .and_then(|ch| ch.to_digit(10))
                        .filter(|digit| (1..=9).contains(digit))
                    {
                        if let Some(id) = self.tab_order.get(digit as usize - 1).copied() {
                            self.switch_tab(id);
                        }
                        return;
                    }
                }
            }
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

fn tab_shortcut(
    key: &Key<&str>,
    modifiers: ModifiersState,
    macos: bool,
    letter: &str,
) -> bool {
    let required = if macos {
        ModifiersState::SUPER
    } else {
        ModifiersState::CONTROL | ModifiersState::SHIFT
    };
    modifiers == required
        && matches!(key, Key::Character(text) if text.eq_ignore_ascii_case(letter))
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

/// A paused cursor is steady and schedules no wakeup. Reset its deadline so
/// resuming never catches up through blink phases missed while unfocused.
fn cursor_blink_schedule(
    enabled: bool, visible: bool, deadline: Instant, now: Instant, interval: Duration,
) -> (bool, Instant, Option<Instant>) {
    if !enabled { return (true, now + interval, None); }
    let (visible, next) = blink_step(visible, deadline, now, interval);
    (visible, next, Some(next))
}

impl ApplicationHandler<UserEvent> for App {
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(profile) = &mut self.profile { profile.wait(self.native_cursor.as_ref().and_then(|layer| layer.phase())); }
        let now = Instant::now();

        // The cursor and SGR 5/6 text blink on *independent* clocks.
        // Hiding the cursor with DECTCEM must not freeze blinking text.
        let cursor_blinks = self.focus.is_focused()
            && self.terminal.screen.cursor_style.blink
            && self.terminal.screen.show_cursor
            && self.window.is_some()
            && self.terminal.screen.view_offset == 0
            && self.ime.preedit.is_none() && !self.help_open && self.url_picker.is_none()
            && !self.native_cursor_enabled();
        let interval = Duration::from_millis(self.config.cursor.blink_interval_ms);
        let (visible, next, wakeup) = cursor_blink_schedule(cursor_blinks,
            self.terminal.screen.cursor_blink_on, self.next_cursor_blink, now, interval);
        if visible != self.terminal.screen.cursor_blink_on {
            self.terminal.screen.cursor_blink_on = visible;
            if let Some(window) = &self.window { window.request_redraw(); }
        }
        self.next_cursor_blink = next;
        let mut next_deadline = wakeup;

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
        // Query only the owned PTYs' foreground process groups, at a
        // bounded frequency while a command/TUI is running. There is no
        // process-tree scan, external 'ps', or per-frame process lookup.
        if self.config.tabs.enabled
            && self.config.tabs.name_mode == "smart"
            && self.window.is_some()
            && self.has_foreground_job()
        {
            if now >= self.next_process_probe {
                if self.refresh_foreground_process_names() {
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
                self.next_process_probe = now + Duration::from_millis(500);
            }
            let next = self.next_process_probe;
            next_deadline = Some(next_deadline.map_or(next, |old| old.min(next)));
        } else {
            // Start probing immediately when the next foreground job begins.
            self.next_process_probe = now;
        }
        if self.config.tabs.enabled && self.config.tabs.show_status && self.window.is_some() {
            if self.tab_status.refresh(now) {
                if let Some(window) = &self.window { window.request_redraw(); }
            }
            let next = self.tab_status.next_refresh;
            next_deadline = Some(next_deadline.map_or(next, |old| old.min(next)));
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
            + 2.0 * self.config.window.padding as f64
            + if self.config.tabs.enabled {
                self.config.font.size as f64 * 1.40 + 6.0
            } else { 0.0 };
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
        self.native_cursor = cursor_layer::CursorLayer::new(&window);
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
            WindowEvent::CloseRequested => {
                self.terminate_all_tabs();
                event_loop.exit();
            },
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
            WindowEvent::KeyboardInput { event, .. } => self.handle_key(event, event_loop),
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
                // Use one steady software cursor while unfocused. Removing
                // the native animation also clears its cached patch, so focus
                // return starts a fresh visible phase even without PTY output.
                self.terminal.screen.cursor_blink_on = true;
                self.next_cursor_blink = Instant::now()
                    + Duration::from_millis(self.config.cursor.blink_interval_ms);
                if !focused {
                    if let Some(layer) = &mut self.native_cursor { layer.hide(); }
                }
                if let Some(window) = &self.window { window.request_redraw(); }
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
                    self.rail_mouse_grab = false;
                    self.selecting = false;
                    self.clicks.reset();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = (position.x, position.y);
                if self.help_open || self.url_picker.is_some() || self.mouse_in_bar() {
                    return;
                }
                if !self.selecting && !self.mouse_reporter.dragging()
                    && !self.active_pane_rect().contains(self.mouse.0.max(0.0) as usize, self.mouse.1.max(0.0) as usize) { return; }
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
                if state == ElementState::Released && self.rail_mouse_grab {
                    self.rail_mouse_grab = false;
                    return;
                }
                if state == ElementState::Pressed {
                    let in_rail = self.layouts[&self.current_tab()].rail_rect(self.content_rect(), self.pane_rail_width())
                        .is_some_and(|rect| rect.contains(self.mouse.0.max(0.0) as usize, self.mouse.1.max(0.0) as usize));
                    if in_rail {
                        if button == MouseButton::Left {
                            if let Some((id, _)) = self.pane_rail_entries().into_iter().find(|(_, rect)|
                                rect.contains(self.mouse.0.max(0.0) as usize, self.mouse.1.max(0.0) as usize))
                            {
                                self.focus_session(id);
                            }
                        }
                        // Consume strip clicks locally; drags originating in
                        // the active pane may still finish over the strip.
                        self.rail_mouse_grab = true;
                        return;
                    }
                }
                if self.mouse_in_bar() {
                    if button == MouseButton::Left && state == ElementState::Pressed {
                        match self.tab_bar_action() {
                            Some(tabs::TabAction::Select(index)) => {
                                if let Some(id) = self.tab_order.get(index).copied() {
                                    self.switch_tab(id);
                                }
                            }
                            Some(tabs::TabAction::Close(index)) => {
                                if let Some(id) = self.tab_order.get(index).copied() {
                                    if self.close_tab(id, true) {
                                        event_loop.exit();
                                    }
                                }
                            }
                            Some(tabs::TabAction::New) => self.new_tab(),
                            None => {}
                        }
                    }
                    // Never forward tab-bar mouse input to a child TUI.
                    self.selecting = false;
                    self.mouse_reporter.reset();
                    return;
                }
                if state == ElementState::Pressed {
                    let target = self.visible_panes().into_iter().find(|(_, rect)|
                        rect.contains(self.mouse.0.max(0.0) as usize, self.mouse.1.max(0.0) as usize));
                    let Some((id, _)) = target else { return; };
                    self.focus_session(id);
                }
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
                    let visible = self.terminal.screen.rows.saturating_sub(5).clamp(1, 23);
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
                if self.mouse_in_bar() { return; }
                if !self.active_pane_rect().contains(self.mouse.0.max(0.0) as usize, self.mouse.1.max(0.0) as usize) { return; }
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
            UserEvent::Output { session_id, bytes } => {
                if let Some(profile) = &mut self.profile { profile.output(bytes.len(), bytes.queued_at); }
                if session_id != self.active_session_id {
                    // Split siblings redraw on output. Hidden tabs parse and
                    // answer queries, repainting only metadata/unread changes.
                    let visible = self.visible_panes().iter().any(|(id, _)| *id == session_id);
                    let mut became_unread = visible;
                    if let Some(tab) = self.inactive_sessions.iter_mut()
                        .find(|tab| tab.id == session_id)
                    {
                        let old_revision = tab.terminal.screen.tab_label_revision;
                        tab.terminal.advance(&bytes);
                        if tab.terminal.screen.tab_label_revision != old_revision {
                            became_unread = true; // Only metadata changes repaint hidden tabs.
                        }
                        if !Self::tab_may_have_foreground_job(&tab.terminal.screen) {
                            // The shell reached its prompt again.
                            became_unread |= tab.pty.clear_foreground_name();
                        }
                        let replies = tab.terminal.screen.take_replies();
                        if !replies.is_empty() { tab.pty.write(&replies); }
                        // Only the keyboard-focused pane may write the host
                        // clipboard or trigger audio/visual attention.
                        let _ = tab.terminal.screen.take_clipboard_writes();
                        let _ = tab.terminal.screen.take_bells();
                        if !visible && !tab.unread {
                            tab.unread = true;
                            became_unread = true;
                        }
                    }
                    if became_unread {
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                    return;
                }
                let previous_mouse_mode = self.terminal.screen.mouse_tracking;
                let previous_alt_screen = self.terminal.screen.in_alternate_screen();
                let previous_view_offset = self.terminal.screen.view_offset;
                let previous_title = self.terminal.screen.title.clone();
                let previous_cursor_style = self.terminal.screen.cursor_style;
                self.terminal.advance(&bytes);
                if let Some(profile) = &mut self.profile { profile.prompt(self.terminal.screen.prompt_seen); }
                if !Self::tab_may_have_foreground_job(&self.terminal.screen) {
                    let _ = self.pty.clear_foreground_name();
                }
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
            UserEvent::InputError { session_id, message } => {
                eprintln!("rst: tab {session_id}: {message}");
                if session_id == self.active_session_id {
                    let mut notice = CopyNotice::warning(message, Instant::now());
                    notice.title = "INPUT ERROR".into();
                    self.present_notice(notice);
                }
            }
            UserEvent::Closed { session_id } => {
                if self.close_session(session_id, false) {
                    event_loop.exit();
                }
            },
        }
    }
}

fn configured_terminal(config: &Config, cols: usize, rows: usize) -> Result<Terminal> {
    let palette = Palette::try_from(&config.colors)?;
    let mut terminal = Terminal::new(cols, rows, config.scrollback.lines, palette);
    terminal.screen.osc52_write_allowed =
        config.clipboard.enabled && config.clipboard.osc52_write;
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
    Ok(terminal)
}

fn main() -> Result<()> {
    let process_started = Instant::now();
    let mut args = std::env::args().skip(1);
    let first_arg = args.next();
    if first_arg.as_deref() == Some("--install-shell-integration") {
        let changes = shell_integration::install()?;
        if changes.is_empty() {
            println!("rst: shell integration already installed");
        } else {
            for line in changes { println!("rst: {line}"); }
        }
        println!("rst: open a new rst window to activate shell integration");
        return Ok(());
    }
    // -e is the convention used by Wayland launchers for Terminal=true desktop entries.
    // Pass argv directly to the PTY; do not reinterpret it with a shell.
    let launch_command = if matches!(first_arg.as_deref(), Some("-e" | "--execute")) {
        let command: Vec<String> = args.collect();
        anyhow::ensure!(!command.is_empty(), "rst: -e requires a program");
        Some(command)
    } else {
        None
    };
    let profile = profile::Profile::from_env(process_started)?;
    let config = config::load()?;
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    // softbuffer's platform error holds non-Send raw window handles.
    // Convert it into an owned diagnostic before passing through anyhow.
    let context = Context::new(event_loop.owned_display_handle())
        .map_err(|err| anyhow::anyhow!("cannot initialize display: {err}"))?;
    let proxy = event_loop.create_proxy();
    let pty = if let Some(command) = launch_command.as_deref() {
        PtySession::start_with_command(
            1, config.window.columns, config.window.rows,
            &config.terminal.term, proxy.clone(), command,
        )?
    } else {
        PtySession::start(
            1, config.window.columns, config.window.rows,
            &config.terminal.term, proxy.clone(),
        )?
    };
    let terminal = configured_terminal(
        &config, config.window.columns, config.window.rows,
    )?;
    let original_font_size = config.font.size;
    let config_blink_ms = config.cursor.blink_interval_ms;
    let mut app = App {
        profile,
        config,
        original_font_size,
        terminal,
        pty,
        active_session_id: 1,
        next_session_id: 2,
        tab_order: vec![1],
        layouts: std::collections::BTreeMap::from([(1, panes::Layout::new(1))]),
        pane_buffer: Vec::new(),
        inactive_sessions: Vec::new(),
        proxy,
        shell_label: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
            .rsplit('/').next().unwrap_or("sh").to_string(),
        home_dir: std::env::var("HOME").ok(),
        context,
        window: None,
        surface: None,
        renderer: None,
        native_cursor: None,
        modifiers: ModifiersState::empty(),
        ime: ImeState::default(),
        mouse: (0.0, 0.0),
        mouse_reporter: MouseReporter::default(),
        selection: None,
        clicks: ClickTracker::default(),
        selecting: false,
        rail_mouse_grab: false,
        next_cursor_blink: Instant::now()
            + Duration::from_millis(config_blink_ms),
        next_text_blink: Instant::now() + Duration::from_millis(800),
        next_process_probe: Instant::now(),
        tab_status: tabs::TabStatus::new(),
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
    fn unfocused_cursor_stays_visible_without_wakeups_and_resumes_fresh() {
        let now = Instant::now();
        let interval = Duration::from_millis(700);
        let mut focus = FocusTracker::default();
        focus.change(false, false);
        let (visible, deadline, wakeup) = cursor_blink_schedule(
            focus.is_focused(), false, now - interval, now, interval);
        assert!(visible);
        assert_eq!(deadline, now + interval);
        assert_eq!(wakeup, None);
        let later = now + interval * 100;
        let (visible, deadline, wakeup) = cursor_blink_schedule(
            focus.is_focused(), visible, deadline, later, interval);
        assert!(visible);
        assert_eq!(wakeup, None);
        focus.change(true, false);
        let (visible, deadline, wakeup) = cursor_blink_schedule(
            focus.is_focused(), visible, deadline, later, interval);
        assert!(visible);
        assert_eq!(wakeup, Some(later + interval));
        assert!(!cursor_blink_schedule(true, visible, deadline, deadline, interval).0);
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
    fn tab_shortcuts_do_not_collide_with_old_window_bindings() {
        let cmd = ModifiersState::SUPER;
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert!(tab_shortcut(&Key::Character("t"), cmd, true, "t"));
        assert!(tab_shortcut(&Key::Character("T"), ctrl_shift, false, "t"));
        assert!(tab_shortcut(&Key::Character("w"), cmd, true, "w"));
        assert!(!tab_shortcut(&Key::Character("t"), cmd, false, "t"));
        assert!(!tab_shortcut(&Key::Character("t"), ctrl_shift, true, "t"));
        assert!(!tab_shortcut(&Key::Character("t"), cmd | ModifiersState::SHIFT, true, "t"));
        assert!(!tab_shortcut(&Key::Character("w"), cmd | ModifiersState::ALT, true, "w"));
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

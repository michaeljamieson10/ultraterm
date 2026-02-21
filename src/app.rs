use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use arboard::Clipboard;
use crossbeam_channel::Receiver;
use log::{error, info, warn};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Event, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState};
#[cfg(target_os = "macos")]
use winit::platform::macos::WindowBuilderExtMacOS;
use winit::window::WindowBuilder;

use crate::input;
use crate::parser::{AnsiParser, MouseTracking};
use crate::pty::{spawn_pty, PtyHandle, SpawnConfig};
use crate::renderer::{OverlayStats, Renderer, SelectionRange};
use crate::screen::Screen;

const SCROLLBACK_LINES: usize = 100_000;

#[derive(Default)]
struct SelectionState {
    anchor: Option<(usize, usize)>,
    head: Option<(usize, usize)>,
    dragging: bool,
}

impl SelectionState {
    fn clear(&mut self) {
        self.anchor = None;
        self.head = None;
        self.dragging = false;
    }

    fn set_anchor(&mut self, cell: (usize, usize)) {
        self.anchor = Some(cell);
        self.head = Some(cell);
        self.dragging = true;
    }

    fn update_head(&mut self, cell: (usize, usize)) {
        if self.dragging {
            self.head = Some(cell);
        }
    }

    fn end_drag(&mut self) {
        self.dragging = false;
    }

    fn range(&self) -> Option<SelectionRange> {
        Some(SelectionRange {
            start: self.anchor?,
            end: self.head?,
        })
    }

    fn has_selection(&self) -> bool {
        self.anchor.is_some() && self.head.is_some() && self.anchor != self.head
    }
}

struct PerfState {
    last_frame: Instant,
    frame_ms: f32,
    fps: f32,
    bytes_this_second: usize,
    bytes_per_second: usize,
    last_rate_tick: Instant,
    dirty_rows: usize,
}

impl PerfState {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            last_frame: now,
            frame_ms: 0.0,
            fps: 0.0,
            bytes_this_second: 0,
            bytes_per_second: 0,
            last_rate_tick: now,
            dirty_rows: 0,
        }
    }

    fn on_bytes(&mut self, count: usize) {
        self.bytes_this_second = self.bytes_this_second.saturating_add(count);
    }

    fn tick_rates(&mut self, now: Instant) {
        if now.duration_since(self.last_rate_tick) >= Duration::from_secs(1) {
            self.bytes_per_second = self.bytes_this_second;
            self.bytes_this_second = 0;
            self.last_rate_tick = now;
        }
    }

    fn on_frame(&mut self, now: Instant, dirty_rows: usize) {
        let dt = now
            .duration_since(self.last_frame)
            .as_secs_f32()
            .max(0.000_1);
        self.last_frame = now;
        self.frame_ms = dt * 1000.0;
        let inst_fps = 1.0 / dt;
        self.fps = if self.fps == 0.0 {
            inst_fps
        } else {
            self.fps * 0.88 + inst_fps * 0.12
        };
        self.dirty_rows = dirty_rows;
    }
}

pub fn run() -> Result<()> {
    let stress_mode = std::env::args().any(|arg| arg == "--stress");

    let event_loop = EventLoop::new().context("failed to create event loop")?;
    let window = Box::new(
        WindowBuilder::new()
            .with_title("ultraterm")
            .with_inner_size(PhysicalSize::new(1280_u32, 800_u32))
            .with_resizable(true)
            .with_transparent(true)
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
            .with_title_hidden(true)
            .with_movable_by_window_background(true)
            .build(&event_loop)
            .context("failed to create window")?,
    );
    window.set_ime_allowed(true);
    let window: &'static _ = Box::leak(window);

    let mut renderer =
        pollster::block_on(Renderer::new(window, 64)).context("failed to initialize renderer")?;
    renderer.set_fullscreen_layout(effective_fullscreen(window));

    let (initial_cols, initial_rows) = renderer.grid_from_window_size(window.inner_size());
    let mut screen = Screen::new(initial_cols, initial_rows, SCROLLBACK_LINES);
    renderer.resize_surface(window.inner_size(), initial_rows);

    let spawn = SpawnConfig {
        cols: initial_cols,
        rows: initial_rows,
        stress_mode,
        ..SpawnConfig::default()
    };
    let (mut pty, pty_rx) = spawn_pty(&spawn).context("failed to launch PTY")?;

    let mut parser = AnsiParser::new();
    let mut modifiers = ModifiersState::default();
    let mut clipboard = Clipboard::new().ok();
    let mut selection = SelectionState::default();
    let mut perf = PerfState::new();
    let mut profiler_enabled = false;
    let mut redraw_deadline = Instant::now();

    let mut last_cursor_cell = (0_usize, 0_usize);
    let mut reported_mouse_button: Option<MouseButton> = None;

    screen.mark_all_dirty();
    renderer.mark_all_dirty(&screen);

    info!(
        "ultraterm started: grid={}x{}, stress_mode={}",
        initial_cols, initial_rows, stress_mode
    );

    event_loop
        .run(move |event, elwt| {
            elwt.set_control_flow(ControlFlow::Poll);

            match event {
                Event::WindowEvent { window_id, event } if window_id == window.id() => {
                    match event {
                        WindowEvent::CloseRequested => {
                            elwt.exit();
                        }
                        WindowEvent::Resized(size) => {
                            if size.width == 0 || size.height == 0 {
                                return;
                            }
                            apply_resize(&mut renderer, &mut screen, &pty, size, &window);
                        }
                        WindowEvent::ScaleFactorChanged { .. } => {
                            let size = window.inner_size();
                            apply_resize(&mut renderer, &mut screen, &pty, size, &window);
                        }
                        WindowEvent::ModifiersChanged(new_modifiers) => {
                            modifiers = new_modifiers.state();
                        }
                        WindowEvent::KeyboardInput { event, .. } => {
                            if handle_font_size_shortcuts(
                                &event,
                                modifiers,
                                &mut renderer,
                                &mut screen,
                                &pty,
                                &window,
                            ) {
                                return;
                            }

                            if handle_shortcuts(
                                &event,
                                modifiers,
                                &mut selection,
                                &screen,
                                &mut clipboard,
                                &pty,
                                &parser,
                                &mut profiler_enabled,
                            ) {
                                window.request_redraw();
                                return;
                            }

                            if let Some(bytes) = input::key_event_to_bytes(
                                &event,
                                modifiers,
                                parser.modes().app_cursor_keys,
                            ) {
                                if let Err(err) = pty.write_all(&bytes) {
                                    error!("failed to send keyboard input: {err}");
                                    elwt.exit();
                                }
                                return;
                            }

                            if let Some(bytes) = keyboard_text_fallback_bytes(&event, modifiers) {
                                if let Err(err) = pty.write_all(&bytes) {
                                    error!("failed to send text input: {err}");
                                    elwt.exit();
                                }
                            }
                        }
                        WindowEvent::Ime(Ime::Commit(text)) => {
                            if !modifiers.super_key()
                                && !modifiers.control_key()
                                && !text.is_empty()
                            {
                                if let Err(err) = pty.write_all(text.as_bytes()) {
                                    error!("failed to send text input: {err}");
                                    elwt.exit();
                                }
                            }
                        }
                        WindowEvent::CursorMoved { position, .. } => {
                            last_cursor_cell = cursor_cell_from_position(
                                position,
                                renderer.cell_width,
                                renderer.cell_height,
                                renderer.grid_left_padding(),
                                renderer.grid_top_offset(),
                                screen.cols(),
                                screen.rows(),
                            );

                            if parser.modes().mouse_tracking == MouseTracking::Off {
                                if selection.dragging {
                                    selection.update_head(last_cursor_cell);
                                    window.request_redraw();
                                }
                            } else {
                                let mode = parser.modes().mouse_tracking;
                                let allow_motion = match mode {
                                    MouseTracking::Off => false,
                                    MouseTracking::Click => false,
                                    MouseTracking::Drag => reported_mouse_button.is_some(),
                                    MouseTracking::Motion => true,
                                };

                                if allow_motion {
                                    let code = if let Some(button) = reported_mouse_button {
                                        input::mouse_button_code(button)
                                            .unwrap_or(0)
                                            .saturating_add(32)
                                    } else {
                                        35
                                    };
                                    send_mouse_report(
                                        &pty,
                                        &parser,
                                        code,
                                        true,
                                        last_cursor_cell,
                                        modifiers,
                                    );
                                }
                            }
                        }
                        WindowEvent::MouseInput { state, button, .. } => {
                            if parser.modes().mouse_tracking == MouseTracking::Off {
                                if button == MouseButton::Left {
                                    match state {
                                        ElementState::Pressed => {
                                            selection.set_anchor(last_cursor_cell);
                                            window.request_redraw();
                                        }
                                        ElementState::Released => {
                                            selection.end_drag();
                                            window.request_redraw();
                                        }
                                    }
                                }
                            } else if let Some(code) = input::mouse_button_code(button) {
                                match state {
                                    ElementState::Pressed => {
                                        reported_mouse_button = Some(button);
                                        send_mouse_report(
                                            &pty,
                                            &parser,
                                            code,
                                            true,
                                            last_cursor_cell,
                                            modifiers,
                                        );
                                    }
                                    ElementState::Released => {
                                        reported_mouse_button = None;
                                        send_mouse_report(
                                            &pty,
                                            &parser,
                                            3,
                                            false,
                                            last_cursor_cell,
                                            modifiers,
                                        );
                                    }
                                }
                            }
                        }
                        WindowEvent::MouseWheel { delta, .. } => {
                            if parser.modes().mouse_tracking != MouseTracking::Off {
                                let lines = match delta {
                                    MouseScrollDelta::LineDelta(_, y) => y,
                                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                                };
                                if lines > 0.0 {
                                    send_mouse_report(
                                        &pty,
                                        &parser,
                                        64,
                                        true,
                                        last_cursor_cell,
                                        modifiers,
                                    );
                                } else if lines < 0.0 {
                                    send_mouse_report(
                                        &pty,
                                        &parser,
                                        65,
                                        true,
                                        last_cursor_cell,
                                        modifiers,
                                    );
                                }
                            }
                        }
                        WindowEvent::RedrawRequested => {
                            let dirty_rows = screen.take_dirty_rows();
                            renderer.update_rows(&screen, &dirty_rows);

                            let now = Instant::now();
                            perf.on_frame(now, dirty_rows.len());
                            let overlay = profiler_enabled.then_some(OverlayStats {
                                fps: perf.fps,
                                frame_ms: perf.frame_ms,
                                dirty_rows: perf.dirty_rows,
                                pty_bytes_per_sec: perf.bytes_per_second,
                            });

                            let selection_range = selection.range();
                            match renderer.render(&screen, selection_range, overlay) {
                                Ok(()) => {}
                                Err(wgpu::SurfaceError::Lost)
                                | Err(wgpu::SurfaceError::Outdated) => {
                                    renderer.resize_surface(window.inner_size(), screen.rows());
                                    screen.mark_all_dirty();
                                }
                                Err(wgpu::SurfaceError::OutOfMemory) => {
                                    error!("wgpu ran out of memory");
                                    elwt.exit();
                                }
                                Err(wgpu::SurfaceError::Timeout) => {
                                    warn!("surface timeout during render");
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Event::AboutToWait => {
                    sync_layout_mode(&mut renderer, &mut screen, &pty, &window);

                    let had_pty = drain_pty(&mut parser, &mut screen, &pty_rx, &mut pty, &mut perf);
                    if had_pty {
                        window.request_redraw();
                    }

                    let now = Instant::now();
                    perf.tick_rates(now);

                    if profiler_enabled && now >= redraw_deadline {
                        redraw_deadline = now + Duration::from_millis(16);
                        window.request_redraw();
                    }

                    match pty.try_wait() {
                        Ok(Some(status)) => {
                            info!("shell exited with status {status}");
                            elwt.exit();
                        }
                        Ok(None) => {}
                        Err(err) => {
                            error!("failed to check child status: {err}");
                            elwt.exit();
                        }
                    }
                }
                _ => {}
            }
        })
        .context("event loop error")
}

fn apply_resize(
    renderer: &mut Renderer,
    screen: &mut Screen,
    pty: &PtyHandle,
    size: PhysicalSize<u32>,
    window: &winit::window::Window,
) {
    let top_offset_changed = renderer.set_fullscreen_layout(effective_fullscreen(window));
    let (cols, rows) = renderer.grid_from_window_size(size);
    let changed = cols != screen.cols() || rows != screen.rows();

    renderer.resize_surface(size, rows);

    if changed {
        screen.resize(cols, rows);
        if let Err(err) = pty.resize(cols, rows) {
            warn!("failed to resize PTY: {err}");
        }
    } else if top_offset_changed {
        screen.mark_all_dirty();
        renderer.mark_all_dirty(screen);
    }

    window.request_redraw();
}

fn sync_layout_mode(
    renderer: &mut Renderer,
    screen: &mut Screen,
    pty: &PtyHandle,
    window: &winit::window::Window,
) {
    let size = window.inner_size();
    if size.width == 0 || size.height == 0 {
        return;
    }

    let top_offset_changed = renderer.set_fullscreen_layout(effective_fullscreen(window));
    if !top_offset_changed {
        return;
    }

    let (cols, rows) = renderer.grid_from_window_size(size);
    let grid_changed = cols != screen.cols() || rows != screen.rows();
    renderer.resize_surface(size, rows);

    if grid_changed {
        screen.resize(cols, rows);
        if let Err(err) = pty.resize(cols, rows) {
            warn!("failed to resize PTY after mode change: {err}");
        }
    }

    screen.mark_all_dirty();
    renderer.mark_all_dirty(screen);
    window.request_redraw();
}

fn effective_fullscreen(window: &winit::window::Window) -> bool {
    if window.fullscreen().is_none() {
        return false;
    }

    let Some(monitor) = window.current_monitor() else {
        return true;
    };

    let window_size = window.inner_size();
    let monitor_size = monitor.size();
    if monitor_size.width == 0 || monitor_size.height == 0 {
        return true;
    }

    let width_ratio = window_size.width as f32 / monitor_size.width as f32;
    let height_ratio = window_size.height as f32 / monitor_size.height as f32;
    width_ratio >= 0.95 && height_ratio >= 0.95
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FontSizeAction {
    Increase,
    Decrease,
}

fn handle_font_size_shortcuts(
    event: &winit::event::KeyEvent,
    modifiers: ModifiersState,
    renderer: &mut Renderer,
    screen: &mut Screen,
    pty: &PtyHandle,
    window: &winit::window::Window,
) -> bool {
    let Some(action) = font_size_shortcut_action(event, modifiers) else {
        return false;
    };

    let changed = match action {
        FontSizeAction::Increase => renderer.increase_font_size(),
        FontSizeAction::Decrease => renderer.decrease_font_size(),
    };
    if !changed {
        return true;
    }

    let size = window.inner_size();
    let (cols, rows) = renderer.grid_from_window_size(size);
    renderer.resize_surface(size, rows);

    let changed = cols != screen.cols() || rows != screen.rows();
    if changed {
        screen.resize(cols, rows);
        if let Err(err) = pty.resize(cols, rows) {
            warn!("failed to resize PTY after font size change: {err}");
        }
    }

    screen.mark_all_dirty();
    renderer.mark_all_dirty(screen);
    window.request_redraw();
    true
}

fn font_size_shortcut_action(
    event: &winit::event::KeyEvent,
    modifiers: ModifiersState,
) -> Option<FontSizeAction> {
    let Key::Character(chars) = &event.logical_key else {
        return None;
    };

    font_size_action_from_chars(chars.as_str(), event.state, modifiers)
}

fn font_size_action_from_chars(
    chars: &str,
    state: ElementState,
    modifiers: ModifiersState,
) -> Option<FontSizeAction> {
    if state != ElementState::Pressed || !modifiers.super_key() {
        return None;
    }

    match chars {
        "+" | "=" => Some(FontSizeAction::Increase),
        "-" | "_" => Some(FontSizeAction::Decrease),
        _ => None,
    }
}

fn drain_pty(
    parser: &mut AnsiParser,
    screen: &mut Screen,
    pty_rx: &Receiver<Vec<u8>>,
    pty: &mut PtyHandle,
    perf: &mut PerfState,
) -> bool {
    let mut total_bytes = 0_usize;
    let mut coalesced = Vec::new();

    while let Ok(chunk) = pty_rx.try_recv() {
        total_bytes += chunk.len();
        coalesced.extend_from_slice(&chunk);
    }

    if coalesced.is_empty() {
        return false;
    }

    perf.on_bytes(total_bytes);
    let responses = parser.process(&coalesced, screen);
    for response in responses {
        if let Err(err) = pty.write_all(&response) {
            warn!("failed to send terminal response to PTY: {err}");
        }
    }

    true
}

fn cursor_cell_from_position(
    position: PhysicalPosition<f64>,
    cell_width: f32,
    cell_height: f32,
    grid_left_padding: f32,
    grid_top_offset: f32,
    cols: usize,
    rows: usize,
) -> (usize, usize) {
    let adjusted_x = (position.x as f32 - grid_left_padding).max(0.0) as f64;
    let adjusted_y = (position.y as f32 - grid_top_offset).max(0.0) as f64;
    let col = (adjusted_x / cell_width as f64).floor().max(0.0) as usize;
    let row = (adjusted_y / cell_height as f64).floor().max(0.0) as usize;
    (
        col.min(cols.saturating_sub(1)),
        row.min(rows.saturating_sub(1)),
    )
}

fn handle_shortcuts(
    event: &winit::event::KeyEvent,
    modifiers: ModifiersState,
    selection: &mut SelectionState,
    screen: &Screen,
    clipboard: &mut Option<Clipboard>,
    pty: &PtyHandle,
    parser: &AnsiParser,
    profiler_enabled: &mut bool,
) -> bool {
    if event.state != ElementState::Pressed || !modifiers.super_key() {
        return false;
    }

    let Key::Character(chars) = &event.logical_key else {
        return false;
    };
    let action = chars.to_ascii_lowercase();

    match action.as_str() {
        "c" => {
            if selection.has_selection() {
                if let Some(range) = selection.range() {
                    let text = screen.extract_selection_text(range.start, range.end);
                    if let Some(clipboard) = clipboard.as_mut() {
                        if let Err(err) = clipboard.set_text(text) {
                            warn!("copy failed: {err}");
                        }
                    }
                    return true;
                }
            }

            if let Err(err) = pty.write_all(&[0x03]) {
                warn!("failed to send Ctrl-C fallback: {err}");
            }
            true
        }
        "v" => {
            if let Some(clipboard) = clipboard.as_mut() {
                if let Ok(text) = clipboard.get_text() {
                    let payload = if parser.modes().bracketed_paste {
                        input::wrap_bracketed_paste(&text)
                    } else {
                        text.into_bytes()
                    };
                    if let Err(err) = pty.write_all(&payload) {
                        warn!("paste failed: {err}");
                    }
                }
            }
            selection.clear();
            true
        }
        "p" if modifiers.shift_key() => {
            *profiler_enabled = !*profiler_enabled;
            true
        }
        "s" if modifiers.shift_key() => {
            let cmd = b"yes 'ultraterm stress output 0123456789 abcdefghijklmnopqrstuvwxyz'\n";
            if let Err(err) = pty.write_all(cmd) {
                warn!("failed to start stress command: {err}");
            }
            true
        }
        _ => false,
    }
}

fn send_mouse_report(
    pty: &PtyHandle,
    parser: &AnsiParser,
    code: u8,
    pressed: bool,
    cell: (usize, usize),
    modifiers: ModifiersState,
) {
    let col_1 = cell.0.saturating_add(1);
    let row_1 = cell.1.saturating_add(1);

    let bytes = if parser.modes().sgr_mouse {
        input::encode_sgr_mouse(code, pressed, col_1, row_1, modifiers)
    } else {
        input::encode_legacy_mouse(code, col_1, row_1, modifiers)
    };

    if let Err(err) = pty.write_all(&bytes) {
        warn!("failed to send mouse report: {err}");
    }
}

fn keyboard_text_fallback_bytes(
    event: &winit::event::KeyEvent,
    modifiers: ModifiersState,
) -> Option<Vec<u8>> {
    keyboard_text_bytes(event.text.as_deref(), event.state, modifiers)
}

fn keyboard_text_bytes(
    text: Option<&str>,
    state: ElementState,
    modifiers: ModifiersState,
) -> Option<Vec<u8>> {
    if state != ElementState::Pressed {
        return None;
    }

    if modifiers.super_key() || modifiers.control_key() || modifiers.alt_key() {
        return None;
    }

    let text = text?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return None;
    }

    Some(text.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::{font_size_action_from_chars, keyboard_text_bytes, FontSizeAction};
    use winit::event::ElementState;
    use winit::keyboard::ModifiersState;

    #[test]
    fn keyboard_text_bytes_accepts_plain_text() {
        let out = keyboard_text_bytes(Some("abc"), ElementState::Pressed, ModifiersState::empty());
        assert_eq!(out, Some(b"abc".to_vec()));
    }

    #[test]
    fn keyboard_text_bytes_rejects_control_text() {
        let out = keyboard_text_bytes(Some("\r"), ElementState::Pressed, ModifiersState::empty());
        assert_eq!(out, None);
    }

    #[test]
    fn keyboard_text_bytes_rejects_with_ctrl_modifier() {
        let out = keyboard_text_bytes(Some("c"), ElementState::Pressed, ModifiersState::CONTROL);
        assert_eq!(out, None);
    }

    #[test]
    fn keyboard_text_bytes_rejects_key_release() {
        let out = keyboard_text_bytes(Some("a"), ElementState::Released, ModifiersState::empty());
        assert_eq!(out, None);
    }

    #[test]
    fn font_size_shortcuts_match_cmd_plus_equals_minus() {
        assert_eq!(
            font_size_action_from_chars("+", ElementState::Pressed, ModifiersState::SUPER),
            Some(FontSizeAction::Increase),
        );
        assert_eq!(
            font_size_action_from_chars("=", ElementState::Pressed, ModifiersState::SUPER),
            Some(FontSizeAction::Increase),
        );
        assert_eq!(
            font_size_action_from_chars("-", ElementState::Pressed, ModifiersState::SUPER),
            Some(FontSizeAction::Decrease),
        );
        assert_eq!(
            font_size_action_from_chars("_", ElementState::Pressed, ModifiersState::SUPER),
            Some(FontSizeAction::Decrease),
        );
        assert_eq!(
            font_size_action_from_chars("+", ElementState::Pressed, ModifiersState::empty(),),
            None,
        );
        assert_eq!(
            font_size_action_from_chars("+", ElementState::Released, ModifiersState::SUPER,),
            None,
        );
    }
}

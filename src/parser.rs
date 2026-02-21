use vte::{Params, Parser, Perform};

use crate::screen::{
    ansi_256_to_rgb, basic_ansi_to_rgb, CursorShape, Rgb, Screen, DEFAULT_BG, DEFAULT_FG,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseTracking {
    Off,
    Click,
    Drag,
    Motion,
}

#[derive(Clone, Debug)]
pub struct TerminalModes {
    pub bracketed_paste: bool,
    pub app_cursor_keys: bool,
    pub mouse_tracking: MouseTracking,
    pub sgr_mouse: bool,
}

impl Default for TerminalModes {
    fn default() -> Self {
        Self {
            bracketed_paste: false,
            app_cursor_keys: false,
            mouse_tracking: MouseTracking::Off,
            sgr_mouse: false,
        }
    }
}

pub struct AnsiParser {
    parser: Parser,
    modes: TerminalModes,
}

impl AnsiParser {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            modes: TerminalModes::default(),
        }
    }

    pub fn modes(&self) -> &TerminalModes {
        &self.modes
    }

    pub fn process(&mut self, bytes: &[u8], screen: &mut Screen) -> Vec<Vec<u8>> {
        let mut responses = Vec::new();
        let mut performer = Performer {
            screen,
            modes: &mut self.modes,
            responses: &mut responses,
        };

        for &byte in bytes {
            self.parser.advance(&mut performer, byte);
        }

        responses
    }
}

struct Performer<'a> {
    screen: &'a mut Screen,
    modes: &'a mut TerminalModes,
    responses: &'a mut Vec<Vec<u8>>,
}

fn param_values(params: &Params) -> Vec<u16> {
    if params.is_empty() {
        return vec![0];
    }

    let mut values = Vec::new();
    for p in params.iter() {
        values.push(p.first().copied().unwrap_or(0));
    }
    values
}

fn first_param(params: &Params, default: u16) -> u16 {
    params
        .iter()
        .next()
        .and_then(|p| p.first())
        .copied()
        .unwrap_or(default)
}

impl<'a> Performer<'a> {
    fn set_mode(&mut self, private: bool, mode: u16, enabled: bool) {
        if private {
            match mode {
                1 => self.modes.app_cursor_keys = enabled,
                25 => self.screen.set_cursor_visible(enabled),
                1000 => {
                    self.modes.mouse_tracking = if enabled {
                        MouseTracking::Click
                    } else {
                        MouseTracking::Off
                    }
                }
                1002 => {
                    self.modes.mouse_tracking = if enabled {
                        MouseTracking::Drag
                    } else {
                        MouseTracking::Off
                    }
                }
                1003 => {
                    self.modes.mouse_tracking = if enabled {
                        MouseTracking::Motion
                    } else {
                        MouseTracking::Off
                    }
                }
                1006 => self.modes.sgr_mouse = enabled,
                1049 => self.screen.set_alt_screen(enabled),
                2004 => self.modes.bracketed_paste = enabled,
                _ => {}
            }
            return;
        }

        if mode == 4 {
            // Insert mode requested by some apps; we render overwrite-only for MVP.
            let _ = enabled;
        }
    }

    fn set_sgr(&mut self, params: &[u16]) {
        let mut i = 0usize;
        while i < params.len() {
            let p = params[i];
            match p {
                0 => self.screen.reset_attrs(),
                1 => self.screen.attrs_mut().bold = true,
                3 => self.screen.attrs_mut().italic = true,
                4 => self.screen.attrs_mut().underline = true,
                7 => self.screen.attrs_mut().inverse = true,
                22 => self.screen.attrs_mut().bold = false,
                23 => self.screen.attrs_mut().italic = false,
                24 => self.screen.attrs_mut().underline = false,
                27 => self.screen.attrs_mut().inverse = false,
                30..=37 => self.screen.set_fg(basic_ansi_to_rgb(p - 30, false)),
                39 => self.screen.set_fg(DEFAULT_FG),
                40..=47 => self.screen.set_bg(basic_ansi_to_rgb(p - 40, false)),
                49 => self.screen.set_bg(DEFAULT_BG),
                90..=97 => self.screen.set_fg(basic_ansi_to_rgb(p - 90, true)),
                100..=107 => self.screen.set_bg(basic_ansi_to_rgb(p - 100, true)),
                38 | 48 => {
                    let is_fg = p == 38;
                    if i + 1 < params.len() {
                        match params[i + 1] {
                            2 if i + 4 < params.len() => {
                                let color = Rgb::new(
                                    params[i + 2].min(255) as u8,
                                    params[i + 3].min(255) as u8,
                                    params[i + 4].min(255) as u8,
                                );
                                if is_fg {
                                    self.screen.set_fg(color);
                                } else {
                                    self.screen.set_bg(color);
                                }
                                i += 4;
                            }
                            5 if i + 2 < params.len() => {
                                let color = ansi_256_to_rgb(params[i + 2]);
                                if is_fg {
                                    self.screen.set_fg(color);
                                } else {
                                    self.screen.set_bg(color);
                                }
                                i += 2;
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn report_cursor_position(&mut self) {
        let cursor = self.screen.cursor();
        let row = cursor.row + 1;
        let col = cursor.col + 1;
        self.responses
            .push(format!("\x1b[{};{}R", row, col).into_bytes());
    }
}

impl<'a> Perform for Performer<'a> {
    fn print(&mut self, c: char) {
        self.screen.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => self.screen.line_feed(),
            b'\r' => self.screen.carriage_return(),
            b'\x08' => self.screen.backspace(),
            b'\t' => self.screen.tab(),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let private = intermediates.contains(&b'?');
        let values = param_values(params);
        let first = first_param(params, 1).max(1);

        match action {
            'A' => self.screen.cursor_up(first as usize),
            'B' => self.screen.cursor_down(first as usize),
            'C' => self.screen.cursor_forward(first as usize),
            'D' => self.screen.cursor_back(first as usize),
            'E' => {
                self.screen.cursor_down(first as usize);
                self.screen.carriage_return();
            }
            'F' => {
                self.screen.cursor_up(first as usize);
                self.screen.carriage_return();
            }
            'G' => {
                let col = first.saturating_sub(1) as usize;
                let row = self.screen.cursor().row;
                self.screen.set_cursor(row, col);
            }
            'H' | 'f' => {
                let row = values.first().copied().unwrap_or(1).saturating_sub(1) as usize;
                let col = values.get(1).copied().unwrap_or(1).saturating_sub(1) as usize;
                self.screen.set_cursor(row, col);
            }
            'J' => {
                self.screen
                    .erase_in_display(values.first().copied().unwrap_or(0) as usize);
            }
            'K' => {
                self.screen
                    .erase_in_line(values.first().copied().unwrap_or(0) as usize);
            }
            'L' => self.screen.insert_lines(first as usize),
            'M' => self.screen.delete_lines(first as usize),
            '@' => self.screen.insert_blank_chars(first as usize),
            'P' => self.screen.delete_chars(first as usize),
            'S' => {
                let (top, bottom) = self.screen.scroll_region();
                self.screen.scroll_up(first as usize, top, bottom);
            }
            'T' => {
                let (top, bottom) = self.screen.scroll_region();
                self.screen.scroll_down(first as usize, top, bottom);
            }
            'X' => self.screen.erase_chars(first as usize),
            'd' => {
                let row = first.saturating_sub(1) as usize;
                let col = self.screen.cursor().col;
                self.screen.set_cursor(row, col);
            }
            'm' => self.set_sgr(&values),
            'n' => {
                if !private {
                    match values.first().copied().unwrap_or(0) {
                        5 => self.responses.push(b"\x1b[0n".to_vec()),
                        6 => self.report_cursor_position(),
                        _ => {}
                    }
                }
            }
            'q' => {
                if intermediates.contains(&b' ') {
                    let shape = match values.first().copied().unwrap_or(0) {
                        3 | 4 => CursorShape::Underline,
                        5 | 6 => CursorShape::Beam,
                        _ => CursorShape::Block,
                    };
                    self.screen.set_cursor_shape(shape);
                }
            }
            'r' => {
                let top = values.first().copied().unwrap_or(1) as usize;
                let bottom = values.get(1).copied().unwrap_or(self.screen.rows() as u16) as usize;
                if top == 0 && bottom == 0 {
                    self.screen.reset_scroll_region();
                } else {
                    self.screen.set_scroll_region(top, bottom);
                }
            }
            's' => self.screen.save_cursor(),
            'u' => self.screen.restore_cursor(),
            'c' => {
                if !private {
                    self.responses.push(b"\x1b[?1;2c".to_vec());
                }
            }
            'h' => {
                for mode in values {
                    self.set_mode(private, mode, true);
                }
            }
            'l' => {
                for mode in values {
                    self.set_mode(private, mode, false);
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' => self.screen.save_cursor(),
            b'8' => self.screen.restore_cursor(),
            b'D' => self.screen.line_feed(),
            b'E' => {
                self.screen.line_feed();
                self.screen.carriage_return();
            }
            b'M' => self.screen.reverse_index(),
            b'c' => {
                self.screen.reset();
                *self.modes = TerminalModes::default();
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}

    fn put(&mut self, _byte: u8) {}

    fn unhook(&mut self) {}
}

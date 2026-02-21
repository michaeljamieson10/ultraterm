use winit::event::{ElementState, KeyEvent, MouseButton};
use winit::keyboard::{Key, ModifiersState, NamedKey};

pub fn key_event_to_bytes(
    event: &KeyEvent,
    modifiers: ModifiersState,
    app_cursor_keys: bool,
) -> Option<Vec<u8>> {
    if event.state != ElementState::Pressed {
        return None;
    }

    let shift = modifiers.shift_key();
    let alt = modifiers.alt_key();
    let ctrl = modifiers.control_key();

    let modifier_param = 1 + (shift as u8) + (alt as u8) * 2 + (ctrl as u8) * 4;

    let bytes = match &event.logical_key {
        Key::Named(NamedKey::ArrowUp) => arrow_sequence('A', modifier_param, app_cursor_keys),
        Key::Named(NamedKey::ArrowDown) => arrow_sequence('B', modifier_param, app_cursor_keys),
        Key::Named(NamedKey::ArrowRight) => arrow_sequence('C', modifier_param, app_cursor_keys),
        Key::Named(NamedKey::ArrowLeft) => arrow_sequence('D', modifier_param, app_cursor_keys),
        Key::Named(NamedKey::Home) => csi_or_ss3("H", modifier_param),
        Key::Named(NamedKey::End) => csi_or_ss3("F", modifier_param),
        Key::Named(NamedKey::Insert) => csi_with_mod(2, modifier_param),
        Key::Named(NamedKey::Delete) => csi_with_mod(3, modifier_param),
        Key::Named(NamedKey::PageUp) => csi_with_mod(5, modifier_param),
        Key::Named(NamedKey::PageDown) => csi_with_mod(6, modifier_param),
        Key::Named(NamedKey::F1) => function_key_sequence(1, modifier_param),
        Key::Named(NamedKey::F2) => function_key_sequence(2, modifier_param),
        Key::Named(NamedKey::F3) => function_key_sequence(3, modifier_param),
        Key::Named(NamedKey::F4) => function_key_sequence(4, modifier_param),
        Key::Named(NamedKey::F5) => function_key_sequence(5, modifier_param),
        Key::Named(NamedKey::F6) => function_key_sequence(6, modifier_param),
        Key::Named(NamedKey::F7) => function_key_sequence(7, modifier_param),
        Key::Named(NamedKey::F8) => function_key_sequence(8, modifier_param),
        Key::Named(NamedKey::F9) => function_key_sequence(9, modifier_param),
        Key::Named(NamedKey::F10) => function_key_sequence(10, modifier_param),
        Key::Named(NamedKey::F11) => function_key_sequence(11, modifier_param),
        Key::Named(NamedKey::F12) => function_key_sequence(12, modifier_param),
        Key::Named(NamedKey::Enter) => Some(vec![b'\r']),
        Key::Named(NamedKey::Tab) => {
            if shift {
                Some(b"\x1b[Z".to_vec())
            } else {
                Some(vec![b'\t'])
            }
        }
        Key::Named(NamedKey::Backspace) => Some(vec![0x7f]),
        Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
        Key::Character(txt) => {
            if txt.is_empty() {
                return None;
            }

            let mut chars = txt.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }

            if ctrl {
                control_code(ch).map(|code| vec![code])
            } else if alt {
                let mut out = Vec::with_capacity(1 + txt.len());
                out.push(0x1b);
                out.extend_from_slice(txt.as_bytes());
                Some(out)
            } else {
                None
            }
        }
        _ => None,
    };

    bytes
}

pub fn wrap_bracketed_paste(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(b"\x1b[201~");
    out
}

pub fn encode_sgr_mouse(
    button_code: u8,
    pressed: bool,
    col_1_based: usize,
    row_1_based: usize,
    modifiers: ModifiersState,
) -> Vec<u8> {
    let mut cb = button_code;
    if modifiers.shift_key() {
        cb = cb.saturating_add(4);
    }
    if modifiers.alt_key() {
        cb = cb.saturating_add(8);
    }
    if modifiers.control_key() {
        cb = cb.saturating_add(16);
    }

    let suffix = if pressed { 'M' } else { 'm' };
    format!("\x1b[<{};{};{}{}", cb, col_1_based, row_1_based, suffix).into_bytes()
}

pub fn encode_legacy_mouse(
    button_code: u8,
    col_1_based: usize,
    row_1_based: usize,
    modifiers: ModifiersState,
) -> Vec<u8> {
    let mut cb = button_code;
    if modifiers.shift_key() {
        cb = cb.saturating_add(4);
    }
    if modifiers.alt_key() {
        cb = cb.saturating_add(8);
    }
    if modifiers.control_key() {
        cb = cb.saturating_add(16);
    }

    let x = (col_1_based.min(223) as u8).saturating_add(32);
    let y = (row_1_based.min(223) as u8).saturating_add(32);
    vec![0x1b, b'[', b'M', cb.saturating_add(32), x, y]
}

pub fn mouse_button_code(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        _ => None,
    }
}

fn arrow_sequence(dir: char, modifier_param: u8, app_cursor: bool) -> Option<Vec<u8>> {
    if modifier_param == 1 && app_cursor {
        return Some(format!("\x1bO{}", dir).into_bytes());
    }

    if modifier_param == 1 {
        return Some(format!("\x1b[{}", dir).into_bytes());
    }

    Some(format!("\x1b[1;{}{}", modifier_param, dir).into_bytes())
}

fn csi_or_ss3(suffix: &str, modifier_param: u8) -> Option<Vec<u8>> {
    if modifier_param == 1 {
        return Some(format!("\x1b[{}", suffix).into_bytes());
    }
    Some(format!("\x1b[1;{}{}", modifier_param, suffix).into_bytes())
}

fn csi_with_mod(code: u8, modifier_param: u8) -> Option<Vec<u8>> {
    if modifier_param == 1 {
        return Some(format!("\x1b[{}~", code).into_bytes());
    }
    Some(format!("\x1b[{};{}~", code, modifier_param).into_bytes())
}

fn function_key_sequence(key: u8, modifier_param: u8) -> Option<Vec<u8>> {
    let base = match key {
        1 => {
            return Some(if modifier_param == 1 {
                b"\x1bOP".to_vec()
            } else {
                format!("\x1b[1;{}P", modifier_param).into_bytes()
            })
        }
        2 => {
            return Some(if modifier_param == 1 {
                b"\x1bOQ".to_vec()
            } else {
                format!("\x1b[1;{}Q", modifier_param).into_bytes()
            })
        }
        3 => {
            return Some(if modifier_param == 1 {
                b"\x1bOR".to_vec()
            } else {
                format!("\x1b[1;{}R", modifier_param).into_bytes()
            })
        }
        4 => {
            return Some(if modifier_param == 1 {
                b"\x1bOS".to_vec()
            } else {
                format!("\x1b[1;{}S", modifier_param).into_bytes()
            })
        }
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => return None,
    };

    if modifier_param == 1 {
        Some(format!("\x1b[{}~", base).into_bytes())
    } else {
        Some(format!("\x1b[{};{}~", base, modifier_param).into_bytes())
    }
}

fn control_code(ch: char) -> Option<u8> {
    let lower = ch.to_ascii_lowercase();
    match lower {
        '@' | ' ' => Some(0),
        'a'..='z' => Some((lower as u8) - b'a' + 1),
        '[' => Some(27),
        '\\' => Some(28),
        ']' => Some(29),
        '^' => Some(30),
        '_' => Some(31),
        _ => None,
    }
}

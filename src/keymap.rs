// ─────────────────────────────────────────────────────────────────────────────
// Linux key → character mappings
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(target_os = "linux")]
pub fn evkey_to_english_char(key: evdev::KeyCode) -> Option<char> {
    use evdev::KeyCode as K;
    match key {
        K::KEY_A => Some('a'), K::KEY_B => Some('b'), K::KEY_C => Some('c'),
        K::KEY_D => Some('d'), K::KEY_E => Some('e'), K::KEY_F => Some('f'),
        K::KEY_G => Some('g'), K::KEY_H => Some('h'), K::KEY_I => Some('i'),
        K::KEY_J => Some('j'), K::KEY_K => Some('k'), K::KEY_L => Some('l'),
        K::KEY_M => Some('m'), K::KEY_N => Some('n'), K::KEY_O => Some('o'),
        K::KEY_P => Some('p'), K::KEY_Q => Some('q'), K::KEY_R => Some('r'),
        K::KEY_S => Some('s'), K::KEY_T => Some('t'), K::KEY_U => Some('u'),
        K::KEY_V => Some('v'), K::KEY_W => Some('w'), K::KEY_X => Some('x'),
        K::KEY_Y => Some('y'), K::KEY_Z => Some('z'),
        K::KEY_1 => Some('1'), K::KEY_2 => Some('2'), K::KEY_3 => Some('3'),
        K::KEY_4 => Some('4'), K::KEY_5 => Some('5'), K::KEY_6 => Some('6'),
        K::KEY_7 => Some('7'), K::KEY_8 => Some('8'), K::KEY_9 => Some('9'),
        K::KEY_0 => Some('0'),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
pub fn evkey_to_hebrew_char(key: evdev::KeyCode) -> Option<char> {
    use evdev::KeyCode as K;
    match key {
        K::KEY_Q => Some('ק'), K::KEY_W => Some('ו'), K::KEY_E => Some('ק'),
        K::KEY_R => Some('ר'), K::KEY_T => Some('ת'), K::KEY_Y => Some('י'),
        K::KEY_U => Some('ו'), K::KEY_I => Some('ט'), K::KEY_O => Some('ם'),
        K::KEY_P => Some('פ'), K::KEY_A => Some('ש'), K::KEY_S => Some('ד'),
        K::KEY_D => Some('ג'), K::KEY_F => Some('כ'), K::KEY_G => Some('ע'),
        K::KEY_H => Some('י'), K::KEY_J => Some('ח'), K::KEY_K => Some('ל'),
        K::KEY_L => Some('ך'), K::KEY_Z => Some('ז'), K::KEY_X => Some('ס'),
        K::KEY_C => Some('ב'), K::KEY_V => Some('ה'), K::KEY_B => Some('נ'),
        K::KEY_N => Some('מ'), K::KEY_M => Some('צ'),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// macOS key → character mappings
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(target_os = "macos")]
pub fn key_to_english_char(key: rdev::Key) -> Option<char> {
    use rdev::Key as K;
    match key {
        K::KeyA => Some('a'), K::KeyB => Some('b'), K::KeyC => Some('c'),
        K::KeyD => Some('d'), K::KeyE => Some('e'), K::KeyF => Some('f'),
        K::KeyG => Some('g'), K::KeyH => Some('h'), K::KeyI => Some('i'),
        K::KeyJ => Some('j'), K::KeyK => Some('k'), K::KeyL => Some('l'),
        K::KeyM => Some('m'), K::KeyN => Some('n'), K::KeyO => Some('o'),
        K::KeyP => Some('p'), K::KeyQ => Some('q'), K::KeyR => Some('r'),
        K::KeyS => Some('s'), K::KeyT => Some('t'), K::KeyU => Some('u'),
        K::KeyV => Some('v'), K::KeyW => Some('w'), K::KeyX => Some('x'),
        K::KeyY => Some('y'), K::KeyZ => Some('z'),
        K::Num1 => Some('1'), K::Num2 => Some('2'), K::Num3 => Some('3'),
        K::Num4 => Some('4'), K::Num5 => Some('5'), K::Num6 => Some('6'),
        K::Num7 => Some('7'), K::Num8 => Some('8'), K::Num9 => Some('9'),
        K::Num0 => Some('0'),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
pub fn key_to_hebrew_char(key: rdev::Key) -> Option<char> {
    use rdev::Key as K;
    match key {
        K::KeyQ => Some('ק'), K::KeyW => Some('ו'), K::KeyE => Some('ק'),
        K::KeyR => Some('ר'), K::KeyT => Some('ת'), K::KeyY => Some('י'),
        K::KeyU => Some('ו'), K::KeyI => Some('ט'), K::KeyO => Some('ם'),
        K::KeyP => Some('פ'), K::KeyA => Some('ש'), K::KeyS => Some('ד'),
        K::KeyD => Some('ג'), K::KeyF => Some('כ'), K::KeyG => Some('ע'),
        K::KeyH => Some('י'), K::KeyJ => Some('ח'), K::KeyK => Some('ל'),
        K::KeyL => Some('ך'), K::KeyZ => Some('ז'), K::KeyX => Some('ס'),
        K::KeyC => Some('ב'), K::KeyV => Some('ה'), K::KeyB => Some('נ'),
        K::KeyN => Some('מ'), K::KeyM => Some('צ'),
        _ => None,
    }
}

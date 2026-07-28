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

/// Inverse of [`evkey_to_english_char`]: the key that types `c` under an
/// English layout. Used to inject a spelling correction, whose text is a
/// *different* string from what the user typed and so cannot be replayed from
/// the original keycodes. Returns `None` for anything the English layout can't
/// produce with a bare (unshifted) key press.
///
/// Linux only: `uinput` speaks keycodes, so a correction has to be spelled back
/// out as key presses. macOS and Windows insert the corrected text directly
/// (see their `replace_word`) and need no inverse map.
#[cfg(target_os = "linux")]
pub fn english_char_to_evkey(c: char) -> Option<evdev::KeyCode> {
    use evdev::KeyCode as K;
    Some(match c {
        'a' => K::KEY_A, 'b' => K::KEY_B, 'c' => K::KEY_C, 'd' => K::KEY_D,
        'e' => K::KEY_E, 'f' => K::KEY_F, 'g' => K::KEY_G, 'h' => K::KEY_H,
        'i' => K::KEY_I, 'j' => K::KEY_J, 'k' => K::KEY_K, 'l' => K::KEY_L,
        'm' => K::KEY_M, 'n' => K::KEY_N, 'o' => K::KEY_O, 'p' => K::KEY_P,
        'q' => K::KEY_Q, 'r' => K::KEY_R, 's' => K::KEY_S, 't' => K::KEY_T,
        'u' => K::KEY_U, 'v' => K::KEY_V, 'w' => K::KEY_W, 'x' => K::KEY_X,
        'y' => K::KEY_Y, 'z' => K::KEY_Z,
        '1' => K::KEY_1, '2' => K::KEY_2, '3' => K::KEY_3, '4' => K::KEY_4,
        '5' => K::KEY_5, '6' => K::KEY_6, '7' => K::KEY_7, '8' => K::KEY_8,
        '9' => K::KEY_9, '0' => K::KEY_0,
        _ => return None,
    })
}

/// Like [`english_char_to_evkey`], but also covers the characters a *replacement*
/// can contain that the word buffer itself never holds: capitals (from case
/// tracking) and spaces (from a multi-word abbreviation expansion). Returns the
/// key plus whether shift has to be held while pressing it.
///
/// Linux only, and for the same reason as its unshifted sibling: `uinput` speaks
/// keycodes, so anything we put back has to be spelled out as key presses.
#[cfg(target_os = "linux")]
pub fn english_char_to_evkey_shifted(c: char) -> Option<(evdev::KeyCode, bool)> {
    if c == ' ' {
        return Some((evdev::KeyCode::KEY_SPACE, false));
    }
    if c.is_ascii_uppercase() {
        return english_char_to_evkey(c.to_ascii_lowercase()).map(|k| (k, true));
    }
    english_char_to_evkey(c).map(|k| (k, false))
}

#[cfg(target_os = "linux")]
pub fn evkey_to_hebrew_char(key: evdev::KeyCode) -> Option<char> {
    use evdev::KeyCode as K;
    match key {
        K::KEY_Q => Some('/'), K::KEY_W => Some('\''), K::KEY_E => Some('ק'),
        K::KEY_R => Some('ר'), K::KEY_T => Some('א'), K::KEY_Y => Some('ט'),
        K::KEY_U => Some('ו'), K::KEY_I => Some('ן'), K::KEY_O => Some('ם'),
        K::KEY_P => Some('פ'), K::KEY_A => Some('ש'), K::KEY_S => Some('ד'),
        K::KEY_D => Some('ג'), K::KEY_F => Some('כ'), K::KEY_G => Some('ע'),
        K::KEY_H => Some('י'), K::KEY_J => Some('ח'), K::KEY_K => Some('ל'),
        K::KEY_L => Some('ך'), K::KEY_SEMICOLON => Some('ף'), K::KEY_APOSTROPHE => Some(','),
        K::KEY_Z => Some('ז'), K::KEY_X => Some('ס'), K::KEY_C => Some('ב'),
        K::KEY_V => Some('ה'), K::KEY_B => Some('נ'), K::KEY_N => Some('מ'),
        K::KEY_M => Some('צ'), K::KEY_COMMA => Some('ת'), K::KEY_DOT => Some('ץ'),
        K::KEY_SLASH => Some('.'), K::KEY_GRAVE => Some(';'), K::KEY_LEFTBRACE => Some(']'),
        K::KEY_RIGHTBRACE => Some('['), K::KEY_MINUS => Some('-'), K::KEY_EQUAL => Some('='),
        K::KEY_BACKSLASH => Some('\\'),
        // Numbers
        K::KEY_1 => Some('1'), K::KEY_2 => Some('2'), K::KEY_3 => Some('3'),
        K::KEY_4 => Some('4'), K::KEY_5 => Some('5'), K::KEY_6 => Some('6'),
        K::KEY_7 => Some('7'), K::KEY_8 => Some('8'), K::KEY_9 => Some('9'),
        K::KEY_0 => Some('0'),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// macOS key → character mappings
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(any(target_os = "macos", target_os = "windows"))]
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

/// Inverse of [`key_to_english_char`]: the key that types `c` under an English
/// layout, plus whether shift has to be held for it.
///
/// macOS and Windows inject a correction as *text*, so unlike Linux they never
/// need this to type anything. It exists to put words back into the *word
/// buffer*: after a completion the buffer has to hold the finished word rather
/// than the prefix the user typed, or the next Space would check a string that
/// is not what is on screen.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn english_char_to_key(c: char) -> Option<(rdev::Key, bool)> {
    use rdev::Key as K;
    if c == ' ' {
        return Some((K::Space, false));
    }
    let shift = c.is_ascii_uppercase();
    let key = match c.to_ascii_lowercase() {
        'a' => K::KeyA, 'b' => K::KeyB, 'c' => K::KeyC, 'd' => K::KeyD,
        'e' => K::KeyE, 'f' => K::KeyF, 'g' => K::KeyG, 'h' => K::KeyH,
        'i' => K::KeyI, 'j' => K::KeyJ, 'k' => K::KeyK, 'l' => K::KeyL,
        'm' => K::KeyM, 'n' => K::KeyN, 'o' => K::KeyO, 'p' => K::KeyP,
        'q' => K::KeyQ, 'r' => K::KeyR, 's' => K::KeyS, 't' => K::KeyT,
        'u' => K::KeyU, 'v' => K::KeyV, 'w' => K::KeyW, 'x' => K::KeyX,
        'y' => K::KeyY, 'z' => K::KeyZ,
        '1' => K::Num1, '2' => K::Num2, '3' => K::Num3, '4' => K::Num4,
        '5' => K::Num5, '6' => K::Num6, '7' => K::Num7, '8' => K::Num8,
        '9' => K::Num9, '0' => K::Num0,
        _ => return None,
    };
    Some((key, shift))
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn key_to_hebrew_char(key: rdev::Key) -> Option<char> {
    use rdev::Key as K;
    match key {
        K::KeyQ => Some('/'), K::KeyW => Some('\''), K::KeyE => Some('ק'),
        K::KeyR => Some('ר'), K::KeyT => Some('א'), K::KeyY => Some('ט'),
        K::KeyU => Some('ו'), K::KeyI => Some('ן'), K::KeyO => Some('ם'),
        K::KeyP => Some('פ'), K::KeyA => Some('ש'), K::KeyS => Some('ד'),
        K::KeyD => Some('ג'), K::KeyF => Some('כ'), K::KeyG => Some('ע'),
        K::KeyH => Some('י'), K::KeyJ => Some('ח'), K::KeyK => Some('ל'),
        K::KeyL => Some('ך'), K::SemiColon => Some('ף'), K::Quote => Some(','),
        K::KeyZ => Some('ז'), K::KeyX => Some('ס'), K::KeyC => Some('ב'),
        K::KeyV => Some('ה'), K::KeyB => Some('נ'), K::KeyN => Some('מ'),
        K::KeyM => Some('צ'), K::Comma => Some('ת'), K::Dot => Some('ץ'),
        K::Slash => Some('.'), K::BackQuote => Some(';'), K::LeftBracket => Some(']'),
        K::RightBracket => Some('['), K::Minus => Some('-'), K::Equal => Some('='),
        K::BackSlash => Some('\\'),
        // Numbers
        K::Num1 => Some('1'), K::Num2 => Some('2'), K::Num3 => Some('3'),
        K::Num4 => Some('4'), K::Num5 => Some('5'), K::Num6 => Some('6'),
        K::Num7 => Some('7'), K::Num8 => Some('8'), K::Num9 => Some('9'),
        K::Num0 => Some('0'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    /// The spelling autocorrect types its result through the inverse map, so a
    /// single wrong entry there would silently inject the wrong letter. Pin it
    /// to the forward map it inverts.
    #[cfg(target_os = "linux")]
    #[test]
    fn english_key_map_round_trips() {
        use super::{english_char_to_evkey, evkey_to_english_char};

        for code in 0u16..=255 {
            let key = evdev::KeyCode::new(code);
            if let Some(c) = evkey_to_english_char(key) {
                assert_eq!(english_char_to_evkey(c), Some(key), "{c}");
            }
        }
        // Every letter a correction can be built from must be typeable.
        for c in 'a'..='z' {
            assert!(english_char_to_evkey(c).is_some(), "{c}");
        }
        // Anything the English layout can't type unshifted has no key.
        for c in ['A', ' ', '\'', 'ש', '-'] {
            assert_eq!(english_char_to_evkey(c), None, "{c}");
        }
    }

    /// The macOS/Windows inverse map refills the word buffer after a
    /// completion, so a wrong entry there desynchronises the buffer from the
    /// screen. Pin it to the forward map the same way.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn english_key_map_round_trips_for_rdev() {
        use super::{english_char_to_key, key_to_english_char};

        for c in 'a'..='z' {
            let (key, shift) = english_char_to_key(c).expect("letter must be typeable");
            assert!(!shift, "{c}");
            assert_eq!(key_to_english_char(key), Some(c), "{c}");
            // A capital is the same key with shift held.
            let upper = c.to_ascii_uppercase();
            assert_eq!(english_char_to_key(upper), Some((key, true)), "{upper}");
        }
        // An expansion may contain spaces; nothing else outside the map does.
        assert_eq!(english_char_to_key(' ').map(|(_, s)| s), Some(false));
        assert_eq!(english_char_to_key('ש'), None);
    }
}

//! Spotlight owns inline autocomplete; replace its bounded search query atomically.
use super::*;
use core_foundation::{base::TCFType, string::CFString};
use core_foundation_sys::base::CFRange;

pub(super) fn selected_range(focus: &Focus) -> Option<CFRange> {
    let value = ax_attribute(focus, "AXSelectedTextRange")?;
    let mut range = CFRange {
        location: 0,
        length: 0,
    };
    unsafe {
        if core_foundation_sys::base::CFGetTypeID(value.as_CFTypeRef()) != AXValueGetTypeID()
            || AXValueGetValue(value.as_CFTypeRef(), 4, (&mut range as *mut CFRange).cast()) == 0
            || range.location < 0
            || range.length < 0
        {
            return None;
        }
    }
    Some(range)
}

fn suffix_range(value: &str, range: CFRange, expected: &str) -> Option<CFRange> {
    if range.length != 0 || value.len() > engine::MAX_SELECTION_BYTES {
        return None;
    }
    let units: Vec<_> = value.encode_utf16().collect();
    let suffix: Vec<_> = expected.encode_utf16().collect();
    let caret = usize::try_from(range.location).ok()?;
    let start = caret.checked_sub(suffix.len())?;
    if suffix.is_empty() || units.get(start..caret)? != suffix {
        return None;
    }
    Some(CFRange {
        location: start as isize,
        length: suffix.len() as isize,
    })
}

#[test]
fn suffix_ranges_use_utf16_offsets_and_require_exact_text() {
    let caret = |location| CFRange {
        location,
        length: 0,
    };
    let range = suffix_range("🙂 receive שלום tail", caret(15), "receive שלום").unwrap();
    assert_eq!((range.location, range.length), (3, 12));
    for (location, expected) in [
        (2, "🙂"),
        (-1, "receive"),
        (100, "receive"),
        (15, "recieve שלום"),
        (15, ""),
    ] {
        if location == 2 {
            assert_eq!(
                suffix_range("🙂", caret(location), expected)
                    .unwrap()
                    .length,
                2
            );
        } else {
            assert!(suffix_range("🙂 receive שלום tail", caret(location), expected).is_none());
        }
    }
}

#[test]
fn suffix_edits_reject_stale_words_selections_and_unreadable_carets() {
    for (value, location, length, expected) in [
        ("previous receive ", 17, 0, "recieve "),
        ("previous keyboard ", 18, 0, "keyboad "),
        ("previous שלום ", 14, 0, "akuo "),
        ("previous akuo ", 14, 0, "שלום "),
        ("previous recieve next", 21, 0, "recieve "),
        ("previous recieve ", 17, 1, "recieve "),
        ("previous recieve ", 17, -1, "recieve "),
        ("previous recieve ", -1, 0, "recieve "),
        ("previous recieve ", 99, 0, "recieve "),
        ("previous recieve ", 17, 0, ""),
    ] {
        assert!(
            suffix_range(value, CFRange { location, length }, expected).is_none(),
            "must not erase {expected:?} from {value:?}"
        );
    }
    for expected in ["recieve ", "keyboad ", "akuo ", "שלום "] {
        let value = format!("previous 🙂 {expected}next");
        let location = format!("previous 🙂 {expected}").encode_utf16().count() as isize;
        let range = suffix_range(
            &value,
            CFRange {
                location,
                length: 0,
            },
            expected,
        )
        .unwrap();
        let mut units: Vec<_> = value.encode_utf16().collect();
        units.splice(
            range.location as usize..location as usize,
            "corrected ".encode_utf16(),
        );
        assert_eq!(
            String::from_utf16(&units).unwrap(),
            "previous 🙂 corrected next"
        );
    }
    assert!(suffix_range(
        &"x".repeat(engine::MAX_SELECTION_BYTES + 1),
        CFRange {
            location: 1,
            length: 0
        },
        "x"
    )
    .is_none());
}

fn same_range(a: CFRange, b: CFRange) -> bool {
    a.location == b.location && a.length == b.length
}

fn wait_for_edit(
    snapshot: &mut impl FnMut() -> Option<(String, CFRange)>,
    matches: impl Fn(&str, CFRange) -> bool,
) -> Option<(String, CFRange)> {
    let start = std::time::Instant::now();
    let timing = crate::timing::injection();
    loop {
        let (current, selection) = snapshot()?;
        if matches(&current, selection) {
            return Some((current, selection));
        }
        if start.elapsed() >= timing.held_release_timeout {
            return None;
        }
        crate::timing::pause(timing.held_poll);
    }
}

fn confirm_edit(
    snapshot: &mut impl FnMut() -> Option<(String, CFRange)>,
    value: &str,
    range: Option<CFRange>,
) -> Option<()> {
    wait_for_edit(snapshot, |current, selection| {
        current == value && range.is_none_or(|range| same_range(selection, range))
    })
    .map(|_| ())
}

// Read-only accessibility controls still support ordinary keyboard editing.
fn replace_suffix_with_keys(
    expected: &str,
    replacement: &str,
    mut snapshot: impl FnMut() -> Option<(String, CFRange)>,
    mut backspace: impl FnMut() -> Option<()>,
    mut write: impl FnMut() -> Option<()>,
) -> Option<()> {
    let (original, caret) = snapshot()?;
    let range = suffix_range(&original, caret, expected)?;
    let original_units: Vec<_> = original.encode_utf16().collect();
    let mut wanted_units = original_units.clone();
    wanted_units.splice(
        range.location as usize..caret.location as usize,
        replacement.encode_utf16(),
    );
    let wanted = String::from_utf16(&wanted_units).ok()?;
    if wanted.len() > engine::MAX_SELECTION_BYTES {
        return None;
    }
    let mut value = original.clone();
    let mut position = caret;
    while position.location > range.location {
        let (current, current_range) = snapshot()?;
        if current != value || !same_range(current_range, position) {
            return None;
        }
        backspace()?;
        let (after, after_range) = wait_for_edit(&mut snapshot, |text, selection| {
            text != value || !same_range(selection, position)
        })?;
        if after_range.length != 0
            || after_range.location < range.location
            || after_range.location >= position.location
        {
            return None;
        }
        let mut remaining = original_units[..after_range.location as usize].to_vec();
        remaining.extend_from_slice(&original_units[caret.location as usize..]);
        if String::from_utf16(&remaining).ok()? != after {
            return None;
        }
        value = after;
        position = after_range;
    }
    // Insertion follows only acknowledged deletion, with the entire literal suffix.
    write()?;
    confirm_edit(
        &mut snapshot,
        &wanted,
        Some(CFRange {
            location: range.location + replacement.encode_utf16().count() as isize,
            length: 0,
        }),
    )
}

fn text_snapshot(
    engine: &Engine<Mac>,
    focus: &Focus,
    generation: u64,
) -> Option<(String, CFRange)> {
    if !engine.replacement_valid(generation) || focused_target().as_ref() != Some(focus) {
        return None;
    }
    let value = ax_attribute(focus, "AXValue")?.downcast::<CFString>()?;
    if value.char_len() > engine::MAX_SELECTION_BYTES as isize {
        return None;
    }
    Some((value.to_string(), selected_range(focus)?))
}

// Keep the edit protocol shared with fault-injection tests, including readback.
fn replace_suffix(
    expected: &str,
    replacement: &str,
    mut snapshot: impl FnMut() -> Option<(String, CFRange)>,
    mut select: impl FnMut(CFRange) -> Option<()>,
    mut write: impl FnMut() -> Option<()>,
) -> Option<()> {
    let (original, caret) = snapshot()?;
    let selected = suffix_range(&original, caret, expected)?;
    let mut units: Vec<_> = original.encode_utf16().collect();
    units.splice(
        selected.location as usize..caret.location as usize,
        replacement.encode_utf16(),
    );
    let wanted = String::from_utf16(&units).ok()?;
    if wanted.len() > engine::MAX_SELECTION_BYTES {
        return None;
    }
    let collapsed = CFRange {
        location: selected.location + replacement.encode_utf16().count() as isize,
        length: 0,
    };
    let result = (|| {
        let (current, range) = snapshot()?;
        if current != original || !same_range(range, caret) {
            return None;
        }
        select(selected)?;
        confirm_edit(&mut snapshot, &original, Some(selected))?;
        write()?;
        // AX success and CGEventPost mean accepted, not necessarily applied.
        confirm_edit(&mut snapshot, &wanted, None)?;
        select(collapsed)?;
        confirm_edit(&mut snapshot, &wanted, Some(collapsed))
    })();
    if result.is_none() {
        // Restore only an unchanged document and our own temporary selection.
        if snapshot().is_some_and(|(value, range)| value == original && same_range(range, selected))
        {
            let _ = select(caret);
        }
    }
    result
}

/// Prefer native replacement; read-only controls use acknowledged keyboard edits.
pub(super) fn replace_expected_suffix(
    engine: &Engine<Mac>,
    focus: &Focus,
    expected: &str,
    replacement: &str,
    generation: u64,
) -> Option<()> {
    let text_attribute = CFString::new("AXSelectedText");
    let range_attribute = CFString::new("AXSelectedTextRange");
    let settable = |attribute: &CFString| unsafe {
        let mut writable = 0;
        AXUIElementIsAttributeSettable(focus.0, attribute.as_concrete_TypeRef(), &mut writable) == 0
            && writable != 0
    };
    if !settable(&range_attribute) || !settable(&text_attribute) {
        let (value, range) = text_snapshot(engine, focus, generation)?;
        let selected = suffix_range(&value, range, expected)?;
        // Backspace must not cross a composed-character boundary into earlier text.
        let value = CFString::new(&value);
        let composed: cocoa::foundation::NSRange = unsafe {
            use objc::{msg_send, sel, sel_impl};
            msg_send![value.as_concrete_TypeRef() as cocoa::base::id,
            rangeOfComposedCharacterSequencesForRange: cocoa::foundation::NSRange {
                location: selected.location as u64,
                length: selected.length as u64,
            }]
        };
        if composed.location != selected.location as u64
            || composed.length != selected.length as u64
        {
            return None;
        }
        let text = text_events(replacement)?;
        let mut deletion = Vec::new();
        for down in [true, false] {
            deletion.push(DeferredEvent {
                kind: if down {
                    KCG_EVENT_KEY_DOWN
                } else {
                    KCG_EVENT_KEY_UP
                },
                event: keyboard_event(51, down, false)?,
            });
        }
        return replace_suffix_with_keys(
            expected,
            replacement,
            || text_snapshot(engine, focus, generation),
            || {
                text_snapshot(engine, focus, generation)?;
                engine.injector.store(true, Ordering::Relaxed);
                post_events(&deletion);
                Some(())
            },
            || {
                text_snapshot(engine, focus, generation)?;
                post_events(&text);
                Some(())
            },
        );
    }
    let replacement_value = CFString::new(replacement);
    replace_suffix(
        expected,
        replacement,
        || text_snapshot(engine, focus, generation),
        |range| unsafe {
            if !engine.replacement_valid(generation) || focused_target().as_ref() != Some(focus) {
                return None;
            }
            let value = AXValueCreate(4, (&range as *const CFRange).cast());
            if value.is_null() {
                return None;
            }
            let value = core_foundation::base::CFType::wrap_under_create_rule(value);
            engine.injector.store(true, Ordering::Relaxed);
            (AXUIElementSetAttributeValue(
                focus.0,
                range_attribute.as_concrete_TypeRef(),
                value.as_CFTypeRef(),
            ) == 0)
                .then_some(())
        },
        || unsafe {
            if !engine.replacement_valid(generation) || focused_target().as_ref() != Some(focus) {
                return None;
            }
            // Never retry a failed setter: it may already have changed text.
            (AXUIElementSetAttributeValue(
                focus.0,
                text_attribute.as_concrete_TypeRef(),
                replacement_value.as_CFTypeRef(),
            ) == 0)
                .then_some(())
        },
    )
}

// Accept only a caret at the query's end, or a selected inline-completion suffix.
// Match the exact engine-owned suffix before writing, preserving earlier query text.
fn query_replacement(
    value: &str,
    range: CFRange,
    expected: &str,
    replacement: &str,
) -> Option<String> {
    if value.len() > engine::MAX_SELECTION_BYTES || range.location < 0 || range.length < 0 {
        return None;
    }
    let units: Vec<u16> = value.encode_utf16().collect();
    let end = usize::try_from(range.location).ok()?;
    if end.checked_add(usize::try_from(range.length).ok()?)? != units.len() {
        return None;
    }
    let prefix = String::from_utf16(units.get(..end)?).ok()?;
    let untouched = prefix.strip_suffix(expected)?;
    Some(format!("{untouched}{replacement}"))
}

pub(super) fn replace_spotlight_query(
    engine: &Engine<Mac>,
    focus: &Focus,
    expected: &str,
    replacement: &str,
    buffered: &[Typed],
    generation: u64,
) -> Option<()> {
    let value_attribute = CFString::new("AXValue");
    let range_attribute = CFString::new("AXSelectedTextRange");
    unsafe {
        // Never fall back to blind Backspaces in Spotlight.
        for attribute in [&value_attribute, &range_attribute] {
            let mut writable = 0;
            if AXUIElementIsAttributeSettable(
                focus.0,
                attribute.as_concrete_TypeRef(),
                &mut writable,
            ) != 0
                || writable == 0
            {
                return None;
            }
        }
        let original_value = ax_attribute(focus, "AXValue")?.downcast::<CFString>()?;
        if original_value.char_len() > engine::MAX_SELECTION_BYTES as isize {
            return None;
        }
        let value = original_value.to_string();
        let range = selected_range(focus)?;
        let lang = Mac::current_layout()?;
        let tail = engine::reading::<Mac>(buffered, lang);
        let query = query_replacement(
            &value,
            range,
            &format!("{expected}{tail}"),
            &format!("{replacement}{tail}"),
        )?;
        let replacement = CFString::new(&query);
        let caret = CFRange {
            location: query.encode_utf16().count() as isize,
            length: 0,
        };
        let caret_value = AXValueCreate(4, (&caret as *const CFRange).cast());
        if caret_value.is_null() {
            return None;
        }
        let caret_value = core_foundation::base::CFType::wrap_under_create_rule(caret_value);
        if !engine.replacement_valid(generation)
            || focused_target().as_ref() != Some(focus)
            || selected_range(focus).is_none_or(|current| {
                current.location != range.location || current.length != range.length
            })
            || ax_attribute(focus, "AXValue")?.downcast::<CFString>()? != original_value
        {
            return None;
        }
        engine.injector.store(true, Ordering::Relaxed);
        if AXUIElementSetAttributeValue(
            focus.0,
            value_attribute.as_concrete_TypeRef(),
            replacement.as_CFTypeRef(),
        ) != 0
        {
            return None;
        }
        // Preserve any newly selected autocomplete suffix, rather than collapsing
        // its selection and leaving extra text after the insertion point.
        if !engine.replacement_valid(generation) {
            return None;
        }
        let after = ax_attribute(focus, "AXValue")?.downcast::<CFString>()?;
        if after == replacement
            && AXUIElementSetAttributeValue(
                focus.0,
                range_attribute.as_concrete_TypeRef(),
                caret_value.as_CFTypeRef(),
            ) != 0
        {
            return None;
        }
        let after = ax_attribute(focus, "AXValue")?
            .downcast::<CFString>()?
            .to_string();
        (query_replacement(&after, selected_range(focus)?, &query, &query).as_deref()
            == Some(query.as_str()))
        .then_some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acceptance_key_reaches_spotlight_only_after_replacing_its_inline_completion() {
        for key in [' ', 'x'] {
            let mut deferred = super::super::DeferredInput::default();
            let ticket = deferred.begin();
            deferred.events.push(key);
            let mut query = query_replacement(
                "thermal",
                CFRange {
                    location: 4,
                    length: 3,
                },
                "her",
                "hrough",
            )
            .unwrap();
            assert_eq!(query, "through");
            query.extend(deferred.finish(ticket).unwrap());
            assert_eq!(query, format!("through{key}"));
        }
    }
    #[test]
    fn spotlight_autocomplete_replacement_preserves_query_and_rejects_stale_text() {
        for (value, location, length, expected, replacement, wanted) in [
            ("ther", 4, 0, "her", "hrough", "through"),
            ("thermal", 4, 3, "her", "hrough", "through"),
            ("find thermal", 9, 3, "her", "hrough", "find through"),
            ("של thermal", 7, 3, "her", "hrough", "של through"),
            ("🙂 thermal", 7, 3, "her", "hrough", "🙂 through"),
            ("therx", 5, 0, "herx", "hroughx", "throughx"),
        ] {
            assert_eq!(
                query_replacement(value, CFRange { location, length }, expected, replacement)
                    .as_deref(),
                Some(wanted)
            );
        }
        for (value, location, length) in [
            ("then", 4, 0),
            ("ther more", 4, 0),
            ("thermal", 3, 4),
            ("ther", -1, 0),
            ("ther", 4, 1),
        ] {
            assert!(
                query_replacement(value, CFRange { location, length }, "ther", "through").is_none()
            );
        }
    }
}

#[cfg(test)]
mod suffix_edit_tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn correction_replaces_only_selected_suffix_and_confirms_utf16_caret() {
        for (expected, replacement) in [
            ("recieve ", "receive "),
            ("akuo ", "שלום "),
            ("שלום ", "akuo "),
            ("תודדה, ", "תודה, "),
            ("word ", "a much longer expansion than one Unicode event "),
            ("שָׁלוֹם ", "שלום "),
            ("recieve next ", "receive next "),
            ("word", ""),
        ] {
            let original = format!("earlier 🙂 {expected}untouched");
            let caret = CFRange {
                location: format!("earlier 🙂 {expected}").encode_utf16().count() as isize,
                length: 0,
            };
            let control = RefCell::new((original, caret));
            replace_suffix(
                expected,
                replacement,
                || Some(control.borrow().clone()),
                |range| {
                    control.borrow_mut().1 = range;
                    Some(())
                },
                || {
                    let mut control = control.borrow_mut();
                    let range = control.1;
                    let mut units: Vec<_> = control.0.encode_utf16().collect();
                    units.splice(
                        range.location as usize..(range.location + range.length) as usize,
                        replacement.encode_utf16(),
                    );
                    control.0 = String::from_utf16(&units).unwrap();
                    Some(())
                },
            )
            .unwrap();
            let control = control.borrow();
            assert_eq!(control.0, format!("earlier 🙂 {replacement}untouched"));
            assert!(same_range(
                control.1,
                CFRange {
                    location: format!("earlier 🙂 {replacement}").encode_utf16().count() as isize,
                    length: 0,
                }
            ));
        }
    }

    #[test]
    fn stale_or_oversized_edits_never_select_or_write() {
        let reads = Cell::new(0);
        assert!(replace_suffix(
            "recieve ",
            "receive ",
            || {
                reads.set(reads.get() + 1);
                Some((
                    if reads.get() == 1 {
                        "earlier recieve next"
                    } else {
                        "earlier receive next"
                    }
                    .to_owned(),
                    CFRange {
                        location: 16,
                        length: 0,
                    },
                ))
            },
            |_| panic!("stale snapshot changed selection"),
            || panic!("stale snapshot wrote text"),
        )
        .is_none());
        for (expected, replacement) in [
            ("recieve ", "receive ".to_owned()),
            ("receive ", "x".repeat(engine::MAX_SELECTION_BYTES)),
        ] {
            assert!(replace_suffix(
                expected,
                &replacement,
                || Some((
                    "earlier receive next".to_owned(),
                    CFRange {
                        location: 16,
                        length: 0
                    }
                )),
                |_| panic!("invalid edit changed selection"),
                || panic!("invalid edit wrote text"),
            )
            .is_none());
        }
    }

    #[test]
    fn ignored_or_wrong_selection_never_writes_even_if_selected_text_matches() {
        for wrong_location in [Some(0), None] {
            let original = "recieve recieve ".to_owned();
            let caret = CFRange {
                location: 16,
                length: 0,
            };
            let control = RefCell::new((original.clone(), caret));
            let writes = Cell::new(0);
            assert!(replace_suffix(
                "recieve ",
                "receive ",
                || Some(control.borrow().clone()),
                |mut range| {
                    if let Some(location) = wrong_location {
                        range.location = location;
                        control.borrow_mut().1 = range;
                    }
                    Some(())
                },
                || {
                    writes.set(writes.get() + 1);
                    Some(())
                },
            )
            .is_none());
            assert_eq!(writes.get(), 0);
            assert_eq!(control.borrow().0, original);
        }
    }

    #[test]
    fn failed_noop_or_partial_write_never_retries_or_moves_caret_past_text() {
        for (after, success) in [
            ("earlier recieve next", true),
            ("earlier rece next", true),
            ("earlier receive next", false),
            ("earlier rece next", false),
        ] {
            let original = "earlier recieve next".to_owned();
            let caret = CFRange {
                location: 16,
                length: 0,
            };
            let control = RefCell::new((original.clone(), caret));
            let selections = RefCell::new(Vec::new());
            let writes = Cell::new(0);
            assert!(replace_suffix(
                "recieve ",
                "receive ",
                || Some(control.borrow().clone()),
                |range| {
                    selections.borrow_mut().push(range);
                    control.borrow_mut().1 = range;
                    Some(())
                },
                || {
                    writes.set(writes.get() + 1);
                    control.borrow_mut().0 = after.to_owned();
                    success.then_some(())
                },
            )
            .is_none());
            assert_eq!(writes.get(), 1);
            assert_eq!(control.borrow().0, after);
            if after == original {
                assert_eq!(selections.borrow().len(), 2);
                assert!(same_range(control.borrow().1, caret));
            } else {
                assert_eq!(selections.borrow().len(), 1);
            }
        }
    }

    #[test]
    fn delayed_write_is_confirmed_before_caret_moves_and_input_resumes() {
        let caret = CFRange {
            location: 16,
            length: 0,
        };
        let control = RefCell::new(("earlier recieve next".to_owned(), caret));
        let pending = Cell::new(false);
        let reads = Cell::new(0);
        replace_suffix(
            "recieve ",
            "a longer correction ",
            || {
                if pending.get() {
                    reads.set(reads.get() + 1);
                    if reads.get() == 3 {
                        control.borrow_mut().0 = "earlier a longer correction next".to_owned();
                        pending.set(false);
                    }
                }
                Some(control.borrow().clone())
            },
            |range| {
                assert!(!pending.get(), "caret moved before write landed");
                control.borrow_mut().1 = range;
                Some(())
            },
            || {
                pending.set(true);
                Some(())
            },
        )
        .unwrap();
        assert_eq!(reads.get(), 3);
        assert_eq!(control.borrow().0, "earlier a longer correction next");
        assert!(same_range(
            control.borrow().1,
            CFRange {
                location: 28,
                length: 0
            }
        ));
    }

    #[test]
    fn losing_focus_after_selection_cancels_write_and_caret_restore() {
        let valid = Cell::new(true);
        let selections = Cell::new(0);
        let writes = Cell::new(0);
        assert!(replace_suffix(
            "recieve ",
            "receive ",
            || valid.get().then(|| (
                "earlier recieve next".to_owned(),
                CFRange {
                    location: 16,
                    length: 0
                }
            )),
            |_| {
                selections.set(selections.get() + 1);
                valid.set(false);
                Some(())
            },
            || {
                writes.set(writes.get() + 1);
                Some(())
            },
        )
        .is_none());
        assert_eq!(selections.get(), 1);
        assert_eq!(writes.get(), 0);
    }
}

#[cfg(test)]
mod keyboard_edit_tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn read_only_controls_apply_complete_corrections_without_range_setters() {
        for (expected, replacement) in [
            ("recieve ", "receive "),
            ("akuo ", "שלום "),
            ("שלום ", "akuo "),
            ("recieve next ", "receive next "),
            ("שָׁלוֹם ", "שלום "),
            ("word ", "a much longer expansion than one event "),
        ] {
            let prefix = "earlier 🙂 ";
            let original = format!("{prefix}{expected}untouched");
            let caret = CFRange {
                location: format!("{prefix}{expected}").encode_utf16().count() as isize,
                length: 0,
            };
            let control = RefCell::new((original, caret));
            let deletions = Cell::new(0);
            replace_suffix_with_keys(
                expected,
                replacement,
                || Some(control.borrow().clone()),
                || {
                    deletions.set(deletions.get() + 1);
                    let mut control = control.borrow_mut();
                    let mut units: Vec<_> = control.0.encode_utf16().collect();
                    let end = control.1.location as usize;
                    // Model deletion of a whole Hebrew base plus its combining marks.
                    let mut start = end - 1;
                    while start > 0 && (0x0591..=0x05c7).contains(&units[start]) {
                        start -= 1;
                    }
                    units.drain(start..end);
                    control.0 = String::from_utf16(&units).unwrap();
                    control.1.location = start as isize;
                    Some(())
                },
                || {
                    let mut control = control.borrow_mut();
                    assert_eq!(control.0, format!("{prefix}untouched"));
                    assert_eq!(control.1.location, prefix.encode_utf16().count() as isize);
                    control.0 = format!("{prefix}{replacement}untouched");
                    control.1.location += replacement.encode_utf16().count() as isize;
                    Some(())
                },
            )
            .unwrap();
            assert_eq!(
                control.borrow().0,
                format!("{prefix}{replacement}untouched")
            );
            assert!(deletions.get() > 0);
        }
    }

    #[test]
    fn dropped_backspace_never_advances_deletion_or_inserts_into_original_word() {
        let deletions = Cell::new(0);
        assert!(replace_suffix_with_keys(
            "recieve ",
            "receive ",
            || Some((
                "earlier recieve next".to_owned(),
                CFRange {
                    location: 16,
                    length: 0
                }
            )),
            || {
                deletions.set(deletions.get() + 1);
                Some(())
            },
            || panic!("inserted before deletion was acknowledged"),
        )
        .is_none());
        assert_eq!(deletions.get(), 1);
    }

    #[test]
    fn delayed_deletion_is_acknowledged_before_next_key_or_insertion() {
        let control = RefCell::new((
            "before x after".to_owned(),
            CFRange {
                location: 8,
                length: 0,
            },
        ));
        let pending = Cell::new(false);
        let reads = Cell::new(0);
        replace_suffix_with_keys(
            "x",
            "replacement",
            || {
                if pending.get() {
                    reads.set(reads.get() + 1);
                    if reads.get() == 3 {
                        control.borrow_mut().0 = "before  after".to_owned();
                        control.borrow_mut().1.location = 7;
                        pending.set(false);
                    }
                }
                Some(control.borrow().clone())
            },
            || {
                assert!(!pending.replace(true));
                Some(())
            },
            || {
                assert!(!pending.get());
                control.borrow_mut().0 = "before replacement after".to_owned();
                control.borrow_mut().1.location = 18;
                Some(())
            },
        )
        .unwrap();
        assert_eq!(reads.get(), 3);
    }

    #[test]
    fn unexpected_host_edit_stops_before_another_deletion_or_insertion() {
        let control = RefCell::new((
            "before recieve after".to_owned(),
            CFRange {
                location: 15,
                length: 0,
            },
        ));
        let deletions = Cell::new(0);
        assert!(replace_suffix_with_keys(
            "recieve ",
            "receive ",
            || Some(control.borrow().clone()),
            || {
                deletions.set(deletions.get() + 1);
                control.borrow_mut().0 = "before receive after".to_owned();
                Some(())
            },
            || panic!("unexpected host edit must cancel insertion"),
        )
        .is_none());
        assert_eq!(deletions.get(), 1);
        assert_eq!(control.borrow().0, "before receive after");
    }
}

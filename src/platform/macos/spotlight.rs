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

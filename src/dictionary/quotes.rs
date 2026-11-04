//! Dictionary spellings use ASCII quotes; Hebrew keyboards may emit geresh.

use std::borrow::Cow;

pub(crate) fn canonical(text: &str) -> Cow<'_, str> {
    if text.contains(['׳', '״']) {
        Cow::Owned(text.replace('׳', "'").replace('״', "\""))
    } else {
        Cow::Borrowed(text)
    }
}

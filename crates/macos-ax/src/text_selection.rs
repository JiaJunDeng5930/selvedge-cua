use objc2_core_foundation::CFRange;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Target, decode, native, submit};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SelectText {
    target: String,
    text: String,
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    suffix: String,
    #[serde(default)]
    selection_type: SelectionType,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
enum SelectionType {
    #[default]
    Range,
    CursorBefore,
    CursorAfter,
}

/// Changes a writable AXSelectedTextRange using the live accessibility text.
pub fn select_text(options: &Value) -> Result<Value, String> {
    let options: SelectText = decode(options, "desktop.selectText")?;
    if options.text.is_empty() {
        return Err("desktop.selectText requires nonempty text".into());
    }
    let target = Target::parse(&options.target)?;
    submit(move |accessibility| {
        let retained = accessibility.target(target)?;
        let element = &retained.element;
        if !native::settable(element, native::SELECTED_TEXT_RANGE)
            .map_err(|error| error.to_string())?
        {
            return Err("The target does not expose a writable AXSelectedTextRange".into());
        }
        let value = native::text_value(element)
            .map_err(|error| error.to_string())?
            .ok_or("The target does not expose a string AXValue for text selection")?;
        let range = options.range_in(&value)?;
        native::set_selected_text_range(element, range).map_err(|error| error.to_string())?;
        Ok(
            json!({"pid": retained.application.processIdentifier(), "target": options.target,
                  "range": {"location": range.location, "length": range.length}}),
        )
    })
}

impl SelectText {
    fn range_in(&self, value: &str) -> Result<CFRange, String> {
        let mut match_location = None;
        let mut offset = 0;
        while let Some(relative) = value[offset..].find(&self.text) {
            let at = offset + relative;
            let end = at + self.text.len();
            // Advance one character so overlapping matches count as ambiguous.
            offset = at
                + value[at..]
                    .chars()
                    .next()
                    .expect("nonempty match")
                    .len_utf8();
            if !value[..at].ends_with(&self.prefix) || !value[end..].starts_with(&self.suffix) {
                continue;
            }
            if match_location.is_some() {
                return Err(
                    "Text selection matches more than once; specify prefix or suffix".into(),
                );
            }
            match_location = Some(at);
        }
        let at =
            match_location.ok_or("Text selection did not match the target's accessibility text")?;
        // AppKit's ranges count UTF-16 code units, not Rust byte offsets.
        let location = value[..at].encode_utf16().count() as isize;
        let length = self.text.encode_utf16().count() as isize;
        let (location, length) = match self.selection_type {
            SelectionType::Range => (location, length),
            SelectionType::CursorBefore => (location, 0),
            SelectionType::CursorAfter => (location + length, 0),
        };
        Ok(CFRange { location, length })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_context_identifies_utf16_range() {
        let selection = decode::<SelectText>(&json!({"target": "ax-1", "text": "😺", "prefix": "第二", "selectionType": "cursorAfter"}), "selectText").unwrap();
        let range = selection.range_in("第一😺第二😺").unwrap();
        assert_eq!((range.location, range.length), (8, 0));
    }

    #[test]
    fn overlapping_occurrences_are_ambiguous() {
        let selection =
            decode::<SelectText>(&json!({"target": "ax-1", "text": "aa"}), "selectText").unwrap();
        assert!(
            selection
                .range_in("aaa")
                .unwrap_err()
                .contains("more than once")
        );
    }
}

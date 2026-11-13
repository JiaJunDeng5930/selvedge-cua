//! Slice adapter for Quartz's UTF-16 event API.

#![allow(unsafe_code)]

use objc2_core_graphics::CGEvent;

pub(super) fn set_text(event: &CGEvent, text: &str) {
    let units: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: The retained event lives for the call. Quartz copies the
    // requested UTF-16 units synchronously. The pointer spans that many
    // initialized units, including complete UTF-16 surrogate pairs.
    unsafe {
        CGEvent::keyboard_set_unicode_string(Some(event), units.len() as u64, units.as_ptr());
    }
}

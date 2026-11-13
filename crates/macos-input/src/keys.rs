use objc2_core_graphics::CGEventFlags;

/// Physical UI Events code names, mapped to the virtual key positions in
/// macOS HIToolbox's Events.h. ANSI names label positions on a US keyboard.
const KEY_CODES: &[(&str, u16)] = &[
    ("KeyA", 0x00),
    ("KeyB", 0x0b),
    ("KeyC", 0x08),
    ("KeyD", 0x02),
    ("KeyE", 0x0e),
    ("KeyF", 0x03),
    ("KeyG", 0x05),
    ("KeyH", 0x04),
    ("KeyI", 0x22),
    ("KeyJ", 0x26),
    ("KeyK", 0x28),
    ("KeyL", 0x25),
    ("KeyM", 0x2e),
    ("KeyN", 0x2d),
    ("KeyO", 0x1f),
    ("KeyP", 0x23),
    ("KeyQ", 0x0c),
    ("KeyR", 0x0f),
    ("KeyS", 0x01),
    ("KeyT", 0x11),
    ("KeyU", 0x20),
    ("KeyV", 0x09),
    ("KeyW", 0x0d),
    ("KeyX", 0x07),
    ("KeyY", 0x10),
    ("KeyZ", 0x06),
    ("Digit0", 0x1d),
    ("Digit1", 0x12),
    ("Digit2", 0x13),
    ("Digit3", 0x14),
    ("Digit4", 0x15),
    ("Digit5", 0x17),
    ("Digit6", 0x16),
    ("Digit7", 0x1a),
    ("Digit8", 0x1c),
    ("Digit9", 0x19),
    ("Backquote", 0x32),
    ("Backslash", 0x2a),
    ("BracketLeft", 0x21),
    ("BracketRight", 0x1e),
    ("Comma", 0x2b),
    ("Equal", 0x18),
    ("Minus", 0x1b),
    ("Period", 0x2f),
    ("Quote", 0x27),
    ("Semicolon", 0x29),
    ("Slash", 0x2c),
    ("Enter", 0x24),
    ("Tab", 0x30),
    ("Space", 0x31),
    ("Backspace", 0x33),
    ("Escape", 0x35),
    ("Insert", 0x72),
    ("Home", 0x73),
    ("PageUp", 0x74),
    ("Delete", 0x75),
    ("End", 0x77),
    ("PageDown", 0x79),
    ("ArrowLeft", 0x7b),
    ("ArrowRight", 0x7c),
    ("ArrowDown", 0x7d),
    ("ArrowUp", 0x7e),
    ("F1", 0x7a),
    ("F2", 0x78),
    ("F3", 0x63),
    ("F4", 0x76),
    ("F5", 0x60),
    ("F6", 0x61),
    ("F7", 0x62),
    ("F8", 0x64),
    ("F9", 0x65),
    ("F10", 0x6d),
    ("F11", 0x67),
    ("F12", 0x6f),
    ("F13", 0x69),
    ("F14", 0x6b),
    ("F15", 0x71),
    ("F16", 0x6a),
    ("F17", 0x40),
    ("F18", 0x4f),
    ("F19", 0x50),
    ("F20", 0x5a),
    ("NumLock", 0x47),
    ("Numpad0", 0x52),
    ("Numpad1", 0x53),
    ("Numpad2", 0x54),
    ("Numpad3", 0x55),
    ("Numpad4", 0x56),
    ("Numpad5", 0x57),
    ("Numpad6", 0x58),
    ("Numpad7", 0x59),
    ("Numpad8", 0x5b),
    ("Numpad9", 0x5c),
    ("NumpadAdd", 0x45),
    ("NumpadDecimal", 0x41),
    ("NumpadDivide", 0x4b),
    ("NumpadEnter", 0x4c),
    ("NumpadEqual", 0x51),
    ("NumpadMultiply", 0x43),
    ("NumpadSubtract", 0x4e),
    ("NumpadComma", 0x5f),
    ("IntlBackslash", 0x0a),
    ("IntlYen", 0x5d),
    ("IntlRo", 0x5e),
    ("Lang1", 0x68),
    ("Lang2", 0x66),
];

// IOKit defines the low bits that distinguish left and right modifiers.
// CGEvent::new_keyboard_event uses these in addition to the shared masks.
const MODIFIERS: &[(&str, u16, CGEventFlags, u64)] = &[
    ("ControlLeft", 0x3b, CGEventFlags::MaskControl, 0x0001),
    ("ShiftLeft", 0x38, CGEventFlags::MaskShift, 0x0002),
    ("ShiftRight", 0x3c, CGEventFlags::MaskShift, 0x0004),
    ("MetaLeft", 0x37, CGEventFlags::MaskCommand, 0x0008),
    ("MetaRight", 0x36, CGEventFlags::MaskCommand, 0x0010),
    ("AltLeft", 0x3a, CGEventFlags::MaskAlternate, 0x0020),
    ("AltRight", 0x3d, CGEventFlags::MaskAlternate, 0x0040),
    ("ControlRight", 0x3e, CGEventFlags::MaskControl, 0x2000),
    ("CapsLock", 0x39, CGEventFlags::MaskAlphaShift, 0),
    ("Fn", 0x3f, CGEventFlags::MaskSecondaryFn, 0),
];

fn key_mapping() -> impl Iterator<Item = (&'static str, u16)> {
    KEY_CODES
        .iter()
        .copied()
        .chain(MODIFIERS.iter().map(|&(name, code, _, _)| (name, code)))
}

pub fn key_code(key: &str) -> Result<u16, String> {
    key_mapping()
        .find_map(|(name, code)| (name == key).then_some(code))
        .ok_or_else(|| format!("unsupported macOS physical key {key:?}"))
}

pub fn key_name(code: u16) -> Option<&'static str> {
    key_mapping().find_map(|(name, key_code)| (key_code == code).then_some(name))
}

pub fn validate_key(key: &str) -> Result<(), String> {
    key_code(key).map(|_| ())
}

pub(super) fn managed_flags() -> CGEventFlags {
    CGEventFlags::from_bits_retain(
        MODIFIERS
            .iter()
            .fold(0, |bits, &(_, _, shared, side)| bits | shared.bits() | side),
    )
}

pub(super) fn modifier_flags(held: &[&str], transition: Option<(&str, bool)>) -> CGEventFlags {
    let mut bits = 0;
    for &(name, _, shared, side) in MODIFIERS {
        let down = match transition {
            Some((key, down)) if key == name => down,
            _ => held.contains(&name),
        };
        if down {
            bits |= shared.bits() | side;
        }
    }
    CGEventFlags::from_bits_retain(bits)
}

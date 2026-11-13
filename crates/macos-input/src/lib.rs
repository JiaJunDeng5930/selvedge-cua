//! Sends single input events through Quartz. The caller owns timing, input
//! state, focus checks, and the ordering of events across calls. Quartz posts
//! return no receipt from the receiving application.

#![deny(unsafe_code)]
#![cfg(target_os = "macos")]

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{CGEvent, CGEventFlags, CGPreflightPostEventAccess};

mod keys;
mod pointing;
mod unicode;
mod window_location;

pub use keys::{key_code, key_name, validate_key};
pub use pointing::{WindowRoute, mouse_button_in_window, move_pointer_in_window, scroll_in_window};

/// A point in the global macOS desktop coordinate system (logical points).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    fn native(self) -> Result<CGPoint, String> {
        // DEFENSE: Coordinates can come directly from a JavaScript callback.
        if !self.x.is_finite() || !self.y.is_finite() {
            return Err("pointer coordinates must be finite".into());
        }
        Ok(CGPoint::new(self.x, self.y))
    }
}

fn check_post_access() -> Result<(), String> {
    // DEFENSE: Quartz post functions return void when Accessibility access is
    // missing, so check the authorization before reporting a successful post.
    if !CGPreflightPostEventAccess() {
        return Err("macOS event posting requires Accessibility permission".into());
    }
    Ok(())
}

fn check_pid(pid: i32) -> Result<(), String> {
    // DEFENSE: Target PIDs can originate in script arguments. Zero and negative
    // numbers do not identify a concrete receiving process.
    if pid <= 0 {
        return Err("input target PID must be positive".into());
    }
    Ok(())
}

pub fn pointer_position() -> Result<Point, String> {
    let event = CGEvent::new(None).ok_or("could not read the mouse position")?;
    let point = CGEvent::location(Some(&event));
    Ok(Point {
        x: point.x,
        y: point.y,
    })
}

/// Posts a physical key event to the specified application. Includes the
/// other managed modifiers even when directed posts leave system-wide state
/// unchanged.
pub fn keyboard(pid: i32, key: &str, down: bool, modifiers: &[&str]) -> Result<(), String> {
    let code = key_code(key)?;
    check_pid(pid)?;
    check_post_access()?;
    let event =
        CGEvent::new_keyboard_event(None, code, down).ok_or("could not create keyboard event")?;
    let flags = (CGEvent::flags(Some(&event)) & !keys::managed_flags())
        | keys::modifier_flags(modifiers, Some((key, down)));
    CGEvent::set_flags(Some(&event), flags);
    CGEvent::post_to_pid(pid, Some(&event));
    Ok(())
}

/// Submits Unicode text to a process whose focused editor the caller checked.
/// KNOWN LIMITATION: Applications which translate only the virtual key code
/// and ignore Quartz's Unicode payload do not receive inserted text this way.
pub fn insert_text(pid: i32, text: &str) -> Result<(), String> {
    check_pid(pid)?;
    if text.is_empty() {
        return Ok(());
    }
    check_post_access()?;
    // Unicode input has no physical key. Use a virtual code outside the
    // hardware range so a receiver cannot confuse it with a normal ANSI key.
    const NO_PHYSICAL_KEY: u16 = u16::MAX;
    let down = CGEvent::new_keyboard_event(None, NO_PHYSICAL_KEY, true)
        .ok_or("could not create text input event")?;
    let up = CGEvent::new_keyboard_event(None, NO_PHYSICAL_KEY, false)
        .ok_or("could not create text input release event")?;
    unicode::set_text(&down, text);
    for event in [&down, &up] {
        CGEvent::set_flags(Some(event), CGEventFlags::empty());
        CGEvent::post_to_pid(pid, Some(event));
    }
    Ok(())
}

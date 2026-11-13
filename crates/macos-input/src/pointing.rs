use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventType, CGMouseButton, CGScrollEventUnit,
};

use crate::{Point, check_pid, check_post_access, keys, window_location};

/// A live window's process, window number, and outer frame origin in desktop points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowRoute {
    pub pid: i32,
    pub window_id: u32,
    pub x: f64,
    pub y: f64,
}

/// Sends a window-directed move or a drag associated with the supplied press group.
pub fn move_pointer_in_window(
    route: WindowRoute,
    point: Point,
    held_button: Option<u8>,
    group: i64,
    modifiers: &[&str],
) -> Result<(), String> {
    let (event_type, button) = match held_button {
        None => (CGEventType::MouseMoved, CGMouseButton::Left),
        Some(raw) => {
            let button = MouseButton::new(raw)?;
            (button.event_types().drag, button.native())
        }
    };
    let flags = keys::modifier_flags(modifiers, None) | CGEventFlags::MaskNonCoalesced;
    route.post_mouse(point, event_type, button, group, flags)
}

/// Sends a mouse button transition to the destination. Its press, drag, and
/// release use the same group.
pub fn mouse_button_in_window(
    route: WindowRoute,
    point: Point,
    button: u8,
    down: bool,
    group: i64,
    modifiers: &[&str],
) -> Result<(), String> {
    let button = MouseButton::new(button)?;
    let types = button.event_types();
    let event_type = if down { types.down } else { types.up };
    route.post_mouse(
        point,
        event_type,
        button.native(),
        group,
        keys::modifier_flags(modifiers, None),
    )
}

/// Sends pixel scrolling to a window location. Positive deltas scroll right and down.
pub fn scroll_in_window(
    route: WindowRoute,
    point: Point,
    dx: i32,
    dy: i32,
    modifiers: &[&str],
) -> Result<(), String> {
    let point = point.native()?;
    // DEFENSE: The signed minimum is a valid i32 script input, but has no
    // opposite of the same type for Quartz's reversed wheel sign.
    let wheel_x = dx
        .checked_neg()
        .ok_or("horizontal scroll delta is out of range")?;
    let wheel_y = dy
        .checked_neg()
        .ok_or("vertical scroll delta is out of range")?;
    let event =
        CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Pixel, 2, wheel_y, wheel_x, 0)
            .ok_or("could not create scroll event")?;
    CGEvent::set_location(Some(&event), point);
    CGEvent::set_flags(Some(&event), keys::modifier_flags(modifiers, None));
    route.post(&event, point, 0)
}

impl WindowRoute {
    fn post_mouse(
        self,
        point: Point,
        event_type: CGEventType,
        button: CGMouseButton,
        group: i64,
        flags: CGEventFlags,
    ) -> Result<(), String> {
        let point = point.native()?;
        let event = CGEvent::new_mouse_event(None, event_type, point, button)
            .ok_or("could not create mouse event")?;
        if event_type != CGEventType::MouseMoved {
            // Quartz creates mouse-up with click count zero. AppKit needs
            // the same click count and event number across the stroke.
            CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventClickState, 1);
            CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventNumber, group);
        }
        CGEvent::set_flags(Some(&event), flags);
        self.post(&event, point, group)
    }

    // KNOWN LIMITATION: Window-directed events depend on Quartz's window
    // routing fields and the app's willingness to handle background events.
    fn post(self, event: &CGEvent, point: CGPoint, group: i64) -> Result<(), String> {
        check_pid(self.pid)?;
        // DEFENSE: CoreGraphics reserves window zero for a null window.
        if self.window_id == 0 {
            return Err("background input requires a concrete window ID".into());
        }
        let local = Point {
            x: point.x - self.x,
            y: point.y - self.y,
        }
        .native()?;
        check_post_access()?;
        window_location::set(event, local)?;
        let window = i64::from(self.window_id);
        // AppKit resolves the window association from field 51. The public
        // fields identify the two windows normally inferred from the cursor.
        // Field 58 follows the press group so a drag stays with that press.
        for (field, value) in [
            (CGEventField::MouseEventWindowUnderMousePointer, window),
            (
                CGEventField::MouseEventWindowUnderMousePointerThatCanHandleThisEvent,
                window,
            ),
            (CGEventField::EventTargetUnixProcessID, i64::from(self.pid)),
            (CGEventField(51), window),
            (CGEventField(58), group),
            (CGEventField::MouseEventSubtype, 3),
        ] {
            CGEvent::set_integer_value_field(Some(event), field, value);
        }
        CGEvent::post_to_pid(self.pid, Some(event));
        Ok(())
    }
}

struct MouseEventTypes {
    down: CGEventType,
    up: CGEventType,
    drag: CGEventType,
}

struct MouseButton(u8);

impl MouseButton {
    fn new(raw: u8) -> Result<Self, String> {
        // DEFENSE: Script-supplied button numbers can exceed Quartz's
        // documented 32-button range.
        if raw >= 32 {
            return Err(format!("unsupported mouse button {raw}; expected 0–31"));
        }
        Ok(Self(raw))
    }

    fn native(&self) -> CGMouseButton {
        CGMouseButton(u32::from(self.0))
    }

    fn event_types(&self) -> MouseEventTypes {
        match self.0 {
            0 => MouseEventTypes {
                down: CGEventType::LeftMouseDown,
                up: CGEventType::LeftMouseUp,
                drag: CGEventType::LeftMouseDragged,
            },
            1 => MouseEventTypes {
                down: CGEventType::RightMouseDown,
                up: CGEventType::RightMouseUp,
                drag: CGEventType::RightMouseDragged,
            },
            _ => MouseEventTypes {
                down: CGEventType::OtherMouseDown,
                up: CGEventType::OtherMouseUp,
                drag: CGEventType::OtherMouseDragged,
            },
        }
    }
}

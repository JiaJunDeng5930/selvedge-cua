//! The AppKit window-local location cannot be supplied by CGEventSetLocation,
//! which only writes the global point. Keep the optional Quartz SPI here.

#![allow(unsafe_code)]

use std::mem;
use std::sync::OnceLock;

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::CGEvent;

type SetWindowLocation = unsafe extern "C" fn(*const CGEvent, f64, f64);

pub(super) fn set(event: &CGEvent, point: CGPoint) -> Result<(), String> {
    static SET_LOCATION: OnceLock<Option<SetWindowLocation>> = OnceLock::new();
    let setter = SET_LOCATION.get_or_init(|| {
        // SAFETY: RTLD_DEFAULT and the NUL-terminated name meet dlsym's
        // preconditions. CoreGraphics is linked for the process lifetime.
        let symbol =
            unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"CGEventSetWindowLocation".as_ptr()) };
        // DEFENSE: This SPI may be absent on another version of macOS.
        if symbol.is_null() {
            return None;
        }
        // SAFETY: The named CoreGraphics SPI takes a CGEventRef and two
        // CGFloat values (64-bit on supported macOS targets).
        Some(unsafe { mem::transmute::<*mut libc::c_void, SetWindowLocation>(symbol) })
    });
    let setter = setter.ok_or("background window input is unavailable on this macOS version")?;
    // SAFETY: The event is live throughout this synchronous call and the
    // CGPoint components are passed by value.
    unsafe { setter(event, point.x, point.y) };
    Ok(())
}

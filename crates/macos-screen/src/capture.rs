use serde::Deserialize;
use serde_json::Value;

use crate::{DesktopRect, Frame};

#[derive(Clone, Copy, Debug)]
pub enum CaptureTarget {
    MainDisplay,
    Display(u32),
    Window(u32),
    Region(DesktopRect),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
enum Target {
    DisplayId(u32),
    WindowId(u32),
    Region(Region),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Region {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl CaptureTarget {
    pub fn is_window(self) -> bool {
        matches!(self, Self::Window(_))
    }

    pub fn from_options(options: &Value) -> Result<Self, String> {
        let options = options
            .as_object()
            .ok_or("Screen capture options must be an object")?;
        let Some(target) = options.get("target") else {
            return Ok(Self::MainDisplay);
        };
        let target: Target = serde_json::from_value(target.clone())
            .map_err(|error| format!("Invalid screen capture target: {error}"))?;
        match target {
            Target::DisplayId(id) => Ok(Self::Display(id)),
            Target::WindowId(id) => Ok(Self::Window(id)),
            Target::Region(region) => {
                let rect = DesktopRect {
                    x: region.x,
                    y: region.y,
                    width: region.width,
                    height: region.height,
                };
                rect.validate()?;
                Ok(Self::Region(rect))
            }
        }
    }

    pub fn capture(self) -> Result<Frame, String> {
        #[cfg(target_os = "macos")]
        {
            native::capture(self)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err("Screen capture is available on macOS".into())
        }
    }
}

pub fn capture(options: &Value) -> Result<Frame, String> {
    CaptureTarget::from_options(options)?.capture()
}

/// Allow macOS to deliver main-thread callbacks while capture runs on a worker.
pub fn pump_main_run_loop() {
    #[cfg(target_os = "macos")]
    native::pump_main_run_loop();
}

#[cfg(target_os = "macos")]
mod native {
    use std::ffi::{CStr, c_char};
    use std::mem::MaybeUninit;
    use std::time::{Duration, Instant};

    use image::RgbaImage;

    use super::{CaptureTarget, DesktopRect, Frame};

    #[derive(Clone, Copy, Default)]
    #[repr(C)]
    struct NativeRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    #[repr(C)]
    struct NativeFrame {
        desktop: NativeRect,
        width: u32,
        height: u32,
        callback_age_ns: u64,
        pixels: *mut u8,
    }

    unsafe extern "C" {
        fn selvedge_capture_screen(
            kind: u32,
            identifier: u32,
            region: NativeRect,
            output: *mut NativeFrame,
            error: *mut c_char,
            capacity: usize,
        ) -> bool;
        fn selvedge_free_screen_pixels(pixels: *mut u8);
        fn selvedge_pump_main_run_loop();
    }

    pub(super) fn pump_main_run_loop() {
        // SAFETY: The shim services the current main thread run loop for a bounded interval.
        unsafe { selvedge_pump_main_run_loop() };
    }

    pub(super) fn capture(target: CaptureTarget) -> Result<Frame, String> {
        let (kind, identifier, region) = match target {
            CaptureTarget::MainDisplay => (0, 0, NativeRect::default()),
            CaptureTarget::Display(id) => (1, id, NativeRect::default()),
            CaptureTarget::Window(id) => (2, id, NativeRect::default()),
            CaptureTarget::Region(rect) => {
                rect.validate()?;
                let DesktopRect {
                    x,
                    y,
                    width,
                    height,
                } = rect;
                (
                    3,
                    0,
                    NativeRect {
                        x,
                        y,
                        width,
                        height,
                    },
                )
            }
        };
        let mut output = MaybeUninit::uninit();
        let mut error = [0; 1024];
        // The native shim initializes output only when it succeeds. Every failure
        // writes a NUL-terminated diagnostic into the caller's buffer.
        let success = unsafe {
            selvedge_capture_screen(
                kind,
                identifier,
                region,
                output.as_mut_ptr(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if !success {
            return Err(unsafe { CStr::from_ptr(error.as_ptr()) }
                .to_string_lossy()
                .into_owned());
        }
        let returned_at = Instant::now();
        // The shim allocated exactly width * height * 4 bytes. It retains no alias.
        let output = unsafe { output.assume_init() };
        let length = output.width as usize * output.height as usize * 4;
        let pixels = unsafe { std::slice::from_raw_parts(output.pixels, length) }.to_vec();
        unsafe { selvedge_free_screen_pixels(output.pixels) };
        let pixels = RgbaImage::from_raw(output.width, output.height, pixels)
            .expect("native RGBA has exactly four bytes per pixel");
        let desktop = DesktopRect {
            x: output.desktop.x,
            y: output.desktop.y,
            width: output.desktop.width,
            height: output.desktop.height,
        };
        let captured_at = returned_at - Duration::from_nanos(output.callback_age_ns);
        Frame::new(pixels, desktop, captured_at)
    }
}

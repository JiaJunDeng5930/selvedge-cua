use std::time::Instant;

use image::RgbaImage;
use serde::Serialize;

mod capture;
mod observation;
mod scroll;

pub use capture::{CaptureTarget, capture, pump_main_run_loop};
pub use observation::{Observation, TimedFrame, summarize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct DesktopRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub struct Frame {
    pixels: RgbaImage,
    desktop: DesktopRect,
    captured_at: Instant,
}

impl Frame {
    pub fn new(
        pixels: RgbaImage,
        desktop: DesktopRect,
        captured_at: Instant,
    ) -> Result<Self, String> {
        desktop.validate()?;
        if pixels.width() == 0 || pixels.height() == 0 {
            return Err("A frame must have pixels".into());
        }
        Ok(Self {
            pixels,
            desktop,
            captured_at,
        })
    }

    /// Monotonic time at which macOS delivered the captured image.
    pub fn captured_at(&self) -> Instant {
        self.captured_at
    }

    pub fn desktop_rect(&self) -> DesktopRect {
        self.desktop
    }

    pub fn dimensions(&self) -> (u32, u32) {
        self.pixels.dimensions()
    }

    fn same_viewport(&self, other: &Self) -> bool {
        self.desktop == other.desktop && self.dimensions() == other.dimensions()
    }

    fn desktop_area(&self, area: PixelRect) -> DesktopRect {
        let (width, height) = self.dimensions();
        let scale_x = self.desktop.width / f64::from(width);
        let scale_y = self.desktop.height / f64::from(height);
        DesktopRect {
            x: self.desktop.x + f64::from(area.x) * scale_x,
            y: self.desktop.y + f64::from(area.y) * scale_y,
            width: f64::from(area.width) * scale_x,
            height: f64::from(area.height) * scale_y,
        }
    }

    fn full_area(&self) -> PixelRect {
        let (width, height) = self.dimensions();
        PixelRect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }
}

impl DesktopRect {
    fn validate(self) -> Result<(), String> {
        if ![
            self.x,
            self.y,
            self.width,
            self.height,
            self.x + self.width,
            self.y + self.height,
        ]
        .into_iter()
        .all(f64::is_finite)
            || self.width <= 0.0
            || self.height <= 0.0
        {
            return Err(
                "Desktop rectangle requires finite coordinates and positive dimensions".into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct PixelRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl PixelRect {
    fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

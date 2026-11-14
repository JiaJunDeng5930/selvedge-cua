//! Window identity and desktop geometry from the public Core Graphics window list.

use std::mem::{MaybeUninit, size_of};

use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGRect,
};
use objc2_core_graphics::{
    CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo, CGWindowListOption,
    kCGNormalWindowLevel, kCGWindowBounds, kCGWindowIsOnscreen, kCGWindowLayer, kCGWindowName,
    kCGWindowNumber, kCGWindowOwnerPID,
};
use serde_json::{Map, Value};

type WindowDescription = CFDictionary<CFString, CFType>;
type DesktopBounds = (f64, f64, f64, f64);

#[derive(Debug)]
pub enum InputWindowError {
    Unavailable(u32),
    OwnerChanged(u32),
    GeometryChanged(u32),
}

impl InputWindowError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "windowUnavailable",
            Self::OwnerChanged(_) => "windowOwnerChanged",
            Self::GeometryChanged(_) => "windowGeometryChanged",
        }
    }
}

impl std::fmt::Display for InputWindowError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(id) => {
                write!(formatter, "window {id} is not a normal on-screen window")
            }
            Self::OwnerChanged(id) => write!(formatter, "window {id} changed owner"),
            Self::GeometryChanged(id) => write!(formatter, "window {id} moved or resized"),
        }
    }
}

/// A normal, on-screen window and the process generation that owned it when
/// resolved. The bounds use the desktop coordinates used by Core Graphics.
#[derive(Clone, Copy, Debug)]
pub struct InputWindow {
    id: u32,
    process: InputProcess,
    bounds: DesktopBounds,
}

impl InputWindow {
    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn pid(&self) -> i32 {
        self.process.pid()
    }

    pub fn process(&self) -> InputProcess {
        self.process
    }

    pub fn bounds(&self) -> DesktopBounds {
        self.bounds
    }

    /// Recheck the original owner and frame before using a prepared input
    /// plan. The plan's coordinates were validated against the original frame.
    pub fn revalidate(&self) -> Result<(), InputWindowError> {
        let current = input_window(self.id)?;
        // DEFENSE: Window bounds, visibility, and owner can change while
        // a script prepares the input plan.
        if current.process != self.process {
            return Err(InputWindowError::OwnerChanged(self.id));
        }
        if current.bounds != self.bounds {
            return Err(InputWindowError::GeometryChanged(self.id));
        }
        Ok(())
    }

    /// Check the original process generation during held-input cleanup. A
    /// moved or closed window does not imply that its process has exited.
    pub fn same_process(&self) -> bool {
        self.process.is_live()
    }
}

/// Resolve a window ID from the same desktop session as the tool. This query
/// reads window metadata without activating its owner.
pub fn input_window(id: u32) -> Result<InputWindow, InputWindowError> {
    let options =
        CGWindowListOption::OptionIncludingWindow | CGWindowListOption::ExcludeDesktopElements;
    let window = window_list(options, id)
        .and_then(|windows| {
            windows.iter().find_map(|window| {
                let geometry = WindowGeometry::from_native(&window)?;
                (geometry.id == id && geometry.on_screen).then_some(geometry)
            })
        })
        .ok_or(InputWindowError::Unavailable(id))?;
    Ok(InputWindow {
        id,
        process: InputProcess::read(window.pid).ok_or(InputWindowError::Unavailable(id))?,
        bounds: window.bounds,
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WindowInfo {
    pub window_id: u32,
    pub pid: i32,
    title: Option<String>,
    bounds: DesktopBounds,
    pub on_screen: bool,
}

struct WindowGeometry {
    id: u32,
    pid: i32,
    bounds: DesktopBounds,
    on_screen: bool,
}

impl WindowGeometry {
    fn from_native(window: &WindowDescription) -> Option<Self> {
        let pid = number(window, keys::owner())?.as_i32()?;
        let bounds = bounds(window)?;
        if pid <= 0
            || bounds.2 <= 0.0
            || bounds.3 <= 0.0
            || number(window, keys::layer())?.as_i32()? != kCGNormalWindowLevel
        {
            return None;
        }
        Some(Self {
            id: u32::try_from(number(window, keys::number())?.as_i64()?).ok()?,
            pid,
            bounds,
            on_screen: window
                .get(keys::onscreen())
                .and_then(|value| value.downcast_ref::<CFBoolean>().map(|flag| flag.value()))
                .unwrap_or(false),
        })
    }
}

impl WindowInfo {
    fn from_native(window: &WindowDescription) -> Option<Self> {
        let geometry = WindowGeometry::from_native(window)?;
        Some(Self {
            window_id: geometry.id,
            pid: geometry.pid,
            bounds: geometry.bounds,
            on_screen: geometry.on_screen,
            title: window
                .get(keys::title())
                .and_then(|value| value.downcast_ref::<CFString>().map(ToString::to_string)),
        })
    }

    pub fn lookup(id: u32) -> Option<Self> {
        let options =
            CGWindowListOption::OptionIncludingWindow | CGWindowListOption::ExcludeDesktopElements;
        window_list(options, id)?
            .iter()
            .filter_map(|window| Self::from_native(&window))
            .find(|window| window.window_id == id)
    }

    pub fn list() -> Option<Vec<Self>> {
        Some(
            window_list(CGWindowListOption::ExcludeDesktopElements, 0)?
                .iter()
                .filter_map(|window| Self::from_native(&window))
                .collect(),
        )
    }

    fn matches(&self, pid: i32, geometry: &Geometry<'_>) -> bool {
        // The two APIs can round a fractional point boundary differently.
        self.pid == pid
            && [self.bounds.0, self.bounds.1, self.bounds.2, self.bounds.3]
                .into_iter()
                .zip(geometry.bounds)
                .all(|(a, b)| (a - b).abs() < 1.0)
    }
}

#[derive(serde::Serialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum WindowInput {
    Ready { window_id: u32 },
    Unmatched,
    Ambiguous,
    Unavailable { window_id: u32 },
}

impl WindowInput {
    fn from_match(window: &WindowInfo) -> Self {
        let window_id = window.window_id;
        if window.on_screen {
            Self::Ready { window_id }
        } else {
            Self::Unavailable { window_id }
        }
    }

    pub fn id(&self) -> Option<u32> {
        match *self {
            Self::Ready { window_id } | Self::Unavailable { window_id } => Some(window_id),
            Self::Unmatched | Self::Ambiguous => None,
        }
    }
}

struct Geometry<'a> {
    bounds: [f64; 4],
    title: Option<&'a str>,
}

impl<'a> Geometry<'a> {
    fn from_ax(window: &'a Map<String, Value>) -> Option<Self> {
        let position = window.get("position")?;
        let size = window.get("size")?;
        Some(Self {
            bounds: [
                position.get("x")?.as_f64()?,
                position.get("y")?.as_f64()?,
                size.get("width")?.as_f64()?,
                size.get("height")?.as_f64()?,
            ],
            title: window.get("title").and_then(Value::as_str),
        })
    }
}

pub(crate) fn find(windows: &[WindowInfo], pid: i32, window: &Map<String, Value>) -> WindowInput {
    use selvedge_rules::accessibility::{WindowMatch, choose_window};

    let Some(geometry) = Geometry::from_ax(window) else {
        return WindowInput::Unmatched;
    };
    let candidates: Vec<_> = windows
        .iter()
        .filter(|window| window.matches(pid, &geometry))
        .collect();
    let named: Vec<_> = candidates
        .iter()
        .filter(|window| geometry.title.is_some() && geometry.title == window.title.as_deref())
        .collect();
    match choose_window(candidates.len(), named.len()) {
        WindowMatch::Unmatched => WindowInput::Unmatched,
        WindowMatch::Ambiguous => WindowInput::Ambiguous,
        WindowMatch::Geometry => WindowInput::from_match(candidates[0]),
        WindowMatch::Title => WindowInput::from_match(named[0]),
    }
}

pub(crate) fn may_match(
    windows: &[WindowInfo],
    pid: i32,
    id: u32,
    window: &Map<String, Value>,
) -> bool {
    let Some(geometry) = Geometry::from_ax(window) else {
        return false;
    };
    windows
        .iter()
        .any(|window| window.window_id == id && window.matches(pid, &geometry))
}

/// The owner PID and the kernel time at which that process started.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputProcess {
    pid: i32,
    start_seconds: u64,
    start_microseconds: u64,
}

/// Resolve a live process generation. The returned handle can distinguish
/// the original application from a later occupant of the same PID.
pub fn input_process(pid: i32) -> Result<InputProcess, String> {
    InputProcess::read(pid).ok_or_else(|| format!("could not verify input process with PID {pid}"))
}

impl InputProcess {
    pub fn pid(&self) -> i32 {
        self.pid
    }

    pub fn is_live(&self) -> bool {
        Self::read(self.pid) == Some(*self)
    }

    fn read(pid: i32) -> Option<Self> {
        // DEFENSE: A script can supply arbitrary PIDs. proc_pidinfo accepts
        // PID zero as a special case rather than a target process.
        if pid <= 0 {
            return None;
        }
        // LaunchServices omits launch dates for directly launched applications.
        // The kernel start time also identifies these processes.
        let mut process = MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: The flavor writes at most size bytes into the provided storage.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                process.as_mut_ptr().cast(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // SAFETY: proc_pidinfo reported a full proc_bsdinfo record.
        let process = unsafe { process.assume_init() };
        // DEFENSE: The kernel can retain a PID for an exited process
        // until its parent reaps it. That process cannot receive input.
        if process.pbi_status == libc::SZOMB {
            return None;
        }
        Some(Self {
            pid,
            start_seconds: process.pbi_start_tvsec,
            start_microseconds: process.pbi_start_tvusec,
        })
    }
}

fn window_list(
    options: CGWindowListOption,
    id: u32,
) -> Option<CFRetained<CFArray<WindowDescription>>> {
    let windows = CGWindowListCopyWindowInfo(options, id)?;
    // SAFETY: Core Graphics returns an immutable array of dictionaries whose
    // keys are CFStrings and whose values are Core Foundation objects.
    Some(unsafe { CFRetained::cast_unchecked(windows) })
}

fn number(window: &WindowDescription, key: &CFString) -> Option<CFRetained<CFNumber>> {
    window.get(key)?.downcast::<CFNumber>().ok()
}

fn bounds(window: &WindowDescription) -> Option<DesktopBounds> {
    let bounds = window.get(keys::bounds())?;
    let bounds = bounds.downcast_ref::<CFDictionary>()?;
    let mut rect = CGRect::default();
    // SAFETY: The bounds dictionary is from Core Graphics and rect is writable.
    if !unsafe { CGRectMakeWithDictionaryRepresentation(Some(bounds), &mut rect) } {
        return None;
    }
    Some((
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
    ))
}

mod keys {
    use super::*;

    pub fn owner() -> &'static CFString {
        // SAFETY: These immutable CFString constants are supplied by Core Graphics.
        unsafe { kCGWindowOwnerPID }
    }

    pub fn bounds() -> &'static CFString {
        unsafe { kCGWindowBounds }
    }

    pub fn number() -> &'static CFString {
        unsafe { kCGWindowNumber }
    }

    pub fn title() -> &'static CFString {
        unsafe { kCGWindowName }
    }

    pub fn layer() -> &'static CFString {
        unsafe { kCGWindowLayer }
    }

    pub fn onscreen() -> &'static CFString {
        unsafe { kCGWindowIsOnscreen }
    }
}

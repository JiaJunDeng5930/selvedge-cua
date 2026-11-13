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
    pub fn revalidate(&self) -> Result<(), String> {
        let current = input_window(self.id)?;
        // DEFENSE: Window bounds, visibility, and owner can change while
        // a script prepares the input plan.
        if current.process != self.process {
            return Err(format!("window {} changed owner", self.id));
        }
        if current.bounds != self.bounds {
            return Err(format!("window {} moved or resized", self.id));
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
pub fn input_window(id: u32) -> Result<InputWindow, String> {
    let options =
        CGWindowListOption::OptionIncludingWindow | CGWindowListOption::ExcludeDesktopElements;
    let (pid, bounds) = window_list(options, id)
        .and_then(|windows| {
            windows.iter().find_map(|window| {
                if number(&window, keys::number())?.as_i64()? != i64::from(id)
                    || number(&window, keys::layer())?.as_i32()? != kCGNormalWindowLevel
                    || !window
                        .get(keys::onscreen())?
                        .downcast_ref::<CFBoolean>()?
                        .as_bool()
                {
                    return None;
                }
                let pid = number(&window, keys::owner())?.as_i32()?;
                let bounds = bounds(&window)?;
                (pid > 0 && bounds.2 > 0.0 && bounds.3 > 0.0).then_some((pid, bounds))
            })
        })
        .ok_or_else(|| format!("window {id} is not a normal on-screen window"))?;
    let process = input_process(pid)?;
    Ok(InputWindow {
        id,
        process,
        bounds,
    })
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

pub fn find(pid: i32, window: &Map<String, Value>) -> Option<u32> {
    let position = window.get("position")?;
    let size = window.get("size")?;
    let geometry = [
        position.get("x")?.as_f64()?,
        position.get("y")?.as_f64()?,
        size.get("width")?.as_f64()?,
        size.get("height")?.as_f64()?,
    ];
    let windows = window_list(CGWindowListOption::ExcludeDesktopElements, 0)?;
    let title = window.get("title").and_then(Value::as_str);
    let matches: Vec<_> = windows
        .iter()
        .filter_map(|window| candidate(&window, pid, geometry, title))
        .collect();
    match matches.as_slice() {
        [(id, _)] => Some(*id),
        _ => {
            // KNOWN LIMITATION: Public metadata may give multiple windows
            // the same frame and title, or omit titles due to capture permissions.
            let mut title_matches = matches
                .into_iter()
                .filter(|(_, title_matches)| *title_matches);
            let (id, _) = title_matches.next()?;
            title_matches.next().is_none().then_some(id)
        }
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

fn candidate(
    window: &WindowDescription,
    pid: i32,
    geometry: [f64; 4],
    title: Option<&str>,
) -> Option<(u32, bool)> {
    if number(window, keys::owner())?.as_i32()? != pid {
        return None;
    }
    let (x, y, width, height) = bounds(window)?;
    // The two APIs can round a fractional point boundary differently.
    if [x, y, width, height]
        .into_iter()
        .zip(geometry)
        .any(|(a, b)| (a - b).abs() >= 1.0)
    {
        return None;
    }
    let id = u32::try_from(number(window, keys::number())?.as_i64()?).ok()?;
    let title_matches = title.is_some_and(|title| {
        window.get(keys::title()).is_some_and(|value| {
            value
                .downcast_ref::<CFString>()
                .is_some_and(|name| name.to_string() == title)
        })
    });
    Some((id, title_matches))
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

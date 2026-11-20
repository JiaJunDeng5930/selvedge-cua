//! Ownership of the native tap and access to its published PCM.
#![allow(unsafe_code)]

use std::{
    ffi::{CStr, c_char, c_void},
    num::NonZeroI32,
    ptr::NonNull,
};

unsafe extern "C" {
    fn selvedge_audio_open(
        pid: i32,
        duration_ms: u32,
        sample_rate: *mut u32,
        error: *mut c_char,
        error_size: usize,
    ) -> *mut c_void;
    fn selvedge_audio_read(
        capture: *const c_void,
        offset: usize,
        output: *mut f32,
        capacity: usize,
    ) -> isize;
    fn selvedge_audio_stop(capture: *mut c_void) -> i32;
    fn selvedge_audio_close(capture: *mut c_void);
    fn selvedge_audio_resample(
        samples: *const f32,
        length: u32,
        sample_rate: u32,
        output: *mut f32,
        capacity: u32,
    ) -> u32;
}

pub(crate) struct Capture {
    native: NonNull<c_void>,
    sample_rate: u32,
    offset: usize,
}

impl Capture {
    pub(crate) fn open(pid: NonZeroI32, duration_ms: u32) -> Result<Self, String> {
        let mut sample_rate = 0;
        let mut error = [0; 256];
        // SAFETY: Both output pointers address initialized, writable storage for this call.
        let native = unsafe {
            selvedge_audio_open(
                pid.get(),
                duration_ms,
                &mut sample_rate,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        let native = NonNull::new(native).ok_or_else(|| {
            CStr::from_bytes_until_nul(&error.map(|byte| byte as u8)).map_or_else(
                |_| "App audio capture could not be started".into(),
                |message| message.to_string_lossy().into_owned(),
            )
        })?;
        Ok(Self {
            native,
            sample_rate,
            offset: 0,
        })
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub(crate) fn read(&mut self, output: &mut [f32]) -> Result<usize, String> {
        // SAFETY: This owner keeps the tap alive. Native code copies only its
        // published samples and writes at most output.len() frames.
        let count = unsafe {
            selvedge_audio_read(
                self.native.as_ptr(),
                self.offset,
                output.as_mut_ptr(),
                output.len(),
            )
        };
        let count = usize::try_from(count).map_err(|_| "Core Audio tap changed its PCM format")?;
        self.offset += count;
        Ok(count)
    }

    pub(crate) fn stop(&mut self) -> Result<(), String> {
        // SAFETY: This is the live handle owned by self; stopping it preserves its PCM.
        let status = unsafe { selvedge_audio_stop(self.native.as_ptr()) };
        if status != 0 {
            return Err(format!(
                "Core Audio could not stop app audio capture (OSStatus {status})"
            ));
        }
        Ok(())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: Only this owner closes its native handle, after all its users return.
        unsafe { selvedge_audio_close(self.native.as_ptr()) };
    }
}

pub(crate) fn resample(samples: &[f32], sample_rate: u32) -> Result<Vec<f32>, String> {
    if sample_rate == 16_000 {
        return Ok(samples.to_vec());
    }
    let capacity = (samples.len() as u64 * 16_000).div_ceil(sample_rate.into()) + 256;
    let capacity = u32::try_from(capacity).map_err(|_| "Audio chunk is too large to resample")?;
    let length =
        u32::try_from(samples.len()).map_err(|_| "Audio chunk is too large to resample")?;
    let mut output = vec![0.0; capacity as usize];
    // SAFETY: The disjoint slices are valid for the supplied frame counts.
    let frames = unsafe {
        selvedge_audio_resample(
            samples.as_ptr(),
            length,
            sample_rate,
            output.as_mut_ptr(),
            capacity,
        )
    };
    if frames == 0 {
        return Err("Could not convert app audio to 16 kHz PCM".into());
    }
    output.truncate(frames as usize);
    Ok(output)
}

//! The sampled offsets used to call the JavaScript trajectory come from the
//! extracted `sample_times` plan. Nanoseconds are the host clock's unit.
use crate::{
    convert::{items, n, number},
    corelib::init::datatypes::Option_1,
    schedule::sample_times,
};

pub fn pointer_offsets(duration_ms: f64, interval_ms: f64) -> Result<Vec<f64>, String> {
    // DEFENSE: JavaScript may supply negative, non-finite or unbounded times.
    if !(0.0..=30_000.0).contains(&duration_ms) || !(4.0..=30_000.0).contains(&interval_ms) {
        return Err("Pointer duration must be 0–30000 ms and interval 4–30000 ms".into());
    }

    match sample_times(
        n((duration_ms * 1_000_000.0).round() as u64),
        n((interval_ms * 1_000_000.0).round() as u64),
    ) {
        Option_1::Some(samples) => Ok(items(samples)
            .map(|offset| number(&offset) as f64 / 1_000_000.0)
            .collect()),
        Option_1::None => Err("Pointer trajectory exceeds 1024 samples".into()),
    }
}

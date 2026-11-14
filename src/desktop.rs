//! The desktop owns the session clock and the native adapters. A screenshot
//! observer can wait independently of the input dispatcher so a script can
//! begin input after the first captured frame and while later frames arrive.
use crate::{
    input::Input,
    locked_use::{DesktopPermission, LockedUse},
};
use selvedge_macos_screen::{self as screen, TimedFrame};
use selvedge_script_server::{Operation, OperationError, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{num::NonZeroU32, path::PathBuf, time::Duration};
use tokio::{
    sync::{Mutex, oneshot},
    task::spawn_blocking,
    time::{Instant, sleep_until},
};

use tokio_util::sync::CancellationToken;

const MAX_FRAMES: u32 = 24;

pub struct Desktop {
    clock: Instant,
    input: Input,
    locked_use: LockedUse,
    capture: Mutex<()>,
}

impl Desktop {
    pub fn new(helper: Option<PathBuf>) -> Self {
        Self {
            clock: Instant::now(),
            input: Input::new(helper.is_some()),
            locked_use: LockedUse::new(helper),
            capture: Mutex::new(()),
        }
    }

    async fn observe(
        &self,
        options: Value,
        started: Option<oneshot::Sender<Result<Value, String>>>,
        cancel: CancellationToken,
        permission: &DesktopPermission,
    ) -> Result<Response, String> {
        let mut started = started;
        let result = self
            .observe_frames(options, &mut started, cancel, permission)
            .await;
        if let (Some(started), Err(error)) = (started, &result) {
            let _ = started.send(Err(error.clone()));
        }
        result
    }

    async fn observe_frames(
        &self,
        options: Value,
        started: &mut Option<oneshot::Sender<Result<Value, String>>>,
        cancel: CancellationToken,
        permission: &DesktopPermission,
    ) -> Result<Response, String> {
        let observation: Observation = options_as(options.clone())?;
        let duration = observation.duration()?;
        let interval = observation.interval(duration)?;
        let budget = observation.budget()?;
        let target = screen::CaptureTarget::from_options(&options)?;
        if permission.protected() && !target.is_window() {
            return Err("Locked Use screenshots require a windowId".into());
        }

        let _capture = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("Screen observation was cancelled".into()),
            lock = self.capture.lock() => lock,
        };
        let mut frames = Vec::new();
        let mut capture_at = Instant::now();
        let mut end = None;
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err("Screen observation was cancelled".into()),
                _ = sleep_until(capture_at) => {}
            }
            let before = if observation.preserve_input_boundaries {
                self.input.observation_epoch()
            } else {
                Some(0)
            };
            permission.check().await?;
            if cancel.is_cancelled() {
                return Err("Screen observation was cancelled".into());
            }
            let request = target;
            let frame = blocking(move || request.capture()).await?;
            let input_epoch = if observation.preserve_input_boundaries
                && before != self.input.observation_epoch()
            {
                None
            } else {
                before
            };
            let at = Instant::from_std(frame.captured_at());
            let at_ms = at.duration_since(self.clock).as_secs_f64() * 1000.0;
            frames.push(TimedFrame {
                frame,
                at_ms,
                input_epoch,
            });

            if let Some(started) = started.take() {
                let _ = started.send(Ok(json!({"atMs": at_ms})));
            }
            let end = *end.get_or_insert(at + duration);
            if at >= end || frames.len() >= MAX_FRAMES as usize {
                break;
            }
            capture_at = (at + interval).min(end);
        }
        let observation = blocking(move || {
            let frames = frames
                .into_iter()
                .map(|mut sample| {
                    if let Some(maximum) = observation.max_dimension {
                        sample.frame = sample.frame.limit_resolution(maximum);
                    }
                    sample
                })
                .collect();
            screen::summarize(frames, budget)
        })
        .await?;
        Ok(Response {
            value: observation.metadata,
            images: observation.images,
        })
    }
}

impl selvedge_script_server::Desktop for Desktop {
    fn pointer_offsets(&self, duration_ms: f64, interval_ms: f64) -> Result<Vec<f64>, String> {
        selvedge_rules::trajectory::pointer_offsets(duration_ms, interval_ms)
    }

    async fn call(
        &self,
        name: Operation,
        options: Value,
        started: Option<oneshot::Sender<Result<Value, String>>>,
        cancel: CancellationToken,
    ) -> Result<Response, OperationError> {
        let permission = match self.locked_use.acquire(&cancel).await {
            Ok(permission) => permission,
            Err(error) => {
                if let Some(started) = started {
                    let _ = started.send(Err(error.clone()));
                }
                return Err(error.into());
            }
        };
        match name {
            Operation::ReadUi
            | Operation::ListWindows
            | Operation::Pointer
            | Operation::Keyboard
            | Operation::Scroll => {
                permission.check().await?;
            }
            _ => {}
        }
        let value = match name {
            Operation::ObserveScreen => {
                return Ok(self.observe(options, started, cancel, &permission).await?);
            }
            Operation::ReadUi => {
                let mut ui = blocking(move || {
                    if cancel.is_cancelled() {
                        return Err("Reading the UI was cancelled".into());
                    }
                    selvedge_macos_ax::read_ui(&options)
                })
                .await?;
                ui["atMs"] = json!(self.clock.elapsed().as_secs_f64() * 1000.0);
                ui["input"] = self.input.snapshot();
                ui
            }
            Operation::ListWindows => {
                blocking(move || {
                    if cancel.is_cancelled() {
                        return Err("Listing windows was cancelled".into());
                    }
                    selvedge_macos_ax::list_windows(&options)
                })
                .await?
            }
            Operation::Pointer => {
                self.input
                    .pointer(options_as(options)?, self.clock, cancel, &permission)
                    .await?
            }
            Operation::Keyboard => {
                self.input
                    .keyboard(options_as(options)?, self.clock, cancel, &permission)
                    .await?
            }
            Operation::Scroll => {
                self.input
                    .scroll(options_as(options)?, self.clock, cancel, &permission)
                    .await?
            }
            Operation::InsertText => {
                let clock = self.clock;
                self.input
                    .exclusive(async {
                        permission.check().await?;
                        if cancel.is_cancelled() {
                            return Err("Input was cancelled before dispatch".into());
                        }
                        blocking(move || {
                            let (pid, delivery) = selvedge_macos_ax::insert_text(
                                &options,
                                selvedge_macos_input::insert_text,
                            )?;
                            Ok(json!({
                                "pid": pid,
                                "delivery": delivery,
                                "sentAtMs": clock.elapsed().as_secs_f64() * 1000.0,
                            }))
                        })
                        .await
                    })
                    .await?
            }
            Operation::PerformUiAction => {
                let clock = self.clock;
                self.input
                    .exclusive(async {
                        permission.check().await?;
                        if cancel.is_cancelled() {
                            return Err("Input was cancelled before dispatch".into());
                        }
                        blocking(move || {
                            let action = selvedge_macos_ax::perform_ui_action(&options)?;
                            Ok(json!({
                                "result": action,
                                "sentAtMs": clock.elapsed().as_secs_f64() * 1000.0,
                            }))
                        })
                        .await
                    })
                    .await?
            }
        };
        Ok(Response {
            value,
            images: Vec::new(),
        })
    }

    async fn finish_run(&self, successful: bool) -> Result<(), String> {
        let permission = self.locked_use.permission().await;
        let input = self
            .input
            .finish_run(successful, self.clock, &permission)
            .await;
        let relock = self.locked_use.finish().await;
        match (input, relock) {
            (Ok(()), result) | (result, Ok(())) => result,
            (Err(input), Err(relock)) => Err(format!("{input}; {relock}")),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    #[serde(default)]
    duration_ms: f64,
    interval_ms: Option<f64>,
    image_budget: Option<usize>,
    max_dimension: Option<NonZeroU32>,
    #[serde(default)]
    preserve_input_boundaries: bool,
}

impl Observation {
    fn duration(&self) -> Result<Duration, String> {
        if !(0.0..=30_000.0).contains(&self.duration_ms) {
            return Err("Observation duration must be between 0 and 30000 ms".into());
        }
        Ok(Duration::from_secs_f64(self.duration_ms / 1000.0))
    }

    fn interval(&self, duration: Duration) -> Result<Duration, String> {
        let ms = self.interval_ms.unwrap_or(100.0);
        if !(16.0..=30_000.0).contains(&ms) {
            return Err("Observation interval must be between 16 and 30000 ms".into());
        }
        let spacing =
            Duration::from_nanos(duration.as_nanos().div_ceil(u128::from(MAX_FRAMES - 1)) as u64);
        Ok(Duration::from_secs_f64(ms / 1000.0).max(spacing))
    }

    fn budget(&self) -> Result<usize, String> {
        let budget = self.image_budget.unwrap_or(6);
        if !(1..=12).contains(&budget) {
            return Err("Observation image budget must be between 1 and 12".into());
        }
        Ok(budget)
    }
}

fn options_as<T: serde::de::DeserializeOwned>(options: Value) -> Result<T, String> {
    serde_json::from_value(options).map_err(|error| format!("Invalid operation options: {error}"))
}

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    spawn_blocking(operation)
        .await
        .map_err(|error| format!("Desktop worker failed: {error}"))?
}

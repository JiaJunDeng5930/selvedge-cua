//! One process-scoped audio observation per script invocation.
#![deny(unsafe_code)]

use selvedge_rules::audio::{self, ReadAction, StartDecision, TranscriptBuffer, TranscriptRead};
use serde::{Deserialize, Serialize};
#[cfg(target_os = "macos")]
use std::sync::Arc;
use std::{num::NonZeroI32, time::Duration};
use tokio::{
    sync::{Mutex, mpsc},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[cfg(target_os = "macos")]
mod apple_speech;
#[cfg(target_os = "macos")]
mod capture;
#[cfg(target_os = "macos")]
mod transcription;
#[cfg(target_os = "macos")]
mod whisper;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartListening {
    pid: NonZeroI32,
    #[serde(default = "default_duration")]
    duration_ms: u32,
    language: Option<String>,
}

fn default_duration() -> u32 {
    15_000
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadTranscript {
    #[serde(default)]
    wait_ms: u32,
    #[serde(default)]
    stop: bool,
}

#[derive(Serialize)]
pub struct StartedListening {
    pid: NonZeroI32,
    backend: TranscriptionBackend,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum TranscriptionBackend {
    AppleSpeech,
    Whisper,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegment {
    /// Offsets on the captured PCM timeline, in milliseconds.
    start_ms: u32,
    end_ms: u32,
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transcript {
    pid: NonZeroI32,
    finished: bool,
    segments: Vec<TranscriptSegment>,
}

type Event = audio::TranscriptEvent<TranscriptSegment, String>;

struct Listening {
    pid: NonZeroI32,
    cancel: CancellationToken,
    stop: CancellationToken,
    worker: JoinHandle<()>,
    events: mpsc::UnboundedReceiver<Event>,
    transcript: TranscriptBuffer<String>,
}

#[derive(Default)]
pub struct AudioObserver {
    listening: Mutex<Option<Listening>>,
    #[cfg(target_os = "macos")]
    model: Arc<std::sync::Mutex<Option<whisper::Model>>>,
}

impl AudioObserver {
    pub async fn start_listening(
        &self,
        options: StartListening,
        run_cancel: CancellationToken,
    ) -> Result<StartedListening, String> {
        if options.pid.get() < 1 || options.pid.get() as u32 == std::process::id() {
            return Err("startListening requires a positive PID other than Selvedge's PID".into());
        }
        if !audio::listen_duration_allowed(options.duration_ms) {
            return Err("startListening durationMs must be from 1 to 30000".into());
        }
        if options.language.as_ref().is_some_and(|language| {
            language.len() > 35
                || language.split(['-', '_']).any(|part| {
                    part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
        }) {
            return Err(
                "startListening language must be a language tag such as zh or en-US".into(),
            );
        }
        let mut listening = self.listening.lock().await;
        match audio::decide_start(listening.is_some(), run_cancel.is_cancelled()) {
            StartDecision::Start => {}
            StartDecision::AlreadyStarted => {
                return Err("Only one audio observation can be started per run".into());
            }
            StartDecision::StartCancelled => return Err("Audio observation was cancelled".into()),
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = &mut listening;
            Err("App audio capture is available on macOS".into())
        }
        #[cfg(target_os = "macos")]
        {
            let (ready, started) = tokio::sync::oneshot::channel();
            let (sender, events) = mpsc::unbounded_channel();
            let pid = options.pid;
            let cancel = run_cancel.child_token();
            let stop = CancellationToken::new();
            let worker_cancel = cancel.clone();
            let worker_stop = stop.clone();
            let model = Arc::clone(&self.model);
            let worker = tokio::task::spawn_blocking(move || {
                let result = transcription::listen(
                    model,
                    options,
                    &worker_cancel,
                    &worker_stop,
                    ready,
                    &sender,
                );
                let _ = sender.send(Event::Finished(result));
            });
            let backend = match started.await {
                Ok(backend) => backend,
                Err(_) => {
                    return match events_or_error(events, worker).await {
                        Ok(()) => Err("Audio observation stopped before starting".into()),
                        Err(error) => Err(error),
                    };
                }
            };
            *listening = Some(Listening {
                pid,
                cancel,
                stop,
                worker,
                events,
                transcript: TranscriptBuffer::default(),
            });
            Ok(StartedListening { pid, backend })
        }
    }

    pub async fn read_transcript(
        &self,
        options: ReadTranscript,
        run_cancel: CancellationToken,
    ) -> Result<Transcript, String> {
        if !audio::transcript_wait_allowed(options.wait_ms) {
            return Err("readTranscript waitMs must be from 0 to 5000".into());
        }
        let mut listening = self.listening.lock().await;
        let listening = listening
            .as_mut()
            .ok_or("Call startListening before readTranscript")?;
        let read = TranscriptRead::new(options.stop, options.wait_ms > 0);
        if read.requests_stop() {
            listening.stop.cancel();
        }

        let mut segments = Vec::new();
        let mut received = None;
        let wait_until =
            tokio::time::Instant::now() + Duration::from_millis(options.wait_ms.into());
        loop {
            // Each channel event is moved into one extracted drain. A worker
            // disconnect without a terminal event represents failure.
            let mut batch = Vec::from_iter(received.take());
            if listening.transcript.is_receiving() {
                while !matches!(batch.last(), Some(Event::Finished(_))) {
                    match listening.events.try_recv() {
                        Ok(event) => batch.push(event),
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => {
                            batch.push(worker_disconnected());
                        }
                    }
                }
            }
            segments.extend(listening.transcript.drain(batch));
            let event = match listening
                .transcript
                .read_action(&read, !segments.is_empty())
            {
                ReadAction::Reply => break,
                ReadAction::WaitForFinalization => tokio::select! {
                    _ = run_cancel.cancelled() => return Err("Audio observation was cancelled".into()),
                    event = listening.events.recv() => event,
                },
                ReadAction::WaitForSegment => tokio::select! {
                    _ = run_cancel.cancelled() => return Err("Audio observation was cancelled".into()),
                    event = listening.events.recv() => event,
                    _ = tokio::time::sleep_until(wait_until) => break,
                },
            };
            received = Some(event.unwrap_or_else(worker_disconnected));
        }
        Ok(Transcript {
            pid: listening.pid,
            finished: listening.transcript.finished().map_err(Clone::clone)?,
            segments,
        })
    }

    /// Returning from a run always releases its tap, including on success.
    pub async fn finish_run(&self) -> Result<(), String> {
        let Some(listening) = self.listening.lock().await.take() else {
            return Ok(());
        };
        listening.cancel.cancel();
        listening
            .worker
            .await
            .map_err(|error| format!("Audio observation worker failed: {error}"))
    }
}

fn worker_disconnected() -> Event {
    Event::Finished(Err("Audio observation worker stopped unexpectedly".into()))
}

#[cfg(target_os = "macos")]
async fn events_or_error(
    mut events: mpsc::UnboundedReceiver<Event>,
    worker: JoinHandle<()>,
) -> Result<(), String> {
    worker
        .await
        .map_err(|error| format!("Audio observation worker failed: {error}"))?;
    match events.try_recv() {
        Ok(Event::Finished(result)) => result,
        _ => Err("Audio observation worker did not report its startup result".into()),
    }
}

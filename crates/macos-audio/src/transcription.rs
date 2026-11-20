use crate::{
    Event, StartListening, TranscriptionBackend, apple_speech::AppleSpeech, capture::Capture,
    whisper,
};
use selvedge_rules::audio::{CaptureAction, capture_action_for};
use std::{
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use tokio::{
    runtime::Handle,
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

enum Transcriber {
    Apple(Box<AppleSpeech>),
    Whisper(whisper::Session),
}

pub(crate) fn listen(
    model: Arc<Mutex<Option<whisper::Model>>>,
    options: StartListening,
    cancel: &CancellationToken,
    stop: &CancellationToken,
    ready: oneshot::Sender<TranscriptionBackend>,
    events: &mpsc::UnboundedSender<Event>,
) -> Result<(), String> {
    let runtime = Handle::current();
    let language = options.language.as_deref().unwrap_or("auto");
    let whisper_language = language
        .split(['-', '_'])
        .next()
        .unwrap_or(language)
        .to_ascii_lowercase();
    let transcriber = match runtime.block_on(AppleSpeech::prepare(language, cancel)) {
        Ok(apple) => Transcriber::Apple(Box::new(apple)),
        Err(apple_error) => {
            if cancel.is_cancelled() {
                return Err("Audio observation was cancelled".into());
            }
            let session = whisper::Session::prepare(&model, &whisper_language)
                .map_err(|whisper_error| format!("{apple_error}; {whisper_error}"))?;
            Transcriber::Whisper(session)
        }
    };
    if cancel.is_cancelled() || events.is_closed() {
        return Err("Audio observation was cancelled".into());
    }
    let capture = Capture::open(options.pid, options.duration_ms)?;
    let deadline = Instant::now() + Duration::from_millis(options.duration_ms.into());
    let sample_rate = capture.sample_rate();
    let backend = match &transcriber {
        Transcriber::Apple(_) => TranscriptionBackend::AppleSpeech,
        Transcriber::Whisper(_) => TranscriptionBackend::Whisper,
    };
    ready
        .send(backend)
        .map_err(|_| "Audio observation was cancelled before startup completed")?;

    let (sender, pcm) = mpsc::unbounded_channel();
    let decoder_failed = CancellationToken::new();
    thread::scope(|scope| {
        let decoder = scope.spawn(|| {
            let result = match transcriber {
                Transcriber::Apple(apple) => {
                    runtime.block_on(apple.transcribe(pcm, sample_rate, cancel, events))
                }
                Transcriber::Whisper(whisper) => {
                    whisper.transcribe(pcm, sample_rate, &whisper_language, cancel, events)
                }
            };
            if result.is_err() {
                decoder_failed.cancel();
            }
            result
        });
        let captured = collect_pcm(capture, deadline, cancel, stop, &decoder_failed, sender);
        let decoded = decoder
            .join()
            .map_err(|_| "Audio transcription worker panicked")?;
        decoded.and(captured)
    })
}

fn collect_pcm(
    mut capture: Capture,
    deadline: Instant,
    cancel: &CancellationToken,
    stop: &CancellationToken,
    decoder_failed: &CancellationToken,
    output: mpsc::UnboundedSender<Vec<f32>>,
) -> Result<(), String> {
    // The tap caps total frames at the observation's duration, including
    // frames queued here while the model is processing earlier speech.
    let mut block = vec![0.0; capture.sample_rate() as usize / 50];
    let mut filled = 0;
    let mut stopped = false;
    loop {
        match capture_action_for(
            stopped,
            cancel.is_cancelled() || decoder_failed.is_cancelled(),
            stop.is_cancelled() || Instant::now() >= deadline,
        ) {
            CaptureAction::AbortCapture => return capture.stop(),
            CaptureAction::StopAndDrain => {
                capture.stop()?;
                stopped = true;
            }
            CaptureAction::ReadCapturedPcm => {}
        }
        let count = capture.read(&mut block[filled..])?;
        filled += count;
        if filled == block.len() || (stopped && count == 0 && filled > 0) {
            output
                .send(block[..filled].to_vec())
                .map_err(|_| "Speech transcription ended before consuming the captured audio")?;
            filled = 0;
        }
        if count == 0 {
            if stopped {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

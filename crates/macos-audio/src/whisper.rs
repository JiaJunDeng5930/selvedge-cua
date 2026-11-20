use crate::{Event, TranscriptSegment, capture};
use std::{ffi::c_void, path::PathBuf, sync::Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

pub(crate) struct Model(WhisperContext);

impl Model {
    fn load() -> Result<Self, String> {
        let path = std::env::var_os("SELVEDGE_WHISPER_MODEL")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::home_dir().map(|home| {
                    home.join("Library/Application Support/Selvedge CUA MCP/whisper-model.bin")
                })
            })
            .ok_or("Set SELVEDGE_WHISPER_MODEL to a local Whisper model file")?;
        if !path.is_file() {
            return Err(format!(
                "Whisper model file not found at {}. Set SELVEDGE_WHISPER_MODEL to a local model file.",
                path.display()
            ));
        }
        let mut parameters = WhisperContextParameters::default();
        parameters.use_gpu(false);
        WhisperContext::new_with_params(&path, parameters)
            .map(Self)
            .map_err(|error| format!("Could not load Whisper model {}: {error}", path.display()))
    }
}

pub(crate) struct Session(WhisperState);

impl Session {
    pub(crate) fn prepare(model: &Mutex<Option<Model>>, language: &str) -> Result<Self, String> {
        if language != "auto" && whisper_rs::get_lang_id(language).is_none() {
            return Err(format!("Unsupported Whisper language: {language}"));
        }
        let mut model = model.lock().map_err(|_| "Whisper model lock failed")?;
        let model = match model.as_ref() {
            Some(model) => model,
            None => model.insert(Model::load()?),
        };
        if language != "auto" && language != "en" && !model.0.is_multilingual() {
            return Err(format!(
                "The configured Whisper model is English-only; requested {language}"
            ));
        }
        model
            .0
            .create_state()
            .map(Self)
            .map_err(|error| format!("Could not start Whisper transcription: {error}"))
    }

    pub(crate) fn transcribe(
        mut self,
        mut pcm: mpsc::UnboundedReceiver<Vec<f32>>,
        sample_rate: u32,
        language: &str,
        cancel: &CancellationToken,
        events: &mpsc::UnboundedSender<Event>,
    ) -> Result<(), String> {
        let mut offset = 0;
        let mut pending = None;
        let mut quiet_frames = 0;
        while let Some(block) = pcm.blocking_recv() {
            if cancel.is_cancelled() {
                return Ok(());
            }
            if block.iter().any(|sample| sample.abs() >= 0.003) {
                quiet_frames = 0;
                pending.get_or_insert_with(|| AudioChunk {
                    start_frame: offset,
                    samples: Vec::new(),
                });
            } else {
                quiet_frames += block.len();
            }
            offset += block.len();
            if let Some(chunk) = pending.as_mut() {
                chunk.samples.extend_from_slice(&block);
                if quiet_frames >= sample_rate as usize / 2
                    || chunk.samples.len() >= sample_rate as usize * 8
                {
                    let chunk = pending.take().expect("pending audio");
                    self.decode(chunk, sample_rate, language, cancel, events)?;
                }
            }
        }
        if let Some(chunk) = pending {
            self.decode(chunk, sample_rate, language, cancel, events)?;
        }
        Ok(())
    }

    fn decode(
        &mut self,
        chunk: AudioChunk,
        sample_rate: u32,
        language: &str,
        cancel: &CancellationToken,
        events: &mpsc::UnboundedSender<Event>,
    ) -> Result<(), String> {
        if cancel.is_cancelled() || events.is_closed() {
            return Ok(());
        }
        let mut pcm = capture::resample(&chunk.samples, sample_rate)?;
        // Whisper needs additional context for very short input. Timestamp
        // bounds below refer to physical PCM and exclude this padding.
        pcm.resize(pcm.len().max(16_000), 0.0);
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(language));
        params.set_no_context(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // The safe callback wrapper in whisper-rs 0.16 miscasts its boxed
        // closure. Borrow this token directly for the synchronous decode.
        #[allow(unsafe_code)]
        unsafe {
            params.set_abort_callback(Some(whisper_cancelled));
            params.set_abort_callback_user_data(std::ptr::from_ref(cancel).cast_mut().cast());
        }
        let result = self.0.full(params, &pcm);
        if cancel.is_cancelled() {
            return Ok(());
        }
        result.map_err(|error| format!("Whisper transcription failed: {error}"))?;

        let chunk_start_ms = (chunk.start_frame as i64 * 1000) / i64::from(sample_rate);
        let chunk_end_ms =
            ((chunk.start_frame + chunk.samples.len()) as i64 * 1000) / i64::from(sample_rate);
        for segment in self.0.as_iter() {
            let text = segment
                .to_str_lossy()
                .map_err(|error| format!("Could not read Whisper transcript: {error}"))?;
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let start_ms = (chunk_start_ms + segment.start_timestamp() * 10)
                .clamp(chunk_start_ms, chunk_end_ms) as u32;
            let end_ms = (chunk_start_ms + segment.end_timestamp() * 10)
                .clamp(i64::from(start_ms), chunk_end_ms) as u32;
            if start_ms < end_ms
                && events
                    .send(Event::Segment(TranscriptSegment {
                        start_ms,
                        end_ms,
                        text: text.to_owned(),
                    }))
                    .is_err()
            {
                return Ok(());
            }
        }
        Ok(())
    }
}

struct AudioChunk {
    start_frame: usize,
    samples: Vec<f32>,
}

#[allow(unsafe_code)]
unsafe extern "C" fn whisper_cancelled(user_data: *mut c_void) -> bool {
    // SAFETY: whisper_full calls this only during its synchronous invocation,
    // and the caller passes a live shared CancellationToken for that duration.
    unsafe { &*user_data.cast::<CancellationToken>() }.is_cancelled()
}

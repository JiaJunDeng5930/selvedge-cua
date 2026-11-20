//! Adapter for the extracted app audio protocol in AudioRules.v.
use crate::{
    audio_rules as extracted,
    convert::{boolean, items, list, n},
    corelib::init::datatypes::{Bool, Option_1, Prod},
};

pub fn audio_available(locked_use_enabled: bool) -> bool {
    matches!(
        extracted::audio_available(boolean(locked_use_enabled)),
        Bool::True
    )
}

pub enum StartDecision {
    Start,
    AlreadyStarted,
    StartCancelled,
}

pub fn decide_start(occupied: bool, cancelled: bool) -> StartDecision {
    match extracted::decide_start(boolean(occupied), boolean(cancelled)) {
        extracted::StartDecision::Start => StartDecision::Start,
        extracted::StartDecision::AlreadyStarted => StartDecision::AlreadyStarted,
        extracted::StartDecision::StartCancelled => StartDecision::StartCancelled,
    }
}

pub fn listen_duration_allowed(ms: u32) -> bool {
    matches!(extracted::listen_duration_allowed(n(ms.into())), Bool::True)
}

pub fn transcript_wait_allowed(ms: u32) -> bool {
    matches!(extracted::transcript_wait_allowed(n(ms.into())), Bool::True)
}

pub enum CaptureAction {
    AbortCapture,
    StopAndDrain,
    ReadCapturedPcm,
}

pub fn capture_action_for(stopped: bool, abort: bool, stop_or_deadline: bool) -> CaptureAction {
    let phase = if stopped {
        extracted::CapturePhase::Draining
    } else {
        extracted::CapturePhase::Capturing
    };
    match extracted::capture_action_for(phase, boolean(abort), boolean(stop_or_deadline)) {
        extracted::CaptureAction::AbortCapture => CaptureAction::AbortCapture,
        extracted::CaptureAction::StopAndDrain => CaptureAction::StopAndDrain,
        extracted::CaptureAction::ReadCapturedPcm => CaptureAction::ReadCapturedPcm,
    }
}

pub enum TranscriptEvent<S, E> {
    Segment(S),
    Finished(Result<(), E>),
}

pub struct TranscriptRead {
    stop: bool,
    can_wait: bool,
}

impl TranscriptRead {
    pub fn new(stop: bool, can_wait: bool) -> Self {
        Self { stop, can_wait }
    }

    fn intent(&self) -> extracted::ReadIntent {
        extracted::read_intent_for(boolean(self.stop), boolean(self.can_wait))
    }

    pub fn requests_stop(&self) -> bool {
        matches!(self.intent(), extracted::ReadIntent::StopAndFinalize)
    }
}

pub enum ReadAction {
    Reply,
    WaitForSegment,
    WaitForFinalization,
}

pub struct TranscriptBuffer<E>(extracted::TranscriptStatus<E>);

impl<E> Default for TranscriptBuffer<E> {
    fn default() -> Self {
        Self(extracted::TranscriptStatus::Receiving)
    }
}

impl<E: Clone> TranscriptBuffer<E> {
    pub fn drain<S: Clone + 'static>(&mut self, events: Vec<TranscriptEvent<S, E>>) -> Vec<S>
    where
        E: 'static,
    {
        let current = std::mem::take(self);
        let events = list(events.into_iter().map(|event| match event {
            TranscriptEvent::Segment(segment) => extracted::TranscriptEvent::SegmentReady(segment),
            TranscriptEvent::Finished(result) => {
                extracted::TranscriptEvent::WorkerFinished(match result {
                    Ok(()) => Option_1::None,
                    Err(why) => Option_1::Some(why),
                })
            }
        }));
        let Prod::Pair(status, segments) = extracted::drain_transcript(current.0, events);
        self.0 = status;
        items(segments).collect()
    }

    pub fn is_receiving(&self) -> bool {
        matches!(self.0, extracted::TranscriptStatus::Receiving)
    }

    pub fn read_action(&self, read: &TranscriptRead, has_segments: bool) -> ReadAction {
        match extracted::read_action_for(
            read.intent(),
            boolean(self.is_receiving()),
            boolean(has_segments),
        ) {
            extracted::ReadAction::Reply => ReadAction::Reply,
            extracted::ReadAction::WaitForSegment => ReadAction::WaitForSegment,
            extracted::ReadAction::WaitForFinalization => ReadAction::WaitForFinalization,
        }
    }

    pub fn finished(&self) -> Result<bool, &E> {
        match &self.0 {
            extracted::TranscriptStatus::Receiving => Ok(false),
            extracted::TranscriptStatus::Finalized => Ok(true),
            extracted::TranscriptStatus::TranscriptFailed(why) => Err(why),
        }
    }
}

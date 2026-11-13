//! Speech-to-text for Parley.
//!
//! [`Transcriber`] takes 16 kHz mono i16 PCM and returns finished segments.
//! [`SherpaTranscriber`] puts Silero VAD in front of one of two engines: Parakeet TDT 0.6b v3 or Whisper large-v3-turbo.
//! [`diarize`] is the speaker-name fallback for mixed audio.

mod diarize;
mod engine;
pub mod score;
mod transcriber;

pub use diarize::{SpeakerSegment, diarize};
pub use engine::{Engine, Recognizer};
pub use sherpa_onnx::OfflineRecognizerResult as Decoded;
pub use transcriber::{SherpaTranscriber, VadConfig};

pub const SAMPLE_RATE: u32 = 16_000;

/// One finished piece of speech. Times are milliseconds from the first sample pushed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SttError {
    #[error("cannot create {what} from {path}")]
    Create { what: &'static str, path: String },
    #[error("{0} returned no result")]
    NoResult(&'static str),
}

pub trait Transcriber {
    /// Feed audio. Returns the segments the VAD closed during this call.
    fn push(&mut self, pcm_16k: &[i16]) -> Result<Vec<Segment>, SttError>;
    /// End of stream: close the segment in progress and return it.
    fn flush(&mut self) -> Result<Vec<Segment>, SttError>;
}

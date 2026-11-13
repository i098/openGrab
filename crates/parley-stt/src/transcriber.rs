use crate::{Engine, Recognizer, SAMPLE_RATE, Segment, SttError, Transcriber};
use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct VadConfig {
    pub threshold: f32,
    /// Silence that closes a segment. This waits inside every delay the bench measures.
    pub min_silence_s: f32,
    pub min_speech_s: f32,
    /// A segment still open after this long is cut.
    pub max_speech_s: f32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            min_silence_s: 0.4,
            min_speech_s: 0.25,
            max_speech_s: 20.0,
        }
    }
}

/// Silero VAD in front of one [`Recognizer`].
pub struct SherpaTranscriber {
    vad: VoiceActivityDetector,
    recognizer: Recognizer,
}

impl SherpaTranscriber {
    pub fn new(
        engine: Engine,
        model_dir: &Path,
        silero_model: &Path,
        vad: &VadConfig,
        threads: i32,
    ) -> Result<Self, SttError> {
        let config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(silero_model.to_string_lossy().into_owned()),
                threshold: vad.threshold,
                min_silence_duration: vad.min_silence_s,
                min_speech_duration: vad.min_speech_s,
                window_size: 512,
                max_speech_duration: vad.max_speech_s,
            },
            sample_rate: SAMPLE_RATE as i32,
            num_threads: 1,
            provider: Some("cpu".into()),
            ..Default::default()
        };
        Ok(Self {
            vad: VoiceActivityDetector::create(&config, 60.0).ok_or_else(|| SttError::Create {
                what: "voice activity detector",
                path: silero_model.display().to_string(),
            })?,
            recognizer: Recognizer::new(engine, model_dir, threads)?,
        })
    }

    fn drain(&mut self) -> Result<Vec<Segment>, SttError> {
        let ms = |samples: i64| (samples * 1000 / i64::from(SAMPLE_RATE)) as u64;
        let mut out = Vec::new();
        while let Some(seg) = self.vad.front() {
            let text = self
                .recognizer
                .decode(seg.samples())?
                .text
                .trim()
                .to_owned();
            if !text.is_empty() {
                out.push(Segment {
                    start_ms: ms(seg.start()),
                    end_ms: ms(seg.start() + i64::from(seg.n())),
                    text,
                });
            }
            self.vad.pop();
        }
        Ok(out)
    }
}

impl Transcriber for SherpaTranscriber {
    fn push(&mut self, pcm_16k: &[i16]) -> Result<Vec<Segment>, SttError> {
        let f: Vec<f32> = pcm_16k.iter().map(|&s| f32::from(s) / 32768.0).collect();
        self.vad.accept_waveform(&f);
        self.drain()
    }

    fn flush(&mut self) -> Result<Vec<Segment>, SttError> {
        self.vad.flush();
        self.drain()
    }
}

use crate::{Decoded, SAMPLE_RATE, SttError};
use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
    OfflineWhisperModelConfig,
};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// NVIDIA Parakeet TDT 0.6b v3 (int8), directory `sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8`.
    Parakeet,
    /// Whisper large-v3-turbo (int8), directory `sherpa-onnx-whisper-turbo`.
    Whisper,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Engine::Parakeet => "parakeet-tdt-0.6b-v3",
            Engine::Whisper => "whisper-large-v3-turbo",
        }
    }
}

/// One offline recognizer: audio of one speech segment in, text out.
pub struct Recognizer(OfflineRecognizer);

impl Recognizer {
    /// `model_dir` is the extracted model directory of `engine`.
    pub fn new(engine: Engine, model_dir: &Path, threads: i32) -> Result<Self, SttError> {
        let file = |name: &str| Some(model_dir.join(name).to_string_lossy().into_owned());
        let mut config = OfflineRecognizerConfig::default();
        config.model_config.num_threads = threads;
        match engine {
            Engine::Parakeet => {
                config.model_config.transducer = OfflineTransducerModelConfig {
                    encoder: file("encoder.int8.onnx"),
                    decoder: file("decoder.int8.onnx"),
                    joiner: file("joiner.int8.onnx"),
                };
                config.model_config.tokens = file("tokens.txt");
                config.decoding_method = Some("greedy_search".into());
            }
            Engine::Whisper => {
                config.model_config.whisper = OfflineWhisperModelConfig {
                    encoder: file("turbo-encoder.int8.onnx"),
                    decoder: file("turbo-decoder.int8.onnx"),
                    language: Some("en".into()),
                    task: Some("transcribe".into()),
                    tail_paddings: -1,
                    ..Default::default()
                };
                config.model_config.tokens = file("turbo-tokens.txt");
                config.decoding_method = Some("greedy_search".into());
            }
        }
        OfflineRecognizer::create(&config)
            .map(Self)
            .ok_or_else(|| SttError::Create {
                what: "recognizer",
                path: model_dir.display().to_string(),
            })
    }

    pub fn decode(&self, pcm: &[f32]) -> Result<Decoded, SttError> {
        let stream = self.0.create_stream();
        stream.accept_waveform(SAMPLE_RATE as i32, pcm);
        self.0.decode(&stream);
        stream.get_result().ok_or(SttError::NoResult("recognizer"))
    }
}

use crate::SttError;
use sherpa_onnx::{
    FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    SpeakerEmbeddingExtractorConfig,
};
use std::path::Path;

/// Who spoke when. Spans of different speakers can overlap.
#[derive(Debug, Clone)]
pub struct SpeakerSegment {
    pub start: f32,
    pub end: f32,
    pub speaker: usize,
}

/// Speaker diarization of a complete 16 kHz mono recording: pyannote segmentation 3.0 plus a speaker-embedding model.
/// `num_speakers` is the call roster size.
pub fn diarize(
    pcm: &[f32],
    segmentation_model: &Path,
    embedding_model: &Path,
    num_speakers: i32,
    threads: i32,
) -> Result<Vec<SpeakerSegment>, SttError> {
    let path = |p: &Path| Some(p.to_string_lossy().into_owned());
    let config = OfflineSpeakerDiarizationConfig {
        segmentation: OfflineSpeakerSegmentationModelConfig {
            pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                model: path(segmentation_model),
                ..Default::default()
            },
            num_threads: threads,
            ..Default::default()
        },
        embedding: SpeakerEmbeddingExtractorConfig {
            model: path(embedding_model),
            num_threads: threads,
            ..Default::default()
        },
        clustering: FastClusteringConfig {
            num_clusters: num_speakers,
            ..Default::default()
        },
        ..Default::default()
    };
    let diarizer = OfflineSpeakerDiarization::create(&config).ok_or_else(|| SttError::Create {
        what: "speaker diarizer",
        path: segmentation_model.display().to_string(),
    })?;
    let result = diarizer
        .process(pcm)
        .ok_or(SttError::NoResult("diarizer"))?;
    Ok(result
        .sort_by_start_time()
        .into_iter()
        .map(|s| SpeakerSegment {
            start: s.start,
            end: s.end,
            speaker: s.speaker as usize,
        })
        .collect())
}

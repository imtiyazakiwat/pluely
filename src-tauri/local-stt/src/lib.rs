//! On-device speech-to-text for Clocks.
//!
//! - Silero VAD (sherpa-onnx) finds speech in 32 ms windows.
//! - A streaming Zipformer transducer (sherpa-onnx) transcribes while the
//!   person talks, so only a short flush is left when they stop.
//! - Smart Turn v3 (ONNX Runtime via `ort`) decides from the audio whether the
//!   speaker finished or only paused.
//!
//! Everything runs on the CPU. The ONNX Runtime used by `ort` is the one
//! sherpa-onnx links statically (see `ort_bridge`).

pub mod engine;
pub mod mel;
pub mod models;
mod ort_bridge;
pub mod pipeline;
pub mod resample;
pub mod turn;

pub use engine::{Engine, EngineConfig, TransducerFiles, Transcript};
pub use pipeline::{Event, Pipeline, PipelineParams, TurnEnd, TurnResult};
pub use resample::Resampler;
pub use turn::SmartTurn;

/// All models run at 16 kHz mono.
pub const SAMPLE_RATE: u32 = 16_000;

/// ONNX Runtime version linked into the binary.
pub fn onnxruntime_version() -> anyhow::Result<String> {
    ort_bridge::init()
}

/// LibriSpeech-trained Zipformers emit UPPER CASE without punctuation. Make it
/// readable for the UI and the LLM prompt: sentence case and a capital "I".
/// Text that already has lower-case letters is only whitespace-normalised.
pub fn tidy_text(raw: &str) -> String {
    let joined = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() || joined.chars().any(char::is_lowercase) {
        return joined;
    }
    let lowered = joined.to_lowercase();
    let words: Vec<String> = lowered
        .split(' ')
        .map(|w| {
            if w == "i" || w.starts_with("i'") {
                let mut s = w.to_string();
                s.replace_range(0..1, "I");
                s
            } else {
                w.to_string()
            }
        })
        .collect();
    let sentence = words.join(" ");
    let mut chars = sentence.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => sentence,
    }
}

#[cfg(test)]
mod tests {
    use super::tidy_text;

    #[test]
    fn tidies_upper_case_transcripts() {
        assert_eq!(tidy_text("  WHAT IS THE  WEATHER "), "What is the weather");
        assert_eq!(tidy_text("I THINK I'M LATE"), "I think I'm late");
        assert_eq!(tidy_text("Already Mixed case"), "Already Mixed case");
        assert_eq!(tidy_text(""), "");
    }
}

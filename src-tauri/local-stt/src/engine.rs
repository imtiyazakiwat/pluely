//! Loaded models shared by every pipeline: the streaming recognizer, the
//! Silero VAD configuration and (optionally) the Smart Turn classifier.

use crate::turn::SmartTurn;
use crate::SAMPLE_RATE;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sherpa_onnx::{
    OnlineRecognizer, OnlineRecognizerConfig, OnlineStream, SileroVadModelConfig, VadModelConfig,
    VoiceActivityDetector,
};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

/// Files of a streaming transducer (Zipformer-style) model.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransducerFiles {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub vad_model: PathBuf,
    pub asr: TransducerFiles,
    /// `None` disables end-of-turn detection: every VAD segment ends a turn.
    pub turn_model: Option<PathBuf>,
    /// Threads for the recognizer. 1 keeps ONNX Runtime from spinning a pool
    /// while the app sits in a meeting.
    pub asr_threads: i32,
    pub turn_threads: usize,
    /// Silence appended when finalising a stream so the recognizer sees past
    /// the last word. Depends on the model's chunk size (see `models::AsrModel`).
    pub tail_padding_s: f32,
}

pub struct Engine {
    tail_padding_s: f32,
    recognizer: OnlineRecognizer,
    /// Serialises recognizer calls. The system-audio pipeline and one-shot mic
    /// transcriptions can share an engine, and sherpa-onnx only promises
    /// thread safety per object.
    decode_lock: Mutex<()>,
    vad_model: PathBuf,
    turn: Option<Mutex<SmartTurn>>,
    ort_version: String,
}

fn existing(path: &Path) -> Result<String> {
    if !path.is_file() {
        bail!("model file not found: {}", path.display());
    }
    Ok(path.to_string_lossy().into_owned())
}

impl Engine {
    pub fn load(cfg: &EngineConfig) -> Result<Self> {
        let ort_version = crate::ort_bridge::init()?;

        let mut rc = OnlineRecognizerConfig::default();
        rc.model_config.transducer.encoder = Some(existing(&cfg.asr.encoder)?);
        rc.model_config.transducer.decoder = Some(existing(&cfg.asr.decoder)?);
        rc.model_config.transducer.joiner = Some(existing(&cfg.asr.joiner)?);
        rc.model_config.tokens = Some(existing(&cfg.asr.tokens)?);
        rc.model_config.num_threads = cfg.asr_threads.max(1);
        rc.decoding_method = Some("greedy_search".into());
        // Endpointing is done by Silero VAD + Smart Turn, not by the recognizer.
        rc.enable_endpoint = false;
        let recognizer = OnlineRecognizer::create(&rc)
            .context("sherpa-onnx rejected the streaming ASR model")?;

        existing(&cfg.vad_model)?;
        let turn = match &cfg.turn_model {
            Some(path) => Some(Mutex::new(SmartTurn::load(path, cfg.turn_threads)?)),
            None => None,
        };

        Ok(Self {
            tail_padding_s: cfg.tail_padding_s,
            recognizer,
            decode_lock: Mutex::new(()),
            vad_model: cfg.vad_model.clone(),
            turn,
            ort_version,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.decode_lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn create_stream(&self) -> OnlineStream {
        let _guard = self.lock();
        self.recognizer.create_stream()
    }

    /// Appends 16 kHz samples and decodes every chunk that is ready.
    /// Returns whether anything was decoded (i.e. the hypothesis may change).
    pub(crate) fn feed(&self, stream: &OnlineStream, samples: &[f32]) -> bool {
        stream.accept_waveform(SAMPLE_RATE as i32, samples);
        let _guard = self.lock();
        let mut decoded = false;
        while self.recognizer.is_ready(stream) {
            self.recognizer.decode(stream);
            decoded = true;
        }
        decoded
    }

    /// Current (non-final) hypothesis for `stream`.
    pub(crate) fn current_text(&self, stream: &OnlineStream) -> String {
        let _guard = self.lock();
        self.recognizer
            .get_result(stream)
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default()
    }

    pub fn ort_version(&self) -> &str {
        &self.ort_version
    }

    pub fn has_turn_detector(&self) -> bool {
        self.turn.is_some()
    }

    pub(crate) fn turn_detector(&self) -> Option<&Mutex<SmartTurn>> {
        self.turn.as_ref()
    }

    pub(crate) fn new_vad(
        &self,
        threshold: f32,
        min_silence_s: f32,
        min_speech_s: f32,
        max_speech_s: f32,
    ) -> Result<VoiceActivityDetector> {
        let config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(self.vad_model.to_string_lossy().into_owned()),
                threshold,
                min_silence_duration: min_silence_s,
                min_speech_duration: min_speech_s,
                window_size: 512,
                max_speech_duration: max_speech_s,
            },
            sample_rate: SAMPLE_RATE as i32,
            num_threads: 1,
            provider: Some("cpu".into()),
            ..Default::default()
        };
        VoiceActivityDetector::create(&config, 60.0).context("failed to create Silero VAD")
    }

    /// Finishes `stream`: appends the model's tail padding of silence so it can
    /// see past the last word, flushes, and returns the final text.
    pub(crate) fn finish_stream(&self, stream: &OnlineStream) -> String {
        let pad = vec![0.0f32; (self.tail_padding_s.max(0.0) * SAMPLE_RATE as f32) as usize];
        if !pad.is_empty() {
            stream.accept_waveform(SAMPLE_RATE as i32, &pad);
        }
        stream.input_finished();
        let _guard = self.lock();
        while self.recognizer.is_ready(stream) {
            self.recognizer.decode(stream);
        }
        self.recognizer
            .get_result(stream)
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default()
    }

    /// Transcribes a complete 16 kHz mono clip in one go. Used for clips that
    /// were already segmented elsewhere (the mic path).
    pub fn transcribe(&self, samples: &[f32]) -> Transcript {
        let started = Instant::now();
        let stream = self.create_stream();
        // Feed in 100 ms pieces so the encoder runs as audio "arrives"; this is
        // equivalent to one big call but keeps peak memory flat.
        for piece in samples.chunks(1600) {
            self.feed(&stream, piece);
        }
        let text = crate::tidy_text(&self.finish_stream(&stream));
        Transcript {
            text,
            audio_s: samples.len() as f64 / SAMPLE_RATE as f64,
            compute_ms: started.elapsed().as_secs_f64() * 1000.0,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Transcript {
    pub text: String,
    pub audio_s: f64,
    pub compute_ms: f64,
}

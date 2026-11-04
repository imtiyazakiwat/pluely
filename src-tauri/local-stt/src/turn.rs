//! Smart Turn v3 end-of-turn classifier (pipecat-ai/smart-turn, BSD-2-Clause).
//!
//! Audio-native: takes the last <= 8 s of the current turn at 16 kHz and
//! returns the probability that the speaker has finished. Mirrors the
//! reference `predict_endpoint()` in the smart-turn repo: left-pad to 8 s with
//! zeros, zero-mean/unit-variance normalise the whole window, Whisper log-mel
//! features, one ONNX forward pass (the model applies the sigmoid itself).

use crate::mel::{WhisperMel, N_MELS};
use anyhow::{anyhow, Context, Result};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::path::Path;
use std::time::Instant;

pub const WINDOW_SAMPLES: usize = 8 * 16_000;

pub struct SmartTurn {
    session: Session,
    mel: WhisperMel,
    window: Vec<f32>,
}

impl SmartTurn {
    pub fn load(model: &Path, intra_threads: usize) -> Result<Self> {
        let env = crate::ort_bridge::environment()?;
        let session = Session::builder(env)
            .map_err(|e| anyhow!("smart-turn session builder: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow!("smart-turn optimisation level: {e}"))?
            .with_intra_threads(intra_threads.max(1))
            .map_err(|e| anyhow!("smart-turn intra threads: {e}"))?
            .with_inter_threads(1)
            .map_err(|e| anyhow!("smart-turn inter threads: {e}"))?
            // The model runs a few times a minute; don't let idle pool threads
            // spin and burn CPU between calls.
            .with_intra_op_spinning(false)
            .map_err(|e| anyhow!("smart-turn spinning: {e}"))?
            .commit_from_file(model)
            .map_err(|e| anyhow!("load {}: {e}", model.display()))?;
        Ok(Self {
            session,
            mel: WhisperMel::new(),
            window: vec![0.0; WINDOW_SAMPLES],
        })
    }

    /// Probability in `[0, 1]` that the turn ended at the end of `audio`.
    pub fn predict(&mut self, audio: &[f32]) -> Result<f32> {
        self.predict_timed(audio).map(|s| s.probability)
    }

    /// Like [`predict`](Self::predict), with a timing breakdown.
    pub fn predict_timed(&mut self, audio: &[f32]) -> Result<TurnScore> {
        let started = Instant::now();
        let (features, frames) = self.features(audio);
        let features_ms = started.elapsed().as_secs_f64() * 1000.0;

        let infer_started = Instant::now();
        let input = Tensor::from_array(([1usize, N_MELS, frames], features))
            .map_err(|e| anyhow!("smart-turn input tensor: {e}"))?;
        let outputs = self
            .session
            .run(ort::inputs!["input_features" => input])
            .map_err(|e| anyhow!("smart-turn inference: {e}"))?;
        let (_, probs) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow!("smart-turn output: {e}"))?;
        let probability = probs
            .first()
            .copied()
            .context("smart-turn returned an empty tensor")?;
        Ok(TurnScore {
            probability,
            features_ms,
            inference_ms: infer_started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    /// Model input for `audio`: `[N_MELS][frames]` row-major, and `frames`.
    pub fn features(&mut self, audio: &[f32]) -> (Vec<f32>, usize) {
        // Keep the most recent 8 s; left-pad shorter turns with zeros.
        let tail = &audio[audio.len().saturating_sub(WINDOW_SAMPLES)..];
        let pad = WINDOW_SAMPLES - tail.len();
        self.window[..pad].fill(0.0);
        self.window[pad..].copy_from_slice(tail);

        // Zero-mean, unit-variance over the full (padded) window, as
        // WhisperFeatureExtractor(do_normalize=True) does for an unpadded input.
        let n = WINDOW_SAMPLES as f64;
        let mean = self.window.iter().map(|&v| v as f64).sum::<f64>() / n;
        let var = self
            .window
            .iter()
            .map(|&v| {
                let d = v as f64 - mean;
                d * d
            })
            .sum::<f64>()
            / n;
        let inv_std = 1.0 / (var + 1e-7).sqrt();
        for v in self.window.iter_mut() {
            *v = ((*v as f64 - mean) * inv_std) as f32;
        }
        self.mel.compute(&self.window)
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct TurnScore {
    pub probability: f32,
    pub features_ms: f64,
    pub inference_ms: f64,
}

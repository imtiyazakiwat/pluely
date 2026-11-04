//! Device-rate to 16 kHz conversion for the capture loop.

use crate::SAMPLE_RATE;
use anyhow::{bail, Result};
use sherpa_onnx::LinearResampler;

/// Streams audio from `input_rate` to 16 kHz. A pass-through at 16 kHz.
pub struct Resampler {
    inner: Option<LinearResampler>,
}

impl Resampler {
    pub fn new(input_rate: u32) -> Result<Self> {
        if !(8_000..=192_000).contains(&input_rate) {
            bail!("unsupported input sample rate {input_rate} Hz");
        }
        if input_rate == SAMPLE_RATE {
            return Ok(Self { inner: None });
        }
        // Band-limited (windowed sinc) resampler from kaldi, via sherpa-onnx.
        let inner = LinearResampler::create(input_rate as i32, SAMPLE_RATE as i32)
            .ok_or_else(|| anyhow::anyhow!("failed to create {input_rate} Hz -> 16 kHz resampler"))?;
        Ok(Self { inner: Some(inner) })
    }

    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        match &self.inner {
            Some(r) => r.resample(samples, false),
            None => samples.to_vec(),
        }
    }

    pub fn flush(&mut self) -> Vec<f32> {
        match &self.inner {
            Some(r) => r.resample(&[], true),
            None => Vec::new(),
        }
    }
}

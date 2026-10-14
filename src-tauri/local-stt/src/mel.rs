//! Whisper-style log-mel features for Smart Turn.
//!
//! Matches the numpy path of `transformers.WhisperFeatureExtractor`
//! (`n_fft = 400`, `hop = 160`, 80 Slaney mel bins up to 8 kHz, periodic Hann
//! window, centred STFT with reflect padding, `log10`, dynamic range clamped to
//! 8, then `(x + 4) / 4`). Parity with the Python reference is checked by
//! `spikes/on-device-stt/smart_turn_reference.py`.

use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::sync::Arc;

pub const N_FFT: usize = 400;
pub const HOP: usize = 160;
pub const N_MELS: usize = 80;
const N_FREQS: usize = N_FFT / 2 + 1;
const SAMPLE_RATE: f64 = 16_000.0;

/// One triangular mel filter, stored as its non-zero span only.
struct Band {
    first_bin: usize,
    weights: Vec<f64>,
}

pub struct WhisperMel {
    window: Vec<f64>,
    bands: Vec<Band>,
    fft: Arc<dyn Fft<f64>>,
    padded: Vec<f64>,
    frame: Vec<Complex<f64>>,
    scratch: Vec<Complex<f64>>,
    power: Vec<f64>,
}

impl Default for WhisperMel {
    fn default() -> Self {
        Self::new()
    }
}

impl WhisperMel {
    pub fn new() -> Self {
        // Periodic Hann: np.hanning(N + 1)[:-1].
        let window = (0..N_FFT)
            .map(|n| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / N_FFT as f64).cos())
            .collect();
        let fft = FftPlanner::new().plan_fft_forward(N_FFT);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        let filters = slaney_mel_filters();
        // Each triangle only touches a handful of FFT bins; keep just those.
        let bands = (0..N_MELS)
            .map(|m| {
                let row = &filters[m * N_FREQS..(m + 1) * N_FREQS];
                let first = row.iter().position(|w| *w > 0.0).unwrap_or(0);
                let last = row.iter().rposition(|w| *w > 0.0).unwrap_or(first);
                Band {
                    first_bin: first,
                    weights: row[first..=last].to_vec(),
                }
            })
            .collect();
        Self {
            window,
            bands,
            fft,
            padded: Vec::new(),
            frame: vec![Complex::default(); N_FFT],
            scratch,
            power: vec![0.0; N_FREQS],
        }
    }

    /// Computes features for `samples` (16 kHz). Returns `(features, frames)`
    /// where `features` is `[N_MELS][frames]` row-major and
    /// `frames == samples.len() / HOP`.
    pub fn compute(&mut self, samples: &[f32]) -> (Vec<f32>, usize) {
        let n = samples.len();
        assert!(n > N_FFT / 2, "need more than {} samples", N_FFT / 2);
        let pad = N_FFT / 2;

        // np.pad(mode="reflect"): mirror without repeating the edge sample.
        self.padded.clear();
        self.padded.extend((1..=pad).rev().map(|i| samples[i] as f64));
        self.padded.extend(samples.iter().map(|&v| v as f64));
        self.padded.extend((n - 1 - pad..n - 1).rev().map(|i| samples[i] as f64));

        // Whisper drops the final STFT frame.
        let frames = (self.padded.len() - N_FFT) / HOP;
        let mut log_mel = vec![0f64; N_MELS * frames];
        let mut max_val = f64::NEG_INFINITY;

        for f in 0..frames {
            let segment = &self.padded[f * HOP..f * HOP + N_FFT];
            for ((slot, &x), &w) in self.frame.iter_mut().zip(segment).zip(&self.window) {
                *slot = Complex::new(x * w, 0.0);
            }
            self.fft.process_with_scratch(&mut self.frame, &mut self.scratch);
            for (p, c) in self.power.iter_mut().zip(&self.frame) {
                *p = c.norm_sqr();
            }
            for (m, band) in self.bands.iter().enumerate() {
                let power = &self.power[band.first_bin..band.first_bin + band.weights.len()];
                let energy: f64 = band.weights.iter().zip(power).map(|(w, p)| w * p).sum();
                let v = energy.max(1e-10).log10();
                max_val = max_val.max(v);
                log_mel[m * frames + f] = v;
            }
        }

        let floor = max_val - 8.0;
        let out = log_mel
            .into_iter()
            .map(|v| ((v.max(floor) + 4.0) / 4.0) as f32)
            .collect();
        (out, frames)
    }
}

fn hz_to_mel(hz: f64) -> f64 {
    if hz >= 1000.0 {
        15.0 + (hz / 1000.0).ln() * (27.0 / 6.4f64.ln())
    } else {
        3.0 * hz / 200.0
    }
}

fn mel_to_hz(mel: f64) -> f64 {
    if mel >= 15.0 {
        1000.0 * ((6.4f64.ln() / 27.0) * (mel - 15.0)).exp()
    } else {
        200.0 * mel / 3.0
    }
}

/// `transformers.audio_utils.mel_filter_bank(201, 80, 0, 8000, 16000,
/// norm="slaney", mel_scale="slaney")`, transposed to `[mel][freq]`.
fn slaney_mel_filters() -> Vec<f64> {
    let mel_max = hz_to_mel(SAMPLE_RATE / 2.0);
    let edges: Vec<f64> = (0..N_MELS + 2)
        .map(|i| mel_to_hz(mel_max * i as f64 / (N_MELS + 1) as f64))
        .collect();
    let fft_freqs: Vec<f64> = (0..N_FREQS)
        .map(|k| (SAMPLE_RATE / 2.0) * k as f64 / (N_FREQS - 1) as f64)
        .collect();
    let mut filters = vec![0f64; N_MELS * N_FREQS];
    for m in 0..N_MELS {
        let (lo, mid, hi) = (edges[m], edges[m + 1], edges[m + 2]);
        let enorm = 2.0 / (hi - lo);
        for (k, &f) in fft_freqs.iter().enumerate() {
            let down = (f - lo) / (mid - lo);
            let up = (hi - f) / (hi - mid);
            filters[m * N_FREQS + k] = down.min(up).max(0.0) * enorm;
        }
    }
    filters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_count_matches_whisper() {
        let mut mel = WhisperMel::new();
        let (feats, frames) = mel.compute(&vec![0.0f32; 128_000]);
        assert_eq!(frames, 800);
        assert_eq!(feats.len(), N_MELS * 800);
    }

    #[test]
    fn mel_filters_are_slaney_normalised() {
        let filters = slaney_mel_filters();
        // Every filter has some weight and none is negative.
        for m in 0..N_MELS {
            let row = &filters[m * N_FREQS..(m + 1) * N_FREQS];
            assert!(row.iter().all(|w| *w >= 0.0));
            assert!(row.iter().any(|w| *w > 0.0), "empty filter {m}");
        }
    }
}

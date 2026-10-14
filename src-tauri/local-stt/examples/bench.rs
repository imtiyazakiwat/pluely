//! Benchmark harness for the on-device STT spike (issue #1).
//!
//! Reads the manifests written by `spikes/on-device-stt/prepare_data.py` and
//! writes one JSON record per clip; `spikes/on-device-stt/score.py` turns those
//! into WER / latency / resource tables.
//!
//! cargo run -p clocks-local-stt --release --example bench -- --mode pipeline \
//!     --sets librispeech,ami-ihm --asr zipformer-en-2023-06-26 --out run.jsonl
//!
//! Modes:
//!   pipeline  Streaming Silero VAD -> Zipformer -> Smart Turn, fed like the
//!             capture loop (device-rate chunks of 1024, resampled to 16 kHz).
//!   whole     One recognizer pass per clip, no VAD (the mic path, and an
//!             ASR-only WER reference).
//!   turn      Smart Turn on the labelled smart-turn clips.
//!   energy    Today's energy VAD (port of speaker/commands.rs) for the
//!             cutoff-latency baseline.

use anyhow::{bail, Context, Result};
use clocks_local_stt::{
    Engine, EngineConfig, Event, Pipeline, PipelineParams, Resampler, SmartTurn, TransducerFiles,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

const SR: u32 = 16_000;
const LEAD_S: f64 = 0.5;
const TRAIL_S: f64 = 3.0;
/// Speech is normalised to this RMS (-26 dBFS), a typical playback level.
const TARGET_SPEECH_RMS: f32 = 0.05;
/// Chunk size the capture loop reads at the device rate (`VadConfig::hop_size`).
const CAPTURE_CHUNK: usize = 1024;

struct Args(HashMap<String, String>);

impl Args {
    fn parse() -> Self {
        let mut map = HashMap::new();
        let mut it = std::env::args().skip(1);
        while let Some(key) = it.next() {
            let key = key.trim_start_matches("--").to_string();
            let value = it.next().unwrap_or_default();
            map.insert(key, value);
        }
        Self(map)
    }
    fn str(&self, key: &str, default: &str) -> String {
        self.0.get(key).cloned().unwrap_or_else(|| default.to_string())
    }
    fn f32(&self, key: &str, default: f32) -> f32 {
        self.0.get(key).map(|v| v.parse().expect(key)).unwrap_or(default)
    }
    fn usize(&self, key: &str, default: usize) -> usize {
        self.0.get(key).map(|v| v.parse().expect(key)).unwrap_or(default)
    }
}

#[derive(Clone)]
struct Clip {
    set: String,
    id: String,
    text: String,
    meta: Value,
    path: PathBuf,
}

impl Clip {
    /// Read on demand so peak RSS reflects the engine, not the test set.
    fn samples(&self) -> Result<Vec<f32>> {
        read_wav(&self.path)
    }
}

fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).with_context(|| path.display().to_string())?;
    let spec = reader.spec();
    if spec.sample_rate != SR || spec.channels != 1 {
        bail!("{}: expected 16 kHz mono", path.display());
    }
    Ok(reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<_, _>>()?)
}

fn load_clips(data: &Path, sets: &[String], limit: usize) -> Result<Vec<Clip>> {
    let mut clips = Vec::new();
    for set in sets {
        let manifest = data.join(set).join("manifest.jsonl");
        let reader = BufReader::new(File::open(&manifest).with_context(|| manifest.display().to_string())?);
        for line in reader.lines().take(limit) {
            let meta: Value = serde_json::from_str(&line?)?;
            let path = data.join(meta["path"].as_str().context("path")?);
            clips.push(Clip {
                set: set.clone(),
                id: meta["id"].as_str().unwrap_or_default().to_string(),
                text: meta["text"].as_str().unwrap_or_default().to_string(),
                path,
                meta,
            });
        }
    }
    Ok(clips)
}

fn asr_files(models: &Path, name: &str) -> Result<TransducerFiles> {
    let dir = models.join(name);
    let (e, d, j) = match name {
        "zipformer-en-2023-06-26" => (
            "encoder-epoch-99-avg-1-chunk-16-left-128.int8.onnx",
            "decoder-epoch-99-avg-1-chunk-16-left-128.int8.onnx",
            "joiner-epoch-99-avg-1-chunk-16-left-128.int8.onnx",
        ),
        "zipformer-en-20M-2023-02-17" => (
            "encoder-epoch-99-avg-1.int8.onnx",
            "decoder-epoch-99-avg-1.int8.onnx",
            "joiner-epoch-99-avg-1.int8.onnx",
        ),
        "kroko-en-2025-08-06" => ("encoder.onnx", "decoder.onnx", "joiner.onnx"),
        other => bail!("unknown ASR model {other}"),
    };
    Ok(TransducerFiles {
        encoder: dir.join(e),
        decoder: dir.join(d),
        joiner: dir.join(j),
        tokens: dir.join("tokens.txt"),
    })
}

/// Flush padding each model needs (chunk size + look-ahead), unless overridden
/// with `--tail-padding`.
fn tail_padding(args: &Args, asr: &str) -> f32 {
    let default = if asr.starts_with("kroko") { 1.3 } else { 0.3 };
    args.f32("tail-padding", default)
}

fn turn_model(models: &Path, which: &str) -> Option<PathBuf> {
    match which {
        "off" => None,
        "gpu" => Some(models.join("smart-turn-v3.2/smart-turn-v3.2-gpu.onnx")),
        _ => Some(models.join("smart-turn-v3.2/smart-turn-v3.2-cpu.onnx")),
    }
}

// ---------------------------------------------------------------- signal prep

/// RMS of 10 ms frames.
fn frame_rms(x: &[f32]) -> Vec<f32> {
    x.chunks(160)
        .map(|c| (c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).sqrt())
        .collect()
}

fn percentile(values: &[f32], q: f32) -> f32 {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f32 * q) as usize]
}

/// Deterministic Gaussian noise (xorshift + Box-Muller) so runs are comparable.
struct Noise(u64);
impl Noise {
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) as f32
    }
    fn gaussian(&mut self) -> f32 {
        let u1 = self.next_f32().max(1e-7);
        let u2 = self.next_f32();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f32::consts::PI * u2).cos()
    }
}

struct Prepared {
    audio: Vec<f32>,
    speech_end_s: f64,
    speech_start_s: f64,
    noise_rms: f32,
}

/// Level-normalises the clip, pads it with noise at the clip's own noise
/// floor, and finds where speech really ends (energy, 10 ms resolution).
fn prepare(clip: &Clip, samples: &[f32]) -> Prepared {
    let rms = frame_rms(samples);
    let floor = percentile(&rms, 0.10).max(1e-5);
    let peak = rms.iter().cloned().fold(0.0, f32::max);
    let threshold = (floor * 3.16).max(peak * 0.0178);
    let active: Vec<f32> = rms.iter().cloned().filter(|r| *r > threshold).collect();
    let speech_rms = if active.is_empty() {
        peak
    } else {
        (active.iter().map(|r| r * r).sum::<f32>() / active.len() as f32).sqrt()
    };
    let gain = (TARGET_SPEECH_RMS / speech_rms.max(1e-4)).min(30.0);
    let last = rms.iter().rposition(|r| *r > threshold).unwrap_or(rms.len() - 1);
    let first = rms.iter().position(|r| *r > threshold).unwrap_or(0);

    let noise_rms = floor * gain;
    let mut noise = Noise(0x9E37_79B9_7F4A_7C15 ^ clip.id.len() as u64);
    let lead = (LEAD_S * SR as f64) as usize;
    let trail = (TRAIL_S * SR as f64) as usize;
    let mut audio = Vec::with_capacity(lead + samples.len() + trail);
    audio.extend((0..lead).map(|_| noise.gaussian() * noise_rms));
    audio.extend(samples.iter().map(|v| (v * gain).clamp(-1.0, 1.0)));
    audio.extend((0..trail).map(|_| noise.gaussian() * noise_rms));
    Prepared {
        audio,
        speech_end_s: LEAD_S + ((last + 1) as f64 * 0.01).min(samples.len() as f64 / SR as f64),
        speech_start_s: LEAD_S + first as f64 * 0.01,
        noise_rms,
    }
}

/// Simulates a device at `rate`: 16 kHz -> `rate`.
fn to_device_rate(audio: &[f32], rate: u32) -> Vec<f32> {
    if rate == SR {
        return audio.to_vec();
    }
    let up = sherpa_onnx::LinearResampler::create(SR as i32, rate as i32).expect("resampler");
    let mut out = up.resample(audio, false);
    out.extend(up.resample(&[], true));
    out
}

// ---------------------------------------------------------------- resources

fn cpu_seconds() -> f64 {
    unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut u);
        let t = |tv: libc::timeval| tv.tv_sec as f64 + tv.tv_usec as f64 / 1e6;
        t(u.ru_utime) + t(u.ru_stime)
    }
}

fn peak_rss_mb() -> f64 {
    unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut u);
        // Bytes on macOS, kilobytes on Linux.
        if cfg!(target_os = "macos") {
            u.ru_maxrss as f64 / 1_048_576.0
        } else {
            u.ru_maxrss as f64 / 1024.0
        }
    }
}

// ---------------------------------------------------------------- modes

fn params_from(args: &Args) -> PipelineParams {
    let d = PipelineParams::default();
    PipelineParams {
        vad_threshold: args.f32("vad-threshold", d.vad_threshold),
        min_silence_s: args.f32("min-silence", d.min_silence_s),
        min_speech_s: args.f32("min-speech", d.min_speech_s),
        max_speech_s: d.max_speech_s,
        turn_threshold: args.f32("turn-threshold", d.turn_threshold),
        max_hold_s: args.f32("max-hold", d.max_hold_s),
        max_turn_s: d.max_turn_s,
        use_turn_detector: args.str("turn", "cpu") != "off",
    }
}

fn run_pipeline(args: &Args, clips: &[Clip], out: &mut impl Write) -> Result<Value> {
    let models = PathBuf::from(args.str("models", "../spikes/on-device-stt/.cache/models"));
    let asr = args.str("asr", "zipformer-en-2023-06-26");
    let cfg = EngineConfig {
        vad_model: models.join("silero-vad/silero_vad.onnx"),
        asr: asr_files(&models, &asr)?,
        turn_model: turn_model(&models, &args.str("turn", "cpu")),
        asr_threads: args.usize("asr-threads", 1) as i32,
        turn_threads: args.usize("turn-threads", 4),
        tail_padding_s: tail_padding(args, &asr),
    };
    let load = Instant::now();
    let engine = Arc::new(Engine::load(&cfg)?);
    let load_ms = load.elapsed().as_secs_f64() * 1000.0;
    let params = params_from(args);
    let rate = args.usize("capture-rate", 48_000) as u32;

    let cpu0 = cpu_seconds();
    let mut audio_total = 0.0;
    let mut wall_total = 0.0;
    for clip in clips {
        let samples = clip.samples()?;
        let prep = prepare(clip, &samples);
        let device = to_device_rate(&prep.audio, rate);
        let mut resampler = Resampler::new(rate)?;
        let mut pipeline = Pipeline::new(engine.clone(), params.clone())?;

        let mut events: Vec<(f64, Event)> = Vec::new();
        let mut buf = Vec::new();
        let mut fed = 0usize;
        let mut resample_ms = 0.0;
        let started = Instant::now();
        for chunk in device.chunks(CAPTURE_CHUNK) {
            let t = Instant::now();
            let samples = resampler.process(chunk);
            resample_ms += t.elapsed().as_secs_f64() * 1000.0;
            fed += samples.len();
            pipeline.accept(&samples, &mut buf);
            let at = fed as f64 / SR as f64;
            events.extend(buf.drain(..).map(|e| (at, e)));
        }
        let tail = resampler.flush();
        fed += tail.len();
        pipeline.accept(&tail, &mut buf);
        pipeline.finish(&mut buf);
        let at = fed as f64 / SR as f64;
        events.extend(buf.drain(..).map(|e| (at, e)));
        let compute_ms = started.elapsed().as_secs_f64() * 1000.0;
        audio_total += prep.audio.len() as f64 / SR as f64;
        wall_total += compute_ms / 1000.0;

        let mut turns = Vec::new();
        let mut segments = Vec::new();
        let mut partials = 0;
        let mut first_partial_s = Value::Null;
        let mut held = 0;
        for (at, e) in &events {
            match e {
                Event::Turn(t) => {
                    let mut v = serde_json::to_value(t)?;
                    v["chunk_end_s"] = json!(at);
                    turns.push(v);
                }
                Event::SegmentEnd { at_s, probability, turn_ms, turn_error } => {
                    segments.push(json!({
                        "at_s": at_s, "probability": probability,
                        "turn_ms": turn_ms, "turn_error": turn_error,
                    }));
                }
                Event::Partial { .. } => {
                    partials += 1;
                    if first_partial_s.is_null() {
                        first_partial_s = json!(at);
                    }
                }
                Event::Held { .. } => held += 1,
                _ => {}
            }
        }
        let hyp = turns
            .iter()
            .filter_map(|t| t["text"].as_str())
            .collect::<Vec<_>>()
            .join(" ");
        writeln!(
            out,
            "{}",
            json!({
                "mode": "pipeline", "tag": args.str("tag", ""), "set": clip.set, "id": clip.id,
                "asr": asr, "capture_rate": rate, "params": params,
                "ref": clip.text, "hyp": hyp, "turns": turns, "segments": segments,
                "held": held, "partials": partials, "first_partial_s": first_partial_s,
                "speech_start_s": prep.speech_start_s, "speech_end_s": prep.speech_end_s,
                "clip_s": samples.len() as f64 / SR as f64,
                "audio_s": prep.audio.len() as f64 / SR as f64,
                "noise_rms": prep.noise_rms, "compute_ms": compute_ms, "resample_ms": resample_ms,
            })
        )?;
    }
    let cpu = cpu_seconds() - cpu0;
    Ok(json!({
        "mode": "pipeline", "tag": args.str("tag", ""), "asr": asr, "clips": clips.len(),
        "load_ms": load_ms, "audio_s": audio_total, "wall_s": wall_total, "cpu_s": cpu,
        "rtf": wall_total / audio_total, "cpu_per_audio_s": cpu / audio_total,
        "peak_rss_mb": peak_rss_mb(), "onnxruntime": engine.ort_version(),
    }))
}

fn run_whole(args: &Args, clips: &[Clip], out: &mut impl Write) -> Result<Value> {
    let models = PathBuf::from(args.str("models", "../spikes/on-device-stt/.cache/models"));
    let asr = args.str("asr", "zipformer-en-2023-06-26");
    let cfg = EngineConfig {
        vad_model: models.join("silero-vad/silero_vad.onnx"),
        asr: asr_files(&models, &asr)?,
        turn_model: None,
        asr_threads: args.usize("asr-threads", 1) as i32,
        turn_threads: 1,
        tail_padding_s: tail_padding(args, &asr),
    };
    let engine = Engine::load(&cfg)?;
    let cpu0 = cpu_seconds();
    let (mut audio_total, mut wall_total) = (0.0, 0.0);
    for clip in clips {
        let t = engine.transcribe(&clip.samples()?);
        audio_total += t.audio_s;
        wall_total += t.compute_ms / 1000.0;
        writeln!(
            out,
            "{}",
            json!({
                "mode": "whole", "tag": args.str("tag", ""), "set": clip.set, "id": clip.id,
                "asr": asr, "ref": clip.text, "hyp": t.text, "audio_s": t.audio_s,
                "compute_ms": t.compute_ms,
            })
        )?;
    }
    let cpu = cpu_seconds() - cpu0;
    Ok(json!({
        "mode": "whole", "tag": args.str("tag", ""), "asr": asr, "clips": clips.len(),
        "audio_s": audio_total, "wall_s": wall_total, "cpu_s": cpu,
        "rtf": wall_total / audio_total, "cpu_per_audio_s": cpu / audio_total,
        "peak_rss_mb": peak_rss_mb(),
    }))
}

/// One offline (non-streaming) pass per clip with a NeMo transducer
/// (Parakeet TDT). Not part of the pipeline: it is the accuracy reference for
/// "what would a large model on this machine get on the same audio".
fn run_offline(args: &Args, clips: &[Clip], out: &mut impl Write) -> Result<Value> {
    use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig};
    let models = PathBuf::from(args.str("models", "../spikes/on-device-stt/.cache/models"));
    let name = args.str("asr", "parakeet-tdt-0.6b-v2");
    let dir = models.join(&name);
    let path = |f: &str| Some(dir.join(f).to_string_lossy().into_owned());
    let mut cfg = OfflineRecognizerConfig::default();
    cfg.model_config.transducer.encoder = path("encoder.int8.onnx");
    cfg.model_config.transducer.decoder = path("decoder.int8.onnx");
    cfg.model_config.transducer.joiner = path("joiner.int8.onnx");
    cfg.model_config.tokens = path("tokens.txt");
    cfg.model_config.model_type = Some("nemo_transducer".into());
    cfg.model_config.num_threads = args.usize("asr-threads", 4) as i32;
    cfg.decoding_method = Some("greedy_search".into());
    let load = Instant::now();
    let recognizer = OfflineRecognizer::create(&cfg).context("offline recognizer")?;
    let load_ms = load.elapsed().as_secs_f64() * 1000.0;
    let cpu0 = cpu_seconds();
    let (mut audio_total, mut wall_total) = (0.0, 0.0);
    for clip in clips {
        let samples = clip.samples()?;
        let started = Instant::now();
        let stream = recognizer.create_stream();
        stream.accept_waveform(SR as i32, &samples);
        recognizer.decode(&stream);
        let text = stream.get_result().map(|r| r.text).unwrap_or_default();
        let compute_ms = started.elapsed().as_secs_f64() * 1000.0;
        let audio_s = samples.len() as f64 / SR as f64;
        audio_total += audio_s;
        wall_total += compute_ms / 1000.0;
        writeln!(
            out,
            "{}",
            json!({
                "mode": "whole", "tag": args.str("tag", ""), "set": clip.set, "id": clip.id,
                "asr": name, "ref": clip.text, "hyp": text.trim(), "audio_s": audio_s,
                "compute_ms": compute_ms,
            })
        )?;
    }
    let cpu = cpu_seconds() - cpu0;
    Ok(json!({
        "mode": "whole", "tag": args.str("tag", ""), "asr": name, "clips": clips.len(),
        "load_ms": load_ms, "audio_s": audio_total, "wall_s": wall_total, "cpu_s": cpu,
        "rtf": wall_total / audio_total, "cpu_per_audio_s": cpu / audio_total,
        "peak_rss_mb": peak_rss_mb(),
    }))
}

fn run_turn(args: &Args, clips: &[Clip], out: &mut impl Write) -> Result<Value> {
    let models = PathBuf::from(args.str("models", "../spikes/on-device-stt/.cache/models"));
    let which = args.str("turn", "cpu");
    let path = turn_model(&models, &which).context("--turn off makes no sense here")?;
    let threads = args.usize("turn-threads", 4);
    let mut model = SmartTurn::load(&path, threads)?;
    let _ = model.predict(&vec![0.0; SR as usize])?; // warm-up
    let cpu0 = cpu_seconds();
    let started = Instant::now();
    for clip in clips {
        let samples = clip.samples()?;
        let s = model.predict_timed(&samples)?;
        writeln!(
            out,
            "{}",
            json!({
                "mode": "turn", "tag": args.str("tag", ""), "id": clip.id, "turn_model": which,
                "threads": threads, "endpoint": clip.meta["endpoint"], "synthetic": clip.meta["synthetic"],
                "source": clip.meta["source"], "probability": s.probability,
                "features_ms": s.features_ms, "inference_ms": s.inference_ms,
                "clip_s": samples.len() as f64 / SR as f64,
            })
        )?;
    }
    Ok(json!({
        "mode": "turn", "tag": args.str("tag", ""), "turn_model": which, "threads": threads,
        "clips": clips.len(), "wall_s": started.elapsed().as_secs_f64(),
        "cpu_s": cpu_seconds() - cpu0, "peak_rss_mb": peak_rss_mb(),
    }))
}

/// Port of `run_vad_capture` + `VadConfig::default()` in src-tauri/src/speaker/commands.rs.
fn energy_vad(device: &[f32], rate: u32) -> (Vec<f64>, usize) {
    const HOP: usize = 1024;
    const SENSITIVITY_RMS: f32 = 0.012;
    const PEAK_THRESHOLD: f32 = 0.035;
    const SILENCE_CHUNKS: usize = 45;
    const MIN_SPEECH_CHUNKS: usize = 7;
    const NOISE_GATE: f32 = 0.003;
    let (mut in_speech, mut silence, mut speech) = (false, 0usize, 0usize);
    let mut emitted = Vec::new();
    let mut discarded = 0;
    let mut consumed = 0usize;
    for chunk in device.chunks_exact(HOP) {
        consumed += HOP;
        let gated: Vec<f32> = chunk
            .iter()
            .map(|&s| {
                let a = s.abs();
                if a < NOISE_GATE {
                    s * (a / NOISE_GATE).powf(1.0 / 3.0)
                } else {
                    s
                }
            })
            .collect();
        let rms = (gated.iter().map(|v| v * v).sum::<f32>() / HOP as f32).sqrt();
        let peak = gated.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        if rms > SENSITIVITY_RMS || peak > PEAK_THRESHOLD {
            if !in_speech {
                in_speech = true;
                speech = 0;
            }
            speech += 1;
            silence = 0;
        } else if in_speech {
            silence += 1;
            if silence >= SILENCE_CHUNKS {
                if speech >= MIN_SPEECH_CHUNKS {
                    emitted.push(consumed as f64 / rate as f64);
                } else {
                    discarded += 1;
                }
                in_speech = false;
                silence = 0;
                speech = 0;
            }
        }
    }
    (emitted, discarded)
}

fn run_energy(args: &Args, clips: &[Clip], out: &mut impl Write) -> Result<Value> {
    for clip in clips {
        let prep = prepare(clip, &clip.samples()?);
        for rate in [48_000u32, 44_100, 16_000] {
            let device = to_device_rate(&prep.audio, rate);
            let (emitted, discarded) = energy_vad(&device, rate);
            writeln!(
                out,
                "{}",
                json!({
                    "mode": "energy", "tag": args.str("tag", ""), "set": clip.set, "id": clip.id,
                    "device_rate": rate, "emitted_s": emitted, "discarded": discarded,
                    "speech_end_s": prep.speech_end_s, "audio_s": prep.audio.len() as f64 / SR as f64,
                })
            )?;
        }
    }
    Ok(json!({ "mode": "energy", "tag": args.str("tag", ""), "clips": clips.len() }))
}

/// Writes Smart Turn input features for `--ids` as raw little-endian f32
/// (`<out>.<id>.f32`, shape 80 x 800) for parity checks against Python.
fn run_features(args: &Args, clips: &[Clip]) -> Result<Value> {
    let models = PathBuf::from(args.str("models", "../spikes/on-device-stt/.cache/models"));
    let mut model = SmartTurn::load(&turn_model(&models, "cpu").unwrap(), 1)?;
    let ids: Vec<String> = args.str("ids", "").split(',').map(str::to_string).collect();
    let prefix = args.str("out", "features");
    let mut written = 0;
    for clip in clips.iter().filter(|c| ids.iter().any(|id| c.id.starts_with(id.as_str()))) {
        let (feats, _) = model.features(&clip.samples()?);
        let bytes: Vec<u8> = feats.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(format!("{prefix}.{}.f32", clip.id), bytes)?;
        written += 1;
    }
    Ok(json!({ "mode": "features", "written": written }))
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.str("mode", "") == "features" {
        let data = PathBuf::from(args.str("data", "../spikes/on-device-stt/.cache/data"));
        let clips = load_clips(&data, &["smart-turn".to_string()], usize::MAX)?;
        eprintln!("{}", run_features(&args, &clips)?);
        return Ok(());
    }
    let data = PathBuf::from(args.str("data", "../spikes/on-device-stt/.cache/data"));
    let sets: Vec<String> = args
        .str("sets", "librispeech,ami-ihm,ami-sdm,earnings22")
        .split(',')
        .map(str::to_string)
        .collect();
    let clips = load_clips(&data, &sets, args.usize("limit", usize::MAX))?;
    let out_path = PathBuf::from(args.str("out", "bench.jsonl"));
    let mut out = BufWriter::new(
        std::fs::OpenOptions::new().create(true).append(true).open(&out_path)?,
    );
    let mode = args.str("mode", "pipeline");
    eprintln!("{mode}: {} clips from {}", clips.len(), sets.join(","));
    let summary = match mode.as_str() {
        "pipeline" => run_pipeline(&args, &clips, &mut out)?,
        "whole" => run_whole(&args, &clips, &mut out)?,
        "offline" => run_offline(&args, &clips, &mut out)?,
        "turn" => run_turn(&args, &clips, &mut out)?,
        "energy" => run_energy(&args, &clips, &mut out)?,
        other => bail!("unknown mode {other}"),
    };
    let mut summary = summary;
    summary["summary"] = json!(true);
    writeln!(out, "{summary}")?;
    out.flush()?;
    eprintln!("{summary}");
    Ok(())
}

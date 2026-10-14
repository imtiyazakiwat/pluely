//! End-to-end tests on real models and real speech.
//!
//! They need the downloaded models and test clips from
//! `spikes/on-device-stt` (`prepare_data.py`, or the app's own download).
//! Override the locations with CLOCKS_MODELS_DIR / CLOCKS_TEST_DATA_DIR.
//! When either is missing the tests print a note and return, so a plain
//! `cargo test` still works on a fresh checkout.

use clocks_local_stt::{
    models, Engine, Event, Pipeline, PipelineParams, Resampler, SmartTurn, TurnEnd,
};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

const SR: usize = 16_000;

fn cache() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spikes/on-device-stt/.cache")
}

fn models_dir() -> PathBuf {
    std::env::var_os("CLOCKS_MODELS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| cache().join("models"))
}

fn data_dir() -> PathBuf {
    std::env::var_os("CLOCKS_TEST_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| cache().join("data"))
}

/// The engine is expensive to build and ONNX Runtime allows one environment
/// per process, so every test shares one.
fn engine() -> Option<Arc<Engine>> {
    static ENGINE: OnceLock<Option<Arc<Engine>>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let root = models_dir();
            let missing: Vec<_> = models::DEFAULT_BUNDLE
                .iter()
                .flat_map(|spec| models::missing_files(&root, spec))
                .collect();
            if !missing.is_empty() {
                eprintln!("SKIP: models not found in {}: {missing:?}", root.display());
                return None;
            }
            let cfg = models::default_engine_config(&root);
            Some(Arc::new(Engine::load(&cfg).expect("engine loads")))
        })
        .clone()
}

fn read_wav(path: &std::path::Path) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path).expect("open wav");
    assert_eq!(reader.spec().sample_rate as usize, SR);
    reader
        .samples::<i16>()
        .map(|s| s.unwrap() as f32 / 32768.0)
        .collect()
}

/// `(audio, reference text)` of the first LibriSpeech clip, if present.
fn librispeech_clip(index: usize) -> Option<(Vec<f32>, String)> {
    let manifest = data_dir().join("librispeech/manifest.jsonl");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        eprintln!("SKIP: test data not found at {}", manifest.display());
        return None;
    };
    let line: serde_json::Value =
        serde_json::from_str(text.lines().nth(index).expect("clip exists")).unwrap();
    let audio = read_wav(&data_dir().join(line["path"].as_str().unwrap()));
    Some((audio, line["text"].as_str().unwrap().to_string()))
}

/// Pads a clip like a call: half a second of room tone in front, silence after.
fn padded(clip: &[f32], trail_s: f32) -> Vec<f32> {
    let mut audio = vec![0.0; SR / 2];
    audio.extend_from_slice(clip);
    audio.extend(std::iter::repeat(0.0).take((trail_s * SR as f32) as usize));
    audio
}

/// Feeds 16 kHz audio the way the capture loop does: at 48 kHz, 1024 samples
/// at a time, through the resampler.
fn run_pipeline(engine: &Arc<Engine>, audio16k: &[f32], params: PipelineParams) -> Vec<Event> {
    let up = sherpa_onnx_upsample(audio16k, 48_000);
    let mut resampler = Resampler::new(48_000).unwrap();
    let mut pipeline = Pipeline::new(engine.clone(), params).unwrap();
    let mut events = Vec::new();
    for chunk in up.chunks(1024) {
        pipeline.accept(&resampler.process(chunk), &mut events);
    }
    pipeline.finish(&mut events);
    events
}

/// Linear-interpolation 3x upsample; good enough to exercise the 48 kHz path.
fn sherpa_onnx_upsample(audio: &[f32], rate: usize) -> Vec<f32> {
    assert_eq!(rate, SR * 3);
    let mut out = Vec::with_capacity(audio.len() * 3);
    for pair in audio.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        out.push(a);
        out.push(a + (b - a) / 3.0);
        out.push(a + 2.0 * (b - a) / 3.0);
    }
    out.push(*audio.last().unwrap_or(&0.0));
    out
}

fn turns(events: &[Event]) -> Vec<&clocks_local_stt::TurnResult> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Turn(t) => Some(t),
            _ => None,
        })
        .collect()
}

#[test]
fn transcribes_a_whole_clip() {
    let (Some(engine), Some((clip, _))) = (engine(), librispeech_clip(0)) else { return };
    let out = engine.transcribe(&clip);
    let text = out.text.to_lowercase();
    assert!(text.contains("quilter"), "got: {}", out.text);
    assert!(text.contains("gospel"), "got: {}", out.text);
    assert!(out.compute_ms < out.audio_s * 1000.0, "slower than real time");
}

#[test]
fn pipeline_releases_one_turn_with_the_whole_sentence() {
    let (Some(engine), Some((clip, _))) = (engine(), librispeech_clip(0)) else { return };
    let events = run_pipeline(&engine, &padded(&clip, 3.0), PipelineParams::default());

    assert!(
        events.iter().any(|e| matches!(e, Event::SpeechStart { .. })),
        "no speech-start event"
    );
    assert!(
        events.iter().any(|e| matches!(e, Event::Partial { .. })),
        "no live partial text"
    );
    let turns = turns(&events);
    assert_eq!(turns.len(), 1, "expected one turn, got {turns:?}");
    let turn = turns[0];
    let text = turn.text.to_lowercase();
    assert!(text.contains("quilter") && text.contains("gospel"), "got: {}", turn.text);
    assert!(
        matches!(turn.end, TurnEnd::Complete | TurnEnd::HoldTimeout),
        "unexpected end {:?}",
        turn.end
    );
    // Sentence case, not the model's raw output.
    assert!(turn.text.chars().next().unwrap().is_uppercase());
}

#[test]
fn turn_is_released_quickly_after_the_speaker_stops() {
    let (Some(engine), Some((clip, _))) = (engine(), librispeech_clip(1)) else { return };
    let audio = padded(&clip, 3.0);
    let speech_end_s = 0.5 + clip.len() as f64 / SR as f64;
    let events = run_pipeline(&engine, &audio, PipelineParams::default());
    let turns = turns(&events);
    assert_eq!(turns.len(), 1, "expected one turn, got {turns:?}");
    let turn = turns[0];
    if turn.end == TurnEnd::Complete {
        // VAD pause + decision. Generous bound: the benchmark p95 is ~1.2 s.
        let wait = turn.released_s - speech_end_s;
        assert!(wait < 1.5, "released {wait:.2}s after the speaker stopped");
    }
    assert!(turn.decision_ms < 500.0, "decision took {} ms", turn.decision_ms);
}

#[test]
fn background_noise_produces_no_turns() {
    let Some(engine) = engine() else { return };
    // Quiet pseudo-random hiss for 4 s.
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let noise: Vec<f32> = (0..4 * SR)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.004
        })
        .collect();
    let events = run_pipeline(&engine, &noise, PipelineParams::default());
    assert!(turns(&events).is_empty(), "noise became a turn: {events:?}");
}

#[test]
fn input_ending_mid_speech_still_returns_the_words() {
    let (Some(engine), Some((clip, _))) = (engine(), librispeech_clip(0)) else { return };
    // Capture stopped 2 s into the sentence: no trailing silence at all.
    let cut = padded(&clip[..2 * SR], 0.0);
    let events = run_pipeline(&engine, &cut, PipelineParams::default());
    let turns = turns(&events);
    assert_eq!(turns.len(), 1, "got {turns:?}");
    assert_eq!(turns[0].end, TurnEnd::EndOfInput);
    assert!(turns[0].text.to_lowercase().contains("quilter"), "got: {}", turns[0].text);
}

#[test]
fn without_a_turn_model_the_first_pause_ends_the_turn() {
    let (Some(engine), Some((clip, _))) = (engine(), librispeech_clip(0)) else { return };
    let params = PipelineParams {
        use_turn_detector: false,
        ..Default::default()
    };
    let events = run_pipeline(&engine, &padded(&clip, 3.0), params);
    let turns = turns(&events);
    assert!(!turns.is_empty());
    assert!(turns.iter().all(|t| t.end == TurnEnd::VadOnly || t.end == TurnEnd::EndOfInput));
}

#[test]
fn smart_turn_separates_finished_from_unfinished_clips() {
    let root = models_dir();
    let model = root.join("smart-turn-v3.2/smart-turn-v3.2-cpu.onnx");
    let manifest = data_dir().join("smart-turn/manifest.jsonl");
    let (Ok(text), true) = (std::fs::read_to_string(&manifest), model.exists()) else {
        eprintln!("SKIP: smart-turn model or data missing");
        return;
    };
    let mut turn = SmartTurn::load(&model, 2).expect("model loads");
    let (mut right, mut seen) = (0, 0);
    let mut per_label = [0usize; 2];
    for line in text.lines() {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        let finished = row["endpoint"].as_bool().unwrap();
        if per_label[finished as usize] >= 25 {
            continue;
        }
        per_label[finished as usize] += 1;
        let audio = read_wav(&data_dir().join(row["path"].as_str().unwrap()));
        let p = turn.predict(&audio).unwrap();
        assert!((0.0..=1.0).contains(&p));
        seen += 1;
        right += ((p >= 0.5) == finished) as usize;
    }
    let accuracy = right as f64 / seen as f64;
    // Measured 93% on 400 clips; the floor leaves room for a small sample.
    assert!(accuracy >= 0.85, "accuracy {accuracy:.2} over {seen} clips");
}

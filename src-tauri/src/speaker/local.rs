// Clocks on-device STT capture loop (issue #1).
//
// The async task only moves samples; a dedicated thread runs
// resample -> Silero VAD -> streaming Zipformer -> Smart Turn, so inference
// never blocks the tokio runtime. Results go to the frontend as events:
//
//   speech-start         a turn opened
//   local-stt-partial    live hypothesis (string)
//   local-stt-held       Smart Turn expects more speech ({ text, probability })
//   speech-transcribed   a finished turn (clocks_local_stt::TurnResult)
//   speech-discarded     speech that produced no words
use crate::local_stt::LocalEngine;
use futures_util::StreamExt;
use tauri::AppHandle;

/// Device-rate samples per hand-off; matches `VadConfig::hop_size`.
#[cfg_attr(not(feature = "local-stt"), allow(dead_code))]
const CHUNK: usize = 1024;

#[cfg(feature = "local-stt")]
pub struct LocalSession {
    resampler: clocks_local_stt::Resampler,
    pipeline: clocks_local_stt::Pipeline,
}

#[cfg(not(feature = "local-stt"))]
pub type LocalSession = std::convert::Infallible;

/// Pipeline settings for the app, chosen from the spike measurements in
/// spikes/on-device-stt/REPORT.md.
#[cfg(feature = "local-stt")]
fn app_params() -> clocks_local_stt::PipelineParams {
    clocks_local_stt::PipelineParams {
        vad_threshold: 0.5,
        min_silence_s: 0.2,
        max_hold_s: 1.5,
        ..Default::default()
    }
}

/// Builds the resampler and pipeline up front so the caller can fall back to
/// the energy VAD + STT provider path if anything is wrong.
#[cfg(feature = "local-stt")]
pub fn prepare(engine: LocalEngine, sample_rate: u32) -> Result<LocalSession, String> {
    let resampler = clocks_local_stt::Resampler::new(sample_rate).map_err(|e| e.to_string())?;
    let pipeline =
        clocks_local_stt::Pipeline::new(engine, app_params()).map_err(|e| format!("{e:#}"))?;
    Ok(LocalSession { resampler, pipeline })
}

#[cfg(not(feature = "local-stt"))]
pub fn prepare(engine: LocalEngine, _sample_rate: u32) -> Result<LocalSession, String> {
    match engine {}
}

#[cfg(feature = "local-stt")]
pub async fn run_local_capture(
    app: AppHandle,
    stream: impl StreamExt<Item = f32> + Unpin,
    session: LocalSession,
) {
    use std::sync::mpsc;
    use tauri::Emitter;
    use tracing::error;

    let mut stream = stream;
    let (tx, rx) = mpsc::channel::<Vec<f32>>();
    let worker_app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("clocks-local-stt".into())
        .spawn(move || worker(worker_app, rx, session));
    if let Err(e) = spawned {
        error!("failed to start the on-device STT thread: {e}");
        let _ = app.emit("local-stt-fallback", format!("could not start: {e}"));
        return;
    }

    let mut chunk = Vec::with_capacity(CHUNK);
    while let Some(sample) = stream.next().await {
        chunk.push(sample);
        if chunk.len() == CHUNK
            && tx
                .send(std::mem::replace(&mut chunk, Vec::with_capacity(CHUNK)))
                .is_err()
        {
            break;
        }
    }
    // Dropping `tx` (here, or when stop_system_audio_capture aborts this task)
    // ends the worker. An unfinished turn is dropped, as the energy path drops
    // its buffer.
}

#[cfg(not(feature = "local-stt"))]
pub async fn run_local_capture(
    _app: AppHandle,
    _stream: impl StreamExt<Item = f32> + Unpin,
    session: LocalSession,
) {
    match session {}
}

#[cfg(feature = "local-stt")]
fn worker(app: AppHandle, rx: std::sync::mpsc::Receiver<Vec<f32>>, mut session: LocalSession) {
    let mut events = Vec::new();
    while let Ok(chunk) = rx.recv() {
        let samples = session.resampler.process(&chunk);
        session.pipeline.accept(&samples, &mut events);
        for event in events.drain(..) {
            forward(&app, event);
        }
    }
}

#[cfg(feature = "local-stt")]
fn forward(app: &AppHandle, event: clocks_local_stt::Event) {
    use clocks_local_stt::Event;
    use tauri::Emitter;
    use tracing::warn;

    let sent = match event {
        Event::SpeechStart { .. } => app.emit("speech-start", ()),
        Event::Partial { text } => app.emit("local-stt-partial", text),
        Event::Held { text, probability } => app.emit(
            "local-stt-held",
            serde_json::json!({ "text": text, "probability": probability }),
        ),
        Event::Turn(turn) => app.emit("speech-transcribed", turn),
        Event::Discarded { .. } => app.emit("speech-discarded", "No words recognised"),
        Event::SegmentEnd {
            turn_error: Some(err),
            ..
        } => {
            warn!("smart-turn failed, sending the turn anyway: {err}");
            Ok(())
        }
        Event::SegmentEnd { .. } => Ok(()),
    };
    if let Err(e) = sent {
        warn!("failed to emit on-device STT event: {e}");
    }
}

//! On-device speech-to-text (beta): model download, engine lifecycle and the
//! commands the frontend calls. The engine itself is the `clocks-local-stt`
//! crate in `src-tauri/local-stt`.
//!
//! Models (~81 MB) are downloaded only when the user asks, into
//! `<app data>/models`, from pinned URLs, and verified by SHA-256. Audio never
//! leaves the machine: it is transcribed in-process.

use serde::Serialize;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

/// What the on-device capture loop runs on. Uninhabited when the feature is
/// compiled out, so the local branch type-checks but can never be taken.
#[cfg(feature = "local-stt")]
pub type LocalEngine = std::sync::Arc<clocks_local_stt::Engine>;
#[cfg(not(feature = "local-stt"))]
pub type LocalEngine = std::convert::Infallible;

#[derive(Default)]
pub struct LocalSttState {
    #[cfg(feature = "local-stt")]
    engine: tokio::sync::Mutex<Option<LocalEngine>>,
    #[cfg_attr(not(feature = "local-stt"), allow(dead_code))]
    downloading: std::sync::atomic::AtomicBool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    id: String,
    title: String,
    license: String,
    bytes: u64,
    ready: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalSttStatus {
    /// Whether this build includes on-device STT at all.
    available: bool,
    models_ready: bool,
    downloading: bool,
    /// Models are loaded in memory (after first use).
    loaded: bool,
    models_dir: String,
    total_bytes: u64,
    missing_bytes: u64,
    models: Vec<ModelStatus>,
    /// ONNX Runtime version, once loaded.
    onnxruntime: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    file: String,
    done_bytes: u64,
    total_bytes: u64,
    finished: bool,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalTranscript {
    text: String,
    audio_s: f64,
    compute_ms: f64,
}

fn models_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("models"))
        .map_err(|e| format!("cannot resolve the app data directory: {e}"))
}

#[cfg(feature = "local-stt")]
mod imp {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use clocks_local_stt::models::{self, ModelFile, DEFAULT_BUNDLE};
    use clocks_local_stt::{Engine, Resampler};
    use sha2::{Digest, Sha256};
    use std::path::Path;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::Duration;
    use tauri::Emitter;
    use tokio::io::AsyncWriteExt;

    const PROGRESS_EVENT: &str = "local-stt-download";

    fn missing(root: &Path) -> Vec<(PathBuf, &'static ModelFile)> {
        DEFAULT_BUNDLE
            .iter()
            .flat_map(|spec| {
                let absent = models::missing_files(root, spec);
                spec.files
                    .iter()
                    .filter(move |f| absent.contains(&f.name))
                    .map(move |f| (spec.file(root, f.name), f))
            })
            .collect()
    }

    pub async fn status(app: &AppHandle) -> Result<LocalSttStatus, String> {
        let root = models_dir(app)?;
        let state = app.state::<LocalSttState>();
        let absent = missing(&root);
        let models = DEFAULT_BUNDLE
            .iter()
            .map(|spec| ModelStatus {
                id: spec.id.into(),
                title: spec.title.into(),
                license: spec.license.into(),
                bytes: spec.total_bytes(),
                ready: models::missing_files(&root, spec).is_empty(),
            })
            .collect();
        let onnxruntime = state
            .engine
            .lock()
            .await
            .as_ref()
            .map(|e| e.ort_version().to_string());
        Ok(LocalSttStatus {
            available: true,
            models_ready: absent.is_empty(),
            downloading: state.downloading.load(Ordering::Acquire),
            loaded: onnxruntime.is_some(),
            models_dir: root.display().to_string(),
            total_bytes: DEFAULT_BUNDLE.iter().map(|s| s.total_bytes()).sum(),
            missing_bytes: absent.iter().map(|(_, f)| f.bytes).sum(),
            models,
            onnxruntime,
        })
    }

    /// The shared engine, loading it on first use (~0.7 s on an M4).
    pub async fn engine(app: &AppHandle) -> Result<LocalEngine, String> {
        let state = app.state::<LocalSttState>();
        let mut slot = state.engine.lock().await;
        if let Some(engine) = slot.as_ref() {
            return Ok(engine.clone());
        }
        let root = models_dir(app)?;
        if !missing(&root).is_empty() {
            return Err("on-device models are not downloaded".into());
        }
        let cfg = models::default_engine_config(&root);
        let engine = tokio::task::spawn_blocking(move || Engine::load(&cfg))
            .await
            .map_err(|e| format!("model loader crashed: {e}"))?
            .map_err(|e| format!("failed to load on-device models: {e:#}"))?;
        let engine = Arc::new(engine);
        *slot = Some(engine.clone());
        Ok(engine)
    }

    pub async fn download(app: &AppHandle) -> Result<(), String> {
        let state = app.state::<LocalSttState>();
        if state.downloading.swap(true, Ordering::AcqRel) {
            return Err("a model download is already running".into());
        }
        let result = download_missing(app).await;
        state.downloading.store(false, Ordering::Release);
        let _ = app.emit(
            PROGRESS_EVENT,
            DownloadProgress {
                file: String::new(),
                done_bytes: 0,
                total_bytes: 0,
                finished: true,
                error: result.as_ref().err().cloned(),
            },
        );
        result
    }

    async fn download_missing(app: &AppHandle) -> Result<(), String> {
        let root = models_dir(app)?;
        let files = missing(&root);
        let total: u64 = files.iter().map(|(_, f)| f.bytes).sum();
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| format!("HTTP client: {e}"))?;
        let mut done = 0;
        for (dest, file) in files {
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            fetch(app, &client, file, &dest, done, total).await?;
            done += file.bytes;
        }
        Ok(())
    }

    /// Streams one file to `<name>.part`, hashing as it goes, and only moves
    /// it into place if size and SHA-256 match the registry.
    async fn fetch(
        app: &AppHandle,
        client: &reqwest::Client,
        file: &ModelFile,
        dest: &Path,
        done_before: u64,
        total: u64,
    ) -> Result<(), String> {
        let part = dest.with_file_name(format!("{}.part", file.name));
        let fail = |msg: String| format!("{}: {msg}", file.name);
        let mut response = client
            .get(file.url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| fail(e.to_string()))?;
        let mut out = tokio::fs::File::create(&part)
            .await
            .map_err(|e| fail(e.to_string()))?;
        let mut hasher = Sha256::new();
        let (mut got, mut reported) = (0u64, 0u64);
        while let Some(chunk) = response.chunk().await.map_err(|e| fail(e.to_string()))? {
            got += chunk.len() as u64;
            if got > file.bytes {
                drop(out);
                let _ = tokio::fs::remove_file(&part).await;
                return Err(fail("server sent more data than expected".into()));
            }
            hasher.update(&chunk);
            out.write_all(&chunk).await.map_err(|e| fail(e.to_string()))?;
            if got - reported >= 512 * 1024 || got == file.bytes {
                reported = got;
                let _ = app.emit(
                    PROGRESS_EVENT,
                    DownloadProgress {
                        file: file.name.into(),
                        done_bytes: done_before + got,
                        total_bytes: total,
                        finished: false,
                        error: None,
                    },
                );
            }
        }
        out.flush().await.map_err(|e| fail(e.to_string()))?;
        drop(out);
        let digest: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if got != file.bytes || digest != file.sha256 {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(fail("download is corrupt (size or checksum mismatch)".into()));
        }
        tokio::fs::rename(&part, dest)
            .await
            .map_err(|e| fail(e.to_string()))
    }

    pub async fn delete(app: &AppHandle) -> Result<(), String> {
        let state = app.state::<LocalSttState>();
        if state.downloading.load(Ordering::Acquire) {
            return Err("wait for the download to finish".into());
        }
        // A capture already running keeps its own handle to the engine.
        *state.engine.lock().await = None;
        let root = models_dir(app)?;
        match tokio::fs::remove_dir_all(&root).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("cannot delete {}: {e}", root.display())),
        }
    }

    pub async fn transcribe(app: &AppHandle, audio_base64: String) -> Result<LocalTranscript, String> {
        let engine = engine(app).await?;
        tokio::task::spawn_blocking(move || {
            let bytes = B64
                .decode(audio_base64.trim())
                .map_err(|e| format!("invalid audio payload: {e}"))?;
            let (samples, rate) = decode_wav(&bytes)?;
            let mut resampler = Resampler::new(rate).map_err(|e| e.to_string())?;
            let mut audio = resampler.process(&samples);
            audio.extend(resampler.flush());
            let t = engine.transcribe(&audio);
            Ok(LocalTranscript {
                text: t.text,
                audio_s: t.audio_s,
                compute_ms: t.compute_ms,
            })
        })
        .await
        .map_err(|e| format!("transcription crashed: {e}"))?
    }

    /// Any PCM WAV (int or float, any rate, any channel count) -> mono f32.
    fn decode_wav(bytes: &[u8]) -> Result<(Vec<f32>, u32), String> {
        let mut reader = hound::WavReader::new(std::io::Cursor::new(bytes))
            .map_err(|e| format!("expected WAV audio: {e}"))?;
        let spec = reader.spec();
        let channels = spec.channels.max(1) as usize;
        let interleaved: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>(),
            hound::SampleFormat::Int => {
                let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(8, 32) - 1)) as f32;
                reader
                    .samples::<i32>()
                    .map(|s| s.map(|v| v as f32 * scale))
                    .collect::<Result<_, _>>()
            }
        }
        .map_err(|e| format!("corrupt WAV: {e}"))?;
        let mono = interleaved
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect();
        Ok((mono, spec.sample_rate))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn wav(spec: hound::WavSpec, write: impl FnOnce(&mut hound::WavWriter<&mut std::io::Cursor<Vec<u8>>>)) -> Vec<u8> {
            let mut cursor = std::io::Cursor::new(Vec::new());
            let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
            write(&mut writer);
            writer.finalize().unwrap();
            cursor.into_inner()
        }

        #[test]
        fn mixes_stereo_int16_down_to_mono() {
            let spec = hound::WavSpec {
                channels: 2,
                sample_rate: 48_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let bytes = wav(spec, |w| {
                for (l, r) in [(16384i16, 0i16), (-16384, -16384), (0, 0)] {
                    w.write_sample(l).unwrap();
                    w.write_sample(r).unwrap();
                }
            });
            let (mono, rate) = decode_wav(&bytes).unwrap();
            assert_eq!(rate, 48_000);
            assert_eq!(mono.len(), 3);
            assert!((mono[0] - 0.25).abs() < 1e-4, "{mono:?}");
            assert!((mono[1] + 0.5).abs() < 1e-4, "{mono:?}");
            assert_eq!(mono[2], 0.0);
        }

        #[test]
        fn reads_float_wav_unchanged() {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            };
            let bytes = wav(spec, |w| {
                for v in [0.5f32, -0.25, 1.0] {
                    w.write_sample(v).unwrap();
                }
            });
            let (mono, rate) = decode_wav(&bytes).unwrap();
            assert_eq!((rate, mono), (16_000, vec![0.5, -0.25, 1.0]));
        }

        #[test]
        fn rejects_non_wav_audio() {
            // MediaRecorder output (webm) must be converted by the frontend first.
            let err = decode_wav(b"\x1a\x45\xdf\xa3 not a wav file").unwrap_err();
            assert!(err.contains("WAV"), "{err}");
        }
    }
}

#[cfg(not(feature = "local-stt"))]
mod imp {
    use super::*;

    const UNAVAILABLE: &str = "this build of Clocks does not include on-device speech recognition";

    pub async fn status(app: &AppHandle) -> Result<LocalSttStatus, String> {
        Ok(LocalSttStatus {
            available: false,
            models_ready: false,
            downloading: false,
            loaded: false,
            models_dir: models_dir(app)?.display().to_string(),
            total_bytes: 0,
            missing_bytes: 0,
            models: Vec::new(),
            onnxruntime: None,
        })
    }
    pub async fn engine(_app: &AppHandle) -> Result<LocalEngine, String> {
        Err(UNAVAILABLE.into())
    }
    pub async fn download(_app: &AppHandle) -> Result<(), String> {
        Err(UNAVAILABLE.into())
    }
    pub async fn delete(_app: &AppHandle) -> Result<(), String> {
        Err(UNAVAILABLE.into())
    }
    pub async fn transcribe(_app: &AppHandle, _audio: String) -> Result<LocalTranscript, String> {
        Err(UNAVAILABLE.into())
    }
}

pub use imp::engine;

#[tauri::command]
pub async fn local_stt_status(app: AppHandle) -> Result<LocalSttStatus, String> {
    imp::status(&app).await
}

/// Downloads any missing model files. Progress arrives as
/// `local-stt-download` events; the last one has `finished: true`.
#[tauri::command]
pub async fn local_stt_download(app: AppHandle) -> Result<(), String> {
    imp::download(&app).await
}

#[tauri::command]
pub async fn local_stt_delete(app: AppHandle) -> Result<(), String> {
    imp::delete(&app).await
}

/// Transcribes a finished WAV clip (the mic path and manual capture mode).
#[tauri::command]
pub async fn local_stt_transcribe(
    app: AppHandle,
    audio_base64: String,
) -> Result<LocalTranscript, String> {
    imp::transcribe(&app, audio_base64).await
}

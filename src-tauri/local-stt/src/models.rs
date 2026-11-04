//! Model registry: what to download, from where, and how to verify it.
//!
//! URLs are pinned to a commit (Hugging Face) or a release asset (GitHub) and
//! every file carries a SHA-256, so a model directory is either exactly what
//! was benchmarked or it is rejected.

use crate::engine::{EngineConfig, TransducerFiles};
use serde::Serialize;
use std::path::Path;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct ModelFile {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct ModelSpec {
    /// Directory name under the models root.
    pub id: &'static str,
    pub title: &'static str,
    pub license: &'static str,
    pub files: &'static [ModelFile],
}

impl ModelSpec {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }

    pub fn file<'a>(&self, root: &'a Path, name: &str) -> std::path::PathBuf {
        root.join(self.id).join(name)
    }
}

pub const SILERO_VAD: ModelSpec = ModelSpec {
    id: "silero-vad",
    title: "Silero VAD",
    license: "MIT",
    files: &[ModelFile {
        name: "silero_vad.onnx",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx",
        sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
        bytes: 643_854,
    }],
};

pub const SMART_TURN_V3_2: ModelSpec = ModelSpec {
    id: "smart-turn-v3.2",
    title: "Smart Turn v3.2 (CPU, int8)",
    license: "BSD-2-Clause",
    files: &[ModelFile {
        name: "smart-turn-v3.2-cpu.onnx",
        url: "https://huggingface.co/pipecat-ai/smart-turn-v3/resolve/f766f81d3cfdf7737ac64aad813d91bbfd56bf93/smart-turn-v3.2-cpu.onnx",
        sha256: "2bb026316b14a660486a75b1733cd3fbab8c2fd0314dc9af7be49f8cca967e4f",
        bytes: 8_679_182,
    }],
};

macro_rules! hf_file {
    ($repo:literal, $commit:literal, $name:literal, $sha256:literal, $bytes:literal) => {
        ModelFile {
            name: $name,
            url: concat!("https://huggingface.co/", $repo, "/resolve/", $commit, "/", $name),
            sha256: $sha256,
            bytes: $bytes,
        }
    };
}

/// A streaming transducer. `spec.files` is `[encoder, decoder, joiner, tokens]`.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct AsrModel {
    pub spec: ModelSpec,
    /// Silence appended when finalising a turn. Must cover the encoder's
    /// chunk plus look-ahead (`T` in the model metadata) minus the
    /// `min_silence_s` of real audio the VAD already fed it.
    pub tail_padding_s: f32,
}

/// Streaming Zipformer, English, LibriSpeech 960 h, int8. 320 ms chunks
/// (`decode_chunk_len` 32, `T` 45). The model the issue proposed; accurate on
/// read speech, weak on meetings and calls (see the spike report).
pub const ZIPFORMER_EN_2023_06_26: AsrModel = AsrModel {
    spec: ModelSpec {
        id: "zipformer-en-2023-06-26",
        title: "Streaming Zipformer (English, LibriSpeech, int8)",
        license: "Apache-2.0",
        files: &[
            hf_file!(
                "csukuangfj/sherpa-onnx-streaming-zipformer-en-2023-06-26",
                "672fbf1b30579d6585301139bb363f42a0ad4a24",
                "encoder-epoch-99-avg-1-chunk-16-left-128.int8.onnx",
                "563fde436d16cf7607cf408cd6b30909819d03162652ef389c2450ced3f45ac1",
                71_083_163
            ),
            hf_file!(
                "csukuangfj/sherpa-onnx-streaming-zipformer-en-2023-06-26",
                "672fbf1b30579d6585301139bb363f42a0ad4a24",
                "decoder-epoch-99-avg-1-chunk-16-left-128.int8.onnx",
                "98da299f471e38bb4e1a8df579b8cc9122d6039576a77e357b3c60f17dd83b02",
                1_307_236
            ),
            hf_file!(
                "csukuangfj/sherpa-onnx-streaming-zipformer-en-2023-06-26",
                "672fbf1b30579d6585301139bb363f42a0ad4a24",
                "joiner-epoch-99-avg-1-chunk-16-left-128.int8.onnx",
                "d944208d660d67c8d72cd2acaeac971fa5ceb8c80e76c1968148846fedd6e297",
                259_335
            ),
            hf_file!(
                "csukuangfj/sherpa-onnx-streaming-zipformer-en-2023-06-26",
                "672fbf1b30579d6585301139bb363f42a0ad4a24",
                "tokens.txt",
                "49e3c2646595fd907228b3c6787069658f67b17377c60aeb8619c4551b2316fb",
                5_048
            ),
        ],
    },
    tail_padding_s: 0.3,
};

/// Banafo Kroko community model: a streaming Zipformer trained on more
/// varied English than LibriSpeech. 1.28 s chunks (`decode_chunk_len` 128,
/// `T` 141), so it needs a longer flush.
pub const KROKO_EN_2025_08_06: AsrModel = AsrModel {
    spec: ModelSpec {
        id: "kroko-en-2025-08-06",
        title: "Kroko streaming Zipformer (English)",
        license: "CC-BY-SA (Banafo Kroko community model)",
        files: &[
            hf_file!(
                "csukuangfj/sherpa-onnx-streaming-zipformer-en-kroko-2025-08-06",
                "572aaf4e2e0c603c3fc2a574d096e755a178faa1",
                "encoder.onnx",
                "d4881c57449d581e0770fd53fa66c2fdc6cd167d92ece7c715e603defc96d9d4",
                70_092_599
            ),
            hf_file!(
                "csukuangfj/sherpa-onnx-streaming-zipformer-en-kroko-2025-08-06",
                "572aaf4e2e0c603c3fc2a574d096e755a178faa1",
                "decoder.onnx",
                "455ba38466fce8d5a57e7db68a323b684079ca4d9e1dd93a740d9b2429aae3b1",
                617_488
            ),
            hf_file!(
                "csukuangfj/sherpa-onnx-streaming-zipformer-en-kroko-2025-08-06",
                "572aaf4e2e0c603c3fc2a574d096e755a178faa1",
                "joiner.onnx",
                "d406f616736350e2a7df3e39398b78eb2fc1a2ca6973a19d3853fa3227e25b52",
                336_817
            ),
            hf_file!(
                "csukuangfj/sherpa-onnx-streaming-zipformer-en-kroko-2025-08-06",
                "572aaf4e2e0c603c3fc2a574d096e755a178faa1",
                "tokens.txt",
                "396dbeb5f4858875690716084f54e90d339679d0ba3e6b5b584f3d7589254d2d",
                6_310
            ),
        ],
    },
    tail_padding_s: 1.3,
};

/// The recognizer the app ships with.
pub const DEFAULT_ASR: AsrModel = KROKO_EN_2025_08_06;

/// Everything the on-device pipeline needs, in download order (small first).
pub const DEFAULT_BUNDLE: &[ModelSpec] = &[SILERO_VAD, SMART_TURN_V3_2, DEFAULT_ASR.spec];

/// Engine configuration for the default bundle under `root`.
pub fn default_engine_config(root: &Path) -> EngineConfig {
    let asr = &DEFAULT_ASR.spec;
    EngineConfig {
        vad_model: SILERO_VAD.file(root, "silero_vad.onnx"),
        asr: TransducerFiles {
            encoder: asr.file(root, asr.files[0].name),
            decoder: asr.file(root, asr.files[1].name),
            joiner: asr.file(root, asr.files[2].name),
            tokens: asr.file(root, asr.files[3].name),
        },
        turn_model: Some(SMART_TURN_V3_2.file(root, "smart-turn-v3.2-cpu.onnx")),
        // One recognizer thread keeps ONNX Runtime from parking a pool while
        // the app listens. Smart Turn only runs at pauses; 4 threads bring it
        // from ~70 ms to ~24 ms on an M4.
        asr_threads: 1,
        turn_threads: 4,
        tail_padding_s: DEFAULT_ASR.tail_padding_s,
    }
}

/// Files of `spec` that are missing or have the wrong size under `root`.
/// (Checksums are verified at download time; size is the cheap startup check.)
pub fn missing_files(root: &Path, spec: &ModelSpec) -> Vec<&'static str> {
    spec.files
        .iter()
        .filter(|f| {
            std::fs::metadata(spec.file(root, f.name))
                .map(|m| m.len() != f.bytes)
                .unwrap_or(true)
        })
        .map(|f| f.name)
        .collect()
}

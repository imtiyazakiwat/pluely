#!/usr/bin/env python3
"""Reference Smart Turn scores, computed exactly like pipecat-ai/smart-turn's
`inference.py` (transformers WhisperFeatureExtractor + onnxruntime).

Used to check that the Rust port in src-tauri/local-stt/src/{mel,turn}.rs gives
the same probabilities on the same clips.

  .cache/venv/bin/python smart_turn_reference.py --out .cache/results/turn-reference.jsonl
"""

import argparse
import json
from pathlib import Path

import numpy as np
import onnxruntime as ort
import soundfile as sf
from transformers import WhisperFeatureExtractor

HERE = Path(__file__).parent


def truncate_audio_to_last_n_seconds(audio_array, n_seconds=8, sample_rate=16000):
    """Verbatim behaviour of smart-turn's audio_utils.py."""
    max_samples = n_seconds * sample_rate
    if len(audio_array) > max_samples:
        return audio_array[-max_samples:]
    if len(audio_array) < max_samples:
        padding = max_samples - len(audio_array)
        return np.pad(audio_array, (padding, 0), mode="constant", constant_values=0)
    return audio_array


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--data", type=Path, default=HERE / ".cache" / "data")
    parser.add_argument("--model", type=Path,
                        default=HERE / ".cache" / "models" / "smart-turn-v3.2" / "smart-turn-v3.2-cpu.onnx")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()

    so = ort.SessionOptions()
    so.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    so.inter_op_num_threads = 1
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    session = ort.InferenceSession(str(args.model), sess_options=so)
    extractor = WhisperFeatureExtractor(chunk_length=8)

    args.out.parent.mkdir(parents=True, exist_ok=True)
    rows = [json.loads(l) for l in open(args.data / "smart-turn" / "manifest.jsonl")]
    with open(args.out, "w") as fh:
        for row in rows:
            audio, sr = sf.read(args.data / row["path"], dtype="float32")
            assert sr == 16000
            audio = truncate_audio_to_last_n_seconds(audio, n_seconds=8)
            inputs = extractor(audio, sampling_rate=16000, return_tensors="np", padding="max_length",
                               max_length=8 * 16000, truncation=True, do_normalize=True)
            feats = np.expand_dims(inputs.input_features.squeeze(0).astype(np.float32), axis=0)
            prob = float(session.run(None, {"input_features": feats})[0][0].item())
            fh.write(json.dumps({"id": row["id"], "probability": prob}) + "\n")
    print(f"wrote {len(rows)} reference scores to {args.out}")


if __name__ == "__main__":
    main()

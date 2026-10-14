#!/usr/bin/env bash
# Runs every benchmark used in REPORT.md. Takes ~30 min on an M4.
#
#   cd spikes/on-device-stt && ./run_matrix.sh && .cache/venv/bin/python score.py .cache/results/*.jsonl
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../../src-tauri"
cargo build -p clocks-local-stt --release --example bench
B=./target/release/examples/bench
R="$HERE/.cache/results"
M="$HERE/.cache/models"
D="$HERE/.cache/data"
mkdir -p "$R"
rm -f "$R/pipeline.jsonl" "$R/whole.jsonl" "$R/energy.jsonl"
common=(--models "$M" --data "$D")

# 1. Proposed pipeline (Zipformer 2023-06-26 + Smart Turn int8), 48 kHz capture,
#    sweeping the Silero silence window - the main latency knob.
for s in 0.1 0.15 0.2 0.3; do
  $B "${common[@]}" --mode pipeline --min-silence "$s" --tag "zf-sil$s" --out "$R/pipeline.jsonl"
done
$B "${common[@]}" --mode pipeline --min-silence 0.2 --vad-threshold 0.4 --tag "zf-sil0.2-th0.4" --out "$R/pipeline.jsonl"

# 2. Same without the turn detector (every VAD segment is sent).
for s in 0.2 0.5; do
  $B "${common[@]}" --mode pipeline --turn off --min-silence "$s" --tag "zf-sil$s-noturn" --out "$R/pipeline.jsonl"
done

# 3. Other streaming models at default settings.
for m in zipformer-en-20M-2023-02-17 kroko-en-2025-08-06; do
  $B "${common[@]}" --mode pipeline --asr "$m" --tag "$m-sil0.2" --out "$R/pipeline.jsonl"
done

# 4. Capture-rate path and recognizer threads.
$B "${common[@]}" --mode pipeline --capture-rate 16000 --tag "zf-sil0.2-16k" --out "$R/pipeline.jsonl"
$B "${common[@]}" --mode pipeline --capture-rate 44100 --tag "zf-sil0.2-44k" --out "$R/pipeline.jsonl"
$B "${common[@]}" --mode pipeline --asr-threads 2 --tag "zf-sil0.2-asr2t" --out "$R/pipeline.jsonl"

# 5. Whole-clip recognition (ASR-only WER, and the mic path's batch latency).
for m in zipformer-en-2023-06-26 zipformer-en-20M-2023-02-17 kroko-en-2025-08-06; do
  $B "${common[@]}" --mode whole --asr "$m" --tag "whole-$m" --out "$R/whole.jsonl"
done

# 6. Today's energy VAD, for the cutoff baseline.
$B "${common[@]}" --mode energy --tag energy --out "$R/energy.jsonl"

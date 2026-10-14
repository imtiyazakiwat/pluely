# Spike: on-device STT (issue #1)

**Recommendation: go, as an opt-in beta with the STT API kept as the default and the fallback.**
On-device recognition is about 3x faster to a transcript than today's path and keeps audio on the
machine, but it does not reach the 200-260 ms target, and accuracy depends heavily on which
streaming model is used. The LibriSpeech-trained Zipformer proposed in the issue is not good
enough for meetings. Kroko is.

Hardware: Apple M4, 10 cores, 16 GB, macOS. Single machine, single run per configuration.
Not measured: Intel Macs, Windows, Linux, live capture of real system audio or a real mic.

## What was built

| Piece | Where |
|---|---|
| Engine crate: Silero VAD, streaming Zipformer (sherpa-onnx), Smart Turn v3.2 (ONNX Runtime) | `src-tauri/local-stt` |
| Tauri commands: status, download (SHA-256 verified), delete, transcribe | `src-tauri/src/local_stt.rs` |
| Capture loop for hands-free system audio | `src-tauri/src/speaker/local.rs` |
| Settings card + fallback to the STT provider | `src/pages/dev/components/stt-configs/LocalStt.tsx`, `src/lib/functions/stt.function.ts` |
| Benchmark harness and scorer | `src-tauri/local-stt/examples/bench.rs`, `spikes/on-device-stt/` |

The feature is a Cargo feature (`local-stt`, on by default). `--no-default-features` leaves it out.

## Test data

Public, downloaded by `prepare_data.py`, never committed. Speech is level-normalised, padded with
room-tone-level noise, and run through a 48 kHz capture simulation (1024-sample chunks, resampled
to 16 kHz) to match `speaker/commands.rs`.

| Set | Clips | Stands in for |
|---|---|---|
| LibriSpeech (dev-clean sample) | 73 | clean read speech |
| AMI headset mic | 120 | a remote participant's system audio |
| AMI distant room mic | 120 | a laptop mic in a meeting room |
| Earnings-22 | 100 | conference-call audio (mixed sample rates, accents) |
| Smart Turn v3.2 test split | 400 (200 finished, 200 unfinished) | end-of-turn labels |

## Latency

Time from the end of speech to the final transcript being available, per stage (p50 / p95, ms).
"End of speech" is found from audio energy in the clean clip, so it is imprecise on noisy
meeting audio and the p95 values are likely inflated.

| Stage | Issue's target | Measured, Kroko, 0.2 s silence window |
|---|---|---|
| Silero VAD cutoff after speech ends | 150-200 | 365 / 1167 |
| Recognizer final flush | ~30 | 31 / 31 (Zipformer: 12 / 13) |
| Smart Turn | 15-25 | 24 / 35 (4 threads; 70 ms on 1 thread) |
| Decision (flush and Smart Turn, run back to back) | n/a | 55 / 66 |
| **Total, released at the first pause** | **200-260** | **434 / 1241** |

- The VAD window is the main knob. Zipformer at 0.1 s silence: 314 / 1061 total, but 37% of
  utterances get split into more than one turn (more on that below).
- About 26% of turns end with Smart Turn saying "not finished", so the turn waits for more speech
  (up to 1.5 s in the app). Many test clips are cut mid-conversation, so some of those are
  correct holds. That tail is deliberate: it is what avoids sending half a sentence.
- Without Smart Turn, 56% of utterances were split into several turns versus 35% with it, at
  similar total latency.
- First live partial text appears about 1.9 s into speech for Zipformer. Kroko decodes in 1.28 s
  chunks, so partials lag more; nothing in the app shows partials yet.

Today's energy VAD, same audio: the cutoff alone is 953 / 1434 ms at a 48 kHz device and
2.9 s at a 16 kHz device (the 45-chunk window scales with sample rate, noted in `CONTEXT.md`),
before the STT network call. On noisy clips it often never closes (14-20% of clips here). That
last figure depends on my level normalisation and, at 16 kHz, on my 3 s trailing silence, so
treat it as indicative.

## Accuracy

Word error rate, Whisper English normaliser, corpus level, lower is better. Streaming pipeline
(VAD, recognizer, Smart Turn) at the 0.2 s window:

| Model | LibriSpeech | AMI headset | AMI room mic | Earnings | All |
|---|---|---|---|---|---|
| Zipformer en 2023-06-26 (the issue's choice) | 7.8 | 32.9 | 55.2 | 55.2 | 41.0 |
| **Kroko en 2025-08-06** | 7.9 | **12.4** | **27.4** | **21.1** | **18.1** |
| Zipformer 20M | 35.8 | 57.4 | 74.2 | 68.7 | 61.4 |

- **Not done: comparison with the current STT API.** There was no API key on the test machine.
  `api_wer.py` runs the same clips through OpenAI/Groq Whisper; it uploads public test audio and
  is billed to your key, so it was not run. The acceptance criterion "WER versus the current STT
  API" is therefore open.
- The Zipformer's error on meetings is mostly deletions and a cut-off final word, not noise
  robustness alone. It is trained on LibriSpeech only.
- Chunk size matters: Kroko needs 1.3 s of silence appended to flush its last chunk. With 0.3 s,
  its WER on LibriSpeech was 16.8 instead of 7.9 and the last words were lost. This is now a
  property of the model in `models.rs`, and an integration test guards it.
- Smart Turn v3.2: 93.2% accuracy on 400 labelled English clips (96% of finished turns, 90.5% of
  unfinished). The Rust feature extraction matches the Python reference to 5e-6; the int8 model
  is sensitive enough to that difference that single probabilities differ by up to 0.17, though
  no decision flipped on those 400 clips. The fp32 model is 94.0% accurate at 32 MB; not used.

## Resources

| | Value |
|---|---|
| Model download | 80.4 MB (Silero 0.6, Smart Turn 8.7, Kroko 71.1) |
| Engine code in the binary | about 27 MB in the probe binary, static |
| CPU while processing audio | 3.9% of one core per second of audio (Zipformer: 4.7%) |
| Real-time factor | 0.027 (37x faster than real time) |
| Peak RSS in the benchmark | 350 MB (includes the benchmark's own buffers) |
| Load time | 0.9 s |
| Recognizer threads | 1 is best; 2 used 2.5x the CPU for no speed-up |

## Decisions

- **Turn detector: Smart Turn v3.2, not LiveKit's.** The LiveKit Model License only allows use
  inside LiveKit Agents, so it cannot ship in Clocks and was not benchmarked. Smart Turn is
  BSD-2-Clause, audio-native, and 8.7 MB. LiveKit's is text-only and needs a transcript first.
- **Recognizer: Kroko.** Less than half the errors on meetings and calls. Licence is CC-BY-SA
  for the community model, so Clocks needs attribution and the model is not bundled, only
  downloaded on request. Check that against Clocks' GPL-3.0 distribution before shipping.
- **Fit with the ask-gate.** Smart Turn runs in Rust at each pause, before the transcript is
  released; it is the "layer 2" from `CONTEXT.md`. `shouldAsk` is unchanged and still gates the
  finished transcript (backchannels, fillers), so the two are complementary.

## Integration, and what is left

Done: everything in "What was built", behind a toggle that defaults to off, with the configured
STT provider as fallback at every step (models missing, load failure, per-call error).

Before this could ship:

1. **Build and run the full app.** This machine has no Xcode, so the app crate (`cidre`) could not
   be built here. The app-side Rust was compiled and unit-tested in a scratch crate that includes
   the real files with only the macOS capture backend stubbed (feature on and off). Nothing was
   run inside the Tauri window, so the settings card, model download, event wiring and live
   capture are untested end to end.
2. **Windows and Linux.** sherpa-onnx ships static libraries for both, but the Windows ones use
   the static C runtime, which may clash with Rust's default; untested.
3. **Build and CI.** The engine downloads its native libraries from GitHub at build time.
4. **Live audio.** Re-run `bench` against recordings from real system audio and a real mic. Tune
   `min_silence_s` (0.15-0.3) and `max_hold_s` on that, not on this simulation.
5. **API comparison.** Run `api_wer.py`.

## Reproduce

```sh
cd spikes/on-device-stt
python3 -m venv .cache/venv && .cache/venv/bin/pip install numpy onnxruntime transformers jiwer soundfile soxr
# models: see src-tauri/local-stt/src/models.rs for URLs and checksums
.cache/venv/bin/python prepare_data.py
./run_matrix.sh                                   # about 30 minutes
.cache/venv/bin/python score.py .cache/results/*.jsonl
cd ../../src-tauri && cargo test -p clocks-local-stt --release
```

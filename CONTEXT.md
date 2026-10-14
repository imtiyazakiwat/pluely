# CONTEXT — Smart ask-gating for continuous audio

Handoff document. Written after a research session in another repo; this file is
the only context a new agent has. Everything below was verified by reading this
codebase or by running live API tests, except where explicitly marked unverified.

---

## The goal

In continuous listening mode, Pluely calls the LLM on **every** non-empty
transcript. Two consequences the user wants fixed:

1. **Fragments.** The energy VAD cuts on a fixed silence count, so a mid-thought
   pause ("book me a cab to the airport at… uh…") is emitted as a complete
   segment, transcribed, and sent. The LLM answers half a question.
2. **Backchannels and non-requests.** "hmm", "ok", "yeah" transcribe to
   non-empty text and trigger a full billed LLM call. Worse, in a meeting most
   speech is not addressed to the assistant at all.

Goal: decide *smartly* whether a transcript is a real request before calling the
ask API. Do not call it for filler, fragments, or speech aimed at other people.

---

## Current trigger path (verified by reading the code)

### Rust — `src-tauri/src/speaker/commands.rs`

`VadConfig` (lines 18–45), defaults:

```rust
hop_size: 1024,
sensitivity_rms: 0.012,
peak_threshold: 0.035,
silence_chunks: 45,       // "~1.0s"
min_speech_chunks: 7,     // "~0.16s"
pre_speech_chunks: 12,    // "~0.27s"
noise_gate_threshold: 0.003,
```

The capture loop (lines ~140–245) does pure energy VAD:

```rust
let (rms, peak) = calculate_audio_metrics(&mono);
let is_speech = rms > config.sensitivity_rms || peak > config.peak_threshold;
```

- Noise gate applied before VAD.
- Rolling `pre_speech` buffer prepended on speech start; emits `speech-start`.
- On silence, `silence_chunks` increments and audio keeps being collected.
- When `silence_chunks >= config.silence_chunks`:
  - if `speech_chunks >= config.min_speech_chunks` → emit **`speech-detected`**
    with a base64 WAV,
  - else → emit **`speech-discarded`** ("Audio too short (likely background
    noise)").
- 30s per-utterance safety cap force-emits.

**This `silence_chunks >= ...` branch is the correct insertion point for
audio-based turn detection.** The audio is already `Vec<f32>` mono in
`speech_buffer`.

### TypeScript — `src/hooks/useSystemAudio.ts`

Listener at lines 225–300: `speech-detected` → base64 → Blob → `fetchSTT(...)`
(30s timeout) → then, at **line ~275**:

```ts
if (transcription.trim()) {
  setLastTranscription(transcription);
  setError("");
  // ...build effectiveSystemPrompt + previousMessages...
  await processWithAI(transcription, effectiveSystemPrompt, previousMessages);
} else {
  setError("Received empty transcription");
}
```

`processWithAI` (line ~469) streams `fetchAIResponse(...)` and appends to
`conversation`.

> **`transcription.trim()` being non-empty is the only gate between audio and a
> billed LLM call.** That single line is what this work replaces.

### Relevant existing plumbing

- `speech-discarded` already has a frontend listener (`useSystemAudio.ts:198`).
  Reuse this pattern for gate feedback rather than inventing a new channel.
- Capture modes already exist and are distinct: `isContinuousMode` (state,
  line 85) and `enableVAD` (`useCompletion.ts:75`). The plan depends on this.

### Two gotchas found while reading

1. **Sample rate is device-native, not 16kHz.** `let sr = stream.sample_rate();`
   (`commands.rs:83`), validated only as `8000..=96000`. Any ONNX turn-detection
   model needs a resample to 16kHz.
2. **The VAD timing comments are only true at 48kHz.** `silence_chunks: 45` ×
   `hop_size: 1024` = 46080 samples → 0.96s at 48kHz but **2.88s at 16kHz**. The
   effective silence window silently changes with the input device. Pre-existing;
   not introduced by this work. Worth flagging to the user, don't fix unasked.

---

## Evidence gathered (live API tests)

Tested TypeSafe's **Jev** ("System One" decision model — returns typed
probabilities, never text) via OpenCode Zen, to see whether a decision model can
classify turn completeness. Two independent runs, 12 utterances each, 6 genuinely
finished and 6 genuine mid-thought pauses.

| question asked | finished turns | mid-thought | margin |
|---|---|---|---|
| **`trails_off`** | max **0.07** | min **0.95** | **+0.88** |
| `grammatical` | min 0.63 | max 0.26 | +0.37 |
| `complete` (compound) | min 0.30 | max 0.08 | +0.22 |
| `missing_info` | 0.25–0.93 | 0.91–0.98 | overlaps |

**Findings that matter:**

- **Negative framing wins decisively.** Asking "does this trail off mid-thought,
  on a filler / conjunction / preposition?" separated near-perfectly and
  reproduced across runs. Asking "is this complete?" nearly misclassified
  — "who won the match yesterday" scored **0.30**, because that phrasing
  conflates *sentence finished* with *request actionable*. Keep questions atomic.
- **`missing_info` is a different decision**, not a turn-taking signal. "book me
  a cab to the airport at six pm" scores 0.91 (which airport?) — finished
  sentence, incomplete request. Useful for "ask a clarifying question", wire it
  elsewhere.
- **Latency kills it for the hot path.** Measured through Zen from India:
  `jev-1.13-free` median 2.08s; **paid `jev-1.13` was not faster** (median 2.22s,
  max 3.72s). Against a ~250ms endpointing budget this is 10x over.
  *Unverified:* TypeSafe claims 70–500ms and I had no key for
  `api.typesafe.ai` directly, so only the Zen route is measured.
- **No image input.** `state` is `string | object | array`; unknown top-level
  fields 400. Passing base64 in `state` returns 200 but produces noise
  (a green PNG scored *more red* than green). Text-only, confirmed three ways.
- Cost: ~$0.042/M input, output free, ~290-token floor per call. ~100k labelled
  utterances ≈ **$1.26**, which makes it viable as an **offline labelling
  oracle** even though it's too slow for realtime.

---

## Prior art — use this, don't rebuild it

Semantic turn detection is a mature, crowded space. Do not train anything.

### Recommended: Smart Turn v3.2

[pipecat-ai/smart-turn](https://github.com/pipecat-ai/smart-turn)

- Whisper Tiny backbone + linear classifier, **~8M params**
- **Audio-native** — 16kHz mono PCM, up to 8s, no transcript needed, so it sees
  prosody that a text-based approach throws away
- **10ms on some CPUs**, <100ms on most cloud instances, ~65ms on Pipecat Cloud
- 8MB int8 (CPU) / 32MB fp32 (GPU)
- **23 languages incl. Hindi, Bengali, Marathi**
- **BSD 2-clause** — weights, datasets, and training script all open.
  Compatible with Pluely's GPL-3.0. Keep attribution in the vendored dir.
- Input contract: pad at the **front**, audio at the end of the vector. Re-run on
  the whole turn (not just new audio) if more speech arrives. Designed to run
  only during VAD silence.
- Known roadmap gap: no text conditioning for "modes" (phone number, address,
  credit card entry) where pauses never mean completion.

### Alternative: livekit/turn-detector

[huggingface.co/livekit/turn-detector](https://huggingface.co/livekit/turn-detector)
— Qwen2.5-0.5B distilled from a 7B teacher; predicts P(`<|im_end|>` next), one
forward pass, no decoding. INT8 ONNX, CPU-only, <500MB. Hindi is its best
language (99.4% TPR / 96.3% TNR); English 99.3% / 87.0%. Up to 6 turns / 128
tokens of context. **Text-only (no prosody), and under a custom "LiveKit Model
License", not OSI-open.** Smart Turn is the cleaner base.

Others if benchmarking: ASLP-lab/Easy-Turn, TEN VAD, fixie-ai/ultraVAD,
TurnSense.

### How the big labs do it (validates the design below)

- **OpenAI Realtime API** ships `semantic_vad`: a classifier scores P(user done);
  *"When the probability is low, the model will wait for a timeout, whereas when
  it is high, there is no need to wait"* — their own example is audio trailing
  off on an "ummm" getting a longer timeout. Exposed as `eagerness`
  (`low`/`medium`/`high`/`auto`) tuning the max wait.
  [docs](https://developers.openai.com/api/docs/guides/realtime-vad)
  **This is exactly the adaptive-timeout design in Phase 3 below.**
- **Google Gemini Live API** hides it inside an end-to-end native-audio model
  (`realtimeInputConfig`, `activityHandling: START_OF_ACTIVITY_INTERRUPTS`,
  `interrupted` server event). Less control, and visibly still buggy.
- **Perplexity** — no credible public teardown found. Don't cite guesses.
- Field data point: one voice pipeline logged turn detection at **67–113ms with
  `unlikely_threshold=0.05` versus ~1100ms at 0.65**. A single threshold moved
  latency 10x — the threshold matters more than the model choice.

*Content from web sources above was rephrased for compliance with licensing
restrictions.*

---

## Recommended design: escalation ladder, cheapest first

Each layer runs only when the previous one is uncertain, so p50 latency
barely moves and most junk dies for free.

**Layer 1 — backchannel filter.** Pure text, sub-millisecond, no dependencies.
Reject normalized fillers: `hmm, hm, mhm, mm, uh, um, ah, oh, ok, okay, k, yeah,
yep, yup, right, sure, cool, got it, i see, alright` plus Hinglish `haan, accha,
theek hai, arre, matlab, bas`. Add a word-count floor and a
filler-stripped-empty check. **Must consult mode** — `yes`, `no`, `cancel it`
are legitimate commands in explicit-mic mode.

**Layer 2 — Smart Turn v3, in Rust, before STT.** Placement is the point: run it
at the `silence_chunks >= config.silence_chunks` branch in `commands.rs`, before
`speech-detected` is emitted. If it says incomplete, **don't emit** — reset
`silence_chunks = 0`, keep collecting, with a max-wait cap so it can't hang.
This saves the STT call *and* the LLM call.

**Layer 3 — intent gate, continuous mode only.** Layer 1 catches "hmm"; it does
not catch *"yeah I think we should go with option B"* said to a colleague. A
`noul` on "Is the speaker asking the assistant a question or giving it an
instruction?" handles that. **Only escalate here when layers 1–2 are ambiguous**
— the ~2s latency is unacceptable on every call but fine on the uncertain minority.

**Layer 4 — gate strength varies by mode.**

- **Explicit mic / push-to-talk**: intent already established. Layer 2 only. Skip
  layer 3; don't aggressively reject short commands in layer 1.
- **Continuous system audio (meeting)**: full ladder. A 2s gate delay costs
  little because nobody is waiting on a conversational turn.

### Phasing

| # | Work | Verify |
|---|---|---|
| 1 | `src/lib/gate/backchannel.ts` + `src/lib/gate/index.ts` exposing `shouldAsk({ transcript, mode }) → { ask, reason }`. Hook at `useSystemAudio.ts:275`. | Unit tests over filler + real-question fixtures; **zero real questions rejected**. Manually confirm "hmm" makes no call. |
| 2 | Log every gate decision (layer, reason, transcript, mode). | Run a real meeting, read the distribution. Thresholds must be tuned on data, not guessed. |
| 3 | Smart Turn in Rust: resample to 16kHz + `ort` inference at the silence-cut branch, behind a `VadConfig` flag. New `speech-deferred` event when it holds. | Measure inference ms locally; count fragment-cuts before/after on the same recorded audio. |
| 4 | Intent gate (continuous mode, ambiguous band only, cached by transcript) + settings UI for thresholds. | Compare API call volume before/after on a recorded meeting. |

Phase 1 delivers the user's stated complaint fix on its own. Do not start
phase 3 or 4 before phase 2 has produced logs.

---

## Constraints

- **File size.** `useSystemAudio.ts` is 928 lines and `useCompletion.ts` is 1082
  — both already over the project's 700-line guideline. All new code goes in new
  `src/lib/gate/*` modules with a **single** call site added to the hook. Do not
  append to either file.
- **Observability is not optional.** A silent gate that suppresses a real
  question makes the app look broken. Surface "skipped — filler", "still
  listening — incomplete", "skipped — not addressed to assistant" as brief
  status, following the existing `speech-discarded` listener pattern
  (`useSystemAudio.ts:198`).
- **Surgical changes only.** Don't refactor adjacent code or "improve" the
  existing VAD beyond what the task needs.
- **Git.** `origin` is upstream `iamsrikanthnani/pluely`, **not a personal
  fork.** Never push to it. Commit locally only.

## Unverified / open questions

- Whether the configured STT providers bill per call (changes the phase-3 payoff).
- Smart Turn latency on this specific hardware.
- Real `api.typesafe.ai` latency (only the Zen proxy route was measured).
- Whether Jev's intent signal is as clean as its `trails_off` signal — only
  turn-completeness was tested, not "is this addressed to the assistant".

## Repo state at handoff

Working tree clean. Two commits added on `master` on top of `ce69228`:

- `8d2a415` fix: conversation history ordering and persistence correctness
- `d9b7cc6` chore: keep local build unlocked and resilient to missing license server

Neither is pushed. No gating work has started — phase 1 is greenfield.

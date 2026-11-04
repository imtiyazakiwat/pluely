/**
 * Ask-gating: decide whether a transcript deserves a billed LLM call.
 *
 * Phase 1 is layer 1 only — a pure-text backchannel filter. `shouldAsk` is
 * synchronous, dependency-free and sub-millisecond, so it can sit directly on
 * the path between STT and the ask API.
 *
 * Later layers (audio-native turn detection in Rust, an intent gate for
 * hands-free capture) plug in behind this same entry point without changing
 * the call site.
 */

import {
  isBelowLengthFloor,
  isNonSpeechAnnotation,
  residualTokens,
} from "./backchannel.ts";
import type { GateMode } from "./backchannel.ts";

export * from "./backchannel.ts";
export * from "./log.ts";

// Registers the `window.clocksGate` console helpers. Side-effect import; no-op
// outside a browser.
import "./debug.ts";

/**
 * Which layer of the escalation ladder produced the decision. Phase 1 only
 * reaches `input` and `backchannel`; `turn` (audio-native turn detection) and
 * `intent` land in later phases. Logged so the distribution stays readable as
 * layers are added.
 */
export type GateLayer = "input" | "backchannel" | "turn" | "intent";

/**
 * Why the gate decided what it decided. `ok` is the only value that permits a
 * call; everything else is a rejection.
 */
export type GateReason = "ok" | "empty" | "non-speech" | "filler" | "too-short";

export interface GateInput {
  /** Raw STT output, exactly as the provider returned it. */
  transcript: string;
  /** Which capture mode produced it. Controls how strict the gate is. */
  mode: GateMode;
}

export interface GateDecision {
  /** Whether to call the ask API. */
  ask: boolean;
  /** Machine-readable reason, suitable for logging and metrics. */
  reason: GateReason;
  /** Which layer settled it. */
  layer: GateLayer;
  /**
   * Short user-facing line explaining a rejection, including what was heard so
   * a wrongly-suppressed question is immediately visible. Empty when
   * `ask` is true.
   */
  status: string;
}

const REJECTION_LABELS: Record<Exclude<GateReason, "ok">, string> = {
  empty: "nothing transcribed",
  "non-speech": "no speech detected",
  filler: "filler",
  "too-short": "too short",
};

const REASON_LAYERS: Record<GateReason, GateLayer> = {
  ok: "backchannel",
  empty: "input",
  "non-speech": "input",
  filler: "backchannel",
  "too-short": "backchannel",
};

const MAX_ECHO_CHARS = 80;

function echo(transcript: string): string {
  const trimmed = transcript.trim().replace(/\s+/gu, " ");
  if (trimmed.length <= MAX_ECHO_CHARS) return trimmed;
  return `${trimmed.slice(0, MAX_ECHO_CHARS - 1)}…`;
}

function reject(transcript: string, reason: Exclude<GateReason, "ok">): GateDecision {
  const heard = echo(transcript);
  const label = REJECTION_LABELS[reason];
  return {
    ask: false,
    reason,
    layer: REASON_LAYERS[reason],
    status: heard
      ? `Heard "${heard}" — not sent to AI (${label})`
      : `Not sent to AI (${label})`,
  };
}

/**
 * Decide whether a transcript should trigger an ask.
 *
 * Rejects, cheapest check first:
 *  - `empty` — nothing but whitespace (preserves the pre-gate behaviour).
 *  - `non-speech` — punctuation or STT annotations only, e.g. `[BLANK_AUDIO]`.
 *  - `filler` — every token is a backchannel for this mode.
 *  - `too-short` — hands-free capture only, a lone non-command word.
 *
 * Anything else passes. A single unrecognised word is enough to pass, which is
 * the deliberate recall bias: letting filler through costs one API call,
 * swallowing a real question makes the app look broken.
 */
export function shouldAsk({ transcript, mode }: GateInput): GateDecision {
  const raw = typeof transcript === "string" ? transcript : "";

  if (raw.trim().length === 0) return reject(raw, "empty");
  if (isNonSpeechAnnotation(raw)) return reject(raw, "non-speech");

  const residual = residualTokens(raw, mode);
  if (residual.length === 0) return reject(raw, "filler");
  if (isBelowLengthFloor(raw, mode, residual)) return reject(raw, "too-short");

  return { ask: true, reason: "ok", layer: REASON_LAYERS.ok, status: "" };
}

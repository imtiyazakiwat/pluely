/**
 * Phase 2 of the ask-gating plan: record every gate decision so thresholds get
 * tuned on data instead of guesses.
 *
 * Every call to the gate is appended to a bounded ring buffer in localStorage.
 * The log answers three questions Phase 1 cannot:
 *
 *  1. How many LLM calls did the gate actually prevent?
 *  2. Which reason dominates — is `filler` carrying the weight, or is the
 *     `too-short` floor doing most of the rejecting? (If it's the floor, that
 *     is the rule with the recall risk and it needs review.)
 *  3. Did it ever gate something real? The transcript is stored so a false
 *     negative can be found after the fact rather than guessed at.
 *
 * PRIVACY: entries contain transcript text and stay in this machine's
 * localStorage. Nothing here performs I/O beyond that. The buffer is capped,
 * `clearGateLog()` wipes it, and `setGateLogSettings({ storeTranscripts:
 * false })` keeps the distribution while dropping the text.
 *
 * Imports are deliberately restricted to dependency-free modules so the gate
 * stays runnable under plain Node for tests.
 */

import { STORAGE_KEYS } from "../../config/constants.ts";
import { safeLocalStorage } from "../storage/helper.ts";
import { residualTokens } from "./backchannel.ts";
import type { GateMode } from "./backchannel.ts";
import type { GateDecision, GateInput, GateLayer, GateReason } from "./index.ts";

/**
 * Maximum entries retained. Oldest are dropped first.
 *
 * Sized for a real meeting, which is what Phase 2 has to survive: hands-free
 * capture cuts a segment every few seconds, so an hour is roughly 900 entries.
 * At 500 the first half of a long session would silently age out — exactly the
 * data being collected. ~250 bytes per entry puts 2000 at well under a
 * megabyte of localStorage.
 */
export const GATE_LOG_LIMIT = 2000;

/** Transcripts longer than this are truncated before storage. */
export const GATE_LOG_TRANSCRIPT_LIMIT = 240;

export interface GateLogEntry {
  /** Monotonic within a session; unique enough to use as a React key. */
  id: string;
  /** Epoch milliseconds. */
  at: number;
  /** Groups entries by app launch, so one meeting can be read in isolation. */
  session: string;
  mode: GateMode;
  ask: boolean;
  layer: GateLayer;
  reason: GateReason;
  /** Truncated transcript, or `""` when transcript storage is off. */
  transcript: string;
  /** Character count of the full transcript, before truncation. */
  chars: number;
  /** Whitespace-delimited word count of the full transcript. */
  words: number;
  /**
   * Tokens left after filler removal. This is the exact feature the
   * `too-short` floor thresholds on, so it is what Phase 3/4 tuning needs.
   */
  residualWords: number;
}

export interface GateLogSettings {
  /** Master switch. Defaults to on — Phase 2 exists to collect this. */
  enabled: boolean;
  /** Whether transcript text is persisted. Defaults to on. */
  storeTranscripts: boolean;
}

export const DEFAULT_GATE_LOG_SETTINGS: GateLogSettings = {
  enabled: true,
  storeTranscripts: true,
};

export interface GateModeBreakdown {
  total: number;
  asked: number;
  gated: number;
}

export interface GateLogSummary {
  total: number;
  /** Decisions that resulted in an LLM call. */
  asked: number;
  /** Decisions the gate stopped. Equals the LLM calls avoided. */
  gated: number;
  /** `asked / total`, or 0 when the log is empty. */
  askRate: number;
  byReason: Record<GateReason, number>;
  byMode: Record<GateMode, GateModeBreakdown>;
  firstAt: number | null;
  lastAt: number | null;
  /** Wall-clock span covered by the log, in milliseconds. */
  spanMs: number;
}

const ALL_REASONS: readonly GateReason[] = [
  "ok",
  "empty",
  "non-speech",
  "filler",
  "too-short",
];

const ALL_MODES: readonly GateMode[] = ["continuous", "explicit"];

/* -------------------------------------------------------------------------- */
/* storage                                                                    */
/* -------------------------------------------------------------------------- */

/**
 * Minimal string store. `safeLocalStorage` already degrades to no-ops without
 * a `window`, which would make the log untestable outside a browser, so a
 * memory store stands in when there is no DOM.
 */
interface GateLogStore {
  read(key: string): string | null;
  write(key: string, value: string): void;
  remove(key: string): void;
}

function createMemoryStore(): GateLogStore {
  const values = new Map<string, string>();
  return {
    read: (key) => values.get(key) ?? null,
    write: (key, value) => void values.set(key, value),
    remove: (key) => void values.delete(key),
  };
}

const store: GateLogStore =
  typeof window === "undefined"
    ? createMemoryStore()
    : {
        read: (key) => safeLocalStorage.getItem(key),
        write: (key, value) => safeLocalStorage.setItem(key, value),
        remove: (key) => safeLocalStorage.removeItem(key),
      };

/** Identifies the current app launch. */
export const GATE_LOG_SESSION = `s${Date.now().toString(36)}`;

let sequence = 0;
let cache: GateLogEntry[] | null = null;

function isEntry(value: unknown): value is GateLogEntry {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as Partial<GateLogEntry>;
  return (
    typeof candidate.id === "string" &&
    typeof candidate.at === "number" &&
    typeof candidate.reason === "string" &&
    typeof candidate.ask === "boolean"
  );
}

function load(): GateLogEntry[] {
  if (cache) return cache;
  const raw = store.read(STORAGE_KEYS.GATE_DECISION_LOG);
  if (!raw) {
    cache = [];
    return cache;
  }
  try {
    const parsed: unknown = JSON.parse(raw);
    // A corrupt or hand-edited log must not break the capture path.
    cache = Array.isArray(parsed) ? parsed.filter(isEntry) : [];
  } catch {
    cache = [];
  }
  return cache;
}

function persist(entries: GateLogEntry[]): void {
  cache = entries;
  store.write(STORAGE_KEYS.GATE_DECISION_LOG, JSON.stringify(entries));
}

/* -------------------------------------------------------------------------- */
/* settings                                                                   */
/* -------------------------------------------------------------------------- */

export function getGateLogSettings(): GateLogSettings {
  const raw = store.read(STORAGE_KEYS.GATE_LOG_SETTINGS);
  if (!raw) return { ...DEFAULT_GATE_LOG_SETTINGS };
  try {
    const parsed = JSON.parse(raw) as Partial<GateLogSettings>;
    return {
      enabled: parsed.enabled ?? DEFAULT_GATE_LOG_SETTINGS.enabled,
      storeTranscripts:
        parsed.storeTranscripts ?? DEFAULT_GATE_LOG_SETTINGS.storeTranscripts,
    };
  } catch {
    return { ...DEFAULT_GATE_LOG_SETTINGS };
  }
}

export function setGateLogSettings(
  patch: Partial<GateLogSettings>
): GateLogSettings {
  const next = { ...getGateLogSettings(), ...patch };
  store.write(STORAGE_KEYS.GATE_LOG_SETTINGS, JSON.stringify(next));
  return next;
}

/* -------------------------------------------------------------------------- */
/* recording                                                                  */
/* -------------------------------------------------------------------------- */

function countWords(transcript: string): number {
  const trimmed = transcript.trim();
  if (!trimmed) return 0;
  return trimmed.split(/\s+/u).length;
}

function truncate(transcript: string): string {
  const collapsed = transcript.trim().replace(/\s+/gu, " ");
  if (collapsed.length <= GATE_LOG_TRANSCRIPT_LIMIT) return collapsed;
  return `${collapsed.slice(0, GATE_LOG_TRANSCRIPT_LIMIT - 1)}…`;
}

/**
 * Build the entry for a decision without touching storage. Pure, so the shape
 * is testable independently of persistence.
 */
export function buildGateLogEntry(
  decision: GateDecision,
  input: GateInput,
  settings: GateLogSettings = getGateLogSettings(),
  now: number = Date.now()
): GateLogEntry {
  const transcript = typeof input.transcript === "string" ? input.transcript : "";
  sequence += 1;
  return {
    id: `${GATE_LOG_SESSION}-${sequence}`,
    at: now,
    session: GATE_LOG_SESSION,
    mode: input.mode,
    ask: decision.ask,
    layer: decision.layer,
    reason: decision.reason,
    transcript: settings.storeTranscripts ? truncate(transcript) : "",
    chars: transcript.trim().length,
    words: countWords(transcript),
    residualWords: residualTokens(transcript, input.mode).length,
  };
}

/**
 * Record a decision. Returns the entry, or `null` when logging is off.
 *
 * Never throws: this sits on the capture path, and losing a log line must
 * never cost the user an answer.
 */
export function logGateDecision(
  decision: GateDecision,
  input: GateInput
): GateLogEntry | null {
  try {
    const settings = getGateLogSettings();
    if (!settings.enabled) return null;

    const entry = buildGateLogEntry(decision, input, settings);
    const entries = load();
    const next =
      entries.length >= GATE_LOG_LIMIT
        ? [...entries.slice(entries.length - GATE_LOG_LIMIT + 1), entry]
        : [...entries, entry];
    persist(next);
    return entry;
  } catch {
    return null;
  }
}

/** All retained entries, oldest first. */
export function readGateLog(): GateLogEntry[] {
  return [...load()];
}

/** Entries from the current app launch only. */
export function readGateLogSession(session = GATE_LOG_SESSION): GateLogEntry[] {
  return load().filter((entry) => entry.session === session);
}

export function clearGateLog(): void {
  cache = [];
  store.remove(STORAGE_KEYS.GATE_DECISION_LOG);
}

/* -------------------------------------------------------------------------- */
/* reading the distribution                                                   */
/* -------------------------------------------------------------------------- */

export function summarizeGateLog(
  entries: GateLogEntry[] = readGateLog()
): GateLogSummary {
  const byReason = Object.fromEntries(
    ALL_REASONS.map((reason) => [reason, 0])
  ) as Record<GateReason, number>;
  const byMode = Object.fromEntries(
    ALL_MODES.map((mode) => [mode, { total: 0, asked: 0, gated: 0 }])
  ) as Record<GateMode, GateModeBreakdown>;

  let asked = 0;
  let firstAt: number | null = null;
  let lastAt: number | null = null;

  for (const entry of entries) {
    if (byReason[entry.reason] !== undefined) byReason[entry.reason] += 1;

    const mode = byMode[entry.mode];
    if (mode) {
      mode.total += 1;
      if (entry.ask) mode.asked += 1;
      else mode.gated += 1;
    }

    if (entry.ask) asked += 1;
    if (firstAt === null || entry.at < firstAt) firstAt = entry.at;
    if (lastAt === null || entry.at > lastAt) lastAt = entry.at;
  }

  const total = entries.length;
  return {
    total,
    asked,
    gated: total - asked,
    askRate: total === 0 ? 0 : asked / total,
    byReason,
    byMode,
    firstAt,
    lastAt,
    spanMs: firstAt === null || lastAt === null ? 0 : lastAt - firstAt,
  };
}

function percent(part: number, whole: number): string {
  if (whole === 0) return "0%";
  return `${((part / whole) * 100).toFixed(1)}%`;
}

/**
 * Human-readable distribution report. This is the Phase 2 deliverable: run a
 * real meeting, then read this before touching any threshold.
 */
export function formatGateLogReport(
  entries: GateLogEntry[] = readGateLog()
): string {
  const summary = summarizeGateLog(entries);
  if (summary.total === 0) {
    return "Ask-gate log is empty. Capture some audio first.";
  }

  const lines: string[] = [];
  const minutes = summary.spanMs / 60000;

  lines.push(
    `Ask-gate decisions: ${summary.total} over ${minutes.toFixed(1)} min`
  );
  lines.push(
    `  asked  ${summary.asked} (${percent(summary.asked, summary.total)})`
  );
  lines.push(
    `  gated  ${summary.gated} (${percent(
      summary.gated,
      summary.total
    )})  <- LLM calls avoided`
  );

  lines.push("");
  lines.push("By reason:");
  for (const reason of ALL_REASONS) {
    const count = summary.byReason[reason];
    if (count === 0) continue;
    lines.push(
      `  ${reason.padEnd(11)} ${String(count).padStart(4)}  ${percent(
        count,
        summary.total
      )}`
    );
  }

  lines.push("");
  lines.push("By mode:");
  for (const mode of ALL_MODES) {
    const breakdown = summary.byMode[mode];
    if (breakdown.total === 0) continue;
    lines.push(
      `  ${mode.padEnd(11)} ${String(breakdown.total).padStart(
        4
      )} total, ${breakdown.asked} asked, ${breakdown.gated} gated`
    );
  }

  // The floor is the rule that can swallow a real request, so surface what it
  // rejected for eyeballing rather than burying it in the raw log.
  const floorRejects = entries.filter((entry) => entry.reason === "too-short");
  if (floorRejects.length > 0) {
    lines.push("");
    lines.push(
      `Length-floor rejects (${floorRejects.length}) — review these for false negatives:`
    );
    for (const entry of floorRejects.slice(-15)) {
      lines.push(`  ${entry.mode.padEnd(11)} ${entry.transcript || "(hidden)"}`);
    }
  }

  return lines.join("\n");
}

/* -------------------------------------------------------------------------- */
/* export                                                                     */
/* -------------------------------------------------------------------------- */

export function gateLogToJson(entries: GateLogEntry[] = readGateLog()): string {
  return JSON.stringify(entries, null, 2);
}

const CSV_COLUMNS = [
  "at",
  "iso",
  "session",
  "mode",
  "ask",
  "layer",
  "reason",
  "chars",
  "words",
  "residualWords",
  "transcript",
] as const;

function csvCell(value: string | number | boolean): string {
  const text = String(value);
  // RFC 4180: quote when the cell contains a delimiter, quote or newline, and
  // double any embedded quotes. Transcripts routinely contain commas.
  if (/[",\r\n]/u.test(text)) return `"${text.replace(/"/gu, '""')}"`;
  return text;
}

export function gateLogToCsv(entries: GateLogEntry[] = readGateLog()): string {
  const rows = [CSV_COLUMNS.join(",")];
  for (const entry of entries) {
    rows.push(
      [
        entry.at,
        new Date(entry.at).toISOString(),
        entry.session,
        entry.mode,
        entry.ask,
        entry.layer,
        entry.reason,
        entry.chars,
        entry.words,
        entry.residualWords,
        entry.transcript,
      ]
        .map(csvCell)
        .join(",")
    );
  }
  return rows.join("\n");
}

/**
 * Ask-gate decision log tests (Phase 2).
 *
 * Run with:  npm run test:gate
 *
 * Outside a browser the log falls back to an in-memory store, so persistence,
 * the ring buffer and the summary maths are all exercisable under plain Node
 * with no DOM shim and no test dependency.
 */

import { strict as assert } from "node:assert";
import { beforeEach, describe, it } from "node:test";

import { shouldAsk } from "./index.ts";
import {
  DEFAULT_GATE_LOG_SETTINGS,
  GATE_LOG_LIMIT,
  GATE_LOG_SESSION,
  GATE_LOG_TRANSCRIPT_LIMIT,
  buildGateLogEntry,
  clearGateLog,
  formatGateLogReport,
  gateLogToCsv,
  gateLogToJson,
  getGateLogSettings,
  logGateDecision,
  readGateLog,
  readGateLogSession,
  setGateLogSettings,
  summarizeGateLog,
} from "./log.ts";
import type { GateMode } from "./backchannel.ts";

/** Record a transcript exactly as the call site does. */
function record(transcript: string, mode: GateMode = "continuous") {
  const input = { transcript, mode };
  const decision = shouldAsk(input);
  return logGateDecision(decision, input);
}

beforeEach(() => {
  clearGateLog();
  setGateLogSettings(DEFAULT_GATE_LOG_SETTINGS);
});

describe("recording", () => {
  it("logs both asked and gated decisions", () => {
    record("hmm");
    record("what is the capital of France");

    const entries = readGateLog();
    assert.equal(entries.length, 2);
    assert.equal(entries[0].ask, false);
    assert.equal(entries[0].reason, "filler");
    assert.equal(entries[1].ask, true);
    assert.equal(entries[1].reason, "ok");
  });

  it("captures layer, mode, reason and transcript", () => {
    const entry = record("hmm", "explicit");
    assert.ok(entry);
    assert.equal(entry.layer, "backchannel");
    assert.equal(entry.mode, "explicit");
    assert.equal(entry.reason, "filler");
    assert.equal(entry.transcript, "hmm");
    assert.equal(entry.session, GATE_LOG_SESSION);
  });

  it("attributes empty and non-speech to the input layer", () => {
    assert.equal(record("")?.layer, "input");
    assert.equal(record("[BLANK_AUDIO]")?.layer, "input");
    assert.equal(record("hmm")?.layer, "backchannel");
  });

  it("records residual word count, the feature the floor thresholds on", () => {
    assert.equal(record("okay so what is the plan")?.residualWords, 4);
    assert.equal(record("got it thanks")?.residualWords, 0);
    assert.equal(record("revenue")?.residualWords, 1);
  });

  it("records raw length even when the transcript is truncated", () => {
    const long = `${"a".repeat(400)} and what about the budget`;
    const entry = record(long);
    assert.ok(entry);
    assert.equal(entry.chars, long.trim().length);
    assert.ok(entry.transcript.length <= GATE_LOG_TRANSCRIPT_LIMIT);
    assert.match(entry.transcript, /…$/);
  });

  it("gives every entry a unique id", () => {
    for (let i = 0; i < 20; i++) record(`question number ${i}`);
    const ids = readGateLog().map((entry) => entry.id);
    assert.equal(new Set(ids).size, ids.length);
  });

  it("persists across reads", () => {
    record("hmm");
    assert.equal(readGateLog().length, 1);
    assert.equal(readGateLog().length, 1);
  });

  it("exposes the current session view", () => {
    record("hmm");
    assert.equal(readGateLogSession().length, 1);
    assert.equal(readGateLogSession("some-other-session").length, 0);
  });
});

describe("ring buffer", () => {
  it("caps at the limit and drops oldest first", () => {
    const overflow = 25;
    const recorded = GATE_LOG_LIMIT + overflow;
    for (let i = 0; i < recorded; i++) {
      record(`what is item number ${i}`);
    }
    const entries = readGateLog();
    assert.equal(entries.length, GATE_LOG_LIMIT);
    // Newest survives, oldest `overflow` entries aged out.
    assert.equal(
      entries[entries.length - 1].transcript,
      `what is item number ${recorded - 1}`
    );
    assert.equal(entries[0].transcript, `what is item number ${overflow}`);
  });

  it("holds a full meeting's worth of decisions", () => {
    // ~900 segments is an hour of hands-free capture. Phase 2 must not lose
    // the start of a session it was run to observe.
    assert.ok(GATE_LOG_LIMIT >= 900, `limit too small: ${GATE_LOG_LIMIT}`);
  });
});

describe("settings", () => {
  it("defaults to enabled with transcripts", () => {
    assert.deepEqual(getGateLogSettings(), DEFAULT_GATE_LOG_SETTINGS);
  });

  it("stops recording when disabled", () => {
    setGateLogSettings({ enabled: false });
    assert.equal(record("hmm"), null);
    assert.equal(readGateLog().length, 0);
  });

  it("keeps the distribution but drops text when transcripts are off", () => {
    setGateLogSettings({ storeTranscripts: false });
    const entry = record("what is their revenue this quarter");
    assert.ok(entry);
    assert.equal(entry.transcript, "");
    // Counts still usable for tuning.
    assert.equal(entry.words, 6);
    assert.equal(entry.residualWords, 6);
    assert.equal(entry.reason, "ok");
  });

  it("merges patches instead of replacing settings", () => {
    setGateLogSettings({ storeTranscripts: false });
    assert.equal(getGateLogSettings().enabled, true);
    assert.equal(getGateLogSettings().storeTranscripts, false);
  });
});

describe("summary", () => {
  it("counts asked, gated and the ask rate", () => {
    record("hmm");
    record("yeah");
    record("what is the capital of France");
    record("summarize this");

    const summary = summarizeGateLog();
    assert.equal(summary.total, 4);
    assert.equal(summary.asked, 2);
    assert.equal(summary.gated, 2);
    assert.equal(summary.askRate, 0.5);
  });

  it("breaks down by reason", () => {
    record("hmm");
    record("got it");
    record("revenue");
    record("[BLANK_AUDIO]");
    record("");
    record("what is the capital of France");

    const { byReason } = summarizeGateLog();
    assert.equal(byReason.filler, 2);
    assert.equal(byReason["too-short"], 1);
    assert.equal(byReason["non-speech"], 1);
    assert.equal(byReason.empty, 1);
    assert.equal(byReason.ok, 1);
  });

  it("breaks down by mode", () => {
    record("yeah", "continuous");
    record("yeah", "explicit");
    record("what is the plan", "continuous");

    const { byMode } = summarizeGateLog();
    assert.deepEqual(byMode.continuous, { total: 2, asked: 1, gated: 1 });
    assert.deepEqual(byMode.explicit, { total: 1, asked: 1, gated: 0 });
  });

  it("handles an empty log without dividing by zero", () => {
    const summary = summarizeGateLog();
    assert.equal(summary.total, 0);
    assert.equal(summary.askRate, 0);
    assert.equal(summary.spanMs, 0);
    assert.equal(summary.firstAt, null);
  });

  it("reports the wall-clock span", () => {
    const early = buildGateLogEntry(
      shouldAsk({ transcript: "hmm", mode: "continuous" }),
      { transcript: "hmm", mode: "continuous" },
      DEFAULT_GATE_LOG_SETTINGS,
      1000
    );
    const late = buildGateLogEntry(
      shouldAsk({ transcript: "hmm", mode: "continuous" }),
      { transcript: "hmm", mode: "continuous" },
      DEFAULT_GATE_LOG_SETTINGS,
      61000
    );
    const summary = summarizeGateLog([late, early]);
    assert.equal(summary.firstAt, 1000);
    assert.equal(summary.lastAt, 61000);
    assert.equal(summary.spanMs, 60000);
  });
});

describe("report", () => {
  it("says so when the log is empty", () => {
    assert.match(formatGateLogReport(), /empty/i);
  });

  it("shows totals, reasons and modes", () => {
    record("hmm");
    record("yeah");
    record("what is the capital of France");

    const report = formatGateLogReport();
    assert.match(report, /Ask-gate decisions: 3/);
    assert.match(report, /asked\s+1/);
    assert.match(report, /gated\s+2/);
    assert.match(report, /filler\s+2/);
    assert.match(report, /continuous/);
  });

  it("surfaces length-floor rejects for false-negative review", () => {
    record("revenue");
    const report = formatGateLogReport();
    assert.match(report, /Length-floor rejects \(1\)/);
    assert.match(report, /revenue/);
  });

  it("does not claim floor rejects when there are none", () => {
    record("hmm");
    assert.ok(!formatGateLogReport().includes("Length-floor rejects"));
  });
});

describe("export", () => {
  it("emits a csv header plus one row per decision", () => {
    record("hmm");
    record("what is the capital of France");

    const lines = gateLogToCsv().trim().split("\n");
    assert.equal(lines.length, 3);
    assert.match(lines[0], /^at,iso,session,mode,ask,layer,reason/);
  });

  it("quotes transcripts containing commas and quotes", () => {
    record('so, what did they say about "pricing"?');
    const csv = gateLogToCsv();
    assert.match(csv, /"so, what did they say about ""pricing""\?"/);
    // One header + one data row: the comma must not have split the row.
    assert.equal(csv.trim().split("\n").length, 2);
  });

  it("round-trips through json", () => {
    record("hmm");
    record("what is the capital of France");
    const parsed = JSON.parse(gateLogToJson());
    assert.equal(parsed.length, 2);
    assert.equal(parsed[0].reason, "filler");
  });
});

describe("resilience", () => {
  it("never throws on the capture path", () => {
    // A decision object from an older build, missing fields the log expects.
    const malformed = { ask: true, reason: "ok" } as never;
    assert.doesNotThrow(() => {
      logGateDecision(malformed, { transcript: "hello there", mode: "explicit" });
    });
  });

  it("clears everything", () => {
    record("hmm");
    clearGateLog();
    assert.equal(readGateLog().length, 0);
    assert.equal(summarizeGateLog().total, 0);
  });
});

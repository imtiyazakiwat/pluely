/**
 * Console surface for the ask-gate decision log.
 *
 * Phase 2's verification step is "run a real meeting, read the distribution",
 * so the log needs a way out. This attaches it to `window.pluelyGate` rather
 * than building UI — settings UI is Phase 4, and a devtools handle is enough to
 * read a distribution and export it for tuning.
 *
 * Open devtools in the Pluely window and run:
 *
 *   pluelyGate.report()     // distribution, formatted
 *   pluelyGate.table()      // every decision as a console table
 *   pluelyGate.gated()      // only what the gate stopped
 *   pluelyGate.copyCsv()    // CSV to clipboard for offline analysis
 *   pluelyGate.clear()      // wipe stored transcripts
 *   pluelyGate.settings({ storeTranscripts: false })
 */

import {
  clearGateLog,
  formatGateLogReport,
  gateLogToCsv,
  gateLogToJson,
  getGateLogSettings,
  readGateLog,
  readGateLogSession,
  setGateLogSettings,
  summarizeGateLog,
} from "./log.ts";
import type { GateLogEntry, GateLogSettings, GateLogSummary } from "./log.ts";

export interface GateDebugApi {
  /** Formatted distribution report. Start here. */
  report(): void;
  /** Raw entries, oldest first. */
  log(): GateLogEntry[];
  /** Entries from this app launch only. */
  session(): GateLogEntry[];
  /** Console table of every decision. */
  table(): void;
  /** Only the decisions the gate stopped. */
  gated(): GateLogEntry[];
  /** Only the decisions that reached the LLM. */
  asked(): GateLogEntry[];
  /** Machine-readable counts. */
  summary(): GateLogSummary;
  csv(): string;
  json(): string;
  /** Copy CSV to the clipboard. Resolves to false if the clipboard is blocked. */
  copyCsv(): Promise<boolean>;
  clear(): void;
  /** Read settings, or pass a patch to update them. */
  settings(patch?: Partial<GateLogSettings>): GateLogSettings;
}

declare global {
  // eslint-disable-next-line no-var
  var pluelyGate: GateDebugApi | undefined;

  interface Window {
    pluelyGate?: GateDebugApi;
  }
}

const TABLE_COLUMNS = [
  "at",
  "mode",
  "ask",
  "reason",
  "words",
  "residualWords",
  "transcript",
];

export function createGateDebugApi(): GateDebugApi {
  return {
    report: () => console.log(formatGateLogReport()),
    log: () => readGateLog(),
    session: () => readGateLogSession(),
    table: () => {
      const rows = readGateLog().map((entry) => ({
        ...entry,
        at: new Date(entry.at).toLocaleTimeString(),
      }));
      console.table(rows, TABLE_COLUMNS);
    },
    gated: () => readGateLog().filter((entry) => !entry.ask),
    asked: () => readGateLog().filter((entry) => entry.ask),
    summary: () => summarizeGateLog(),
    csv: () => gateLogToCsv(),
    json: () => gateLogToJson(),
    copyCsv: async () => {
      try {
        await navigator.clipboard.writeText(gateLogToCsv());
        return true;
      } catch {
        return false;
      }
    },
    clear: () => clearGateLog(),
    settings: (patch) =>
      patch ? setGateLogSettings(patch) : getGateLogSettings(),
  };
}

if (typeof window !== "undefined") {
  window.pluelyGate = createGateDebugApi();
}

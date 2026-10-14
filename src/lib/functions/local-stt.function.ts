import { invoke } from "@tauri-apps/api/core";
import { STORAGE_KEYS } from "@/config";
import { safeLocalStorage } from "../storage";
import { floatArrayToWav } from "../utils";
import { blobToBase64 } from "./common.function";

/**
 * On-device speech recognition (beta).
 *
 * Silero VAD + a streaming Zipformer + Smart Turn run inside the Rust backend
 * (src-tauri/local-stt). When enabled:
 *  - hands-free system audio is transcribed in Rust and arrives as
 *    `speech-transcribed` events (no STT request at all);
 *  - every other STT call (mic, manual capture) goes through
 *    `transcribeLocally` first and falls back to the configured provider.
 */

/** Window event fired when the toggle changes in this window. */
export const LOCAL_STT_CHANGED = "local-stt-changed";

export const LOCAL_STT_EVENTS = {
  /** A finished turn from the system-audio pipeline. */
  TRANSCRIBED: "speech-transcribed",
  /** Live hypothesis while someone is speaking. */
  PARTIAL: "local-stt-partial",
  /** Smart Turn expects more speech and is holding the turn. */
  HELD: "local-stt-held",
  /** Capture could not use on-device STT and fell back to the provider. */
  FALLBACK: "local-stt-fallback",
  /** Model download progress. */
  DOWNLOAD: "local-stt-download",
} as const;

export interface LocalSttModel {
  id: string;
  title: string;
  license: string;
  bytes: number;
  ready: boolean;
}

export interface LocalSttStatus {
  /** Whether this build includes on-device STT. */
  available: boolean;
  modelsReady: boolean;
  downloading: boolean;
  loaded: boolean;
  modelsDir: string;
  totalBytes: number;
  missingBytes: number;
  models: LocalSttModel[];
  onnxruntime: string | null;
}

export interface LocalSttDownloadProgress {
  file: string;
  doneBytes: number;
  totalBytes: number;
  finished: boolean;
  error: string | null;
}

/** Mirrors `clocks_local_stt::TurnResult` (snake_case from Rust). */
export interface LocalSttTurn {
  text: string;
  end: "complete" | "vad_only" | "hold_timeout" | "max_length" | "end_of_input";
  probability: number | null;
  segments: number;
  start_s: number;
  vad_end_s: number;
  released_s: number;
  asr_flush_ms: number;
  turn_ms: number | null;
  decision_ms: number;
}

export function isLocalSttEnabled(): boolean {
  return safeLocalStorage.getItem(STORAGE_KEYS.LOCAL_STT_ENABLED) === "true";
}

export function setLocalSttEnabled(enabled: boolean): void {
  safeLocalStorage.setItem(STORAGE_KEYS.LOCAL_STT_ENABLED, String(enabled));
  window.dispatchEvent(new Event(LOCAL_STT_CHANGED));
}

export const getLocalSttStatus = () => invoke<LocalSttStatus>("local_stt_status");
export const downloadLocalSttModels = () => invoke<void>("local_stt_download");
export const deleteLocalSttModels = () => invoke<void>("local_stt_delete");

/**
 * Converts recorder output to WAV. WAV passes through untouched; anything else
 * (MediaRecorder webm/mp4) is decoded by WebAudio, resampled to 16 kHz and
 * mixed down to mono.
 */
async function toWav(audio: Blob): Promise<Blob> {
  if (audio.type.includes("wav")) return audio;
  const context = new OfflineAudioContext(1, 1, 16000);
  const decoded = await context.decodeAudioData(await audio.arrayBuffer());
  const mono = new Float32Array(decoded.length);
  for (let c = 0; c < decoded.numberOfChannels; c++) {
    const channel = decoded.getChannelData(c);
    for (let i = 0; i < channel.length; i++) {
      mono[i] += channel[i] / decoded.numberOfChannels;
    }
  }
  return floatArrayToWav(mono, decoded.sampleRate, "wav");
}

/** Transcribes a finished clip on this machine. Throws if unavailable. */
export async function transcribeLocally(audio: Blob): Promise<string> {
  const audioBase64 = await blobToBase64(await toWav(audio));
  const result = await invoke<{ text: string; audioS: number; computeMs: number }>(
    "local_stt_transcribe",
    { audioBase64 }
  );
  return result.text;
}

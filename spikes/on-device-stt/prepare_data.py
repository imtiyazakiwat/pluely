#!/usr/bin/env python3
"""Download small, labelled evaluation sets for the on-device STT spike (issue #1).

Every set is pulled from the Hugging Face datasets-server API, converted to
16 kHz mono 16-bit WAV, and described by a `manifest.jsonl` that the Rust
benchmark (`src-tauri/local-stt/examples/bench.rs`) reads.

Sets:
  librispeech    73 read-speech utterances (clean, studio). CC-BY-4.0.
  ami-ihm        AMI meetings, individual headset mics. Stand-in for system
                 audio of a remote meeting participant. CC-BY-4.0.
  ami-sdm        AMI meetings, single distant room mic. Stand-in for a laptop
                 mic in a meeting room. CC-BY-4.0.
  earnings22     Earnings calls (telephone-band, accented). Stand-in for
                 conference-call system audio. CC-BY-SA-4.0.
  smart-turn     English turn-completion clips with complete/incomplete labels
                 from Pipecat's held-out Smart Turn v3.2 test set. CC-BY-4.0.

Audio is only used locally for measurement and is never committed.
"""

from __future__ import annotations

import argparse
import io
import json
import random
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np
import soundfile as sf
import soxr

ROWS_API = "https://datasets-server.huggingface.co/rows"
PAGE = 100
TARGET_SR = 16000
USER_AGENT = "clocks-on-device-stt-spike/0.1"


def http_get(url: str, retries: int = 6) -> bytes:
    last_err: Exception | None = None
    for attempt in range(retries):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
            with urllib.request.urlopen(req, timeout=60) as resp:
                return resp.read()
        except urllib.error.HTTPError as err:
            last_err = err
            # datasets-server rate-limits bursts of /rows calls; back off hard.
            time.sleep((20.0 if err.code == 429 else 1.5) * (attempt + 1))
        except Exception as err:  # noqa: BLE001 - retry any transport error
            last_err = err
            time.sleep(1.5 * (attempt + 1))
    raise RuntimeError(f"GET failed after {retries} attempts: {url[:120]}: {last_err}")


def fetch_page(dataset: str, config: str, split: str, offset: int) -> list[dict]:
    query = urllib.parse.urlencode(
        {"dataset": dataset, "config": config, "split": split, "offset": offset, "length": PAGE}
    )
    payload = json.loads(http_get(f"{ROWS_API}?{query}"))
    if "error" in payload:
        raise RuntimeError(f"{dataset}/{config}/{split}@{offset}: {payload['error']}")
    return [r["row"] for r in payload["rows"]]


def sample_rows(dataset, config, split, total_rows, pages, keep, seed) -> list[dict]:
    """Fetch `pages` random pages and return the rows accepted by `keep`."""
    rng = random.Random(seed)
    max_page = max(0, (total_rows - PAGE) // PAGE)
    offsets = sorted(rng.sample(range(max_page + 1), min(pages, max_page + 1)))
    rows: list[dict] = []
    for i, page in enumerate(offsets):
        for row in fetch_page(dataset, config, split, page * PAGE):
            if keep(row):
                rows.append(row)
        print(f"  {dataset}/{config}: page {i + 1}/{len(offsets)}, {len(rows)} candidates", flush=True)
        time.sleep(1.0)
    return rows


def done(name: str, out_root: Path) -> bool:
    """A set is complete once its manifest exists; reruns skip it."""
    finished = (out_root / name / "manifest.jsonl").exists()
    if finished:
        print(f"{name}: already prepared, skipping")
    return finished


def decode_audio(blob: bytes) -> np.ndarray:
    audio, sr = sf.read(io.BytesIO(blob), dtype="float32", always_2d=True)
    audio = audio.mean(axis=1)
    if sr != TARGET_SR:
        # Earnings calls arrive at 8/16/24/44.1 kHz. Real calls do too.
        audio = soxr.resample(audio, sr, TARGET_SR, quality="HQ").astype(np.float32)
    return audio


def write_set(name: str, items: list[dict], out_root: Path) -> None:
    out_dir = out_root / name
    out_dir.mkdir(parents=True, exist_ok=True)

    def work(item: dict) -> dict | None:
        path = out_dir / f"{item['id']}.wav"
        try:
            if not path.exists():
                audio = decode_audio(http_get(item.pop("url")))
                sf.write(path, audio, TARGET_SR, subtype="PCM_16")
            else:
                item.pop("url")
            info = sf.info(path)
            return {**item, "path": str(path.relative_to(out_root)), "duration": round(info.duration, 3)}
        except Exception as err:  # noqa: BLE001 - skip a broken clip, keep the set
            print(f"  skip {item['id']}: {err}", file=sys.stderr)
            return None

    with ThreadPoolExecutor(max_workers=8) as pool:
        written = [r for r in pool.map(work, items) if r]

    with open(out_dir / "manifest.jsonl", "w") as fh:
        for row in written:
            fh.write(json.dumps(row) + "\n")
    total = sum(r["duration"] for r in written)
    print(f"{name}: {len(written)} clips, {total / 60:.1f} min")


def word_count(text: str) -> int:
    return len([w for w in text.split() if any(c.isalnum() for c in w)])


def speech_set(rows, id_fn, text_key, duration_fn, n, seed, min_words=6, min_s=2.0, max_s=20.0):
    """Pick `n` request-sized utterances: at least `min_words` words, 2-20 s long."""
    picked = [
        r for r in rows
        if word_count(r[text_key]) >= min_words and min_s <= duration_fn(r) <= max_s
    ]
    rng = random.Random(seed)
    rng.shuffle(picked)
    out = []
    for r in picked[:n]:
        safe_id = "".join(c if c.isalnum() or c in "-_" else "_" for c in str(id_fn(r)))
        out.append({"id": safe_id, "text": r[text_key], "url": r["audio"][0]["src"]})
    return out


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, default=Path(__file__).parent / ".cache" / "data")
    parser.add_argument("--meeting-clips", type=int, default=120)
    parser.add_argument("--call-clips", type=int, default=100)
    parser.add_argument("--turn-clips-per-label", type=int, default=200)
    parser.add_argument("--seed", type=int, default=20261014)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)

    # LibriSpeech dummy: all 73 rows, one page.
    if not done("librispeech", args.out):
        rows = fetch_page("hf-internal-testing/librispeech_asr_dummy", "clean", "validation", 0)
        write_set(
            "librispeech",
            [{"id": r["id"], "text": r["text"], "url": r["audio"][0]["src"]} for r in rows],
            args.out,
        )

    for config, name in (("ihm", "ami-ihm"), ("sdm", "ami-sdm")):
        if done(name, args.out):
            continue
        rows = sample_rows("edinburghcstr/ami", config, "test", 12643, 30, lambda r: True, args.seed)
        items = speech_set(rows, lambda r: r["audio_id"], "text",
                           lambda r: r["end_time"] - r["begin_time"], args.meeting_clips, args.seed)
        write_set(name, items, args.out)

    if not done("earnings22", args.out):
        rows = sample_rows("distil-whisper/earnings22", "chunked", "test", 57391, 30,
                           lambda r: True, args.seed)
        # Segment ids repeat across calls, so qualify them with the call's file id.
        items = speech_set(rows, lambda r: f"{r['file_id']}_{r['segment_id']}", "transcription",
                           lambda r: r["end_ts"] - r["start_ts"], args.call_clips, args.seed)
        write_set("earnings22", items, args.out)

    if not done("smart-turn", args.out):
        rows = sample_rows(
            "pipecat-ai/smart-turn-data-v3.2-test", "default", "train", 31527, 40,
            lambda r: r["language"] == "eng", args.seed,
        )
        rng = random.Random(args.seed)
        rng.shuffle(rows)
        items = []
        for label in (True, False):
            chosen = [r for r in rows if bool(r["endpoint_bool"]) == label][: args.turn_clips_per_label]
            for r in chosen:
                items.append({
                    "id": r["id"],
                    "text": r.get("spoken_text") or "",
                    "endpoint": bool(r["endpoint_bool"]),
                    "synthetic": bool(r.get("synthetic")),
                    "source": r.get("dataset"),
                    "url": r["audio"][0]["src"],
                })
        write_set("smart-turn", items, args.out)


if __name__ == "__main__":
    main()

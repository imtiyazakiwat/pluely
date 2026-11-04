#!/usr/bin/env python3
"""Summarise bench.jsonl records into the tables used in REPORT.md.

  .cache/venv/bin/python score.py .cache/results/*.jsonl [--json summary.json]

WER uses the Whisper English normaliser (the Open ASR Leaderboard convention)
and is corpus-level: total edits / total reference words per set.
"""

from __future__ import annotations

import argparse
import json
import statistics
from collections import defaultdict
from pathlib import Path

import jiwer
from transformers.models.whisper.english_normalizer import EnglishTextNormalizer

HERE = Path(__file__).parent
NORMALIZE = EnglishTextNormalizer(json.load(open(HERE / ".cache" / "whisper-normalizer.json")))
SPEECH_SETS = ["librispeech", "ami-ihm", "ami-sdm", "earnings22"]


def pct(values, q):
    v = sorted(values)
    if not v:
        return float("nan")
    k = (len(v) - 1) * q
    lo, hi = int(k), min(int(k) + 1, len(v) - 1)
    return v[lo] + (v[hi] - v[lo]) * (k - lo)


def wer(rows) -> tuple[float, int]:
    refs, hyps = [], []
    for r in rows:
        ref = NORMALIZE(r["ref"])
        if not ref.strip():
            continue
        refs.append(ref)
        hyps.append(NORMALIZE(r["hyp"]) or "")
    if not refs:
        return float("nan"), 0
    out = jiwer.process_words(refs, hyps)
    return out.wer * 100, sum(len(r.split()) for r in refs)


def load(paths):
    rows, summaries = [], []
    for p in paths:
        for line in open(p):
            try:
                r = json.loads(line)
            except json.JSONDecodeError:
                # A run still in progress leaves a partial last line.
                continue
            (summaries if r.get("summary") else rows).append(r)
    return rows, summaries


def final_turn_latency(r):
    """Speech end -> text available for the turn that contains the speech end."""
    turns = r["turns"]
    if not turns:
        return None
    t = turns[-1]
    if t["end"] == "end_of_input":
        return None
    # Audio arrives in capture chunks; the decision runs once the chunk holding
    # the trigger (pause or hold expiry) is in, and takes `decision_ms`.
    released_ms = (t["chunk_end_s"] - r["speech_end_s"]) * 1000
    return {
        "end": t["end"],
        "vad_cutoff_ms": (t["vad_end_s"] - r["speech_end_s"]) * 1000,
        "asr_flush_ms": t["asr_flush_ms"],
        "turn_ms": t["turn_ms"],
        "decision_ms": t["decision_ms"],
        "e2e_ms": released_ms + t["decision_ms"],
    }


def summarise(rows, summaries):
    out = {"wer": [], "latency": [], "fragmentation": [], "resources": [], "energy": []}

    by_run = defaultdict(list)
    for r in rows:
        if r["mode"] in ("pipeline", "whole"):
            by_run[(r["mode"], r["tag"], r["asr"])].append(r)

    for (mode, tag, asr), rs in sorted(by_run.items()):
        line = {"mode": mode, "tag": tag, "asr": asr}
        for s in SPEECH_SETS:
            subset = [r for r in rs if r["set"] == s]
            if subset:
                w, n = wer(subset)
                line[s] = round(w, 2)
                line[f"{s}_words"] = n
        all_w, all_n = wer(rs)
        line["all"] = round(all_w, 2)
        line["all_words"] = all_n
        out["wer"].append(line)

        if mode != "pipeline":
            continue
        lats = [x for x in (final_turn_latency(r) for r in rs) if x]
        released_now = [x for x in lats if x["end"] in ("complete", "vad_only")]
        lat = {"tag": tag, "asr": asr, "utterances": len(rs), "with_latency": len(lats),
               "held_to_timeout_pct": round(100 * sum(x["end"] == "hold_timeout" for x in lats) / max(1, len(lats)), 1)}
        for key in ("vad_cutoff_ms", "asr_flush_ms", "turn_ms", "decision_ms", "e2e_ms"):
            vals = [x[key] for x in (released_now if key != "e2e_ms" else lats) if x[key] is not None]
            if vals:
                lat[key] = {"p50": round(pct(vals, 0.5), 1), "p95": round(pct(vals, 0.95), 1)}
        vals = [x["e2e_ms"] for x in released_now]
        if vals:
            lat["e2e_released_at_vad_end_ms"] = {"p50": round(pct(vals, 0.5), 1), "p95": round(pct(vals, 0.95), 1)}
        out["latency"].append(lat)

        frag = {"tag": tag, "asr": asr}
        for s in SPEECH_SETS + ["all"]:
            subset = [r for r in rs if s == "all" or r["set"] == s]
            if not subset:
                continue
            turns = [len(r["turns"]) for r in subset]
            frag[s] = {
                "turns_per_utt": round(statistics.mean(turns), 2),
                "split_pct": round(100 * sum(t > 1 for t in turns) / len(turns), 1),
                "missed_pct": round(100 * sum(t == 0 for t in turns) / len(turns), 1),
            }
        out["fragmentation"].append(frag)

    for s in summaries:
        if s["mode"] in ("pipeline", "whole", "turn"):
            out["resources"].append({k: (round(v, 4) if isinstance(v, float) else v)
                                     for k, v in s.items() if k != "summary"})

    energy = defaultdict(list)
    for r in rows:
        if r["mode"] == "energy":
            energy[r["device_rate"]].append(r)
    for rate, rs in sorted(energy.items()):
        cut, missed, split = [], 0, 0
        for r in rs:
            after = [t for t in r["emitted_s"] if t >= r["speech_end_s"]]
            if not r["emitted_s"]:
                missed += 1
                continue
            if len(r["emitted_s"]) > 1:
                split += 1
            if after:
                cut.append((after[0] - r["speech_end_s"]) * 1000)
        out["energy"].append({
            "device_rate": rate, "clips": len(rs),
            "cutoff_ms": {"p50": round(pct(cut, 0.5), 1), "p95": round(pct(cut, 0.95), 1)},
            "never_triggered_pct": round(100 * missed / len(rs), 1),
            "split_pct": round(100 * split / len(rs), 1),
        })
    return out


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("paths", nargs="+", type=Path)
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()
    rows, summaries = load(args.paths)
    out = summarise(rows, summaries)
    if args.json:
        args.json.write_text(json.dumps(out, indent=2))
    for section, items in out.items():
        print(f"== {section}")
        for item in items:
            print("  " + json.dumps(item))


if __name__ == "__main__":
    main()

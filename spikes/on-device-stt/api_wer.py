#!/usr/bin/env python3
"""Same clips, sent to a cloud STT API, for the WER/latency comparison.

Not run in the spike (no API key was available on the test machine). Uses the
OpenAI-compatible /audio/transcriptions endpoint that Clocks' "OpenAI Whisper"
and "Groq Whisper" providers call. Writes records `score.py` understands.

  OPENAI_API_KEY=... .cache/venv/bin/python api_wer.py --provider openai --model whisper-1
  GROQ_API_KEY=...   .cache/venv/bin/python api_wer.py --provider groq --model whisper-large-v3-turbo
  .cache/venv/bin/python score.py .cache/results/api.jsonl .cache/results/whole.jsonl

This uploads the public test clips (LibriSpeech/AMI/Earnings-22) to the
provider and is billed to your key (~45 minutes of audio).
"""

import argparse
import json
import os
import time
import urllib.request
import uuid
from pathlib import Path

HERE = Path(__file__).parent
ENDPOINTS = {
    "openai": ("https://api.openai.com/v1/audio/transcriptions", "OPENAI_API_KEY"),
    "groq": ("https://api.groq.com/openai/v1/audio/transcriptions", "GROQ_API_KEY"),
}


def transcribe(url: str, key: str, model: str, wav: bytes) -> str:
    boundary = uuid.uuid4().hex
    parts = [
        f'--{boundary}\r\nContent-Disposition: form-data; name="model"\r\n\r\n{model}\r\n'.encode(),
        f'--{boundary}\r\nContent-Disposition: form-data; name="language"\r\n\r\nen\r\n'.encode(),
        f'--{boundary}\r\nContent-Disposition: form-data; name="response_format"\r\n\r\njson\r\n'.encode(),
        (f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="audio.wav"\r\n'
         "Content-Type: audio/wav\r\n\r\n").encode() + wav + b"\r\n",
        f"--{boundary}--\r\n".encode(),
    ]
    req = urllib.request.Request(
        url,
        data=b"".join(parts),
        headers={"Authorization": f"Bearer {key}", "Content-Type": f"multipart/form-data; boundary={boundary}"},
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read())["text"]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--provider", choices=sorted(ENDPOINTS), required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--sets", default="librispeech,ami-ihm,ami-sdm,earnings22")
    parser.add_argument("--data", type=Path, default=HERE / ".cache" / "data")
    parser.add_argument("--out", type=Path, default=HERE / ".cache" / "results" / "api.jsonl")
    args = parser.parse_args()

    url, env = ENDPOINTS[args.provider]
    key = os.environ.get(env)
    if not key:
        raise SystemExit(f"set {env} first")
    args.out.parent.mkdir(parents=True, exist_ok=True)
    with open(args.out, "a") as out:
        for name in args.sets.split(","):
            for line in open(args.data / name / "manifest.jsonl"):
                clip = json.loads(line)
                wav = (args.data / clip["path"]).read_bytes()
                started = time.perf_counter()
                text = transcribe(url, key, args.model, wav)
                out.write(json.dumps({
                    "mode": "whole", "tag": f"api-{args.provider}", "set": name, "id": clip["id"],
                    "asr": f"{args.provider}:{args.model}", "ref": clip["text"], "hyp": text,
                    "audio_s": clip["duration"],
                    "compute_ms": (time.perf_counter() - started) * 1000,  # includes the network
                }) + "\n")
                out.flush()


if __name__ == "__main__":
    main()

"""Voiceover clips from ElevenLabs, cached by their text.

    python voice.py "Some sentence."       fetch (or reuse) a clip, print its path and length

A scene's `say TEXT` (vnc_drive.py) asks for its clip before the take goes on,
so the driver knows how long the line runs; edit.py lays the same cached
clips under the video. Nothing is fetched twice: the cache key is the text,
voice, model and settings.

The API key is ELEVENLABS_API_KEY, from the environment or the repo's `.env`.
ELEVENLABS_VOICE and ELEVENLABS_MODEL override the defaults.
"""

import hashlib
import json
import os
import subprocess
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CACHE = ROOT / "target" / "tutorials" / "voice"
VOICE = os.environ.get("ELEVENLABS_VOICE", "Gfpl8Yo74Is0W6cPUWWT")
MODEL = os.environ.get("ELEVENLABS_MODEL", "eleven_multilingual_v2")
SETTINGS = {"stability": 0.5, "similarity_boost": 0.75, "style": 0.0, "use_speaker_boost": True}


def _api_key() -> str:
    key = os.environ.get("ELEVENLABS_API_KEY")
    if not key and (ROOT / ".env").exists():
        for line in (ROOT / ".env").read_text().splitlines():
            name, _, value = line.partition("=")
            if name.strip() == "ELEVENLABS_API_KEY":
                key = value.strip().strip("'\"")
    if not key:
        raise SystemExit("voice: no ELEVENLABS_API_KEY (environment or .env)")
    return key


def path_for(text: str) -> Path:
    key = json.dumps([VOICE, MODEL, SETTINGS, text], sort_keys=True)
    return CACHE / f"{hashlib.sha256(key.encode()).hexdigest()[:20]}.mp3"


def is_cached(text: str) -> bool:
    return path_for(text).exists()


def clip(text: str) -> Path:
    """The clip for TEXT, fetched from ElevenLabs on first use."""
    path = path_for(text)
    if path.exists():
        return path
    req = urllib.request.Request(
        f"https://api.elevenlabs.io/v1/text-to-speech/{VOICE}?output_format=mp3_44100_128",
        data=json.dumps({"text": text, "model_id": MODEL, "voice_settings": SETTINGS}).encode(),
        headers={"xi-api-key": _api_key(), "Content-Type": "application/json", "Accept": "audio/mpeg"},
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            audio = resp.read()
    except urllib.error.HTTPError as err:
        raise SystemExit(f"voice: ElevenLabs said {err.code}: {err.read()[:300].decode(errors='replace')}")
    CACHE.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".part")
    tmp.write_bytes(audio)
    tmp.rename(path)
    path.with_suffix(".txt").write_text(text + "\n")
    return path


def length(path: Path) -> float:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path)],
        capture_output=True, text=True, check=True,
    ).stdout
    return float(out.strip())


if __name__ == "__main__":
    p = clip(" ".join(sys.argv[1:]))
    print(f"{p} {length(p):.2f}s")

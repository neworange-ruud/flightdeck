"""Cut a raw tutorial recording into the final video, from its timeline.

    python edit.py RAW.mov TIMELINE.log OUT.mp4

The timeline holds host timestamps: `start` and `stop` (written by `vm record`)
and the marks a scene wrote (see vnc_drive.py): cut/uncut, fast/normal,
caption. screencapture starts a moment after it is launched, so the video's
t=0 is placed at stop - duration (the stop mark is written right before the
stop signal, over an already-open SSH connection). The recording is
variable frame rate (a frame only when the screen changes), so it is made
constant before trimming.

Output: 1920x1080, 30 fps, H.264. Cut stretches are dropped, fast stretches
sped up (with a small "Nx" badge), captions drawn as a bar at the bottom.
"""

import json
import shlex
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

W, H = 1920, 1080
FONT = "/System/Library/Fonts/SFNS.ttf"
# macOS draws a purple "screen is being recorded" dot in the top-right corner
# of a full-screen app; paint it out from the pixels around it.
INDICATOR = "delogo=x=1898:y=1:w=20:h=17"


@dataclass
class Segment:
    start: float
    end: float
    speed: float
    caption: str


def duration(path: Path) -> float:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "json", str(path)],
        capture_output=True, text=True, check=True,
    ).stdout
    return float(json.loads(out)["format"]["duration"])


def segments(timeline: Path, video_len: float) -> list[Segment]:
    marks = []
    for line in timeline.read_text().splitlines():
        stamp, *words = shlex.split(line)
        marks.append((float(stamp), words))
    start = next(t for t, w in marks if w[0] == "start")
    stop = next(t for t, w in reversed(marks) if w[0] == "stop")
    t0 = stop - video_len  # host time of the video's first frame
    del start

    segs, cut, speed, caption, at = [], False, 1.0, "", 0.0
    for t, words in sorted(marks):
        kind, args = words[0], words[1:]
        if kind in ("start", "stop"):
            continue
        vt = min(max(t - t0, 0.0), video_len)
        if vt > at and not cut:
            segs.append(Segment(at, vt, speed, caption))
        at = max(at, vt)
        if kind == "cut":
            cut = True
        elif kind == "uncut":
            cut = False
        elif kind == "fast":
            speed = float(args[0])
        elif kind == "normal":
            speed = 1.0
        elif kind == "caption":
            caption = args[0] if args else ""
    if at < video_len and not cut:
        segs.append(Segment(at, video_len, speed, caption))
    return [s for s in segs if s.end - s.start > 0.05]


def overlay_png(path: Path, caption: str, speed: float) -> None:
    """A transparent 1920x1080 frame with the caption bar and speed badge."""
    img = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    if caption:
        font = ImageFont.truetype(FONT, 34)
        box = d.textbbox((0, 0), caption, font=font)
        tw, th = box[2] - box[0], box[3] - box[1]
        pad_x, pad_y = 28, 18
        x0 = (W - tw) // 2 - pad_x
        y0 = H - 110 - th - pad_y
        d.rounded_rectangle(
            (x0, y0, x0 + tw + 2 * pad_x, y0 + th + 2 * pad_y),
            radius=14, fill=(20, 18, 16, 225), outline=(255, 255, 255, 40), width=1,
        )
        d.text((x0 + pad_x - box[0], y0 + pad_y - box[1]), caption, font=font, fill=(245, 242, 235, 255))
    if speed != 1.0:
        font = ImageFont.truetype(FONT, 26)
        label = f"{speed:g}× speed"
        box = d.textbbox((0, 0), label, font=font)
        x0, y0 = W - (box[2] - box[0]) - 64, 70
        d.rounded_rectangle(
            (x0 - 16, y0 - 10, x0 + box[2] - box[0] + 16, y0 + box[3] - box[1] + 12),
            radius=10, fill=(20, 18, 16, 210),
        )
        d.text((x0 - box[0], y0 - box[1]), label, font=font, fill=(245, 242, 235, 255))
    img.save(path)


def main() -> None:
    raw, timeline, out = map(Path, sys.argv[1:4])
    segs = segments(timeline, duration(raw))
    if not segs:
        raise SystemExit("nothing left after cuts")

    with tempfile.TemporaryDirectory() as tmp:
        inputs = ["-i", str(raw)]
        chains, labels = [], []
        for i, s in enumerate(segs):
            png = Path(tmp) / f"o{i}.png"
            overlay_png(png, s.caption, s.speed)
            inputs += ["-i", str(png)]
            chains.append(
                f"[0:v]fps=60,trim={s.start:.3f}:{s.end:.3f},setpts=(PTS-STARTPTS)/{s.speed},"
                f"fps=30,scale={W}:{H}:flags=lanczos,{INDICATOR}[v{i}];"
                f"[v{i}][{i + 1}:v]overlay=0:0,format=yuv420p[s{i}]"
            )
            labels.append(f"[s{i}]")
        graph = ";".join(chains) + ";" + "".join(labels) + f"concat=n={len(segs)}:v=1:a=0[out]"
        subprocess.run(
            ["ffmpeg", "-loglevel", "error", "-y", *inputs, "-filter_complex", graph,
             "-map", "[out]", "-c:v", "libx264", "-preset", "slow", "-crf", "18",
             "-movflags", "+faststart", str(out)],
            check=True,
        )
    kept = sum((s.end - s.start) / s.speed for s in segs)
    print(f"{out}  {kept:.1f}s from {len(segs)} segments")


if __name__ == "__main__":
    main()

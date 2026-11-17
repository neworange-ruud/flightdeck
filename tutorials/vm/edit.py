"""Cut a raw tutorial recording into the final video, from its timeline.

    python edit.py RAW.mov TIMELINE.log OUT.mp4

The timeline holds host timestamps: `start` and `stop` (written by `vm record`)
and the marks a scene wrote (see vnc_drive.py): cut/uncut, fast/normal,
caption, highlight/unhighlight, voice, hold/release. The video's t=0 is the start mark plus
screencapture's start-up time (or an explicit `t0 EPOCH` line). The recording
is variable frame rate (a frame only when the screen changes), so it is made
constant (and scaled to 1080p) first; then every segment is rendered on its
own, in parallel, and the pieces are joined without re-encoding. (One filter
graph feeding the recording into dozens of trims deadlocks ffmpeg 7's
scheduler on longer takes.)

Output: 1920x1080, 30 fps, H.264 + AAC. The guest screen is smaller than that
in points (1440x810 by default) and is scaled up, so the UI reads bigger. Cut
stretches are dropped, fast stretches sped up (with a small "Nx" badge),
captions drawn as a bar at the bottom, highlights as a fading frame (and, for
`spot`, a dimmed surround), and voice lines (voice.py's cached clips) laid
in where they were said, never overlapping.
"""

import json
import os
import shlex
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

import voice

W, H = 1920, 1080
FPS = 30
FONT = "/System/Library/Fonts/SFNS.ttf"
# macOS draws a purple "screen is being recorded" dot in the top-right corner
# of a full-screen app: (right inset, top, width, height) in points. Painted
# out from the pixels around it.
INDICATOR_PT = (2, 1, 20, 17)
# From launching screencapture to its first frame (measured on a take by the
# frame where a click's effect appears).
START_LATENCY = 0.35
ACCENT = (255, 184, 0)
FADE = 0.3
VOICE_GAP = 0.2


@dataclass
class Segment:
    start: float
    end: float
    speed: float
    caption: str
    highlights: tuple = ()
    out: float = 0.0  # where it starts in the output

    @property
    def length(self) -> float:
        return (self.end - self.start) / self.speed


@dataclass
class Timeline:
    segments: list
    voices: list = field(default_factory=list)  # (video time, text)


def probe(path: Path) -> tuple[float, int]:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
         "format=duration:stream=width", "-of", "json", str(path)],
        capture_output=True, text=True, check=True,
    ).stdout
    info = json.loads(out)
    return float(info["format"]["duration"]), int(info["streams"][0]["width"])


def parse(timeline: Path, video_len: float) -> Timeline:
    marks = []
    for line in timeline.read_text().splitlines():
        stamp, *words = shlex.split(line)
        marks.append((float(stamp), words))
    # Host time of the video's first frame: an explicit `t0` mark if a take
    # was synced by hand, else the start mark (written right after
    # screencapture was launched) plus its start-up time. Not the stop: the
    # file's end does not line up with the stop signal.
    explicit = [w[1] for _, w in marks if w[0] == "t0"]
    if explicit:
        t0 = float(explicit[0])
    else:
        t0 = next(t for t, w in marks if w[0] == "start") + START_LATENCY

    # A static screen records no frames, so a take that ends on one has a
    # file shorter than the take: the timeline runs to the stop mark, and the
    # constant-rate copy repeats the last frame (see main).
    stops = [t for t, w in marks if w[0] == "stop"]
    if stops:
        video_len = max(video_len, stops[-1] - t0)

    segs, voices = [], []
    # `cut`/`uncut` are the scene's; `hold`/`release` the driver's own (while
    # it fetches voice clips), so the two never undo each other.
    cut, held, speed, caption, lights, at = False, False, 1.0, "", (), 0.0
    for t, words in sorted(marks, key=lambda m: m[0]):
        kind, args = words[0], words[1:]
        if kind in ("start", "stop", "t0"):
            continue
        vt = min(max(t - t0, 0.0), video_len)
        if kind == "voice":
            voices.append((vt, args[0]))
            continue
        if vt > at and not (cut or held):
            segs.append(Segment(at, vt, speed, caption, lights))
        at = max(at, vt)
        if kind == "cut":
            cut = True
        elif kind == "uncut":
            cut = False
        elif kind == "hold":
            held = True
        elif kind == "release":
            held = False
        elif kind == "fast":
            speed = float(args[0])
        elif kind == "normal":
            speed = 1.0
        elif kind == "caption":
            caption = args[0] if args else ""
        elif kind == "highlight":
            x, y, w, h = map(float, args[:4])
            lights = lights + ((x, y, w, h, args[4] if len(args) > 4 else "box"),)
        elif kind == "unhighlight":
            lights = ()
    if at < video_len and not (cut or held):
        segs.append(Segment(at, video_len, speed, caption, lights))
    segs = [s for s in segs if s.end - s.start > 0.05]
    out = 0.0
    for s in segs:
        s.out = out
        out += s.length
    return Timeline(segs, voices)


def output_time(segs: list, vt: float) -> float:
    """Where video time VT lands in the output (a cut moves it to the next kept frame)."""
    for s in segs:
        if vt < s.end:
            return s.out + max(vt - s.start, 0.0) / s.speed
    return segs[-1].out + segs[-1].length


def base_png(path: Path, caption: str, speed: float) -> None:
    """A transparent frame with the caption bar and speed badge."""
    img = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    if caption:
        font = ImageFont.truetype(FONT, 34)
        box = d.textbbox((0, 0), caption, font=font)
        tw, th = box[2] - box[0], box[3] - box[1]
        pad_x, pad_y = 28, 18
        x0 = (W - tw) // 2 - pad_x
        y0 = H - 58 - th - 2 * pad_y
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


def highlight_png(path: Path, lights: tuple, k: float) -> None:
    """Highlight frames (points scaled by K) with a soft glow; `spot` dims the rest."""
    img = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    rects = []
    for x, y, w, h, style in lights:
        pad = 8
        rects.append(((x * k - pad, y * k - pad, (x + w) * k + pad, (y + h) * k + pad), style))
    if any(style == "spot" for _, style in rects):
        shade = Image.new("L", (W, H), 120)
        hole = ImageDraw.Draw(shade)
        for r, style in rects:
            if style == "spot":
                hole.rounded_rectangle(r, radius=14, fill=0)
        img.paste((0, 0, 0, 255), (0, 0), shade)
    glow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow)
    for r, _ in rects:
        gd.rounded_rectangle(r, radius=14, outline=ACCENT + (200,), width=12)
    img = Image.alpha_composite(img, glow.filter(ImageFilter.GaussianBlur(10)))
    d = ImageDraw.Draw(img)
    for r, _ in rects:
        d.rounded_rectangle(r, radius=14, outline=ACCENT + (255,), width=5)
    img.save(path)


def write_vtt(path: Path, cues: list) -> None:
    """Subtitles for the voiceover: each line split at its sentences, the
    line's time shared out by length."""
    def stamp(t: float) -> str:
        return f"{int(t // 3600):02d}:{int(t % 3600 // 60):02d}:{t % 60:06.3f}"

    out = ["WEBVTT", ""]
    for start, end, text in cues:
        parts, cur = [], ""
        for word in text.split():
            cur = f"{cur} {word}".strip()
            if word[-1] in ".?!:" and len(cur) > 24:
                parts.append(cur)
                cur = ""
        if cur:
            parts.append(cur)
        total = sum(len(p) for p in parts)
        t = start
        for part in parts:
            span = (end - start) * len(part) / total
            out += [f"{stamp(t)} --> {stamp(t + span)}", part, ""]
            t += span
    path.write_text("\n".join(out))


def ffmpeg(*args: str) -> None:
    subprocess.run(["ffmpeg", "-nostdin", "-loglevel", "error", "-y", *args], check=True)


ENCODE = ["-c:v", "libx264", "-preset", "slow", "-crf", "18", "-pix_fmt", "yuv420p", "-r", str(FPS)]


def render_segment(i: int, s: Segment, src: Path, tmp: Path, k: float,
                   prev: tuple, nxt: tuple, frames: int, hold: float) -> Path:
    """Segment I of the constant-rate 1080p SRC, with its overlays, as an MP4."""
    base = tmp / f"b{i}.png"
    base_png(base, s.caption, s.speed)
    inputs = ["-ss", f"{s.start:.3f}", "-t", f"{s.end - s.start:.3f}", "-i", str(src), "-i", str(base)]
    graph = f"[0:v]setpts=(PTS-STARTPTS)/{s.speed},fps={FPS}"
    if hold > 0:  # the last segment holds its last frame while the voice ends
        graph += f",tpad=stop_mode=clone:stop_duration={hold:.3f}"
    graph += "[v];[v][1:v]overlay=0:0"
    length = frames / FPS
    if s.highlights:
        hl = tmp / f"h{i}.png"
        highlight_png(hl, s.highlights, k)
        inputs += ["-loop", "1", "-framerate", str(FPS), "-t", f"{length:.3f}", "-i", str(hl)]
        fade = min(FADE, length / 2)
        fx = "format=rgba"
        if s.highlights != prev:
            fx += f",fade=t=in:st=0:d={fade:.3f}:alpha=1"
        if s.highlights != nxt:
            fx += f",fade=t=out:st={length - fade:.3f}:d={fade:.3f}:alpha=1"
        graph = f"[2:v]{fx}[hl];" + graph + "[c];[c][hl]overlay=0:0:eof_action=pass"
    out = tmp / f"s{i:03d}.mp4"
    ffmpeg(*inputs, "-filter_complex", graph, "-frames:v", str(frames), *ENCODE, "-an", str(out))
    return out


def main() -> None:
    raw, timeline, out = map(Path, sys.argv[1:4])
    video_len, raw_w = probe(raw)
    k = W / (raw_w / 2)  # screen points -> output pixels (Retina raw)
    tl = parse(timeline, video_len)
    segs = tl.segments
    if not segs:
        raise SystemExit("nothing left after cuts")
    # Whole frames per segment; their sum is the output's real timeline.
    frames = [max(1, round(s.length * FPS)) for s in segs]
    at = 0
    for s, n in zip(segs, frames):
        s.out = at / FPS
        at += n
    total = at / FPS

    # Voice lines where they were said, pushed later if they would overlap.
    clips, cues, end = [], [], 0.0
    for vt, text in sorted(tl.voices):
        path = voice.clip(text)
        start = max(output_time(segs, vt), end + VOICE_GAP if clips else 0.0)
        if clips and start - output_time(segs, vt) > 0.5:
            print(f"voice: pushed {start - output_time(segs, vt):.1f}s later: {text[:50]}")
        clips.append((start, path))
        cues.append((start, start + voice.length(path), text))
        end = start + voice.length(path)
    final = max(total, end + 0.8)
    hold = final - total
    frames[-1] += round(hold * FPS)

    ri, gi, gw, gh = (round(v * k) for v in INDICATOR_PT)
    indicator = f"delogo=x={W - ri - gw}:y={gi}:w={gw}:h={gh}"

    with tempfile.TemporaryDirectory() as tmpdir:
        tmp = Path(tmpdir)
        cfr = tmp / "cfr.mp4"
        ffmpeg("-i", str(raw), "-vf",
               f"fps={FPS},scale={W}:{H}:flags=lanczos,{indicator},tpad=stop_mode=clone:stop_duration=60",
               "-c:v", "libx264", "-preset", "veryfast", "-crf", "8", "-g", str(FPS),
               "-pix_fmt", "yuv420p", "-an", str(cfr))
        jobs = []
        with ThreadPoolExecutor(max_workers=max(2, (os.cpu_count() or 4) // 3)) as pool:
            for i, s in enumerate(segs):
                prev = segs[i - 1].highlights if i else ()
                nxt = segs[i + 1].highlights if i + 1 < len(segs) else ()
                last_hold = hold if i == len(segs) - 1 else 0.0
                jobs.append(pool.submit(render_segment, i, s, cfr, tmp, k, prev, nxt, frames[i], last_hold))
            parts = [j.result() for j in jobs]
        listing = tmp / "parts.txt"
        listing.write_text("".join(f"file '{p}'\n" for p in parts))

        args = ["-f", "concat", "-safe", "0", "-i", str(listing)]
        maps = ["-map", "0:v", "-c:v", "copy"]
        if clips:
            for _, path in clips:
                args += ["-i", str(path)]
            delays = [
                f"[{j + 1}:a]aresample=48000,adelay=delays={round(at * 1000)}:all=1[a{j}]"
                for j, (at, _) in enumerate(clips)
            ]
            mix = "".join(f"[a{j}]" for j in range(len(clips)))
            graph = ";".join(delays) + f";{mix}amix=inputs={len(clips)}:normalize=0:duration=longest,apad[aout]"
            args += ["-filter_complex", graph]
            maps += ["-map", "[aout]", "-c:a", "aac", "-b:a", "192k"]
        ffmpeg(*args, *maps, "-t", f"{final:.3f}", "-movflags", "+faststart", str(out))
    if cues:
        write_vtt(out.with_suffix(".vtt"), cues)
    print(f"{out}  {final:.1f}s from {len(segs)} segments, {len(clips)} voice lines")


if __name__ == "__main__":
    main()

"""Drive the tutorial VM over VNC from a small scene script.

    python vnc_drive.py serve HOST::PORT PASSWORD SOCKET   hold the VNC connection
    python vnc_drive.py send SOCKET SCRIPT_FILE            run a script (- = stdin)

`tutorials/vm/up.sh` starts the daemon (`serve`) once per VM boot; `vm run`
sends scripts to it. One connection for the VM's whole life, because
connecting while the guest records its screen can crash the VM.

One command per line; `#` starts a comment; arguments are shell-quoted.
Coordinates are screen POINTS (the VM's display, 1440x810 by default); the
framebuffer is Retina, so they are doubled on the wire.

    glide X Y [SECS]     move the pointer smoothly (ease in-out), default 0.6 s
    move X Y             jump the pointer
    click [BUTTON]       press and release (1 left, 2 middle, 3 right)
    dclick               double click
    type TEXT [CPS]      type at CPS characters per second (default 14)
    paste TEXT           type as fast as the guest accepts (setup steps)
    key CHORD...         e.g. `key cmd-a bsp`, `key ctrl-g`, `key opt-2`, `key enter`
    scroll up|down [N]   wheel clicks
    clicktext TEXT [N]   find TEXT on screen (OCR, vm/guest/find-text.swift), glide
                         there and click; N picks the match, top to bottom
                         (default -1: the lowest), for targets that move between takes
    glidetext TEXT [N]   the same without the click
    pause SECS           wait
    waitfor SECS CMD...  run CMD on the host until it succeeds (or fail after SECS)
    sh CMD...            run CMD on the host once

Editing marks, written with host timestamps to $TUT_TIMELINE (set by
`vm record start`); `vm record stop` applies them:

    cut                  start a stretch to drop from the video (dead time)
    uncut                end it
    fast FACTOR          play what follows FACTOR times faster (an agent working)
    normal               back to normal speed
    caption TEXT         show TEXT from here until the next caption (or `caption`)
    highlighttext TEXT [N] [PAD]   highlight TEXT found on screen (see clicktext),
                         PAD points around it (default 8)
    highlight X Y W H [spot]   draw attention to a screen area (points) until
                         `unhighlight`; `spot` also dims everything around it
    unhighlight          remove all highlights

Voiceover (ElevenLabs, see voice.py):

    say TEXT             speak TEXT from here; waits first for the previous line
                         to finish, so lines never overlap
    waitvoice [EXTRA]    wait until the current line has been spoken (+EXTRA s)

Before a script runs, the clips for all its `say` lines are fetched (between
`hold` and `release` marks, dropped like a cut, so the fetching never shows in the video), which makes `say` instant and
lets the driver know how long each line runs.

Virtualization.framework's VNC server maps keys to a US layout and ignores a
keysym's shift level, so capitals and shifted symbols are sent with Shift
held, and it maps X11 Alt to Command and Meta to Option, so `cmd-` becomes
`alt-` and `opt-` becomes `meta-`.
"""

import math
import os
import shlex
import subprocess
import sys
import time

from vncdotool import api

import voice

SCALE = 2  # points -> framebuffer pixels
SHIFTED = dict(zip('~!@#$%^&*()_+{}|:"<>?', "`1234567890-=[]\\;',./"))
NAMED = {"\n": "enter", "\t": "tab", " ": "space"}


class Driver:
    def __init__(self, client):
        self.c = client
        self.x, self.y = 720.0, 405.0
        self.voice_end = 0.0

    def _move(self, x, y):
        self.x, self.y = x, y
        self.c.mouseMove(round(x * SCALE), round(y * SCALE))

    def glide(self, x, y, secs=0.6):
        x0, y0 = self.x, self.y
        steps = max(1, round(float(secs) * 60))
        for i in range(1, steps + 1):
            t = i / steps
            e = 4 * t**3 if t < 0.5 else 1 - (-2 * t + 2) ** 3 / 2
            self._move(x0 + (float(x) - x0) * e, y0 + (float(y) - y0) * e)
            time.sleep(float(secs) / steps)

    def move(self, x, y):
        self._move(float(x), float(y))

    def click(self, button="1"):
        self.c.mousePress(int(button))

    def dclick(self):
        self.c.mousePress(1)
        time.sleep(0.08)
        self.c.mousePress(1)

    def _char(self, ch):
        if ch in NAMED:
            self.c.keyPress(NAMED[ch])
        elif ch.isupper() or ch in SHIFTED:
            self.c.keyDown("shift")
            self.c.keyPress(ch.lower() if ch.isupper() else SHIFTED[ch])
            self.c.keyUp("shift")
        else:
            self.c.keyPress(ch)

    def type(self, text, cps="14"):
        gap = 1.0 / float(cps)
        for ch in text:
            self._char(ch)
            # A little human unevenness; spaces and punctuation a bit slower.
            jitter = 0.35 * gap * math.sin(len(text) * 7.1 + ord(ch))
            time.sleep(gap + jitter + (gap * 0.6 if ch in " .,:;" else 0))

    def paste(self, text):
        for ch in text:
            self._char(ch)
            time.sleep(0.015)

    def key(self, *chords):
        from vncdotool.client import KEYMAP

        for chord in chords:
            parts = chord.split("-")
            parts = [{"cmd": "alt", "opt": "meta"}.get(p, p) for p in parts]
            # An unknown name would fail inside vncdotool's client and leave it
            # failing every later call: check first.
            for p in parts:
                if len(p) != 1 and p not in KEYMAP:
                    raise ValueError(f"unknown key {p!r} in {chord!r}")
            mods, last = parts[:-1], parts[-1]
            for m in mods:
                self.c.keyDown(m)
            self.c.keyPress(last)
            for m in reversed(mods):
                self.c.keyUp(m)
            time.sleep(0.05)

    def scroll(self, direction, n="3"):
        button = 4 if direction == "up" else 5
        for _ in range(int(n)):
            self.c.mousePress(button)
            time.sleep(0.03)

    def _find(self, text, n="-1"):
        """TEXT's box on screen (x, y, w, h points), via OCR in the guest.
        Retries for a few seconds: what a scene looks for may still be drawing."""
        vm = os.path.join(os.path.dirname(os.path.abspath(__file__)), "vm")
        for _ in range(6):
            out = subprocess.run(
                [vm, "ssh", f"/tmp/find-text {shlex.quote(text)} 2>/dev/null"],
                capture_output=True, text=True,
            ).stdout.split("\n")
            boxes = [tuple(map(int, line.split())) for line in out if line.strip()]
            if boxes:
                return boxes[int(n)]
            time.sleep(1)
        raise ValueError(f"text not on screen: {text!r}")

    def glidetext(self, text, n="-1", secs="0.9"):
        x, y, w, h = self._find(text, n)
        self.glide(x + w / 2, y + h / 2, secs)

    def clicktext(self, text, n="-1", secs="0.9"):
        self.glidetext(text, n, secs)
        time.sleep(0.3)
        self.click()

    def highlighttext(self, text, n="-1", pad="8"):
        x, y, w, h = self._find(text, n)
        p = float(pad)
        self.highlight(*(f"{v:g}" for v in (x - p, y - p, w + 2 * p, h + 2 * p)))

    def pause(self, secs):
        time.sleep(float(secs))

    def waitfor(self, secs, *cmd):
        deadline = time.time() + float(secs)
        while subprocess.run(list(cmd), capture_output=True).returncode != 0:
            if time.time() > deadline:
                raise SystemExit(f"waitfor timed out: {shlex.join(cmd)}")
            time.sleep(0.5)

    def sh(self, *cmd):
        subprocess.run(list(cmd), check=True)

    def _mark(self, *words):
        path = os.environ.get("TUT_TIMELINE")
        if path:
            with open(path, "a") as f:
                f.write(f"{time.time():.3f} {shlex.join(words)}\n")

    def cut(self):
        self._mark("cut")

    def uncut(self):
        self._mark("uncut")

    def fast(self, factor):
        self._mark("fast", factor)

    def normal(self):
        self._mark("normal")

    def caption(self, text=""):
        self._mark("caption", text)

    def highlight(self, x, y, w, h, style="box"):
        self._mark("highlight", x, y, w, h, style)

    def unhighlight(self):
        self._mark("unhighlight")

    def say(self, text):
        self.waitvoice("0.25")
        secs = voice.length(voice.clip(text))
        self._mark("voice", text)
        self.voice_end = time.time() + secs

    def waitvoice(self, extra="0"):
        left = self.voice_end + float(extra) - time.time()
        if left > 0:
            time.sleep(left)


def run_script(driver: Driver, text: str) -> None:
    lines = [(n, shlex.split(raw, comments=True)) for n, raw in enumerate(text.splitlines(), 1)]
    missing = [w[1] for _, w in lines if len(w) == 2 and w[0] == "say" and not voice.is_cached(w[1])]
    if missing:
        driver._mark("hold")  # a second or two a line: not on camera
        for line in missing:
            voice.clip(line)
        driver._mark("release")
    for n, words in lines:
        if not words:
            continue
        name, args = words[0], words[1:]
        fn = getattr(driver, name, None)
        if fn is None or name.startswith("_"):
            raise ValueError(f"line {n}: unknown command {name!r}")
        fn(*args)


def serve(server: str, password: str, sock_path: str) -> None:
    """Hold ONE VNC connection for the VM's life and run scripts sent to a
    unix socket, one at a time. Connecting to Virtualization.framework's VNC
    server while the guest is screen recording can crash the VM, so nothing
    connects after the daemon has (see tutorials/vm/lib.sh)."""
    import socket

    # The framebuffer takes its size at connect time: right after the guest
    # switched its display mode it can still be the old one, and every click
    # would land in the wrong place. Reconnect until it matches the display.
    want = os.environ.get("VM_DISPLAY", "").removesuffix("pt")
    for attempt in range(30):
        client = api.connect(server, password=password or None, timeout=15)
        if not want:
            break
        client.refreshScreen()
        got = client.screen.size
        if f"{got[0] // SCALE}x{got[1] // SCALE}" == want:
            break
        print(f"framebuffer {got[0]}x{got[1]}, want {want} points; reconnecting", flush=True)
        client.disconnect()
        time.sleep(1)
    else:
        raise SystemExit(f"VNC framebuffer never matched {want}")
    driver = Driver(client)
    driver.move(720, 405)  # proves the connection before we accept work
    if os.path.exists(sock_path):
        os.unlink(sock_path)
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(sock_path)
    srv.listen(1)
    print(f"vnc daemon ready on {sock_path}", flush=True)
    while True:
        conn, _ = srv.accept()
        with conn:
            data = b""
            while not data.endswith(b"\0"):
                chunk = conn.recv(65536)
                if not chunk:
                    break
                data += chunk
            text = data.rstrip(b"\0").decode()
            try:
                run_script(driver, text)
                reply = "ok\n"
            except Exception as exc:  # report, keep the connection alive
                reply = f"error: {exc}\n"
            try:
                conn.sendall(reply.encode())
            except OSError:
                pass  # the sender went away; the daemon must outlive it


def send(sock_path: str, script: str) -> None:
    import socket

    text = sys.stdin.read() if script == "-" else open(script).read()
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(sock_path)
    s.sendall(text.encode() + b"\0")
    reply = b""
    while chunk := s.recv(4096):
        reply += chunk
    reply = reply.decode().strip()
    if reply != "ok":
        raise SystemExit(reply)


def main() -> None:
    mode = sys.argv[1]
    if mode == "serve":  # serve HOST::PORT PASSWORD SOCKET
        serve(*sys.argv[2:5])
    elif mode == "send":  # send SOCKET SCRIPT|-
        send(*sys.argv[2:4])
    else:
        raise SystemExit("usage: vnc_drive.py serve HOST::PORT PASSWORD SOCKET | send SOCKET SCRIPT")


if __name__ == "__main__":
    main()

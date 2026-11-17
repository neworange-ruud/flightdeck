"""Drive the tutorial VM over VNC from a small scene script.

    python vnc_drive.py HOST::PORT PASSWORD SCRIPT_FILE   (or - for stdin)

One command per line; `#` starts a comment; arguments are shell-quoted.
Coordinates are screen POINTS (1920x1080); the VM framebuffer is Retina, so
they are doubled on the wire.

    glide X Y [SECS]     move the pointer smoothly (ease in-out), default 0.6 s
    move X Y             jump the pointer
    click [BUTTON]       press and release (1 left, 2 middle, 3 right)
    dclick               double click
    type TEXT [CPS]      type at CPS characters per second (default 14)
    paste TEXT           type as fast as the guest accepts (setup steps)
    key CHORD...         e.g. `key cmd-a backspace`, `key ctrl-g`, `key enter`
    scroll up|down [N]   wheel clicks
    pause SECS           wait
    waitfor SECS CMD...  run CMD on the host until it succeeds (or fail after SECS)
    sh CMD...            run CMD on the host once

Virtualization.framework's VNC server maps keys to a US layout and ignores a
keysym's shift level, so capitals and shifted symbols are sent with Shift
held, and it maps X11 Alt to Command, so `cmd-` becomes `alt-`.
"""

import math
import shlex
import subprocess
import sys
import time

from vncdotool import api

SCALE = 2  # points -> framebuffer pixels
SHIFTED = dict(zip('~!@#$%^&*()_+{}|:"<>?', "`1234567890-=[]\\;',./"))
NAMED = {"\n": "enter", "\t": "tab", " ": "space"}


class Driver:
    def __init__(self, client):
        self.c = client
        self.x, self.y = 960.0, 540.0

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
        for chord in chords:
            parts = chord.split("-")
            parts = ["alt" if p == "cmd" else p for p in parts]
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


def main() -> None:
    server, password, script = sys.argv[1], sys.argv[2], sys.argv[3]
    lines = (sys.stdin if script == "-" else open(script)).read().splitlines()
    client = api.connect(server, password=password or None, timeout=10)
    driver = Driver(client)
    try:
        for n, raw in enumerate(lines, 1):
            words = shlex.split(raw, comments=True)
            if not words:
                continue
            name, args = words[0], words[1:]
            fn = getattr(driver, name, None)
            if fn is None or name.startswith("_"):
                raise SystemExit(f"line {n}: unknown command {name!r}")
            fn(*args)
    finally:
        client.disconnect()
        api.shutdown()


if __name__ == "__main__":
    main()

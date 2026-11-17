"""Drive the tutorial VM over VNC from a small scene script.

    python vnc_drive.py serve HOST::PORT PASSWORD SOCKET   hold the VNC connection
    python vnc_drive.py send SOCKET SCRIPT_FILE            run a script (- = stdin)

`tutorials/vm/up.sh` starts the daemon (`serve`) once per VM boot; `vm run`
sends scripts to it. One connection for the VM's whole life, because
connecting while the guest records its screen can crash the VM.

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

Editing marks, written with host timestamps to $TUT_TIMELINE (set by
`vm record start`); `vm record stop` applies them:

    cut                  start a stretch to drop from the video (dead time)
    uncut                end it
    fast FACTOR          play what follows FACTOR times faster (an agent working)
    normal               back to normal speed
    caption TEXT         show TEXT from here until the next caption (or `caption`)

Virtualization.framework's VNC server maps keys to a US layout and ignores a
keysym's shift level, so capitals and shifted symbols are sent with Shift
held, and it maps X11 Alt to Command, so `cmd-` becomes `alt-`.
"""

import math
import os
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


def run_script(driver: Driver, text: str) -> None:
    for n, raw in enumerate(text.splitlines(), 1):
        words = shlex.split(raw, comments=True)
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

    client = api.connect(server, password=password or None, timeout=15)
    driver = Driver(client)
    driver.move(960, 540)  # proves the connection before we accept work
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
            conn.sendall(reply.encode())


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

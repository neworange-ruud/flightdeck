#!/usr/bin/env python3
"""FlightDeck terminal performance bench (M0 spike S4, beads remote-control-bmej.1.4).

Runs the same workloads against the GPUI desktop element and the ratatui TUI
and prints one table. Unix only (it drives PTYs with the standard library);
see desktop/NOTES-M0.md, "Performance (S4)", for what every number means and
how to run the Windows equivalents by hand.

    cargo build --release -p flightdeck --bin flightdeck
    cargo build --release -p flightdeck-desktop
    python3 desktop/benches/perf.py all            # everything, ~12 minutes
    python3 desktop/benches/perf.py gui-idle tui-idle --seconds 20

GUI numbers come from `flightdeck-desktop --bench …` (the in-process probe in
desktop/src/terminal/bench.rs), except CPU and RSS, which this script samples
from outside with `ps` for both front-ends.

TUI numbers come from driving the real `flightdeck -I` binary inside a PTY
(160x50) from this script, with a throwaway HOME and git repo and an agent
whose command is `/bin/sh`:

- latency: write one key (`ж`, which nothing else on screen contains) to the
  TUI's PTY → the TUI's frame output containing its echo arrives here. That
  is key read → PTY write → echo parsed → frame flushed, plus two PTY hops;
- throughput: Enter on `CMD; printf '%s-%s\\n' FD DONE` → the flushed frame
  that contains `FD-DONE`;
- idle: `ps` over the TUI process with 1 terminal, or 4 (the agent plus three
  child shells, Ctrl-T), idle or each running a 1 s `date` ticker.

The full desktop app (the host-driven path the `--bench` window bypasses):

- app-idle: `flightdeck-desktop -I` with one agent, idle or ticking;
- mission-idle: four recovered sessions shown as Mission control tiles, each
  reporting `waiting` (still badges) or `working` (animated spinners);

both sampled with `ps`, with terminal and tile frames counted by the app itself
(FLIGHTDECK_BENCH_FRAME_LOG=1).
"""

import argparse
import fcntl
import os
import shutil
import statistics
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
TUI = os.path.join(ROOT, "target", "release", "flightdeck")
GUI = os.path.join(ROOT, "target", "release", "flightdeck-desktop")
TICKER = "while true; do date; sleep 1; done\r"
DONE = b"FD-DONE"
F2 = b"\x1bOQ"
CTRL_T = b"\x14"


def cpu_seconds(pid):
    """Cumulative CPU time of `pid` (user + system) from `ps`, in seconds."""
    out = subprocess.run(["ps", "-o", "time=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    if not out:
        return None
    parts = out.replace("-", ":").split(":")
    secs = 0.0
    for p in parts:
        secs = secs * 60 + float(p)
    return secs


def rss_mb(pid):
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    return int(out) / 1024 if out else None


def sample(pid, seconds, warmup=5.0):
    """Average CPU % of one core and RSS (mean, max) over `seconds`, after `warmup`."""
    time.sleep(warmup)
    c0, t0 = cpu_seconds(pid), time.monotonic()
    rss = []
    while time.monotonic() - t0 < seconds:
        time.sleep(1.0)
        r = rss_mb(pid)
        if r is not None:
            rss.append(r)
    c1, t1 = cpu_seconds(pid), time.monotonic()
    if c0 is None or c1 is None or not rss:
        return None
    return {"cpu_pct": 100.0 * (c1 - c0) / (t1 - t0), "rss_mb": statistics.mean(rss), "rss_max_mb": max(rss)}


def pct(values, p):
    v = sorted(values)
    rank = max(1, -(-p * len(v) // 100))
    return v[min(rank, len(v)) - 1]


def summary_ms(values):
    if not values:
        return "n=0"
    return (f"n={len(values)} p50={pct(values, 50) * 1000:.2f}ms "
            f"p95={pct(values, 95) * 1000:.2f}ms max={max(values) * 1000:.2f}ms")


def big_file(path, mb=50):
    """~`mb` MiB of log-like lines, every fifth with an SGR colour."""
    if os.path.exists(path) and os.path.getsize(path) >= mb * 1024 * 1024:
        return path
    words = ("lorem ipsum dolor sit amet consectetur adipiscing elit sed do "
             "eiusmod tempor incididunt ut labore").split()
    n = i = 0
    with open(path, "w") as f:
        while n < mb * 1024 * 1024:
            i += 1
            body = " ".join(words[(i + k) % len(words)] for k in range(12))
            if i % 5 == 0:
                line = f"{i:08d} \x1b[3{i % 7 + 1}m{body[:20]}\x1b[0m {body}\n"
            else:
                line = f"{i:08d} {body}\n"
            f.write(line)
            n += len(line)
    return path


# --- TUI -------------------------------------------------------------------

def sandbox(workdir, prefix, agent_args=()):
    """A throwaway HOME and git repo whose default agent is `/bin/sh
    AGENT_ARGS…`. Returns (dir, home, repo)."""
    root = tempfile.mkdtemp(prefix=prefix, dir=workdir)
    home, repo = os.path.join(root, "home"), os.path.join(root, "repo")
    os.makedirs(os.path.join(home, ".flightdeck"))
    os.makedirs(repo)
    git = ["git", "-c", "user.email=bench@example.invalid", "-c", "user.name=bench"]
    subprocess.run(git + ["init", "-q", "-b", "main", repo], check=True)
    subprocess.run(git + ["-C", repo, "commit", "-q", "--allow-empty", "-m", "init"], check=True)
    args = ", ".join(f'"{a}"' for a in agent_args)
    with open(os.path.join(home, ".flightdeck", "config.toml"), "w") as f:
        f.write('[ui]\ndefault_agent = "sh"\nagent_tab_position = "left"\n'
                'use_f2_to_leave_terminal_focus = true\n'
                '[update]\ncheck = false\n'
                f'[agents.sh]\ndisplay_name = "sh"\ncommand = "/bin/sh"\nargs = [{args}]\n')
    return root, home, repo


class Tui:
    """The real TUI in a 160x50 PTY, in a throwaway HOME and repo."""

    def __init__(self, workdir):
        self.dir, home, repo = sandbox(workdir, "perf-tui-")
        master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 160, 0, 0))
        env = dict(os.environ, HOME=home, SHELL="/bin/sh", PS1="$ ", TERM="xterm-256color")
        self.proc = subprocess.Popen([TUI, "-I"], stdin=slave, stdout=slave, stderr=slave,
                                     cwd=repo, env=env, start_new_session=True)
        os.close(slave)
        self.master = master
        self.buf = bytearray()
        self.lock = threading.Condition()
        threading.Thread(target=self._read, daemon=True).start()
        # Keys typed before the agent's shell is up are lost, so wait until a
        # command demonstrably runs (the output, not the echoed line).
        for _ in range(20):
            since = self.mark()
            self.send("echo RDY-$((20+22))\r")
            if self.wait_for(b"RDY-42", since, 1.0):
                break
        else:
            raise RuntimeError("the TUI's shell never answered")
        time.sleep(0.5)

    def _read(self):
        while True:
            try:
                data = os.read(self.master, 65536)
            except OSError:
                return
            if not data:
                return
            with self.lock:
                self.buf.extend(data)
                self.lock.notify_all()

    def send(self, data):
        if isinstance(data, str):
            data = data.encode()
        os.write(self.master, data)

    def mark(self):
        with self.lock:
            return len(self.buf)

    def wait_for(self, needle, since, timeout):
        """When `needle` first appears in the output after offset `since`."""
        deadline = time.monotonic() + timeout
        with self.lock:
            while True:
                if self.buf.find(needle, max(0, since - len(needle))) >= 0:
                    return time.monotonic()
                left = deadline - time.monotonic()
                if left <= 0:
                    return None
                self.lock.wait(left)

    def close(self):
        self.proc.kill()
        self.proc.wait()
        # Take the shells with it.
        subprocess.run(["pkill", "-9", "-f", self.dir], capture_output=True)
        shutil.rmtree(self.dir, ignore_errors=True)

    def open_children(self, n, ticker):
        """`n` child shells (Ctrl-T from app focus), each optionally ticking."""
        for _ in range(n):
            self.send(F2)
            time.sleep(0.3)
            self.send(CTRL_T)
            time.sleep(1.0)
            self.send(b"\r")  # focus the new terminal
            time.sleep(0.5)
            if ticker:
                self.send(TICKER)
                time.sleep(0.3)


def tui_latency(args):
    tui = Tui(args.workdir)
    try:
        tui.send("exec cat > /dev/null\r")
        time.sleep(1.0)
        key = "ж".encode()
        samples = []
        for i in range(args.keys):
            time.sleep(0.03 + (i * 7919 % 41) / 1000)
            if i % 50 == 49:
                tui.send(b"\r")
                time.sleep(0.2)
                continue
            since = tui.mark()
            t0 = time.monotonic()
            tui.send(key)
            t1 = tui.wait_for(key, since, 2.0)
            if t1 is not None:
                samples.append(t1 - t0)
        print(f"tui latency key_to_frame: {summary_ms(samples)}", flush=True)
    finally:
        tui.close()


def tui_throughput(args, cmd):
    tui = Tui(args.workdir)
    try:
        since = tui.mark()
        t0 = time.monotonic()
        tui.send(f"{cmd}; printf '%s-%s\\n' FD DONE\r")
        t1 = tui.wait_for(DONE, since, 600)
        took = "timeout" if t1 is None else f"{(t1 - t0) * 1000:.1f}ms"
        print(f"tui throughput cmd={cmd!r} total={took}", flush=True)
    finally:
        tui.close()


def tui_idle(args, terminals, ticker):
    tui = Tui(args.workdir)
    try:
        if ticker:
            tui.send(TICKER)
            time.sleep(0.3)
        tui.open_children(terminals - 1, ticker)
        s = sample(tui.proc.pid, args.seconds)
        label = f"tui idle terminals={terminals} ticker={ticker}"
        print(f"{label}: cpu={s['cpu_pct']:.2f}% rss={s['rss_mb']:.1f}MB (max {s['rss_max_mb']:.1f})", flush=True)
    finally:
        tui.close()


# --- GUI -------------------------------------------------------------------

def pty_ceiling(cmd):
    """How fast `cmd`'s output can be read from a PTY at all (a tight
    `os.read` loop, nothing parsed or drawn): the bound both front-ends share,
    and a gauge of how loaded the machine is right now."""
    master, slave = os.openpty()
    t0 = time.monotonic()
    proc = subprocess.Popen(["/bin/sh", "-c", cmd], stdin=slave, stdout=slave, stderr=slave)
    os.close(slave)
    n = 0
    while True:
        try:
            data = os.read(master, 65536)
        except OSError:
            break
        if not data:
            break
        n += len(data)
    proc.wait()
    os.close(master)
    print(f"pty ceiling cmd={cmd!r} total={(time.monotonic() - t0) * 1000:.1f}ms bytes={n}", flush=True)


def gui(args, *bench):
    out = subprocess.run([args.gui, "--bench", *bench], capture_output=True, text=True, cwd=args.workdir)
    for line in out.stdout.splitlines():
        print(f"gui {line}", flush=True)
    if out.returncode != 0:
        print(out.stderr[-2000:], file=sys.stderr)


def gui_idle(args, terminals, ticker):
    cmd = [args.gui, "--bench", "idle", "--terminals", str(terminals), "--seconds", str(int(args.seconds + 8))]
    if ticker:
        cmd.append("--ticker")
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, text=True, cwd=args.workdir)
    lines = []
    for line in proc.stdout:
        lines.append(line.rstrip())
        if line.startswith("idle_start"):
            break
    s = sample(proc.pid, args.seconds)
    lines.extend(l.rstrip() for l in proc.stdout)
    proc.wait()
    label = f"gui idle terminals={terminals} ticker={ticker}"
    print(f"{label}: cpu={s['cpu_pct']:.2f}% rss={s['rss_mb']:.1f}MB (max {s['rss_max_mb']:.1f})", flush=True)
    for line in lines:
        if line.startswith("terminal="):
            print(f"  {line}")


def recovered_tabs(root, home, repo, tabs, ticker, status="working"):
    """Make the app start with `tabs` recovered sessions on the base branch
    (which it resumes at launch) and Mission control on screen, each one a
    live tile. A tile is only shown for a session that is working or waiting
    for the user, so the agent is a script named `claude` (the command name is
    what FlightDeck reads lifecycle status by) that reports `status` through
    the status file Claude Code's hooks write, then runs the idle shell or the
    ticker. `waiting` draws a still badge; `working` animates a spinner per
    session, which is the shell's own frame cost, not the terminals'."""
    import json
    bindir = os.path.join(root, "bin")
    os.makedirs(bindir, exist_ok=True)
    agent = os.path.join(bindir, "claude")
    body = "while true; do date; sleep 1; done" if ticker else "exec /bin/sh"
    with open(agent, "w") as f:
        f.write(f"#!/bin/sh\nprintf '{status}\\n' >> .flightdeck/agent-status\n" + body + "\n")
    os.chmod(agent, 0o755)
    config = os.path.join(home, ".flightdeck", "config.toml")
    with open(config) as f:
        text = f.read()
    with open(config, "w") as f:
        f.write(text.replace('command = "/bin/sh"', f'command = "{agent}"'))
    state = {"version": 2, "project_root_relative": ".", "base_branch": "main", "tabs": []}
    for i in range(tabs):
        state["tabs"].append({
            "id": f"bench-{i}", "name": f"bench{i}", "slug": f"bench{i}", "agent": "sh",
            "branch": "main", "worktree_path_relative": ".", "base_branch": "main",
            "base_commit_sha": "0", "created_at": "2026-01-01T00:00:00Z",
            "attached_existing_branch": True, "recovered": False, "last_known_status": "unknown",
            "manual_status": None, "containerized": False, "container_image": None,
            "runs_on_base": True, "resume_args": [],
            "activity": {"last_output_at": None, "last_status_change_at": None,
                         "last_git_change_at": None},
        })
    os.makedirs(os.path.join(repo, ".flightdeck"), exist_ok=True)
    with open(os.path.join(repo, ".flightdeck", "state.json"), "w") as f:
        json.dump(state, f)
    with open(os.path.join(repo, ".flightdeck", "config.toml"), "w") as f:
        f.write('[project]\nname = "repo"\ndefault_base_branch = "main"\n')
    with open(os.path.join(home, ".flightdeck", "workspace.json"), "w") as f:
        json.dump({"version": 1, "projects": [os.path.realpath(repo)], "active": 0,
                   "ui": {"view": "mission"}}, f)


def app_idle(args, ticker, mission_tiles=0, status="working"):
    """The full desktop app (the host-driven path), left alone: CPU/RSS from
    `ps`, frames from its FLIGHTDECK_BENCH_FRAME_LOG report. Its agents are an
    idle `/bin/sh` or a `date` ticker: one isolated session (`-I`) in the
    Projects view, or `mission_tiles` recovered sessions shown as Mission
    control tiles."""
    agent = ("-c", "while true; do date; sleep 1; done") if ticker else ()
    root, home, repo = sandbox(args.workdir, "perf-app-", agent)
    argv = [args.gui, "-I"]
    if mission_tiles:
        recovered_tabs(root, home, repo, mission_tiles, ticker, status)
        argv = [args.gui]
    env = dict(os.environ, HOME=home, SHELL="/bin/sh", PS1="$ ", FLIGHTDECK_BENCH_FRAME_LOG="1")
    proc = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                            text=True, cwd=repo, env=env)
    reports = []
    reader = threading.Thread(target=lambda: reports.extend(proc.stdout), daemon=True)
    reader.start()
    try:
        time.sleep(3.0)
        first = len(reports)
        s = sample(proc.pid, args.seconds)
        window = [r for r in reports[first:] if r.startswith("frames=")]
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        shutil.rmtree(root, ignore_errors=True)
    fps = "n/a"
    if len(window) >= 2:
        f0, t0 = [float(x.split("=")[1]) for x in window[0].split()]
        f1, t1 = [float(x.split("=")[1]) for x in window[-1].split()]
        fps = f"{(f1 - f0) / (t1 - t0):.2f}"
    label = f"app idle ticker={ticker}" + (
        f" mission_tiles={mission_tiles} status={status}" if mission_tiles else "")
    print(f"{label}: cpu={s['cpu_pct']:.2f}% rss={s['rss_mb']:.1f}MB (max {s['rss_max_mb']:.1f}) "
          f"terminal_frames_per_s={fps}", flush=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("what", nargs="+", help="all | gui-latency gui-throughput gui-idle gui-scroll "
                                            "app-idle mission-idle tui-latency tui-throughput tui-idle")
    ap.add_argument("--seconds", type=float, default=30.0, help="idle sampling window")
    ap.add_argument("--keys", type=int, default=300)
    ap.add_argument("--workdir", default=tempfile.gettempdir())
    ap.add_argument("--gui", default=GUI, help="the flightdeck-desktop binary (to A/B two builds)")
    ap.add_argument("--rounds", type=int, default=1, help="repeat each throughput run")
    args = ap.parse_args()
    want = set(args.what)
    if "all" in want:
        want = {"gui-latency", "gui-throughput", "gui-idle", "gui-scroll", "app-idle", "mission-idle",
                "tui-latency", "tui-throughput", "tui-idle"}
    big = big_file(os.path.join(args.workdir, "perf-big.txt"))
    loads = [f"cat {big}", "seq 1 2000000"]
    print(f"gui binary: {args.gui}")
    print(f"host: {os.uname().sysname} {os.uname().release} {os.uname().machine}, {os.cpu_count()} cpus")
    if "gui-latency" in want:
        gui(args, "latency", "--keys", str(args.keys))
    if "tui-latency" in want:
        tui_latency(args)
    for _ in range(args.rounds):
        for cmd in loads:
            if want & {"gui-throughput", "tui-throughput"}:
                pty_ceiling(cmd)
            if "gui-throughput" in want:
                gui(args, "throughput", "--cmd", cmd)
            if "tui-throughput" in want:
                tui_throughput(args, cmd)
    if "gui-scroll" in want:
        gui(args, "scroll", "--seconds", "10")
    for ticker in (False, True):
        if "app-idle" in want:
            app_idle(args, ticker)
        if "mission-idle" in want:
            for status in ("waiting", "working"):
                app_idle(args, ticker, mission_tiles=4, status=status)
    for terminals in (1, 4):
        for ticker in (False, True):
            if "gui-idle" in want:
                gui_idle(args, terminals, ticker)
            if "tui-idle" in want:
                tui_idle(args, terminals, ticker)


if __name__ == "__main__":
    main()

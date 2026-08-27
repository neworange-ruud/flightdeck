#!/usr/bin/env python3
"""Run a command on a real PTY and copy its output to stdout.

FlightDeck is a TUI: it reads crossterm events from a terminal and renders to
one, so it cannot run headless — exactly the problem `tests/e2e/support/desktop.rs`
solves with `portable-pty` for the Rust harness. The Playwright suite launches
the same binary from Node, and this is the smallest way to give it a terminal
without adding a native npm dependency (node-pty) that would have to compile on
every runner. `python3` is present on the GitHub runners this job uses and on
every developer machine that can build FlightDeck.

Usage:
    pty-spawn.py <rows> <cols> <command> [args...]

The child's PTY output is written to this process's stdout, so the caller drains
it by reading the pipe — a PTY nobody reads fills up and blocks the TUI.

On SIGTERM/SIGINT the child is SIGKILLed and this process exits. SIGKILL, not
SIGTERM, and for the reason `desktop.rs` documents: FlightDeck traps
SIGHUP/SIGTERM/SIGINT and runs a graceful shutdown, and on macOS a session-leader
desktop that starts one while the harness still holds the PTY master open can
wedge in the kernel exit path. Uncatchable is what makes teardown reliable.

Unix only. Windows has no `pty` module and the GitHub Windows runners have no
bash for the fixture either, which is why the Rust E2E suite is
`#![cfg(not(windows))]` too.
"""

import fcntl
import os
import pty
import signal
import struct
import sys
import termios


def main() -> int:
    if len(sys.argv) < 4:
        print(__doc__, file=sys.stderr)
        return 2
    rows, cols = int(sys.argv[1]), int(sys.argv[2])
    argv = sys.argv[3:]

    pid, fd = pty.fork()
    if pid == 0:
        # Child: it is now the session leader with the PTY slave as its
        # controlling terminal, which is all FlightDeck needs.
        os.execvp(argv[0], argv)
        return 127  # unreachable; execvp either succeeds or raises

    # A grid big enough that the TUI renders its chrome without truncating.
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def terminate(_signum: int, _frame: object) -> None:
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass
        os._exit(0)

    signal.signal(signal.SIGTERM, terminate)
    signal.signal(signal.SIGINT, terminate)

    try:
        while True:
            try:
                data = os.read(fd, 8192)
            except OSError:
                break  # the child exited and the PTY closed
            if not data:
                break
            sys.stdout.buffer.write(data)
            sys.stdout.buffer.flush()
    finally:
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Measure herdr-lazy startup and local Marketplace search in an isolated PTY."""

import argparse
import fcntl
import json
import math
import os
from pathlib import Path
import platform
import pty
import select
import statistics
import struct
import subprocess
import tempfile
import termios
import time


ITEMS = 1_000
COLUMNS, ROWS = 120, 24
TIMEOUT_SECONDS = 5
FRAME_START = b"\x1b[H\x1b[2J"
LIST_FOOTERS = (b"[?]\x1b[0m more", b"[x] Remove")
BROWSER_FOOTERS = (b"[esc]\x1b[0m back", b"[Ctrl+R] Refresh")


def prepare_fixture(root):
    config = root / "config"
    config.mkdir()
    cache = root / "cache" / "herdr-lazy"
    cache.mkdir(parents=True)
    (cache / "marketplace.json").write_text(
        json.dumps({
            "plugins": [
                {
                    "fullName": f"example/plugin-{i:04d}",
                    "description": "Workspace navigation with fzf" if i % 3 == 0 else "Agent tools",
                    "topics": ["workspace" if i % 3 == 0 else "agent"],
                    "stars": i,
                }
                for i in range(ITEMS)
            ]
        }),
        encoding="utf-8",
    )
    bundle = config / "plugins.list"
    bundle.touch()
    herdr = root / "herdr"
    herdr.write_text(
        "#!/bin/sh\n"
        "case \"$*\" in\n"
        "  --version) printf '%s\\n' 'herdr 0.9.5' ;;\n"
        "  'plugin list --json') printf '%s\\n' '{\"result\":{\"plugins\":[]}}' ;;\n"
        "  *) exit 1 ;;\n"
        "esac\n",
        encoding="utf-8",
    )
    herdr.chmod(0o700)
    return {
        "HOME": str(root),
        "XDG_CONFIG_HOME": str(config),
        "XDG_CACHE_HOME": str(cache.parent),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_STATE_HOME": str(root / "state"),
        "HERDR_BIN_PATH": str(herdr),
        "HERDR_PLUGIN_CONFIG_DIR": str(config),
        "HERDR_LAZY_LIST": str(bundle),
        "PATH": str(root),
        "TERM": "xterm-256color",
    }


def read_frame(master, markers, footers):
    if isinstance(markers, bytes):
        markers = (markers,)
    deadline = time.monotonic() + TIMEOUT_SECONDS
    output = b""
    while True:
        for marker in markers:
            start = output.find(marker)
            if start >= 0 and any(footer in output[start + len(marker):] for footer in footers):
                return
        timeout = deadline - time.monotonic()
        if timeout <= 0 or not select.select([master], [], [], timeout)[0]:
            raise RuntimeError(f"timed out waiting for {markers!r} and a frame footer")
        try:
            chunk = os.read(master, 65536)
        except OSError as error:
            raise RuntimeError(f"terminal closed while waiting for {markers!r}") from error
        if not chunk:
            raise RuntimeError(f"terminal closed while waiting for {markers!r}")
        output += chunk


def sample(binary, root, env):
    master, slave = pty.openpty()
    child = None
    try:
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLUMNS, 0, 0))
        started = time.perf_counter_ns()
        child = subprocess.Popen(
            [binary, "ui"], cwd=root, env=env, stdin=slave, stdout=slave, stderr=slave
        )
        os.close(slave)
        slave = None
        read_frame(master, FRAME_START, LIST_FOOTERS)
        startup = time.perf_counter_ns() - started
        os.write(master, b"/")
        counts = (f"{ITEMS} of {ITEMS}".encode(), f"{ITEMS}/{ITEMS}".encode())
        read_frame(master, counts, BROWSER_FOOTERS)

        search = []
        for query in (b"f", b"fz", b"fzf"):
            started = time.perf_counter_ns()
            os.write(master, query[-1:])
            read_frame(master, b"\x1b[1m" + query + b"\x1b[0m\x1b[7m ", BROWSER_FOOTERS)
            search.append(time.perf_counter_ns() - started)

        os.write(master, b"\x03")
        if child.wait(timeout=TIMEOUT_SECONDS) != 0:
            raise RuntimeError("herdr-lazy exited with an error")
        return startup, search
    finally:
        if child is not None and child.poll() is None:
            child.kill()
            child.wait()
        if slave is not None:
            os.close(slave)
        os.close(master)


def report(label, samples):
    ordered = sorted(samples)
    p95 = ordered[math.ceil(0.95 * len(ordered)) - 1]
    print(f"{label:<26} n={len(samples):<4} "
          f"median={statistics.median(samples) / 1e6:7.3f} ms  p95={p95 / 1e6:7.3f} ms")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, help="path to a built herdr-lazy release binary")
    parser.add_argument("--runs", type=int, default=30, help="measured runs (default 30)")
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"binary is not executable: {binary}")

    startups, searches = [], []
    with tempfile.TemporaryDirectory(prefix="herdr-lazy-bench-") as work:
        root = Path(work)
        env = prepare_fixture(root)
        for i in range(args.runs + 2):
            startup, search = sample(binary, root, env)
            if i >= 2:
                startups.append(startup)
                searches.extend(search)

    print(f"{platform.system()} {platform.release()} {platform.machine()} · {COLUMNS}×{ROWS} PTY")
    print(f"{binary} · {ITEMS:,} synthetic entries · 2 warm-up runs discarded")
    report("Startup to first frame", startups)
    report("Search input to frame", searches)


if __name__ == "__main__":
    main()

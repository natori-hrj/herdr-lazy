"""Measure native 120×24 PTY first frame and app lifetime resource usage."""

import argparse
import ctypes
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import pty
import random
import runpy
import signal
import statistics
import struct
import tempfile
import termios
import time

pty_bench = runpy.run_path(str(Path(__file__).with_name("benchmark-pty.py")))


def darwin_wakeup_sampler():
    if platform.system() != "Darwin":
        return lambda _: {}
    try:
        query = ctypes.CDLL("/usr/lib/libproc.dylib").proc_pid_rusage
    except (OSError, AttributeError):
        return lambda _: {}
    query.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
    query.restype = ctypes.c_int

    def wakeups(pid):
        # Darwin's RUSAGE_INFO_V0 is a 16-byte UUID followed by ten uint64 fields.
        # Only the package-idle and interrupt counters are used here.
        info = (ctypes.c_uint64 * 12)()
        if query(pid, 0, ctypes.byref(info)) != 0:
            return {}
        return {"package_idle_wakeups": info[4], "interrupt_wakeups": info[5]}

    return wakeups


def sample(binary, env, wakeups):
    master, slave = pty.openpty()
    child_pid = None
    try:
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 120, 0, 0))
        file_actions = [(os.POSIX_SPAWN_DUP2, slave, fd) for fd in range(3)]
        start = time.perf_counter_ns()
        child_pid = os.posix_spawn(binary, [binary, "ui"], env, file_actions=file_actions)
        os.close(slave)
        slave = None
        pty_bench["read_frame"](master, pty_bench["FRAME_START"], pty_bench["LIST_FOOTERS"])
        first_frame_ms = (time.perf_counter_ns() - start) / 1e6
        wake_counts = wakeups(child_pid)
        os.write(master, b"\x03")
        end_by = time.monotonic() + 5
        while True:
            pid, status, usage = os.wait4(child_pid, os.WNOHANG)
            if pid:
                child_pid = None
                code = os.waitstatus_to_exitcode(status)
                if code != 0:
                    raise RuntimeError(f"unexpected TUI exit: {code}")
                break
            if time.monotonic() >= end_by:
                raise TimeoutError("TUI did not exit after Ctrl-C")
            time.sleep(0.002)
        return {
            "first_frame_ms": first_frame_ms,
            "cpu_ms": 1000 * (usage.ru_utime + usage.ru_stime),
            "peak_rss_bytes": usage.ru_maxrss * (1 if platform.system() == "Darwin" else 1024),
            **wake_counts,
        }
    finally:
        if child_pid is not None:
            try:
                os.kill(child_pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(child_pid, 0)
        if slave is not None:
            os.close(slave)
        os.close(master)


def emit(record):
    print(json.dumps(record), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--runs", type=int, default=30)
    parser.add_argument("binaries", nargs="+")
    args = parser.parse_args()
    binaries = {name: str(Path(path).resolve()) for name, path in
                (item.split("=", 1) for item in args.binaries)}
    if args.runs < 1 or len(binaries) != len(args.binaries):
        parser.error("positive run count and distinct binary labels required")
    emit({"mode": "pty_first_frame", "platform": platform.platform(),
          "wakeups": "Darwin proc_pid_rusage at first frame when available",
          "resource_scope": "wait4 rusage through Ctrl-C exit",
          "sha256": {name: hashlib.sha256(Path(path).read_bytes()).hexdigest()
                     for name, path in binaries.items()}})
    rng = random.Random(4700)
    wakeups = darwin_wakeup_sampler()
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        env = pty_bench["prepare_fixture"](root)
        env["HERDR_BIN_PATH"] = str(args.fixture.resolve())
        for delay in (0, 250):
            env["HERDR_BENCH_DELAY_MS"] = str(delay)
            results = {name: [] for name in binaries}
            for index in range(args.runs + 2):
                order = list(binaries)
                rng.shuffle(order)
                for name in order:
                    values = sample(binaries[name], env, wakeups)
                    if index >= 2:
                        results[name].append(values)
                        emit({"sample": index - 1, "label": name, "delay_ms": delay, **values})
            for name, rows in results.items():
                summary = {}
                for metric in sorted({metric for row in rows for metric in row}):
                    values = sorted(row[metric] for row in rows if metric in row)
                    summary[metric] = {"n": len(values), "median": statistics.median(values),
                                       "p95": values[math.ceil(0.95 * len(values)) - 1]}
                emit({"label": name, "delay_ms": delay, "n": len(rows), "summary": summary})


if __name__ == "__main__":
    main()

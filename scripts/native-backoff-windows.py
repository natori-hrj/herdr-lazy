#!/usr/bin/env python3
"""Benchmark pinned Windows executables via the local, headless `probe` command."""

import argparse
import ctypes
from ctypes import wintypes
import json
import math
import os
from pathlib import Path
import random
import shutil
import statistics
import subprocess
import tempfile
import time

if os.name == "nt":
    import _winapi
    import msvcrt


MODE = "headless_probe_e2e"
WORKLOADS = (("short", 0), ("delayed", 250))
WARMUPS = 2
TIMEOUT_MS = 60_000
CREATE_SUSPENDED = 0x00000004
METRICS = ("latency_ms", "cpu_ms", "peak_working_set_bytes")


class MemoryCounters(ctypes.Structure):
    _fields_ = [
        ("cb", wintypes.DWORD),
        ("page_faults", wintypes.DWORD),
        ("peak_working_set", ctypes.c_size_t),
        ("other_counters", ctypes.c_size_t * 7),
    ]


class Windows:
    def __init__(self):
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        handle, boolean = wintypes.HANDLE, wintypes.BOOL
        dword = wintypes.DWORD
        filetime = ctypes.POINTER(wintypes.FILETIME)

        def bind(name, result, *args):
            function = getattr(kernel, name)
            function.restype, function.argtypes = result, args
            return function

        self.create_job = bind("CreateJobObjectW", handle, wintypes.LPVOID, wintypes.LPCWSTR)
        self.assign_job = bind("AssignProcessToJobObject", boolean, handle, handle)
        self.terminate_job = bind("TerminateJobObject", boolean, handle, wintypes.UINT)
        self.resume = bind("ResumeThread", dword, handle)
        self.times = bind("GetProcessTimes", boolean, handle, filetime, filetime, filetime, filetime)
        self.memory = bind(
            "K32GetProcessMemoryInfo", boolean, handle, ctypes.POINTER(MemoryCounters), dword
        )

    def process_metrics(self, process):
        created, exited, kernel, user = (wintypes.FILETIME() for _ in range(4))
        cpu_ms = None
        if self.times(process, *(ctypes.byref(t) for t in (created, exited, kernel, user))):
            ticks = sum((t.dwHighDateTime << 32) | t.dwLowDateTime for t in (kernel, user))
            cpu_ms = ticks / 10_000  # FILETIME ticks are 100 ns.

        counters = MemoryCounters()
        counters.cb = ctypes.sizeof(counters)
        peak = None
        if self.memory(process, ctypes.byref(counters), counters.cb):
            peak = counters.peak_working_set or None
        return cpu_ms, peak


def fixture_env(root, fixture, delay_ms):
    dirs = {name: root / name for name in ("config", "cache", "data", "state", "tmp")}
    for directory in dirs.values():
        directory.mkdir()
    bundle = dirs["config"] / "plugins.list"
    bundle.touch()
    system_root = Path(os.environ["SystemRoot"])
    home_drive, home_path = os.path.splitdrive(str(root))
    # Allowlist: no inherited Herdr session, user config, credentials, or proxies.
    return {
        "SystemRoot": str(system_root),
        "WINDIR": str(system_root),
        "PATH": os.pathsep.join((str(fixture.parent), str(system_root / "System32"))),
        "HOME": str(root),
        "USERPROFILE": str(root),
        "HOMEDRIVE": home_drive,
        "HOMEPATH": home_path,
        "APPDATA": str(dirs["config"]),
        "LOCALAPPDATA": str(dirs["cache"]),
        "TEMP": str(dirs["tmp"]),
        "TMP": str(dirs["tmp"]),
        "XDG_CONFIG_HOME": str(dirs["config"]),
        "XDG_CACHE_HOME": str(dirs["cache"]),
        "XDG_DATA_HOME": str(dirs["data"]),
        "XDG_STATE_HOME": str(dirs["state"]),
        "HERDR_BIN_PATH": str(fixture),
        "HERDR_PLUGIN_CONFIG_DIR": str(dirs["config"]),
        "HERDR_PLUGIN_STATE_DIR": str(dirs["state"]),
        "HERDR_LAZY_LIST": str(bundle),
        "HERDR_BENCH_DELAY_MS": str(delay_ms),
    }


def run_probe(win, binary, root, env):
    # File-backed capture cannot deadlock waiting for inherited pipe handles.
    with open(os.devnull, "rb") as stdin, (root / "probe.log").open("w+b") as log:
        startup = subprocess.STARTUPINFO()
        startup.dwFlags = subprocess.STARTF_USESTDHANDLES
        startup.hStdInput = msvcrt.get_osfhandle(stdin.fileno())
        startup.hStdOutput = startup.hStdError = msvcrt.get_osfhandle(log.fileno())
        handles = [startup.hStdInput, startup.hStdOutput]
        for handle in handles:
            os.set_handle_inheritable(handle, True)
        startup.lpAttributeList = {"handle_list": handles}

        job = win.create_job(None, None)
        if not job:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            command = subprocess.list2cmdline([str(binary), "probe"])
            started = time.perf_counter_ns()
            # Start suspended so even the first fixture subprocess is in the job.
            process, thread, _, _ = _winapi.CreateProcess(
                str(binary), command, None, None, True,
                CREATE_SUSPENDED | subprocess.CREATE_NO_WINDOW,
                env, str(root), startup,
            )
            assigned = False
            try:
                assigned = bool(win.assign_job(job, process))
                if not assigned:
                    error = ctypes.WinError(ctypes.get_last_error())
                    raise RuntimeError(f"cannot contain probe in a Windows job: {error}") from error
                if win.resume(thread) == 0xFFFFFFFF:
                    raise ctypes.WinError(ctypes.get_last_error())
                if _winapi.WaitForSingleObject(process, TIMEOUT_MS) != 0:  # WAIT_OBJECT_0
                    raise RuntimeError(f"probe timed out after {TIMEOUT_MS} ms")
                latency_ms = (time.perf_counter_ns() - started) / 1_000_000
                cpu_ms, peak = win.process_metrics(process)
                status = _winapi.GetExitCodeProcess(process)
            finally:
                try:
                    if assigned:
                        if not win.terminate_job(job, 1):
                            raise ctypes.WinError(ctypes.get_last_error())
                    else:
                        _winapi.TerminateProcess(process, 1)  # Never resumed.
                    if _winapi.WaitForSingleObject(process, 5_000) != 0:
                        raise RuntimeError("probe process did not terminate during cleanup")
                finally:
                    _winapi.CloseHandle(thread)
                    _winapi.CloseHandle(process)
        finally:
            _winapi.CloseHandle(job)

        log.seek(0)
        output = log.read(65_536)
        # `probe` can exit zero even when it could not launch Herdr.
        if status != 0 or not all(marker in output for marker in (
            b"[herdr --version] success=true",
            b"[herdr plugin list --json] success=true",
        )):
            raise RuntimeError(f"probe failed (exit {status}): {output[:2048]!r}")
        return dict(zip(METRICS, (latency_ms, cpu_ms, peak)))


def emit(kind, **fields):
    print(json.dumps({"type": kind, "mode": MODE, **fields}), flush=True)


def summarize(rows):
    result = {}
    for metric in METRICS:
        values = sorted(row[metric] for row in rows if row[metric] is not None)
        result[metric] = {
            "count": len(values),
            "median": statistics.median(values) if values else None,
            "p95": values[math.ceil(0.95 * len(values)) - 1] if values else None,
        }
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="+", metavar="LABEL=PATH", help="pinned .exe and its label")
    parser.add_argument("--fixture", required=True, type=Path, help="compiled synthetic herdr.exe")
    parser.add_argument("--runs", type=int, default=30, help="measured runs per binary/workload")
    parser.add_argument("--seed", type=int, default=47, help="interleaving seed")
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("requires native Windows CPython")
    if args.runs < 1:
        parser.error("--runs must be positive")

    binaries = {}
    for spec in args.binary:
        label, sep, path = spec.partition("=")
        if not sep or not label or not path or label in binaries:
            parser.error(f"expected unique LABEL=PATH: {spec!r}")
        binaries[label] = Path(path).resolve()
    fixture = args.fixture.resolve()
    for path in [*binaries.values(), fixture]:
        if path.suffix.lower() != ".exe" or not path.is_file():
            parser.error(f"expected an existing .exe: {path}")

    win = Windows()
    rng = random.Random(args.seed)
    cases = [(label, workload, delay) for label in binaries for workload, delay in WORKLOADS]
    samples = {(label, workload): [] for label, workload, _ in cases}
    emit(
        "metadata", platform="windows", seed=args.seed, warmups_per_case=WARMUPS,
        runs_per_case=args.runs, timeout_ms=TIMEOUT_MS,
        binaries={label: str(path) for label, path in binaries.items()},
        fixture=str(fixture), workloads=dict(WORKLOADS),
        latency_scope="launch_to_probe_exit", resource_scope="probe_process_only",
    )
    with tempfile.TemporaryDirectory(prefix="herdr-native-backoff-") as work:
        isolated_fixture = Path(work) / "herdr.exe"
        shutil.copyfile(fixture, isolated_fixture)
        for round_index in range(WARMUPS + args.runs):
            rng.shuffle(cases)
            for label, workload, delay in cases:
                try:
                    with tempfile.TemporaryDirectory(prefix="run-", dir=work) as directory:
                        root = Path(directory)
                        env = fixture_env(root, isolated_fixture, delay)
                        row = run_probe(win, binaries[label], root, env)
                except (OSError, RuntimeError) as error:
                    raise SystemExit(
                        f"{label}/{workload} round {round_index + 1}: {error}"
                    ) from error
                if round_index >= WARMUPS:
                    samples[label, workload].append(row)
                    emit(
                        "sample", label=label, workload=workload, delay_ms=delay,
                        sample=round_index - WARMUPS + 1, **row,
                    )
    for label in binaries:
        for workload, delay in WORKLOADS:
            rows = samples[label, workload]
            emit(
                "summary", label=label, workload=workload, delay_ms=delay,
                count=len(rows), **summarize(rows),
            )


if __name__ == "__main__":
    main()

"""Build pinned revisions and run measurements on this job's native runner."""

import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile


def run(*args, **kwargs):
    print("+", *args, flush=True)
    subprocess.run(args, check=True, **kwargs)


def revision(directory):
    return subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=directory, text=True).strip()


def main():
    repo = Path(__file__).resolve().parent.parent
    pins = json.loads((repo / "scripts/native-backoff-pins.json").read_text())
    evidence = repo / "native-evidence"
    evidence.mkdir(exist_ok=True)
    suffix = ".exe" if os.name == "nt" else ""
    metadata = {"runner": platform.platform(), "arch": platform.machine(),
                "workflow_head": revision(repo), "builds": {}}
    toolchain = subprocess.check_output(["rustc", "+stable", "-vV"], text=True)
    (evidence / "rustc.txt").write_text(toolchain)
    with tempfile.TemporaryDirectory(dir=os.environ.get("RUNNER_TEMP")) as temporary:
        root = Path(temporary)
        fixture = root / f"herdr{suffix}"
        run("rustc", "+stable", "--edition=2021", "-O",
            str(repo / "scripts/native-backoff-fixture.rs"), "-o", str(fixture))
        metadata["fixture_sha256"] = hashlib.sha256(fixture.read_bytes()).hexdigest()
        target = root / "target"
        binaries = []
        for label, sha in pins.items():
            checkout = root / "source"
            run("git", "worktree", "add", "--detach", str(checkout), sha, cwd=repo)
            try:
                if revision(checkout) != sha:
                    raise RuntimeError(f"wrong revision for {label}")
                run("cargo", "+stable", "build", "--release", "--locked",
                    "--target-dir", str(target), cwd=checkout)
                executable = root / f"{label}{suffix}"
                shutil.copy2(target / "release" / f"herdr-lazy{suffix}", executable)
                metadata["builds"][label] = {
                    "revision": sha, "sha256": hashlib.sha256(executable.read_bytes()).hexdigest()
                }
                binaries.append(f"{label}={executable}")
                if label == "integrated":
                    with (evidence / "resource-tests.txt").open("w") as log:
                        run("cargo", "+stable", "test", "--locked", "--target-dir", str(target),
                            "herdr_output::tests::", "--", "--nocapture", cwd=checkout,
                            stdout=log, stderr=subprocess.STDOUT)
            finally:
                run("git", "worktree", "remove", str(checkout), cwd=repo)
        (evidence / "builds.json").write_text(json.dumps(metadata, indent=2))
        mode = "windows" if os.name == "nt" else "posix"
        with (evidence / "samples.jsonl").open("w") as output:
            run(sys.executable, str(repo / f"scripts/native-backoff-{mode}.py"),
                "--fixture", str(fixture), *binaries, stdout=output)


if __name__ == "__main__":
    main()

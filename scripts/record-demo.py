#!/usr/bin/env python3
"""Record the actual herdr-lazy TUI using a disposable, offline Unix PTY fixture."""

import argparse
import codecs
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import tempfile
import termios
import time


COLUMNS, ROWS = 112, 24
TIMEOUT = 5
FRAME = b"\x1b[H\x1b[2J"
LIST = (b"[?]\x1b[0m more", b"[x] Remove")
BROWSER = (b"[esc]\x1b[0m back", b"[Ctrl+R] Refresh")


def fixture(root):
    config = root / "config"
    config.mkdir()
    bundle = config / "plugins.list"
    bundle.write_text("demo/workspace-tools\ndemo/focus-timer\ndemo/session-notes\n", encoding="utf-8")
    plugins = [
        {
            "plugin_id": "workspace-tools",
            "name": "Workspace Tools",
            "enabled": True,
            "description": "Navigate worktrees and keep related terminals together.",
            "actions": [{"id": "workspace-tools.list", "title": "List worktrees"}],
            "panes": [{"id": "workspace-tools.open", "title": "Open workspace picker"}],
            "events": [{"on": "workspace.created"}],
            "source": {"kind": "github", "owner": "demo", "repo": "workspace-tools",
                       "resolved_commit": "adbe2134ffaa0123456789abcdeffedcba098765"},
        },
        {
            "plugin_id": "focus-timer",
            "name": "Focus Timer",
            "enabled": True,
            "description": "Start a timer from your current workspace.",
            "source": {"kind": "github", "owner": "demo", "repo": "focus-timer",
                       "resolved_commit": "abcd1234ffaa0123456789abcdeffedcba098765"},
        },
    ]
    cache = root / "cache" / "herdr-lazy"
    cache.mkdir(parents=True)
    catalog = [
        ("workspace-tools", "Navigate worktrees and related terminal panes", 132, ["worktree"]),
        ("focus-timer", "Track focus sessions", 42, ["productivity"]),
        ("session-notes", "Capture a short note from the current pane", 28, ["notes"]),
        ("worktree-inspector", "Inspect active Git worktrees", 68, ["worktree"]),
        ("worktree-bookmarks", "Jump to recently visited worktrees", 54, ["worktree"]),
    ]
    (cache / "marketplace.json").write_text(
        json.dumps({"plugins": [
            {"fullName": f"demo/{name}", "description": description, "stars": stars, "topics": topics}
            for name, description, stars, topics in catalog
        ]}),
        encoding="utf-8",
    )
    snapshot = root / "plugins.json"
    snapshot.write_text(json.dumps({"result": {"plugins": plugins}}), encoding="utf-8")
    stub = root / "herdr"
    stub.write_text(
        "#!/bin/sh\n"
        'case "$*" in\n'
        '  --version) printf "%s\\n" "herdr 0.9.5" ;;\n'
        '  "plugin list --json") while IFS= read -r line || [ -n "$line" ]; do\n'
        '    printf "%s\\n" "$line"; done < "$DEMO_PLUGIN_LIST" ;;\n'
        '  *) exit 1 ;;\n'
        'esac\n',
        encoding="utf-8",
    )
    stub.chmod(0o700)
    return {
        "HOME": str(root),
        "XDG_CONFIG_HOME": str(config),
        "XDG_CACHE_HOME": str(cache.parent),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_STATE_HOME": str(root / "state"),
        "HERDR_BIN_PATH": str(stub),
        "HERDR_PLUGIN_CONFIG_DIR": str(config),
        "HERDR_LAZY_LIST": str(bundle),
        "DEMO_PLUGIN_LIST": str(snapshot),
        "PATH": str(root),
        "TERM": "xterm-256color",
    }


def read_screen(master, marker, footers):
    deadline = time.monotonic() + TIMEOUT
    output = bytearray()
    while True:
        start = output.find(marker)
        if start >= 0 and any(footer in output[start + len(marker):] for footer in footers):
            return bytes(output)
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([master], [], [], remaining)[0]:
            raise RuntimeError(f"no completed screen for {marker!r}")
        chunk = os.read(master, 8192)
        if not chunk:
            raise RuntimeError(f"terminal closed before {marker!r}")
        output.extend(chunk)


def record(binary, root, env, columns):
    output = []
    decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
    master, slave = pty.openpty()
    child = None
    try:
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, columns, 0, 0))
        child = subprocess.Popen(
            [binary, "ui"], cwd=root, env=env, stdin=slave, stdout=slave, stderr=slave
        )
        os.close(slave)
        slave = None

        def capture(at, marker, footers):
            output.append([at, "o", decoder.decode(read_screen(master, marker, footers))])

        capture(0.1, FRAME, LIST)
        os.write(master, b"l")
        capture(4.0, b"List worktrees", (b"[esc]\x1b[0m back", b"Esc Back"))
        os.write(master, b"\x1b")
        capture(9.0, FRAME, LIST)
        os.write(master, b"/")
        capture(11.0, FRAME, BROWSER)
        for i, query in enumerate((b"w", b"wo", b"wor", b"work", b"workt", b"worktr",
                                   b"worktre", b"worktree")):
            os.write(master, query[-1:])
            capture(13.0 + i * 0.4, b"\x1b[1m" + query + b"\x1b[0m\x1b[7m ", BROWSER)
        os.write(master, b"\x1b")
        capture(21.0, FRAME, LIST)
        os.write(master, b"q")
        capture(24.0, b"\x1b[?1049l", (b"",))
        if child.wait(timeout=TIMEOUT) != 0:
            raise RuntimeError("herdr-lazy exited with an error")
    finally:
        if child is not None and child.poll() is None:
            child.kill()
            child.wait()
        if slave is not None:
            os.close(slave)
        os.close(master)
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, help="path to a built herdr-lazy release binary")
    parser.add_argument("output", type=Path, help="new asciinema .cast file")
    parser.add_argument("--columns", type=int, choices=(80, COLUMNS), default=COLUMNS)
    parser.add_argument(
        "--theme", choices=("terminal", "catppuccin", "tokyo-night", "gruvbox", "nord"),
        help="palette for builds that support HERDR_LAZY_THEME",
    )
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"binary is not executable: {binary}")
    if args.output.exists():
        parser.error(f"output already exists: {args.output}")
    with tempfile.TemporaryDirectory(prefix="herdr-lazy-demo-") as directory:
        root = Path(directory)
        env = fixture(root)
        if args.theme:
            env["HERDR_LAZY_THEME"] = args.theme
        frames = record(binary, root, env, args.columns)
    header = {
        "version": 2,
        "width": args.columns,
        "height": ROWS,
        "title": "herdr-lazy · Manage, inspect and search",
        "env": {"TERM": "xterm-256color", "SHELL": "/bin/sh"},
    }
    with args.output.open("x", encoding="utf-8") as cast:
        for item in [header, *frames]:
            cast.write(json.dumps(item, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()

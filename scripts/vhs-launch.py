#!/usr/bin/env python3
"""Launch the TUI with a fictional catalog and an allowlisted environment."""

import hashlib
import json
import os
from pathlib import Path
import runpy
import sys

binary, root = map(Path, sys.argv[1:3])
fixture = runpy.run_path(str(Path(__file__).with_name("record-demo.py")))["fixture"]
env = fixture(root)

worktrees = (
    ("tools", "Navigate related worktrees"),
    ("inspector", "Inspect active worktrees"),
    ("bookmarks", "Jump to saved worktrees"),
    ("review", "Open review workspaces"),
    ("cleanup", "Find stale worktrees"),
    ("notes", "Keep notes per worktree"),
    ("layout", "Restore a pane layout"),
    ("status", "Review branch status"),
    ("history", "Find recent branches"),
    ("tasks", "Group tasks by branch"),
    ("sessions", "Resume work sessions"),
    ("compare", "Compare branch changes"),
)
catalog_rows = [
    (f"worktree-{name}", description) for name, description in worktrees
] + [
    ("focus-timer", "Track focus sessions"),
    ("session-notes", "Capture session notes"),
    ("command-history", "Search recent commands"),
    ("git-summary", "Summarize Git activity"),
    ("pane-switcher", "Switch to a named pane"),
    ("keymap-helper", "Show shortcut reminders"),
]
descriptions = dict(catalog_rows)
bundle = [
    "worktree-tools", "focus-timer", "session-notes", "command-history",
    "git-summary", "pane-switcher", "worktree-review", "worktree-layout",
]
Path(env["HERDR_LAZY_LIST"]).write_text(
    "".join(f"demo/{name}\n" for name in bundle), encoding="utf-8"
)

snapshot = Path(env["DEMO_PLUGIN_LIST"])
installed = json.loads(snapshot.read_text(encoding="utf-8"))
plugins = installed["result"]["plugins"]
plugins[0]["description"] = "Navigate related worktrees and pin frequently used workspaces."
if sys.argv[3:] == ["--unicode"]:
    plugins[0]["description"] = (
        "Worktree 👩‍💻 tools for cafe\u0301 and 日本語 projects. "
        "Navigate related worktrees and pin frequently used workspaces "
        "while keeping related terminal panes together."
    )
plugins[0]["source"]["repo"] = "worktree-tools"
for name in [*bundle[2:6], "keymap-helper"]:
    plugins.append({
        "plugin_id": name,
        "name": name.replace("-", " ").title(),
        "description": descriptions[name],
        "enabled": name != "git-summary",
        "source": {
            "kind": "github",
            "owner": "demo",
            "repo": name,
            "resolved_commit": hashlib.sha256(name.encode("utf-8")).hexdigest()[:40],
        },
    })
snapshot.write_text(json.dumps(installed), encoding="utf-8")

catalog = {
    "plugins": [
        {"fullName": f"demo/{name}", "description": description, "stars": 144 - index * 5}
        for index, (name, description) in enumerate(catalog_rows)
    ]
}
cache = Path(env["XDG_CACHE_HOME"]) / "herdr-lazy" / "marketplace.json"
cache.write_text(json.dumps(catalog), encoding="utf-8")
stub = root / "curl"
stub.write_text(
    '#!/bin/sh\n/bin/sleep 2.6\nexec /bin/cat "$DEMO_CATALOG"\n', encoding="utf-8"
)
stub.chmod(0o700)
# The offline downloader serves the same fictional catalog after a visible delay.
response = root / "catalog.json"
response.write_bytes(cache.read_bytes())
env["DEMO_CATALOG"] = str(response)
env["LC_ALL"] = "C.UTF-8"
os.chdir(root)
os.execve(binary, [str(binary), "ui"], env)

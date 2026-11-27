#!/usr/bin/env python3
"""Launch the released TUI with the documentation fixture and an allowlisted environment."""

import json
import os
from pathlib import Path
import runpy
import sys

binary, root = map(Path, sys.argv[1:])
fixture = runpy.run_path(str(Path(__file__).with_name("record-demo.py")))["fixture"]
env = fixture(root)
catalog = root / "cache" / "herdr-lazy" / "marketplace.json"
data = json.loads(catalog.read_text(encoding="utf-8"))
descriptions = (
    "Navigate related worktrees",
    "Track focus sessions",
    "Capture a workspace note",
    "日本語の作業メモ · Inspect worktrees",
    "Jump to a recent worktree",
)
for item, description in zip(data["plugins"], descriptions):
    item["description"] = description
snapshot = root / "plugins.json"
installed = json.loads(snapshot.read_text(encoding="utf-8"))
installed["result"]["plugins"][0]["description"] = "Navigate related worktrees"
installed["result"]["plugins"][1]["description"] = "Track focus sessions"
snapshot.write_text(json.dumps(installed), encoding="utf-8")
catalog.write_text(json.dumps(data, ensure_ascii=False), encoding="utf-8")
env["LC_ALL"] = "C.UTF-8"
os.chdir(root)
os.execve(binary, [str(binary), "ui"], env)

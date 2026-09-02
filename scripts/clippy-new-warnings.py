#!/usr/bin/env python3
"""Fail on clippy diagnostics that land on lines this branch ADDED.

The workspace carries pre-existing clippy warnings; a blanket `-D warnings`
gate would either fail on them or force unrelated cleanups. This reads
`cargo clippy --message-format=json` and `git diff -U0 <base>` and reports
only diagnostics whose primary span falls inside an added hunk (or anywhere
in a file that does not exist on the base ref).

Usage: clippy-new-warnings.py [--base main] -- <cargo clippy args...>
"""
import json
import re
import subprocess
import sys


def added_lines(base):
    """{path: set(line numbers)} added vs base; None value = whole file is new."""
    out = subprocess.run(
        ["git", "diff", "-U0", "--no-color", base, "--", "."],
        capture_output=True, text=True, check=True,
    ).stdout
    untracked = subprocess.run(
        ["git", "ls-files", "--others", "--exclude-standard"],
        capture_output=True, text=True, check=True,
    ).stdout.split()
    files = {path: None for path in untracked}
    path = None
    for line in out.splitlines():
        if line.startswith("+++ "):
            target = line[4:]
            if target == "/dev/null":
                path = None
                continue
            path = target[2:] if target.startswith("b/") else target
            files.setdefault(path, set())
        elif line.startswith("--- /dev/null"):
            pass
        elif line.startswith("@@") and path is not None:
            m = re.match(r"@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@", line)
            start = int(m.group(1))
            count = int(m.group(2)) if m.group(2) is not None else 1
            if files[path] is not None:
                files[path].update(range(start, start + count))
    # A file whose old side is /dev/null is entirely new.
    for block in re.finditer(r"--- /dev/null\n\+\+\+ b/(.+)", out):
        files[block.group(1)] = None
    return files


def main():
    args = sys.argv[1:]
    base = "main"
    if args[:1] == ["--base"]:
        base = args[1]
        args = args[2:]
    if args[:1] == ["--"]:
        args = args[1:]
    added = added_lines(base)
    proc = subprocess.run(
        ["cargo", "clippy", "--message-format=json", *args],
        capture_output=True, text=True,
    )
    new = []
    for line in proc.stdout.splitlines():
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if msg.get("reason") != "compiler-message":
            continue
        diag = msg["message"]
        if diag.get("level") not in ("warning", "error"):
            continue
        for span in diag.get("spans", []):
            if not span.get("is_primary"):
                continue
            path = span["file_name"]
            if path not in added:
                continue
            lines = added[path]
            if lines is None or span["line_start"] in lines:
                new.append(f"{path}:{span['line_start']}: {diag['message']}")
    if proc.returncode != 0 and not proc.stdout:
        sys.stderr.write(proc.stderr)
        return 2
    if new:
        print("new clippy diagnostics on added lines:")
        for item in sorted(set(new)):
            print("  " + item)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

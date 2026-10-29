#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
# SPDX-License-Identifier: Apache-2.0
"""Non-blocking PreToolUse hint: a grep for a bare identifier -> RossLabs LSP.

Reads one hook payload (Claude Code or Codex PreToolUse JSON) on stdin. When
the tool is Grep, or a shell command running rg/grep, and the pattern is a
bare identifier inside a git repository whose languages have a language
server on PATH, prints `hookSpecificOutput.additionalContext` naming the
exact `rosslabs-lsp refs --symbol` call. Otherwise prints nothing.

It never blocks: every path exits 0, no permission decision is emitted, and
no language server is started. Readiness is decided from file extensions in
the repository plus `shutil.which` on the server binary. Set
ROSSLABS_LSP_HINT=0 to silence it.
"""

from __future__ import annotations

import json
import os
import re
import shlex
import shutil
import sys

IDENT = re.compile(r"^[A-Za-z_][A-Za-z0-9_]{2,}$")
# Wrappers agents add around an identifier: \bname\b, \<name\>, \bname.
WRAPPERS = re.compile(r"^(?:\\b|\\<)?(.*?)(?:\\b|\\>)?$")
# Identifier-shaped words that are almost never code symbols.
NOT_SYMBOLS = {"TODO", "FIXME", "XXX", "HACK", "NOTE", "true", "false", "null", "None"}

# Mirrors code-intel/src/registry.rs SERVERS (extension -> server binary).
SERVERS = {
    "python": ("ty", (".py", ".pyi")),
    "rust": ("rust-analyzer", (".rs",)),
    "swift": ("sourcekit-lsp", (".swift",)),
    "typescript": ("typescript-language-server", (".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs")),
    "c": ("clangd", (".c", ".h", ".cc", ".cpp", ".hpp", ".m", ".mm")),
}
EXT_TO_LANG = {ext: lang for lang, (_, exts) in SERVERS.items() for ext in exts}
SKIP_DIRS = {"target", "node_modules", "build", "dist", "out", "vendor", "__pycache__", "DerivedData"}
SCAN_LIMIT = 3000  # directory entries; keeps the hook well under 100 ms
SCAN_DEPTH = 4

SEARCH_TOOLS = {"rg", "grep", "egrep", "ggrep"}
SHELL_TOOLS = {"Bash", "shell", "local_shell", "exec_command"}
# rg/grep options that consume the following argument.
VALUE_OPTS = {
    "-e", "--regexp", "-f", "--file", "-g", "--glob", "--iglob", "-t", "--type", "-T",
    "--type-not", "-m", "--max-count", "-A", "-B", "-C", "--context", "--after-context",
    "--before-context", "-M", "--max-columns", "-j", "--threads", "--color", "--colors",
    "--sort", "--sortr", "--include", "--exclude", "--exclude-dir", "--max-depth", "-d",
    "--type-add", "--encoding", "-E", "--pre", "--pre-glob", "--engine", "--path-separator",
}


def identifier(pattern: str) -> str | None:
    """Return the identifier a search pattern names, or None."""
    if not isinstance(pattern, str):
        return None
    m = WRAPPERS.match(pattern.strip())
    name = m.group(1) if m else pattern
    if IDENT.match(name) and name not in NOT_SYMBOLS:
        return name
    return None


def parse_search_command(command: str) -> tuple[str, str | None] | None:
    """Find (pattern, path) of the first rg/grep in a shell command."""
    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars=True)
        lexer.whitespace_split = True
        tokens = list(lexer)
    except ValueError:
        return None
    segments: list[list[str]] = [[]]
    for tok in tokens:
        if tok in {"|", "||", "&&", ";", "&", "(", ")"}:
            segments.append([])
        else:
            segments[-1].append(tok)
    for seg in segments:
        while seg and re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", seg[0]):
            seg = seg[1:]  # leading VAR=value assignments
        if not seg or os.path.basename(seg[0]) not in SEARCH_TOOLS:
            continue
        pattern = None
        positional: list[str] = []
        args = seg[1:]
        i = 0
        while i < len(args):
            a = args[i]
            if a == "--":
                positional.extend(args[i + 1 :])
                break
            if a in ("-e", "--regexp"):
                if i + 1 < len(args) and pattern is None:
                    pattern = args[i + 1]
                i += 2
                continue
            if a.startswith("--regexp="):
                pattern = pattern or a.split("=", 1)[1]
            elif a in VALUE_OPTS:
                i += 2
                continue
            elif not a.startswith("-"):
                positional.append(a)
            i += 1
        if pattern is None:
            if not positional:
                return None
            pattern, positional = positional[0], positional[1:]
        return pattern, (positional[0] if positional else None)
    return None


def git_root(start: str) -> str | None:
    d = os.path.abspath(start)
    if not os.path.isdir(d):
        d = os.path.dirname(d)
    while True:
        if os.path.exists(os.path.join(d, ".git")):
            return d
        parent = os.path.dirname(d)
        if parent == d:
            return None
        d = parent


def languages_present(root: str) -> set[str]:
    """Languages with at least one file under root, by a bounded walk."""
    found: set[str] = set()
    seen = 0
    stack = [(root, 0)]
    while stack and seen < SCAN_LIMIT and len(found) < len(SERVERS):
        d, depth = stack.pop()
        try:
            with os.scandir(d) as it:
                for e in it:
                    seen += 1
                    if seen >= SCAN_LIMIT:
                        break
                    name = e.name
                    if e.is_dir(follow_symlinks=False):
                        if depth < SCAN_DEPTH and not name.startswith(".") and name not in SKIP_DIRS:
                            stack.append((e.path, depth + 1))
                    else:
                        lang = EXT_TO_LANG.get(os.path.splitext(name)[1].lower())
                        if lang:
                            found.add(lang)
        except OSError:
            continue
    return found


def ready_languages(target: str) -> list[str]:
    """Languages of target (a file or directory) whose server is on PATH."""
    if os.path.isfile(target):
        lang = EXT_TO_LANG.get(os.path.splitext(target)[1].lower())
        langs = {lang} if lang else set()
    else:
        langs = languages_present(target)
    return sorted(l for l in langs if shutil.which(SERVERS[l][0]))


def code_only_filter(tool_input: dict) -> bool:
    """False when a Grep call is restricted to non-code files (glob/type)."""
    glob = tool_input.get("glob")
    if isinstance(glob, str) and glob:
        exts = re.findall(r"[.{,]([A-Za-z0-9+]+)", glob)
        if exts and not any("." + x.lower() in EXT_TO_LANG for x in exts):
            return False
    t = tool_input.get("type")
    code_types = {"py", "python", "rust", "rs", "swift", "ts", "typescript", "js", "javascript", "c", "cpp", "objc"}
    if isinstance(t, str) and t and t not in code_types:
        return False
    return True


def decide(payload: dict) -> str | None:
    """Return the additionalContext text, or None to stay silent."""
    tool = payload.get("tool_name")
    tool_input = payload.get("tool_input") or {}
    cwd = payload.get("cwd") or os.getcwd()
    if not isinstance(tool_input, dict):
        return None
    if tool == "Grep":
        pattern, path = tool_input.get("pattern"), tool_input.get("path")
        if not code_only_filter(tool_input):
            return None
    elif tool in SHELL_TOOLS:
        command = tool_input.get("command")
        if isinstance(command, list):
            command = " ".join(str(c) for c in command)
        if not isinstance(command, str):
            return None
        parsed = parse_search_command(command)
        if not parsed:
            return None
        pattern, path = parsed
    else:
        return None
    name = identifier(pattern)
    if not name:
        return None
    target = path if isinstance(path, str) and path else cwd
    if not os.path.isabs(target):
        target = os.path.join(cwd, target)
    if not os.path.exists(target):
        return None
    root = git_root(target)
    if not root:
        return None
    langs = ready_languages(target)
    if not langs:
        return None
    where = "" if os.path.abspath(target) == os.path.abspath(cwd) else f" --path {shlex.quote(target)}"
    return (
        f"`{name}` is a bare identifier and this repository has ready language servers for "
        f"{', '.join(langs)}. RossLabs LSP answers symbol questions with compiler accuracy, "
        f"skipping comments, strings and unrelated same-named symbols: "
        f"`rosslabs-lsp refs --symbol {name}{where}` lists every reference with its containing "
        f"function, and `rosslabs-lsp def --symbol {name}{where}` gives the definition "
        f"(MCP tools `refs` / `def` with {{file, symbol: \"{name}\"}}). "
        f"grep stays the right tool for strings, comments, config, or when LSP reports ready: false."
    )


def main() -> int:
    if os.environ.get("ROSSLABS_LSP_HINT") == "0":
        return 0
    try:
        payload = json.load(sys.stdin)
        text = decide(payload) if isinstance(payload, dict) else None
    except Exception:  # a hint must never break the tool call
        return 0
    if text:
        event = payload.get("hook_event_name") or "PreToolUse"
        json.dump({"hookSpecificOutput": {"hookEventName": event, "additionalContext": text}}, sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main())

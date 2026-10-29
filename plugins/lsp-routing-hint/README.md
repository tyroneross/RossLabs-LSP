<!--
SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
SPDX-License-Identifier: Apache-2.0
-->
# lsp-routing-hint

A PreToolUse hook that never blocks. When an agent greps for a bare code
identifier (`workspace_root`, `\bMyType\b`) inside a git repository that has a
ready language server, it adds one line of context naming the exact
RossLabs LSP call:

```text
`workspace_root` is a bare identifier and this repository has ready language servers for rust.
... `rosslabs-lsp refs --symbol workspace_root --path <dir>` lists every reference ...
```

It stays silent for regex patterns, strings, non-code globs (`*.md`), paths
outside git, and languages without a server on PATH. It never starts a language
server: readiness comes from file extensions in a bounded walk plus a PATH
lookup, about 30 ms. It always exits 0 and never emits a permission decision.
Set `ROSSLABS_LSP_HINT=0` to silence it.

Triggers: Claude Code `Grep`, and `Bash` (Claude Code and Codex) commands that
run `rg`, `grep` or `egrep`. Both hosts send the same PreToolUse JSON
(`tool_name`, `tool_input`, `cwd`) and accept the same
`hookSpecificOutput.additionalContext` reply, so one script serves both.

## Install

Nothing is wired automatically. Pick one.

**Claude Code plugin** (uses [`hooks/hooks.json`](hooks/hooks.json)):

```
/plugin marketplace add tyroneross/RossLabs-LSP
/plugin install lsp-routing-hint@rosslabs-lsp
```

**Claude Code settings** (`~/.claude/settings.json` or `.claude/settings.json`),
with `<repo>` replaced by this checkout's absolute path:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Grep|Bash",
        "hooks": [
          {"type": "command", "command": "python3 <repo>/plugins/lsp-routing-hint/scripts/lsp_routing_hint.py", "timeout": 5}
        ]
      }
    ]
  }
}
```

**Codex** (`~/.codex/config.toml`; trust it once with `/hooks`):

```toml
[[hooks.PreToolUse]]
matcher = "Bash"

[[hooks.PreToolUse.hooks]]
type = "command"
command = "python3 <repo>/plugins/lsp-routing-hint/scripts/lsp_routing_hint.py"
timeout = 5
```

## Test

```bash
python3 -m unittest discover -s plugins/lsp-routing-hint/tests
```

# RossLabs LSP: routing and effectiveness assessment (2026-10-28)

Read-only assessment of how agents on one workstation reach `rosslabs-lsp`, how often
they call it, and how well it answers. Window: last 14 days of local transcripts.
Counts come from a Python miner over Claude Code and Codex session logs; no transcript
content is reproduced here.

## Headline

Agents almost never use RossLabs LSP for symbol navigation. The tool answers
correctly and fast once warm, but no host-level rule, hook, or skill routes symbol
questions to it, so agents grep instead: about 13,000 symbol-shaped searches against
roughly 14 organic LSP symbol queries in Claude Code, and about 1,000 against 21 in Codex.

## 1. Routing surfaces

| Surface | State |
|---|---|
| Global Claude rules (`~/.claude/CLAUDE.md`) | No mention of rosslabs-lsp or symbol routing |
| Shared `~/AGENTS.md` | No mention |
| Codex `~/.codex/AGENTS.md` | No mention |
| Claude MCP (`~/.claude.json`) | Registered as `rosslabs-lsp` (current binary). Tools are **deferred**: the model sees only names until it runs ToolSearch |
| Claude plugins (`~/.claude/settings.json`) | `ty-lsp@rosslabs-lsp` plus official `typescript-lsp`, `rust-analyzer-lsp`, `swift-lsp` enabled. The native `LSP` tool was called 0 times in 14 days |
| Claude hooks | No PreToolUse hook on Grep/Glob/Bash that nudges symbol searches toward LSP |
| Codex MCP (`~/.codex/config.toml`) | Registered as `code-intel` (legacy name), pointing at the separate `code-intel` binary built 2026-10-02, not the current `rosslabs-lsp` build. Codex made 0 MCP calls in 14 days |
| Cursor (`~/.cursor/mcp.json`) | Not registered |
| Gemini (`~/.gemini/settings.json`) | Not registered (no MCP servers at all) |
| build-loop skills | Reference only `rosslabs-lsp compat` (Phase 6 Learn, acp-practice). Nothing routes def/refs |
| MCP server `initialize` | Returns no `instructions`; tool descriptions are one line each and do not state the "prefer over grep for cross-file symbols" rule |

The only place the routing rule exists is the README, which agents do not read at session start.

## 2. Actual usage (14 days)

| Host | Sessions | Sessions using LSP | MCP calls | CLI calls | Symbol-shaped grep/rg |
|---|---|---|---|---|---|
| Claude Code | 1,820 (incl. subagents) | 16 | 86 | ~20 | 12,907 of 50,647 searches |
| Codex | 314 | 20 | 0 | 34 | 1,044 of 6,470 searches |

- Claude MCP by tool: diag 50, def 19, compat 8, refs 5, doctor 4. CLI: acp 8, hover 5, def 2, refs 2, compat 2.
- Codex CLI by subcommand: refs 21, compat 11, diag 1, doctor 1. All via shell; the registered MCP server is unused.
- Claude MCP error rate 20/86 (23%): def 10/19 errors came from one synthetic test session querying symbols that do not exist ("no symbol named …"); diag 6/50 timed out waiting for Rust diagnostics; compat 1. Codex: 1 timeout.
- `rosslabs-lsp acp` ran 8 times in Claude, but the installed binary rejects `acp` (see §3), indicating those calls failed or used a dev build `[INFERRED]`.
- "Symbol-shaped" = grep pattern is a bare camelCase/PascalCase/snake_case identifier or contains a definition keyword (`fn `, `class `, `def `, `function `, `\b`). The heuristic over-counts; even at 10% precision it dwarfs LSP use by ~90x in Claude.

## 3. Effectiveness measurements

Host load average ~50 during measurement; a daemon was already running.

| Query | Latency | Result vs grep |
|---|---|---|
| `doctor` | 0.01 s | 5/5 servers ready (python, rust, swift, typescript, c) |
| Rust `refs` (first call, this crate) | 24.7 s | 7 refs; grep found the same 7 plus the declaration |
| Rust `refs` (repeat) | 0.04 s | same |
| Rust `def` on a call site | 0.02 s | correct definition line |
| TypeScript `refs` (small repo) | 1.6 s, repeat 2.1 s | 2 refs; matches grep minus declaration. No warm reuse across CLI calls |
| grep for comparison | ~0.05 s | correct here because the names were unique |

Findings:
- Results were correct in every sample; samples used unique names, so LSP's advantage over grep (overloads, common names, re-exports) was not exercised.
- Cold start is the main cost: ~25 s for rust-analyzer on first query, ~2 s per TypeScript query every time.
- The CLI requires `file:line:col`; only MCP accepts `symbol`. Shell-only hosts (Codex today) must locate a position first, usually with grep.
- The installed binary (built 2026-10-20) predates `acp doctor` (commit 710ae6c): `rosslabs-lsp acp` returns "unknown query", and Claude's tool list lacks `acp_doctor`.
- The compatibility registry holds 1 record (terminal-input). There is no `agent-hooks` section yet.

## 4. Recommendations

1. **Put the routing rule where agents read it.** Add a 3-line "Symbol navigation" rule to the shared agent instructions (global AGENTS.md and Claude rules): use `rosslabs-lsp` def/refs/hover for cross-file symbol questions; grep for strings, config, and prose. Owner: user-level instruction files.
2. **Ship the rule inside the MCP server.** Return `instructions` from `initialize` and lengthen def/refs descriptions to say when to prefer them over grep and that `symbol` works without a position. Hosts that defer tools still surface server instructions. Owner: this repo, `code-intel/src/mcp.rs`.
3. **Fix Codex registration and stale binaries.** Rename `[mcp_servers.code-intel]` to `rosslabs-lsp` pointing at the current binary, reinstall from HEAD, and make `doctor` report its own build commit so staleness is visible. Add a `--symbol` option to the CLI for shell-only hosts. Owner: this repo (`code-intel/src/main.rs`, README install section) plus the Codex config.
4. **Add a soft PreToolUse nudge.** A Claude hook on Grep (and Bash `rg|grep`) that, when the pattern is a bare identifier and the path is a code repo with a ready server, injects a one-line hint ("rosslabs-lsp refs answers this with compiler accuracy") without blocking. Ship it as a plugin hook so it travels with the plugin, and an equivalent Codex hook when available. Owner: this repo, `plugins/` (new hook), with the Claude hooks config.
5. **Wire `agent-hooks` compatibility + `hooks doctor` at hook-edit time.** Add an `agent-hooks` section to `compatibility/registry.json` (per-host hook events, payload fields, timeouts), and call `rosslabs-lsp compat --section agent-hooks` plus `hooks doctor` (a) from the existing write-time hook-config lint before any hooks JSON/TOML write and (b) from build-loop's acp-practice and Phase 6 Learn checks when a diff touches hook configs. Owner: this repo (`compatibility/`, `code-intel/src/compat.rs`) and build-loop (`skills/acp-practice`, `skills/build-loop/references/phase-6-learn.md`).

Also worth a follow-up: pre-warm rust-analyzer in the daemon on session start for the current repo to remove the 25 s first-query cost, and keep TypeScript servers warm between CLI calls.

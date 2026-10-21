<!--
SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
SPDX-License-Identifier: Apache-2.0
-->
# RossLabs LSP

Compiler-grade code intelligence for **any** LLM agent. One implementation, two
compatible commands, two surfaces, no vendor lock.

`grep "save"` returns 200 lines. `rosslabs-lsp refs foo.py:42:11` returns the 6 places
that call *that* function. The difference is semantic vs textual, and it is the
difference between an agent that renames safely and one that misses three call sites.

## Why this exists

Language servers already solve this, and every editor has used them for a decade.
But the wiring is per-host and non-portable:

- **Claude Code** declares LSP servers in plugin manifests (`lspServers`)
- **Codex CLI** has no native LSP at all ([openai/codex#8745](https://github.com/openai/codex/issues/8745), [#14799](https://github.com/openai/codex/issues/14799))
- **Local models and scripts** have no LSP client whatsoever

RossLabs LSP is the vendor-neutral layer under all of them. The server does the
thinking; this owns the client, and exposes it two ways.

## Install

```bash
cargo install --path . --locked --force
rosslabs-lsp doctor        # which language servers are present
```

`--force` lets Cargo transfer the legacy `code-intel` executable from the old
package name to RossLabs LSP. It is safe for a fresh install too.

## Use it as a CLI

Works in anything that can run a command — an agent, a git hook, CI, a shell script.

```bash
rosslabs-lsp def     src/query.rs::workspace_root   # where is this defined
rosslabs-lsp refs    src/query.rs::workspace_root   # every caller
rosslabs-lsp hover   src/query.rs::workspace_root   # inferred type + docs
rosslabs-lsp symbols scripts/build.py               # outline a file
rosslabs-lsp diag    scripts/build.py               # type + compile errors
rosslabs-lsp doctor                                 # server coverage
rosslabs-lsp acp doctor                             # ACP agent versions + handshake timings
```

JSON on stdout, errors on stderr, non-zero exit on failure.

## Addressing

Three forms, and the first is the one to reach for:

| Form | Meaning |
|---|---|
| `file.rs::symbol` | by name — survives edits above it |
| `file.rs::Type/method` | qualified, for nested or repeated names |
| `file.rs:42:11` | exact position, 1-based |

`file.rs:42:11` breaks the moment anything above line 42 moves, and it breaks
*silently*: the position now holds a different symbol, so the query returns a
confident wrong answer. Measured here — insert 20 lines above a symbol:

| Address | Result |
|---|---|
| `::for_path` | 7 references, correct |
| `:69:10` | **0 references, no error** |

An ambiguous name is an error listing the candidates, never a guess:

```
'dupe' is ambiguous — 2 matches: Outer/dupe (line 13), Other/dupe (line 43).
Qualify it, e.g. file::Parent/dupe
```

## Output

Compact by default. Every hit is the path, the line, and the symbol that
contains it:

```json
{"at": "src/registry.rs:107", "in": "tests/maps_extension_to_server"}
```

The containing symbol is the point. `registry.rs:107` says where;
`tests/maps_extension_to_server` says what, which is what a caller acts on.

Measured on this repo across 7 real symbols, against `grep` answering the same
question: raw LSP payloads cost **3.03x** grep's tokens, compact costs **1.04x**.
On the ambiguous names where the tool earns its place, it is cheaper *and*
right — symbol `which`: grep 326 tokens for 15 hits of which 13 are wrong,
against 97 tokens for 2 hits that are both correct.

Pass `--raw` for untouched LSP payloads.

## Use it as an MCP server

Same queries, exposed as tools, for any MCP-capable host.

Claude Code — `.mcp.json` or plugin manifest:

```json
{ "mcpServers": { "rosslabs-lsp": { "command": "rosslabs-lsp", "args": ["mcp"] } } }
```

Codex CLI — `~/.codex/config.toml`:

```toml
[mcp_servers.rosslabs-lsp]
command = "rosslabs-lsp"
args = ["mcp"]

[mcp_servers.rosslabs-lsp.env]
MCP_IDLE_TIMEOUT_MS = "1800000"
```

Tools: `def`, `refs`, `hover`, `symbols`, `diag`, `doctor`, `compat`, `acp_doctor`.
The query tools take a
`symbol` argument in preference to `line`/`column`, for the same durability
reason. MCP mode starts a daemon itself — without a warm pool every query
spawns its own language server, measured at 3.5s against 0.02s.

## Language coverage

| Language | Server | Install |
|---|---|---|
| Python | `ty` | `uv tool install ty` |
| Rust | `rust-analyzer` | `brew install rust-analyzer` |
| Swift | `sourcekit-lsp` | ships with Xcode |
| TypeScript / JS | `typescript-language-server` + TypeScript with `lib/tsserver.js` | `npm i -g typescript-language-server typescript@6` |
| C / C++ / ObjC | `clangd` | ships with Xcode / llvm |

`rosslabs-lsp doctor` reports which are actually installed.

For JavaScript and TypeScript, RossLabs LSP keeps a workspace's compiler as the
first choice and discovers a sibling global TypeScript install as the
`tsserver.fallbackPath`. Set `ROSSLABS_LSP_TSSERVER_PATH` to an explicit
`lib/tsserver.js` file when the compiler lives elsewhere. The legacy
`CODE_INTEL_TSSERVER_PATH` name remains supported. `doctor` reports
whether that fallback is ready; an executable `typescript-language-server` on
its own is not enough.

For an Obsidian-style UI cross-path check, run `rosslabs-lsp ui path/to/main.js`.
It maps static `dpl-*` source classes to the sibling `styles.css` and reports
icon calls, pseudo-element graphics, literal text nodes, and accessible labels.
When an IBR evidence packet is present, it also reports whether its source hashes
are current and whether its raw scanner verdicts require screenshot review. It
is evidence for wiring and render-review status, not a substitute for a rendered
layout check.

## Design notes

**One core, thin adapters.** `query.rs` holds every query. The CLI and the MCP
server are translation layers over it. A third surface would cost an adapter, not
a second implementation.

**Empty is not the same as none.** rust-analyzer answers `[]` in milliseconds while
still loading a workspace, and returns `-32801 ContentModified` until it is ready.
Position queries retry until the server produces a real answer or a 60s budget
expires, and the response carries `"ready": false` plus a warning when it does not.
A one-shot query here silently reports false negatives.

**Positions are UTF-16.** LSP counts characters in UTF-16 code units — not bytes,
not chars. On any line with non-ASCII text a byte offset points at the wrong symbol
without failing. See `to_lsp_position`.

**Servers are always reaped.** `Client` kills its child on drop, and a watchdog
thread hard-kills after 90s. Multiple independent clients leaking servers is a
measured failure mode, not a theoretical one — see `easy-terminal/docs/lsp-memory-rca.md`.

**MCP lifecycle is conformant.** The `mcp` mode implements all four exit paths in
`~/dev/docs/standards/mcp-lifecycle/SPEC.md` (v1, frozen 2026-08-12): stdin EOF,
SIGTERM/INT/HUP, parent-death watchdog, and an opt-in idle timeout. Verified with
that spec's own `conformance.mjs`.

## The daemon

`rosslabs-lsp daemon` keeps eligible language servers warm on a Unix socket. Measured
on this host, repeated `refs` against a Rust workspace:

| | first query | repeat |
|---|---|---|
| no daemon | 7.9s | 4.2s |
| daemon | 6.0s | **0.05s** |

Only servers whose measured cold start exceeds a threshold are pooled — `ty` at 0.06s
is not, and bypasses the daemon entirely rather than paying a socket round-trip for
nothing. Servers idle out after 5 minutes.

The daemon is a latency optimization and never a behavior change: both paths run the
same `run_with_client`, and output parity is checked rather than assumed. If the socket
is absent or stale, the CLI spawns directly and still answers.

## Status

Five languages, covering the author's codebase; adding one is a registry
entry plus an installed server. Read-only — no rename or refactor.

## Compatibility

`code-intel` remains installed as a command alias. Existing scripts, the
`CODE_INTEL_TSSERVER_PATH` variable, and `CODE_INTEL_SOCKET` continue to work.
New integrations should use `rosslabs-lsp`, `ROSSLABS_LSP_TSSERVER_PATH`, and
`ROSSLABS_LSP_SOCKET`.

## License

Apache-2.0 © Tyrone Ross, Jr

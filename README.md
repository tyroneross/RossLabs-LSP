<!--
SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
SPDX-License-Identifier: Apache-2.0
-->
# RossLabs LSP

LSP tooling for coding agents. Language servers have given editors compiler-grade
code intelligence for a decade; agents mostly still navigate by text search. This
repo closes that gap in two layers.

| | What it is | Who it serves |
|---|---|---|
| **[RossLabs LSP CLI](code-intel/)** | A Rust implementation that fronts any language server as a **CLI** and as an **MCP server** | Any agent — Claude Code, Codex, local models, CI, git hooks |
| **[`plugins/ty-lsp/`](plugins/ty-lsp/)** | A Claude Code plugin registering `ty` as the Python language server | Claude Code only |

## Why two layers

`lspServers` is a Claude Code plugin key. It works well and costs nothing — but it
exists nowhere else. Codex CLI has no native LSP at all
([openai/codex#8745](https://github.com/openai/codex/issues/8745),
[#14799](https://github.com/openai/codex/issues/14799)), and a local model has no LSP
client whatsoever.

So the plugin is the fast path where a host supports it, and `rosslabs-lsp` is the
vendor-neutral path everywhere else. Same servers underneath.

**A single-language plugin makes the host's built-in LSP tool wrong on every other
language, silently.** Installing `ty-lsp` gives Claude Code a `LSP` tool that works
— for Python. Point it at TypeScript, Rust, Swift, or C and it does not error and
does not report "no server": it answers from nothing and returns a confident,
incomplete result. Verified here: with only `ty` registered, `findReferences` on an
exported TypeScript function returned **1 hit — the declaration** — while
`rosslabs-lsp refs` on the same symbol returned **4**, including both real importers.
A declaration-only result is the signature of this failure, and it is
indistinguishable from an honest "this symbol has no callers."

The rule that follows: **install the plugin for the fast path, but route
cross-file symbol questions through `rosslabs-lsp` unless you have confirmed a
server is registered for that language.** RossLabs LSP reports `"ready"` on every
answer, so an unready result reads as UNKNOWN rather than empty. The built-in tool
carries no such signal.

## Quick start

```bash
cd code-intel && cargo install --path . --locked --force
rosslabs-lsp doctor                            # which language servers are present
rosslabs-lsp refs src/query.rs::workspace_root # every caller of a symbol
```

**Address symbols by name, not by line.** `file.rs:42:11` breaks the moment
anything above line 42 moves, and it breaks *silently* — the query resolves to
whatever now occupies that position. Verified here: insert 20 lines above a
symbol and the line-addressed query returns 0 hits with no error, while
`file.rs::for_path` still returns all 7. Line addressing still works when you
want an exact point.

Answers name the symbol containing each hit, because a line number is not
something you can act on:

```json
{"at": "src/registry.rs:107", "in": "tests/maps_extension_to_server"}
```

An ambiguous name is refused with its candidates rather than resolved to the
first match, and a name that does not exist lists what the file actually has.
Pass `--raw` for untouched LSP payloads.

Optionally keep slow servers warm:

```bash
rosslabs-lsp daemon &                  # pools rust-analyzer, sourcekit-lsp
```

Measured here, repeated `refs` against a Rust workspace: **4.2s without the daemon,
0.05s with it**. Fast servers like `ty` (0.06s) are never pooled — they bypass the
daemon rather than pay a socket round-trip. The daemon is optional: if it is not
running, the CLI spawns directly and returns the same answer. MCP mode starts one
itself, since a long-lived session that spawns a server per query costs 3.5s
instead of 0.02s.

Register it as an MCP server — Codex CLI, `~/.codex/config.toml`:

```toml
[mcp_servers.rosslabs-lsp]
command = "rosslabs-lsp"
args = ["mcp"]

[mcp_servers.rosslabs-lsp.env]
MCP_IDLE_TIMEOUT_MS = "1800000"
```

## Claude Code plugins

```
/plugin marketplace add tyroneross/RossLabs-LSP
/plugin install ty-lsp@rosslabs-lsp
```

## Language coverage

| Language | Server | Install |
|---|---|---|
| Python | `ty` | `uv tool install ty` |
| Rust | `rust-analyzer` | `brew install rust-analyzer` |
| Swift | `sourcekit-lsp` | ships with Xcode |
| TypeScript / JS | `typescript-language-server` + TypeScript with `lib/tsserver.js` | `npm i -g typescript-language-server typescript@6` |
| C / C++ / ObjC | `clangd` | ships with Xcode / llvm |

## Compatibility

`code-intel` remains installed as an alias for existing scripts and MCP configs.
New usage should call `rosslabs-lsp`. The source directory keeps its legacy
`code-intel/` path so existing repository links continue to work.

`--force` is required when upgrading an existing `code-intel` installation
because Cargo tracks binary ownership by package name.

## License

Apache-2.0 © Tyrone Ross, Jr. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

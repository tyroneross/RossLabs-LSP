<!--
SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
SPDX-License-Identifier: Apache-2.0
-->
# code-intel

Compiler-grade code intelligence for **any** LLM agent. One binary, two surfaces,
no vendor lock.

`grep "save"` returns 200 lines. `code-intel refs foo.py:42:11` returns the 6 places
that call *that* function. The difference is semantic vs textual, and it is the
difference between an agent that renames safely and one that misses three call sites.

## Why this exists

Language servers already solve this, and every editor has used them for a decade.
But the wiring is per-host and non-portable:

- **Claude Code** declares LSP servers in plugin manifests (`lspServers`)
- **Codex CLI** has no native LSP at all ([openai/codex#8745](https://github.com/openai/codex/issues/8745), [#14799](https://github.com/openai/codex/issues/14799))
- **Local models and scripts** have no LSP client whatsoever

`code-intel` is the vendor-neutral layer under all of them. The server does the
thinking; this owns the client, and exposes it two ways.

## Install

```bash
cargo install --path .
code-intel doctor          # which language servers are present
```

## Use it as a CLI

Works in anything that can run a command — an agent, a git hook, CI, a shell script.

```bash
code-intel def     src/query.rs:88:18    # where is this defined
code-intel refs    src/query.rs:51:8     # every caller
code-intel hover   src/query.rs:51:8     # inferred type + docs
code-intel symbols scripts/build.py      # outline a file
code-intel diag    scripts/build.py      # type + compile errors
code-intel doctor                        # server coverage
```

JSON on stdout, errors on stderr, non-zero exit on failure.

## Use it as an MCP server

Same queries, exposed as tools, for any MCP-capable host.

Claude Code — `.mcp.json` or plugin manifest:

```json
{ "mcpServers": { "code-intel": { "command": "code-intel", "args": ["mcp"] } } }
```

Codex CLI — `~/.codex/config.toml`:

```toml
[mcp_servers.code-intel]
command = "code-intel"
args = ["mcp"]

[mcp_servers.code-intel.env]
MCP_IDLE_TIMEOUT_MS = "1800000"
```

Tools: `def`, `refs`, `hover`, `symbols`, `diag`, `doctor`.

## Language coverage

| Language | Server | Install |
|---|---|---|
| Python | `ty` | `uv tool install ty` |
| Rust | `rust-analyzer` | `brew install rust-analyzer` |
| Swift | `sourcekit-lsp` | ships with Xcode |
| TypeScript / JS | `typescript-language-server` | `npm i -g typescript-language-server typescript` |
| C / C++ / ObjC | `clangd` | ships with Xcode / llvm |

`code-intel doctor` reports which are actually installed.

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

## Status

v0.1. Servers are spawned per invocation; a resident warm pool is v0.2, and it is
what will make cold-start-heavy servers (rust-analyzer, jdtls) fast rather than
merely correct.

## License

Apache-2.0 © Tyrone Ross, Jr

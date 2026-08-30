<!--
SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
SPDX-License-Identifier: Apache-2.0
-->
# ty-lsp

Python language server ([ty](https://github.com/astral-sh/ty), by Astral) for Claude Code,
providing code intelligence for Python projects.

`ty` is written in Rust and ships as a single static binary — no Node or Python runtime
is required for the server itself.

## Supported Extensions

`.py`, `.pyi`

## Scope — and what it does to other languages

This plugin registers **Python only**. That is worth stating loudly, because
installing it changes how the host behaves on languages it does not cover.

Once any `lspServers` plugin is installed, Claude Code has a working `LSP` tool, so
an agent will reach for it. On a file whose language has no registered server the
tool does not error and does not say "no server" — it returns a confident,
incomplete answer. Verified with only `ty` registered: `findReferences` on an
exported TypeScript function returned **1 hit, the declaration**, where
`rosslabs-lsp refs` returned **4**, including both real importers. Declaration-only
is the signature; it looks exactly like "no callers."

So: use this plugin for Python, and reach for
[RossLabs LSP](../../code-intel/) for everything else — it covers Python,
TypeScript, Rust, Swift, and C from one binary, and every answer carries a
`"ready"` boolean so an unready result reads as UNKNOWN rather than empty.

```bash
rosslabs-lsp doctor                        # per-language readiness
rosslabs-lsp refs path/to/file.ts::mySymbol
```

One caveat on `doctor`: it answers **globally**, while some servers resolve their
toolchain **per workspace**. `typescript-language-server` finds `typescript` in a
project's `node_modules`, so `doctor` can report TypeScript NOT READY on a machine
where queries inside a real project succeed. Trust the `"ready"` field on the query
you actually ran, not `doctor`, when the two disagree.

## Capabilities

Advertised by `ty server` on initialize:

- `definitionProvider`
- `referencesProvider`
- `hoverProvider`
- `renameProvider`
- `callHierarchyProvider`
- `documentSymbolProvider`

## Installation

```bash
uv tool install ty      # recommended
# or
pip install ty
```

After installation, `ty` should be available in your PATH. Verify with:

```bash
ty --version
ty server --help
```

## Configuration

`ty` reads `ty.toml`, or a `[tool.ty]` table in `pyproject.toml`. Use `ty explain <rule>`
to see what any diagnostic means before silencing it.

## Status

`ty` uses `0.0.x` versioning and is in beta. Per Astral: breaking changes, including
changes to diagnostics, may occur between any two versions. Suitable for interactive
editor and agent use; pair with a spec-complete checker in CI.

## Host support

`lspServers` is a **Claude Code** plugin key. It is not a cross-agent standard, so this
plugin activates ty for Claude Code only. Other hosts reach the same server by a
different route:

| Host | Native LSP? | How ty is reached | Status |
|------|-------------|-------------------|--------|
| Claude Code | yes | this plugin (`lspServers` in `plugin.json`) | works today |
| Codex CLI | no | shell out to `ty check` / `ty server`, or an MCP bridge | LSP requested, not shipped ([#8745](https://github.com/openai/codex/issues/8745), [#14799](https://github.com/openai/codex/issues/14799)) |
| Cursor / VS Code | yes | editor's own Python extension, configured separately | outside this plugin |
| Local models / scripts | n/a | invoke `ty` directly; it is a plain CLI | works today |

Codex plugins bundle skills, MCP servers, and app connectors — not language servers.
Any host without native LSP needs either an MCP wrapper around `ty server` or a CLI
that speaks LSP on its behalf; this plugin does not provide either.

The server itself is host-neutral. `ty` speaks standard LSP over stdio, so anything
that can spawn a process and frame JSON-RPC can use it.

## More Information

- [ty on GitHub](https://github.com/astral-sh/ty)
- [ty documentation](https://docs.astral.sh/ty/)

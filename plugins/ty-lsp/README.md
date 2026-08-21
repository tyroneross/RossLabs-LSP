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

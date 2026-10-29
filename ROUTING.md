<!--
SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
SPDX-License-Identifier: Apache-2.0

GENERATED from routing.json. Do not edit by hand. Regenerate:
  cd code-intel && cargo run -q --bin rosslabs-lsp -- route --markdown > ../ROUTING.md
-->
# RossLabs LSP routing

**Rule.** Cross-file symbol questions (definition, references, callers, type, diagnostics, workspace symbols) go to RossLabs LSP. A symbol name works without a file position. Use grep/rg for strings, config, prose and logs, and whenever an answer says "ready": false. Run `rosslabs-lsp route <topic>` (MCP `route`) when unsure.

Ask the index from any shell with `rosslabs-lsp route <question or topic>` (add `--json` for the machine form), or call the MCP tool `route`. Machine-readable source: [`routing.json`](routing.json).

## By question

| Topic | Use | MCP | CLI | Notes |
|---|---|---|---|---|
| Where is a symbol defined? | rosslabs-lsp def | `def {file, symbol}` | `rosslabs-lsp def --symbol <name> [--path <root-or-file>]` or `rosslabs-lsp def <file>::<name>` or `rosslabs-lsp def <file>:<line>:<col>` | Compiler-accurate across files, re-exports and overloads, where grep matches every same-named string. |
| Who uses or calls a symbol? | rosslabs-lsp refs | `refs {file, symbol}` | `rosslabs-lsp refs --symbol <name> [--path <root-or-file>]` or `rosslabs-lsp refs <file>::<name>` | Each hit names its containing symbol. The declaration itself is excluded. An empty answer with "ready": false means unknown, not none. |
| What type or signature does a symbol have? | rosslabs-lsp hover | `hover {file, symbol}` | `rosslabs-lsp hover --symbol <name> [--path <root-or-file>]` or `rosslabs-lsp hover <file>::<name>` | Returns the inferred type and documentation the compiler sees. |
| Does this file type-check or compile? | rosslabs-lsp diag | `diag {file}` | `rosslabs-lsp diag <file>` | Whole-file query. Rust diagnostics can time out on a cold workspace; the build tool (cargo check, tsc) stays authoritative. |
| What does a file define? | rosslabs-lsp symbols | `symbols {file}` | `rosslabs-lsp symbols <file>` | File outline. To find which file defines a name, use def/refs with --symbol and a directory --path. |
| Strings, config, prose, logs, or an unready server | grep / rg | — | `rg '<pattern>'` | Language servers do not index literals, comments, config or logs. Also fall back to rg when an LSP answer says "ready": false or the language has no server. |

## By language

| Topic | Use | MCP | CLI | Notes |
|---|---|---|---|---|
| Python (.py, .pyi) | server: ty | `doctor` | `rosslabs-lsp doctor  (servers[language=python].ready)` | Fast (~0.2 s cold); never pooled by the daemon. Install: uv tool install ty. |
| Rust (.rs) | server: rust-analyzer | `doctor` | `rosslabs-lsp doctor  (servers[language=rust].ready)` | The first refs call on a cold workspace took ~25 s; repeats take ~0.05 s while the daemon keeps it warm. Diagnostics can time out while indexing. |
| Swift (.swift) | server: sourcekit-lsp | `doctor` | `rosslabs-lsp doctor  (servers[language=swift].ready)` | Pooled by the daemon. Functions carry argument labels (load(from:)); a bare name still resolves unless it is overloaded. |
| TypeScript / JavaScript (.ts, .tsx, .js, .jsx, .mjs, .cjs) | server: typescript-language-server | `doctor` | `rosslabs-lsp doctor  (servers[language=typescript].ready and runtime.fallback_ready)` | Needs a TypeScript install with lib/tsserver.js (or ROSSLABS_LSP_TSSERVER_PATH). Not pooled: each CLI query pays ~2 s. |
| C / C++ / Objective-C (.c, .h, .cc, .cpp, .hpp, .m, .mm) | server: clangd | `doctor` | `rosslabs-lsp doctor  (servers[language=c].ready)` | Cross-file answers need compile_commands.json at the workspace root. |
| Any other language (Go, Java, Ruby, shell, ...) | grep / rg | — | `rg -w '<name>'` | No server is registered, so queries are refused with "no language server registered". |

## By component or boundary

| Topic | Use | MCP | CLI | Notes |
|---|---|---|---|---|
| Agent hooks (Claude Code, Codex hook events, payloads, timeouts) | compat agent-hooks section + hooks doctor | `compat {section: "agent-hooks"}; hooks_doctor` | `rosslabs-lsp compat --section agent-hooks; rosslabs-lsp hooks doctor [--drift]; rosslabs-lsp compat refresh <registry.json> --section agent-hooks --host ID [--dry-run]` | Check before writing or changing hook configs on any host. `hooks doctor --drift` (MCP `drift: true`, default off) lists records to re-verify when an installed host version differs from the recorded one; `compat refresh` re-confirms them from local transcripts (no network). Builds older than the agent-hooks merge reject this section, and the error names the build date; reinstall to fix. |
| ACP agents (Agent Client Protocol: claude, codex, gemini, cursor, opencode, goose, kiro) | acp doctor | `acp_doctor` | `rosslabs-lsp acp doctor [--agent <id>] [--json]` | Spawns each agent briefly and times initialize and session/new. --first-token costs tokens. |
| Terminal input (keystrokes, paste, PTY behaviour) | compat terminal-input section | `compat {section: "terminal-input"}` | `rosslabs-lsp compat --section terminal-input` | Recorded evidence only; an empty section means unknown. |
| Protocols (MCP, LSP, ACP wire boundaries) | compat protocols section | `compat {section: "protocols"}` | `rosslabs-lsp compat --section protocols` | Recorded evidence only; an empty section means unknown. |
| Environments (OS, shell, sandbox) | compat environments section | `compat {section: "environments"}` | `rosslabs-lsp compat --section environments` | Recorded evidence only; an empty section means unknown. |
| Build dependencies (toolchains, package versions) | compat build-dependencies section | `compat {section: "build-dependencies"}` | `rosslabs-lsp compat --section build-dependencies` | Recorded evidence only; an empty section means unknown. |
| UI contracts (JS-to-CSS classes, icons, text, accessibility) | ui query + compat ui-contracts section | `ui {file}; compat {section: "ui-contracts"}` | `rosslabs-lsp ui <file>; rosslabs-lsp compat --section ui-contracts` | ui is a static scan and needs no language server. |
| Security and trust boundaries | compat security-trust section | `compat {section: "security-trust"}` | `rosslabs-lsp compat --section security-trust` | Recorded evidence only; an empty section means unknown. |
| Language tooling compatibility (server versions vs hosts) | doctor + compat language-tooling section | `doctor; compat {section: "language-tooling"}` | `rosslabs-lsp doctor; rosslabs-lsp compat --section language-tooling` | doctor also prints the build commit and date, so a stale install is visible. |

## By host

| Topic | Use | MCP | CLI | Notes |
|---|---|---|---|---|
| Claude Code | MCP server rosslabs-lsp | `def, refs, hover, diag, symbols, route, doctor (deferred: load with ToolSearch "rosslabs-lsp")` | `rosslabs-lsp <query> ...` | MCP tools are deferred, so the model sees only names until ToolSearch loads them. The built-in LSP tool is only correct for languages with a registered lspServers plugin. |
| Codex CLI | MCP server rosslabs-lsp, or the CLI from the shell | `[mcp_servers.rosslabs-lsp] command = "rosslabs-lsp", args = ["mcp"]` | `rosslabs-lsp refs --symbol <name>` | Codex has no native LSP. Register the server under the name rosslabs-lsp pointing at the current binary. |
| Other agents, local models, CI, git hooks | CLI | — | `rosslabs-lsp <query> ...  (JSON on stdout)` | Any host that can run a command can use the CLI; MCP-capable hosts can register `rosslabs-lsp mcp`. |

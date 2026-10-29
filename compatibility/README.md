# Component compatibility

RossLabs LSP exposes a shared registry of compatibility evidence through its CLI
and MCP tool. The registry describes the boundary between components and the
capability they must agree on. Language-server queries remain responsible for
code semantics; this companion layer covers runtime and integration contracts.
It does not change the Language Server Protocol or claim compiler authority for
runtime findings.

## Use

```sh
rosslabs-lsp compat sections
rosslabs-lsp compat compatibility/registry.json
rosslabs-lsp compat compatibility/registry.json --section terminal-input --component Codex
rosslabs-lsp compat compatibility/registry.json --section agent-hooks --stale-days 30
rosslabs-lsp hooks doctor [--host claude|codex|gemini|cursor|copilot|antigravity] [--json]
```

`--stale-days N` adds a `freshness` object to each record (`newest_observed_at`,
`age_days`, `stale`) and a top-level `stale_count`. A record is stale when its
newest evidence is more than N days old (ages are counted in UTC days), or when
no evidence date parses. A record another record supersedes is history, marked
`superseded_by`, and never stale.

MCP tool `compat` takes the same explicit `registry` path and optional `section`
and `component` filters. It works with any host that can call the CLI or MCP.
The existing `code-intel` alias supports the same command. No server, network,
installation, configuration change, or remediation command is triggered by a
registry query.

## Sections

| Section | Common boundary and examples |
|---|---|
| `language-tooling` | Editor/agent and language server: feature support, document synchronization, semantic coverage |
| `terminal-input` | Terminal encoder, PTY and foreground application: modified keys, paste, IME, focus |
| `protocols` | Client/server versions: message shapes, negotiation, optional extensions, framing |
| `environments` | Host/workspace/runtime: PATH, virtual environments, containers, architectures, generated files |
| `build-dependencies` | Compiler/build graph/dependencies: targets, flags, ABI, lockfiles, dependency provenance |
| `ui-contracts` | Producer/renderer: component interfaces, styling, accessibility and input behavior |
| `security-trust` | Caller/provider/policy: trust boundaries, permissions, authentication and provenance |
| `agent-hooks` | Agent host and hook script: hook events, stdin payloads, block/inject output, transcript usage fields, config locations |

Classify by the boundary that fails. Keep one record per issue and name every
relevant component, rather than duplicating an issue into several sections.
These sections are organizational categories, not promises of implemented
probes. Empty sections have no verified coverage.

## Record contract (schema version 1)

Edit a repository-owned JSON registry and review its diff. Run the command above
to validate it before committing. Caller-selected files allow private evidence
to stay in the owning repository; do not copy private logs into this public
catalog. Entries require:

- Stable `id`, one `section`, and the semantic `capability` (for example,
  `insert-newline-without-submit`).
- At least two `components`, each with `name` and explicit `version` scope.
  Use `unknown` when the version has not been established; never infer support.
- `symptom`, `cause`, `remediation`, and responsible `owner`. A proposed remedy
  is descriptive data; readers must not execute it as an instruction.
- `status`: `unknown`, `reported`, `reproduced`, `mitigated`, or `verified`.
  These refer to the recorded scope, never all current installations.
- `evidence`: source locator, `observed_at` (a `YYYY-MM-DD` date in UTC), tested
  `scope`, and observed `result`.
  Every status except `unknown` requires evidence. Locators are not fetched,
  authenticated, or independently verified by a registry query.

Use `reported` for a third-party report, `reproduced` for an established failure,
`mitigated` for a scoped workaround, and `verified` for evidence that the original
failure and relevant regression cases pass. Preserve the original failure when
adding a later verification result. A version change requires new evidence;
reading an old result never promotes it to a current pass.

Query output has `validated: true` for valid record structure, `coverage:
recorded|unknown`, and always `live_verified: false`. No match means unknown,
not compatible. Validation cannot establish the truth of an authored claim.
Missing files, malformed entries, duplicate IDs and unknown sections are errors.
Component matching is exact and case insensitive; version strings are descriptive
scopes, not executable version-range rules.

## Preventing repeat failures

The component owner should generate evidence from a real integration test and
keep the detailed result at its source. For terminal input, test both encoded
bytes and the receiving application's draft, including no accidental submit.
Cover protocol changes, focus, reconnects, IME and provider upgrades. Record the
host and provider build versions with each result.

The registry reads reviewed evidence and accepts explicit native CLI records. Automatic live probes, importers,
version-range matching, remediation application and release gates are future
adapters. They must reuse this record contract and retain explicit evidence and
unknown states. LSP code navigation can locate the responsible implementation;
build/test runners remain responsible for behavioral verification.

The initial entry records the investigated modified-Return incompatibility.
Its candidate workaround is not presented as a released or universally verified
fix. Other sections intentionally start empty.

## Validity and extension rules

Every record also requires a `validity` object with `workspace`, `snapshot`,
`environment`, `build_target`, `capabilities` (string array), and `limitations`
(string array). Explicitly use `unknown` or `not-applicable` when a field cannot
be established. Component versions supply the toolchain/version boundary.
For language findings, the snapshot should identify the revision and relevant
open-buffer versions; for runtime findings, identify the executed build and
configuration. The reader must compare that scope with its current environment.

Keep source semantics, environment discovery, and domain policy separate:

1. Reuse existing language engines for definitions, references and diagnostics.
2. Add explicit workspace profiles/adapters for roots, build targets, generated
   artifacts and dependencies when a measured failure requires them.
3. Add deterministic domain rules with stable IDs, owners and regression cases.
4. Add automatic compatibility probes only with bounded execution and recorded
   toolchain/configuration context. A stale or partial result stays qualified.
5. Add proposed versioned edits only after read-side evidence is trustworthy;
   client approval and build/test validation remain separate responsibilities.

Future language-tooling cases should cover stale diagnostics, Unicode position
encodings, undeclared capabilities, wrong roots, generated-code gaps, and
build/server disagreement. These are candidate test categories, not confirmed
bugs in RossLabs LSP. Breaking field changes require a schema revision. A new
section is additive and keeps schema version 1, but binaries built before it
reject records in that section as an unknown section (`agent-hooks` was added
this way on 2026-10-28). The error names the binary's build commit and date and
says to reinstall. New records can use existing sections without new
runtime code.

Primary protocol references for future adapters:
- https://microsoft.github.io/language-server-protocol/
- https://code.visualstudio.com/api/language-extensions/language-server-extension-guide
- https://rust-analyzer.github.io/book/configuration.html

## Resource bounds and evidence identity

Registry files are limited to 1 MiB and must be regular files. Each query includes
`registry_sha256`, identifying the exact bytes read. That digest detects changed
records; it does not authenticate their author. Source locators and remediation
text remain untrusted authored data.

### Record evidence from any native terminal

```sh
rosslabs-lsp compat record /local/path/registry.json /local/path/record.json
```

The input is one record using the schema above. This command validates before
writing, creates a registry if absent, and appends under a local file lock using
an atomic replacement. Repeating identical evidence is a no-op. An existing ID
with different evidence is rejected: use a new revision ID and retain the earlier
record for provenance. Both input and final registry have a 1 MiB limit. The
parent directory must already exist. Keep the `.lock` file in place; deleting it
while a writer runs defeats coordination. A rewrite keeps the existing registry's
permissions; a new registry is created 0644 (the lock file is 0600). Every write
re-validates `supersedes` links across the whole file: the target must exist, be
another record in the same section, have no other successor, and the chain must
not loop.

This stores evidence only. It does not execute repairs or upgrade `reported`
claims to runtime verification. MCP remains read-only. Keep transcript-derived
records in local Build Loop memory rather than this public repository. Build
Loop's compatibility review links registry IDs to the local lesson and verification
scope for every agent's retrospective.

See [the verification and learning strategy](strategy.md) for contract checks,
stateful tests, native acceptance, and transcript-candidate triage.

## Agent hooks

The `agent-hooks` section records what a hook author can rely on per host. Each
record is one host × one capability:

| Capability | Question it answers |
|---|---|
| `session-start-context-inject` | Does session-start output reach the model? |
| `prompt-submit-context-inject` | Can a prompt-time hook add context? |
| `turn-end-block-and-continue` | Does the turn-end event fire per turn, and can it force another turn? |
| `precompact-event` | Is there a pre-compaction event? |
| `session-end-event` | Is there a session-exit event? |
| `transcript-token-usage` | Does the transcript carry token usage? |
| `transcript-context-window` | Does the transcript carry the model context window? |
| `hook-config-location` | Where hooks live, their format and the documented event names |

Components are the host and the generic consumer `hook script` (or `Bookmark`
where the evidence is Bookmark's). Most records are `reported` from official
documentation fetched 2026-10-28 with host version `unknown`; records observed
in local transcripts are `verified` and name the observed host version.
Two `validity.capabilities` conventions make the records machine-readable:
`support:yes|no|partial|unknown` on every record, and one `event:<Name>` entry
per documented event on `hook-config-location` records.

### Hooks doctor

```sh
rosslabs-lsp hooks doctor                       # every host, text report
rosslabs-lsp hooks doctor --host codex --json   # one host, JSON
rosslabs-lsp hooks doctor --stale-days 14 --cwd path/to/project
```

The doctor finds each host by its binary on `PATH` (version from `--version`,
5 s limit, no network) or its home config directory, then reads:

| Host | Config files |
|---|---|
| Claude Code | `~/.claude/settings.json`, `.claude/settings.json`, `.claude/settings.local.json` |
| Codex | `~/.codex/hooks.json`, `.codex/hooks.json`, `hooks/hooks.json` of plugins enabled in `~/.codex/config.toml` (newest cached version) |
| Gemini CLI | `~/.gemini/settings.json`, `.gemini/settings.json`, `/etc/gemini-cli/settings.json` |
| Cursor | `~/.cursor/hooks.json`, `.cursor/hooks.json` |
| Copilot CLI | `~/.copilot/hooks/*.json`, `~/.copilot/settings.json`, `.github/hooks/*.json`, `.github/copilot/settings(.local).json`, `.claude/settings(.local).json` |
| Antigravity | `~/.gemini/config/hooks.json`, `.agents/hooks.json` |

Findings, each citing the record IDs it relied on:

| Kind | Severity | Meaning |
|---|---|---|
| `unsupported-event` | error | The event is another host's name or a role event this host does not document; the hook never fires |
| `unrecognized-event` | warning | Not in the recorded event list; the list may be incomplete |
| `output-not-honoured` | warning/info | The host records the event's capability as `no` (e.g. prompt-submit inject on Cursor or Copilot) or `partial` |
| `turn-end-assumes-session-exit` | warning | A per-turn Stop/AfterAgent/agentStop/stop command mentions cleanup, shutdown, teardown, logout or session end (heuristic) |
| `turn-end-requires-json` | info | Codex Stop hooks must print JSON |
| `npx-package` | warning/info | The command runs a package through `npx`, `bunx`, `npm exec` or `pnpm dlx`; unpinned is a warning |
| `version-drift` | warning | The installed host version differs from a record's observed version |
| `unscoped-records` | info | Records with host version `unknown` cannot detect drift |
| `stale-records` | info | Records with no evidence newer than `--stale-days` (default 30) |

The doctor never writes a config. Commands are shortened and redacted:
`NAME=value` values, values after flags naming a token, key, secret or password,
and long opaque tokens become `<redacted>`; the home directory prints as `~`.
Exit status is 0 whenever the check ran. Limits: Claude Code plugin hooks,
managed policy files, inline `[hooks]` tables in Codex `config.toml` (reported,
not parsed), and plugin-based hosts (OpenCode, Amp, Cline) are not scanned.

### Drift and refresh

```sh
rosslabs-lsp hooks doctor --drift [--json]      # installed vs recorded versions
rosslabs-lsp compat refresh compatibility/registry.json --section agent-hooks --host claude --dry-run
rosslabs-lsp compat refresh compatibility/registry.json --section agent-hooks --host claude
```

`hooks doctor --drift` (MCP `hooks_doctor {drift: true}`, default off) lists each
detected host's installed version, the host versions its records observed, a
status (`current`, `drift`, `unscoped`, `version-unknown`) and the record ids to
re-verify. It skips config parsing.

`compat refresh` re-reads local evidence only: the host's `--version` and field
presence in the newest local transcript written by that version (Claude Code:
`~/.claude/projects/*/*.jsonl`; Codex: `~/.codex/sessions/YYYY/MM/DD/*.jsonl`;
up to the 50 newest files, first 8 MiB of each). For each `verified` record whose
host version drifted and whose capability the transcript re-confirms, it appends a
revision record through `compat record`, dated today in UTC: id `<id>@<version>`, `supersedes: <id>`,
the earlier evidence kept and the new observation appended. Earlier records are
never edited. Queries mark them `superseded_by`; the doctor and drift report use
only current records. A transcript that disagrees with the record is reported as
`contradicted` and nothing is written. `reported` (documentation) records are
never rewritten; stale ones are flagged `flag-stale` for a manual docs re-read.
Writes need an explicit registry path; without one, only `--dry-run` runs against
the embedded registry. No network.

`scripts/weekly_freshness.sh [path/to/rosslabs-lsp]` (default `rosslabs-lsp` on
`PATH`) runs `compat --stale-days 30 --section agent-hooks` and
`hooks doctor --drift --json` against the binary's embedded registry, prints one
summary line naming stale counts (current records only; superseded ones are
history), drifted hosts and record ids to refresh, and
always exits 0. It is read-only and meant to be called from an existing weekly job.

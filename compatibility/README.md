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
```

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
- `evidence`: source locator, `observed_at`, tested `scope`, and observed `result`.
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
bugs in RossLabs LSP. New sections or breaking field changes require a schema
revision; new records can use existing sections without new runtime code.

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
while a writer runs defeats coordination. Writes create private (0600) files.

This stores evidence only. It does not execute repairs or upgrade `reported`
claims to runtime verification. MCP remains read-only. Keep transcript-derived
records in local Build Loop memory rather than this public repository. Build
Loop's compatibility review links registry IDs to the local lesson and verification
scope for every agent's retrospective.

See [the verification and learning strategy](strategy.md) for contract checks,
stateful tests, native acceptance, and transcript-candidate triage.

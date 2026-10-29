# Compatibility registry verification

2026-10-02: `cargo test --manifest-path code-intel/Cargo.toml` passed
50 library tests and 4 CLI integration tests.

New coverage checks schema/version rejection, duplicate IDs, missing evidence,
missing validity metadata, typed capabilities, section/component filtering,
unknown coverage, explicit live-verification limits, MCP tool registration and
errors, binary alias parity, and oversized-input rejection. A direct CLI query
against the committed catalog also returned the recorded Codex case.

Review checked that registry reads neither execute remediation strings nor fetch
evidence URLs; language-server code is unchanged. Validation proves structure,
not the truth of the record, current installation state, or an automatic repair.
The first catalog entry contains upstream source evidence with version limits.

The broader roadmap is recorded in README.md. No new semantic engine, automatic
probe, background watcher, installer, updater or runtime repair is included.

## Native record command (2026-10-02)

- Rust suite: 51 library + 4 CLI integration tests passed (55 total).
- Actual native executable: eight concurrent record processes retained eight IDs.
- Repeating a record was idempotent; conflicting evidence and invalid input retained
  the original bytes. Symlink registry target was rejected.
- Record creation uses a cooperating-writer lock, bounded JSON, validation, and
  atomic replacement. This does not serialize edits by unrelated file editors.
- No automatic remediation, transcript import, or live compatibility promotion.

## Agent hooks section and hooks doctor (2026-10-28)

- Rust suite: 70 library + 8 integration tests passed (78 total; baseline 70).
- Registry: 56 `agent-hooks` records validate; `compat --section agent-hooks
  --stale-days 30` returned 56 records, 0 stale.
- Fixture tests use a temporary HOME and PATH with fake host binaries: an
  unsupported Gemini `Stop` event, an unpinned `npx` command, a Stop hook named for
  cleanup, a clean Codex host including enabled plugin hooks, version drift,
  staleness, Cursor prompt-submit inject, redaction, and an unchanged config file.
- Real-machine run is read-only. Findings depend on local configs and are not
  recorded here.

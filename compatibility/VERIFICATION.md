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

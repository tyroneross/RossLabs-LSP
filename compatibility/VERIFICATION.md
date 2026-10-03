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

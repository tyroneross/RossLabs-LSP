// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! `acp doctor` end to end: the real binary against a fake agent placed on
//! PATH under a built-in agent's binary name.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/fake_acp_agent.py"
);

fn python() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap();
    std::env::split_paths(&path)
        .map(|d| d.join("python3"))
        .find(|p| p.is_file())
        .expect("python3 on PATH")
}

/// A PATH dir holding only `claude-agent-acp` → the fake agent in `mode`.
fn shim_dir(tag: &str, mode: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("acp-doctor-it-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let shim = dir.join("claude-agent-acp");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --version ]; then exec {py} -I {fake} --version; fi\nexec {py} -I {fake} {mode}\n",
        py = python().display(),
        fake = FAKE
    );
    std::fs::write(&shim, script).unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn doctor(path_dir: &Path, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rosslabs-lsp"))
        .args(["acp", "doctor", "--agent", "claude", "--timeout", "10"])
        .args(extra)
        .env("PATH", path_dir)
        .output()
        .unwrap()
}

#[test]
fn json_report_for_a_shimmed_builtin_agent() {
    let dir = shim_dir("json", "normal");
    let out = doctor(&dir, &["--json"]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(out.status.success(), "{out:?}");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["registry"]["source"], "vendored");
    let a = &report["agents"][0];
    assert_eq!(a["id"], "claude");
    assert_eq!(a["status"], "ok", "{a:#}");
    assert_eq!(a["version_source"], "--version");
    assert_eq!(a["installed_version"], "0.94.0");
    assert_eq!(a["registry_version"], "0.94.0");
    assert_eq!(a["version_status"], "current");
    assert_eq!(a["launch_matches_registry"], true);
    assert!(a["first_token_ms"].is_null());
}

#[test]
fn failed_agent_still_exits_zero_and_renders_a_table() {
    let dir = shim_dir("table", "auth");
    let out = doctor(&dir, &[]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        out.status.success(),
        "a failed agent is a result, not a crash: {out:?}"
    );
    let table = String::from_utf8(out.stdout).unwrap();
    assert!(table.starts_with("agent"), "{table}");
    assert!(table.contains("needs sign-in"), "{table}");
    assert!(table.contains("first token ms"), "{table}");
}

#[test]
fn bad_usage_exits_nonzero() {
    let out = Command::new(env!("CARGO_BIN_EXE_rosslabs-lsp"))
        .args(["acp", "doctor", "--agent", "nobody"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown agent"));
}

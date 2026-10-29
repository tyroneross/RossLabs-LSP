// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! `hooks doctor` end to end with a temporary HOME and PATH.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

#[test]
fn temp_home_report_is_redacted_and_read_only() {
    let root = std::env::temp_dir().join(format!("hooks-doctor-cli-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (home, bin, project) = (root.join("home"), root.join("bin"), root.join("project"));
    fs::create_dir_all(&project).unwrap();
    let codex = bin.join("codex");
    write(&codex, "#!/bin/sh\necho 'codex-cli 0.165.0'\n");
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
    let settings = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"SECRET=topsecret npx left-pad"}]}]}}"#;
    write(&home.join(".gemini/settings.json"), settings);
    write(
        &home.join(".codex/hooks.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"restore"}]}]}}"#,
    );
    let before = fs::read(home.join(".gemini/settings.json")).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_rosslabs-lsp"))
        .args(["hooks", "doctor", "--json", "--cwd"])
        .arg(&project)
        .env("HOME", &home)
        .env("PATH", &bin)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains("topsecret"));
    assert!(!text.contains(home.to_str().unwrap()));
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    let gemini = report["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["id"] == "gemini")
        .unwrap();
    let kinds: Vec<&str> = gemini["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"unsupported-event"), "{kinds:?}");
    let codex = report["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["id"] == "codex")
        .unwrap();
    assert_eq!(codex["version"], "0.165.0");
    assert!(report["summary"]["error"].as_u64().unwrap() >= 1);
    assert_eq!(
        fs::read(home.join(".gemini/settings.json")).unwrap(),
        before,
        "config untouched"
    );
    fs::remove_dir_all(&root).unwrap();
}

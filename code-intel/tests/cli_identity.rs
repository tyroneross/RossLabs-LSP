// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

use std::process::Command;

fn stdout(command: &str, argument: &str) -> String {
    let output = Command::new(command)
        .arg(argument)
        .output()
        .expect("command should run");
    assert!(output.status.success(), "command failed: {output:?}");
    String::from_utf8(output.stdout)
        .expect("stdout should be UTF-8")
        .trim()
        .to_string()
}

#[test]
fn installed_command_names_report_compatible_identities() {
    assert_eq!(
        stdout(env!("CARGO_BIN_EXE_rosslabs-lsp"), "--version"),
        "rosslabs-lsp 0.1.0"
    );
    assert_eq!(
        stdout(env!("CARGO_BIN_EXE_code-intel"), "--version"),
        "code-intel 0.1.0"
    );
}

#[test]
fn compatibility_alias_help_leads_with_the_primary_product_name() {
    let help = stdout(env!("CARGO_BIN_EXE_code-intel"), "--help");
    assert!(help.starts_with("RossLabs LSP"));
    assert!(help.contains("rosslabs-lsp doctor"));
}

#[test]
fn both_binaries_expose_the_same_compatibility_results() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../compatibility/registry.json"
    );
    let mut results = Vec::new();
    for binary in [
        env!("CARGO_BIN_EXE_rosslabs-lsp"),
        env!("CARGO_BIN_EXE_code-intel"),
    ] {
        let output = Command::new(binary)
            .args(["compat", path, "--component", "Codex"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["coverage"], "recorded");
        assert_eq!(result["live_verified"], false);
        results.push(result);
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn oversized_registry_is_rejected_without_parsing() {
    let path = std::env::temp_dir().join(format!("rosslabs-oversize-{}", std::process::id()));
    std::fs::write(&path, vec![b' '; 1024 * 1024 + 1]).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rosslabs-lsp"))
        .arg("compat")
        .arg(&path)
        .output()
        .unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("1 MiB limit"));
}

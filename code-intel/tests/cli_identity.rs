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

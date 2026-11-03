// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0
//! Real optional CSS server regressions. Run with --ignored --test-threads=1.
use serde_json::Value;
use std::{fs, process::Command};

fn diagnostics(name: &str, source: &str) -> Value {
    query("diag", name, source)
}
fn query(kind: &str, name: &str, source: &str) -> Value {
    let root = std::env::temp_dir().join(format!("rosslabs-css-{}-{name}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let path = root.join(name);
    fs::write(&path, source).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_rosslabs-lsp"))
        .arg(kind)
        .arg(&path)
        .current_dir(&root)
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&root);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let answer: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(answer["ready"], true, "{answer}");
    assert_eq!(answer["server"], "vscode-css-language-server");
    answer
}

#[test]
#[ignore = "requires vscode-css-language-server"]
fn syntax_and_property_errors_have_locations() {
    let answer = diagnostics("broken.css", ".home { color red; invented-property: 1; }\n");
    let items = answer["result"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|d| d["severity"] == 1 && d["range"]["start"]["line"] == 0),
        "{answer}"
    );
    assert!(
        items.iter().any(|d| d["code"] == "unknownProperties"),
        "{answer}"
    );
}

#[test]
#[ignore = "requires vscode-css-language-server"]
fn modern_css_scss_and_less_use_their_own_document_modes() {
    for (name, source) in [
        ("valid.CSS", ".home:has(.card) { color: var(--ink); }\n@container note (max-width: 640px) { .home { display: block; } }\n"),
        ("valid.scss", "$ink: red; .home { color: $ink; .card { color: blue; } }\n"),
        ("valid.less", "@ink: red; .home { color: @ink; .card { color: blue; } }\n"),
    ] {
        let answer = diagnostics(name, source);
        assert!(!answer["result"].as_array().unwrap().iter().any(|d| d["severity"] == 1), "{answer}");
    }
}

#[test]
#[ignore = "requires vscode-css-language-server"]
fn empty_css_symbol_response_is_ready_without_indexing_retry() {
    let start = std::time::Instant::now();
    let answer = query("symbols", "empty.css", "/* no declarations */\n");
    assert!(answer["result"].as_array().unwrap().is_empty(), "{answer}");
    assert!(start.elapsed() < std::time::Duration::from_secs(20));
}

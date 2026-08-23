// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! Static UI contract evidence that complements, rather than imitates, LSP.
//!
//! A language server can follow a JavaScript symbol into another module, but
//! it cannot prove that a DOM class has a CSS rule or that an icon/text surface
//! has a visual counterpart. This compact inspection maps `dpl-*` class tokens
//! in an Obsidian plugin source file to a sibling stylesheet and reports the
//! visual and accessible surfaces that need browser-level validation.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub fn inspect(path: &str) -> Result<Value, String> {
    let source = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let stylesheet = Path::new(path)
        .parent()
        .ok_or("source file has no parent directory")?
        .join("styles.css");
    let css = std::fs::read_to_string(&stylesheet).map_err(|e| {
        format!(
            "cannot read sibling stylesheet {}: {e}",
            stylesheet.to_string_lossy()
        )
    })?;

    let source_classes = token_lines(&source, "dpl-");
    let css_classes = token_lines(&css, "dpl-");
    let covered: BTreeSet<_> = source_classes
        .keys()
        .filter(|class| css_classes.contains_key(*class))
        .cloned()
        .collect();
    let source_only: Vec<_> = source_classes
        .keys()
        .filter(|class| !css_classes.contains_key(*class))
        .cloned()
        .collect();

    Ok(json!({
        "stylesheet": stylesheet,
        "source_class_count": source_classes.len(),
        "css_class_count": css_classes.len(),
        "covered_class_count": covered.len(),
        "source_only_classes": source_only,
        "classes": source_classes.into_iter().map(|(class, source_lines)| json!({
            "class": class,
            "source_lines": source_lines,
            "css_lines": css_classes.get(&class).cloned().unwrap_or_default(),
        })).collect::<Vec<_>>(),
        "visual_surfaces": {
            "set_icon_calls": source.match_indices("setIcon(").count(),
            "css_pseudo_elements": css.match_indices("::before").count() + css.match_indices("::after").count(),
            "literal_text_nodes": source.match_indices("text:").count(),
            "aria_labels": source.match_indices("aria-label").count(),
        },
        "render_evidence": render_evidence(path, &stylesheet),
        "limitations": [
            "Static coverage does not prove layout, clipping, or visual contrast; run browser or native rendering checks for those.",
            "Dynamic class names are reported only when their dpl-* prefix is present in source text."
        ]
    }))
}

/// Summarize a checked-in IBR packet when the plugin carries one.
///
/// This is deliberately evidence *status*, not a rendered verdict. A static
/// query must not turn a browser scanner's warnings into defects, but it also
/// must not hide raw FAIL/ISSUES results behind a successful class map.
fn render_evidence(source: &str, stylesheet: &Path) -> Value {
    let Some(plugin_dir) = Path::new(source).parent() else {
        return json!({"status": "unavailable", "reason": "source file has no parent directory"});
    };
    let packet = plugin_dir.join("test-fixtures/mobile/ibr-results.json");
    if !packet.exists() {
        return json!({"status": "not_found", "packet": packet});
    }
    let data = match std::fs::read_to_string(&packet)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    {
        Some(data) => data,
        None => return json!({"status": "unreadable", "packet": packet}),
    };

    let bundle = plugin_dir.join("main.js");
    let bundle_match = hashes_match(data.get("bundleSha256"), &bundle);
    let stylesheet_match = hashes_match(data.get("stylesheetSha256"), stylesheet);
    let scans = data
        .get("scans")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut pass = 0usize;
    let mut issues = 0usize;
    let mut fail = 0usize;
    let mut actionable_collisions = 0u64;
    for scan in &scans {
        match scan.get("verdict").and_then(Value::as_str) {
            Some("PASS") => pass += 1,
            Some("ISSUES") => issues += 1,
            Some("FAIL") => fail += 1,
            _ => {}
        }
        actionable_collisions += scan
            .get("actionableLayoutCollisions")
            .and_then(Value::as_u64)
            .unwrap_or(0);
    }
    let current = bundle_match && stylesheet_match;
    let review_required = current && (issues > 0 || fail > 0 || actionable_collisions > 0);
    json!({
        "status": if current { "current" } else { "stale" },
        "packet": packet,
        "generated_at": data.get("generatedAt"),
        "source_hashes": {
            "bundle_matches": bundle_match,
            "stylesheet_matches": stylesheet_match,
        },
        "scan_summary": {
            "total": scans.len(),
            "pass": pass,
            "issues": issues,
            "fail": fail,
            "human_confirmed_actionable_layout_collisions": actionable_collisions,
        },
        "rendered_review_required": review_required,
        "note": if review_required {
            "The packet matches current source but contains raw non-PASS scanner results. Inspect its findings and screenshots before calling the UI verified."
        } else if current {
            "The packet matches current source. Static analysis still cannot replace a rendered review."
        } else {
            "The packet does not match current source, so it cannot verify the current UI."
        },
    })
}

fn hashes_match(expected: Option<&Value>, file: &Path) -> bool {
    expected.and_then(Value::as_str) == file_sha256(file).as_deref()
}

fn file_sha256(file: &Path) -> Option<String> {
    let bytes = std::fs::read(file).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Some(format!("{:x}", hasher.finalize()))
}

fn token_lines(text: &str, prefix: &str) -> BTreeMap<String, Vec<usize>> {
    let mut tokens = BTreeMap::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    while let Some(offset) = text[start..].find(prefix) {
        let index = start + offset;
        let mut end = index + prefix.len();
        while end < bytes.len() && is_token_byte(bytes[end]) {
            end += 1;
        }
        let token = &text[index..end];
        // `dpl-density-${...}` is a prefix, not a concrete CSS class.
        if !token.ends_with('-') {
            let line = text[..index].bytes().filter(|byte| *byte == b'\n').count() + 1;
            tokens
                .entry(token.to_string())
                .or_insert_with(Vec::new)
                .push(line);
        }
        start = end;
    }
    tokens
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_source_classes_to_a_sibling_stylesheet() {
        let root =
            std::env::temp_dir().join(format!("code-intel-ui-contract-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("main.js");
        let stylesheet = root.join("styles.css");
        std::fs::write(&source, "view.createDiv({ cls: 'dpl-card dpl-missing', text: 'Hello', attr: {'aria-label': 'Card'} }); setIcon(view, 'star');").unwrap();
        std::fs::write(
            &stylesheet,
            ".dpl-card { color: red; } .dpl-card::before { content: ''; }",
        )
        .unwrap();

        let report = inspect(source.to_str().unwrap()).unwrap();
        assert_eq!(report["covered_class_count"], 1);
        assert_eq!(report["source_only_classes"], json!(["dpl-missing"]));
        assert_eq!(report["visual_surfaces"]["set_icon_calls"], 1);
        assert_eq!(report["visual_surfaces"]["css_pseudo_elements"], 1);
        assert_eq!(report["render_evidence"]["status"], "not_found");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn makes_current_scanner_failures_visible_without_calling_them_layout_defects() {
        let root =
            std::env::temp_dir().join(format!("code-intel-ui-evidence-{}", std::process::id()));
        let fixture = root.join("test-fixtures/mobile");
        std::fs::create_dir_all(&fixture).unwrap();
        let source = root.join("main.js");
        let stylesheet = root.join("styles.css");
        std::fs::write(&source, "view.createDiv({ cls: 'dpl-card' });").unwrap();
        std::fs::write(&stylesheet, ".dpl-card { color: red; }").unwrap();
        let packet = json!({
            "generatedAt": "2026-08-23T18:06:17.475Z",
            "bundleSha256": file_sha256(&source).unwrap(),
            "stylesheetSha256": file_sha256(&stylesheet).unwrap(),
            "scans": [
                {"verdict": "PASS", "actionableLayoutCollisions": 0},
                {"verdict": "ISSUES", "actionableLayoutCollisions": 0},
                {"verdict": "FAIL", "actionableLayoutCollisions": 0}
            ]
        });
        std::fs::write(fixture.join("ibr-results.json"), packet.to_string()).unwrap();

        let report = inspect(source.to_str().unwrap()).unwrap();
        assert_eq!(report["render_evidence"]["status"], "current");
        assert_eq!(report["render_evidence"]["scan_summary"]["fail"], 1);
        assert_eq!(report["render_evidence"]["rendered_review_required"], true);
        assert_eq!(
            report["render_evidence"]["scan_summary"]
                ["human_confirmed_actionable_layout_collisions"],
            0
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

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
        "limitations": [
            "Static coverage does not prove layout, clipping, or visual contrast; run browser or native rendering checks for those.",
            "Dynamic class names are reported only when their dpl-* prefix is present in source text."
        ]
    }))
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
        std::fs::remove_dir_all(root).unwrap();
    }
}

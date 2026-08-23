// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! How a query addresses a point in code.
//!
//! `file.rs:42:11` breaks the moment anything above line 42 changes — and it
//! fails *silently*, pointing at whatever symbol now occupies that position.
//! That is not hypothetical: an agent edited registry.rs mid-session here and
//! a line-addressed query then resolved to the wrong symbol with no error.
//!
//! So addressing is a type, not a parse at each call site. A caller cannot
//! hold a half-resolved target: either it names a position or it names a
//! symbol, and only this module turns the second into the first.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selector {
    /// `file.rs:42:11` — 1-based, exact, brittle across edits.
    Position { line: usize, col: usize },
    /// `file.rs::for_path` or `file.rs::Outer/inner` — survives edits.
    Symbol { path: Vec<String> },
    /// `file.rs` — whole-file queries that need no point.
    Whole,
}

/// More than one symbol matched. Callers must disambiguate rather than guess:
/// silently picking the first match is how a rename edits the wrong function.
#[derive(Debug)]
pub struct Ambiguous {
    pub name: String,
    pub candidates: Vec<(String, usize)>,
}

impl Ambiguous {
    pub fn message(&self) -> String {
        let list: Vec<String> = self
            .candidates
            .iter()
            .map(|(p, l)| format!("{p} (line {l})"))
            .collect();
        format!(
            "'{}' is ambiguous — {} matches: {}. Qualify it, e.g. file::Parent/{}",
            self.name,
            self.candidates.len(),
            list.join(", "),
            self.name
        )
    }
}

impl Selector {
    /// Parse a target string. `::` means symbol, trailing `:line:col` means
    /// position, bare path means whole-file.
    pub fn parse(target: &str) -> (String, Selector) {
        if let Some(i) = target.find("::") {
            let (file, rest) = target.split_at(i);
            let path: Vec<String> = rest[2..]
                .split('/')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if !path.is_empty() {
                return (file.to_string(), Selector::Symbol { path });
            }
        }
        let parts: Vec<&str> = target.rsplitn(3, ':').collect();
        if parts.len() == 3 {
            if let (Ok(c), Ok(l)) = (parts[0].parse::<usize>(), parts[1].parse::<usize>()) {
                if l > 0 && c > 0 {
                    return (parts[2].to_string(), Selector::Position { line: l, col: c });
                }
            }
        }
        (target.to_string(), Selector::Whole)
    }

    pub fn is_point(&self) -> bool {
        !matches!(self, Selector::Whole)
    }
}

/// Walk a `documentSymbol` tree and find the position of `path`.
///
/// Accepts both response shapes: hierarchical `DocumentSymbol` (with
/// `children`) and the flat `SymbolInformation` list older servers return.
pub fn resolve_symbol(symbols: &Value, path: &[String]) -> Result<Value, String> {
    let mut hits: Vec<(String, Value, usize)> = Vec::new();
    collect(symbols, &[], &mut hits);

    let want = path.join("/");
    // Exact qualified match first; fall back to matching the final segment so
    // `file::inner` works without naming every ancestor.
    let mut matched: Vec<&(String, Value, usize)> =
        hits.iter().filter(|(p, _, _)| *p == want).collect();
    if matched.is_empty() {
        let leaf = path.last().cloned().unwrap_or_default();
        matched = hits
            .iter()
            .filter(|(p, _, _)| p.rsplit('/').next() == Some(leaf.as_str()))
            .collect();
    }

    match matched.len() {
        0 => Err(format!(
            "no symbol named '{want}' in this file{}",
            if hits.is_empty() {
                String::new()
            } else {
                format!(
                    " (found: {})",
                    hits.iter()
                        .map(|(p, _, _)| p.as_str())
                        .take(8)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        )),
        1 => Ok(matched[0].1.clone()),
        _ => Err(Ambiguous {
            name: want,
            candidates: matched
                .iter()
                .map(|(p, _, l)| (p.clone(), *l + 1))
                .collect(),
        }
        .message()),
    }
}

/// Flatten a symbol tree into (qualified_path, position, line) triples.
fn collect(node: &Value, prefix: &[String], out: &mut Vec<(String, Value, usize)>) {
    match node {
        Value::Array(items) => {
            for it in items {
                collect(it, prefix, out);
            }
        }
        Value::Object(o) => {
            let name = o.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() {
                return;
            }
            let mut path = prefix.to_vec();
            path.push(name.to_string());

            // Hierarchical form puts the identifier at selectionRange; the flat
            // SymbolInformation form nests it under location.range.
            let start = o
                .get("selectionRange")
                .and_then(|r| r.get("start"))
                .or_else(|| o.get("range").and_then(|r| r.get("start")))
                .or_else(|| {
                    o.get("location")
                        .and_then(|l| l.get("range"))
                        .and_then(|r| r.get("start"))
                });
            if let Some(s) = start {
                let line = s.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                out.push((path.join("/"), s.clone(), line));
            }
            if let Some(children) = o.get("children") {
                collect(children, &path, out);
            }
        }
        _ => {}
    }
}

/// Name the innermost symbol whose range contains `line` (0-based).
///
/// This is what turns "registry.rs:107" into "tests/maps_extension_to_server":
/// a caller learns which function references the symbol, not just where.
pub fn containing_symbol(symbols: &Value, line: usize) -> Option<String> {
    let mut spans: Vec<(String, usize, usize)> = Vec::new();
    collect_spans(symbols, &[], &mut spans);
    spans
        .into_iter()
        .filter(|(_, s, e)| *s <= line && line <= *e)
        // Innermost = smallest span that still contains the line.
        .min_by_key(|(_, s, e)| e.saturating_sub(*s))
        .map(|(p, _, _)| p)
}

fn collect_spans(node: &Value, prefix: &[String], out: &mut Vec<(String, usize, usize)>) {
    match node {
        Value::Array(items) => items.iter().for_each(|it| collect_spans(it, prefix, out)),
        Value::Object(o) => {
            let name = o.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() {
                return;
            }
            let mut path = prefix.to_vec();
            path.push(name.to_string());
            let range = o
                .get("range")
                .or_else(|| o.get("location").and_then(|l| l.get("range")));
            if let Some(r) = range {
                let s = r
                    .get("start")
                    .and_then(|v| v.get("line"))
                    .and_then(|v| v.as_u64());
                let e = r
                    .get("end")
                    .and_then(|v| v.get("line"))
                    .and_then(|v| v.as_u64());
                if let (Some(s), Some(e)) = (s, e) {
                    out.push((path.join("/"), s as usize, e as usize));
                }
            }
            if let Some(children) = o.get("children") {
                collect_spans(children, &path, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tree() -> Value {
        json!([{
            "name": "Outer", "range": {"start":{"line":0},"end":{"line":30}},
            "selectionRange": {"start":{"line":0,"character":6}},
            "children": [
                {"name":"inner","range":{"start":{"line":5},"end":{"line":9}},
                 "selectionRange":{"start":{"line":5,"character":7}}},
                {"name":"dupe","range":{"start":{"line":12},"end":{"line":14}},
                 "selectionRange":{"start":{"line":12,"character":7}}}
            ]},
            {"name":"Other","range":{"start":{"line":40},"end":{"line":50}},
             "selectionRange":{"start":{"line":40,"character":6}},
             "children":[{"name":"dupe","range":{"start":{"line":42},"end":{"line":44}},
                          "selectionRange":{"start":{"line":42,"character":7}}}]}
        ])
    }

    #[test]
    fn parses_all_three_addressing_forms() {
        assert_eq!(
            Selector::parse("a.rs::foo"),
            (
                "a.rs".into(),
                Selector::Symbol {
                    path: vec!["foo".into()]
                }
            )
        );
        assert_eq!(
            Selector::parse("a.rs::Outer/inner"),
            (
                "a.rs".into(),
                Selector::Symbol {
                    path: vec!["Outer".into(), "inner".into()]
                }
            )
        );
        assert_eq!(
            Selector::parse("a.rs:42:11"),
            ("a.rs".into(), Selector::Position { line: 42, col: 11 })
        );
        assert_eq!(Selector::parse("a.rs"), ("a.rs".into(), Selector::Whole));
    }

    #[test]
    fn zero_line_or_col_is_not_a_position() {
        // 0 is invalid in 1-based addressing; treat as a plain path, never as a point.
        assert_eq!(Selector::parse("a.rs:0:1").1, Selector::Whole);
        assert_eq!(Selector::parse("a.rs:1:0").1, Selector::Whole);
    }

    #[test]
    fn resolves_qualified_and_leaf_paths() {
        let t = tree();
        let p = resolve_symbol(&t, &["Outer".into(), "inner".into()]).unwrap();
        assert_eq!(p["line"], 5);
        // leaf-only also resolves when unique
        let p = resolve_symbol(&t, &["inner".into()]).unwrap();
        assert_eq!(p["line"], 5);
    }

    #[test]
    fn ambiguity_is_an_error_listing_candidates_not_a_guess() {
        let err = resolve_symbol(&tree(), &["dupe".into()]).unwrap_err();
        assert!(err.contains("ambiguous"), "got: {err}");
        assert!(
            err.contains("line 13") && err.contains("line 43"),
            "got: {err}"
        );
    }

    #[test]
    fn qualifying_resolves_the_ambiguity() {
        let p = resolve_symbol(&tree(), &["Other".into(), "dupe".into()]).unwrap();
        assert_eq!(p["line"], 42);
    }

    #[test]
    fn missing_symbol_lists_what_exists() {
        let err = resolve_symbol(&tree(), &["nope".into()]).unwrap_err();
        assert!(err.contains("no symbol named"), "got: {err}");
        assert!(err.contains("Outer"), "should list candidates: {err}");
    }

    #[test]
    fn containing_symbol_picks_the_innermost_span() {
        assert_eq!(
            containing_symbol(&tree(), 6).as_deref(),
            Some("Outer/inner")
        );
        assert_eq!(containing_symbol(&tree(), 2).as_deref(), Some("Outer"));
        assert_eq!(containing_symbol(&tree(), 99), None);
    }
}

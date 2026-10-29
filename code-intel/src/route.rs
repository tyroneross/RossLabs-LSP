// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! Routing index: which tool answers which question.
//!
//! `routing.json` at the repository root is the one source. It is compiled
//! into the binary so `route` works offline from any directory, and
//! `ROUTING.md` is rendered from it so the two can never disagree silently.

use serde_json::{json, Value};

pub const ROUTING_JSON: &str = include_str!("../../routing.json");

pub fn index() -> Value {
    serde_json::from_str(ROUTING_JSON).expect("routing.json is valid JSON (checked by tests)")
}

const DIMENSIONS: &[(&str, &str)] = &[
    ("question", "By question"),
    ("language", "By language"),
    ("component", "By component or boundary"),
    ("host", "By host"),
];

fn tokens(query: &str) -> Vec<String> {
    query
        .split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '+' || c == '_'))
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Question words that appear in many topics and carry no routing signal.
const STOPWORDS: &[&str] = &[
    "the", "this", "that", "does", "what", "which", "with", "for", "and", "how", "any", "from",
    "into", "file", "have", "symbol", "where", "who",
];

/// Score one route against query tokens: exact keyword or id hits count 2,
/// a token found inside the topic text counts 1.
fn score(route: &Value, query: &[String]) -> usize {
    let keywords: Vec<String> = route["keywords"]
        .as_array()
        .map(|k| {
            k.iter()
                .filter_map(Value::as_str)
                .map(str::to_lowercase)
                .collect()
        })
        .unwrap_or_default();
    let id = route["id"].as_str().unwrap_or("").to_lowercase();
    let topic_tokens = tokens(route["topic"].as_str().unwrap_or(""));
    query
        .iter()
        .map(|t| {
            if keywords.iter().any(|k| k == t) || *t == id {
                2
            } else if t.len() > 2
                && !STOPWORDS.contains(&t.as_str())
                && topic_tokens.iter().any(|w| w == t)
            {
                1
            } else {
                0
            }
        })
        .sum()
}

/// Matching routes for a free-text question or topic, best first.
/// An empty query returns every route.
pub fn lookup(query: &str) -> Value {
    let doc = index();
    let routes = doc["routes"].as_array().cloned().unwrap_or_default();
    let q = tokens(query);
    let matches: Vec<Value> = if q.is_empty() {
        routes
    } else {
        let mut scored: Vec<(usize, usize, Value)> = routes
            .into_iter()
            .enumerate()
            .map(|(i, r)| (score(&r, &q), i, r))
            .filter(|(s, _, _)| *s > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, _, r)| r).collect()
    };
    let mut out = json!({
        "schema_version": doc["schema_version"],
        "query": query,
        "rule": doc["rule"],
        "matches": matches,
    });
    if matches.is_empty() {
        out["hint"] = json!(
            "No route matched. Symbol questions go to def/refs/hover; text goes to rg. Run `rosslabs-lsp route` with no topic for the full index."
        );
    }
    out
}

/// Plain-text form for the CLI.
pub fn text(result: &Value) -> String {
    let mut s = String::new();
    let matches = result["matches"].as_array().cloned().unwrap_or_default();
    if matches.is_empty() {
        s.push_str(result["hint"].as_str().unwrap_or(""));
        return s;
    }
    for r in matches {
        s.push_str(&format!(
            "[{}] {}\n  use: {}\n",
            r["dimension"].as_str().unwrap_or(""),
            r["topic"].as_str().unwrap_or(""),
            r["use"].as_str().unwrap_or("")
        ));
        for key in ["mcp", "cli", "notes"] {
            if let Some(v) = r[key].as_str().filter(|v| !v.is_empty()) {
                s.push_str(&format!("  {key}: {v}\n"));
            }
        }
    }
    s.trim_end().to_string()
}

fn cell(v: &Value) -> String {
    let s = v.as_str().unwrap_or("");
    if s.is_empty() {
        return "—".into();
    }
    s.replace('|', "\\|")
}

fn code(v: &Value) -> String {
    let s = v.as_str().unwrap_or("");
    if s.is_empty() {
        return "—".into();
    }
    // Backtick-wrapped cells may not contain a bare pipe, even escaped, in
    // every renderer; split alternatives into separate code spans instead.
    s.split("  |  ")
        .map(|part| format!("`{}`", part.replace('`', "'")))
        .collect::<Vec<_>>()
        .join(" or ")
}

/// Render ROUTING.md. The checked-in file must equal this output byte for byte.
pub fn markdown() -> String {
    let doc = index();
    let mut s = String::new();
    s.push_str("<!--\nSPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>\nSPDX-License-Identifier: Apache-2.0\n\nGENERATED from routing.json. Do not edit by hand. Regenerate:\n  cd code-intel && cargo run -q --bin rosslabs-lsp -- route --markdown > ../ROUTING.md\n-->\n");
    s.push_str("# RossLabs LSP routing\n\n");
    s.push_str(&format!("**Rule.** {}\n\n", cell(&doc["rule"])));
    s.push_str(
        "Ask the index from any shell with `rosslabs-lsp route <question or topic>` \
         (add `--json` for the machine form), or call the MCP tool `route`. \
         Machine-readable source: [`routing.json`](routing.json).\n",
    );
    let routes = doc["routes"].as_array().cloned().unwrap_or_default();
    for (dim, title) in DIMENSIONS {
        s.push_str(&format!("\n## {title}\n\n"));
        s.push_str("| Topic | Use | MCP | CLI | Notes |\n|---|---|---|---|---|\n");
        for r in routes.iter().filter(|r| r["dimension"] == *dim) {
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                cell(&r["topic"]),
                cell(&r["use"]),
                code(&r["mcp"]),
                code(&r["cli"]),
                cell(&r["notes"])
            ));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_json_has_a_versioned_schema_and_complete_routes() {
        let doc: Value = serde_json::from_str(ROUTING_JSON).unwrap();
        assert_eq!(doc["schema_version"], 1);
        let mut ids = std::collections::HashSet::new();
        for r in doc["routes"].as_array().unwrap() {
            let id = r["id"].as_str().unwrap();
            assert!(ids.insert(id), "duplicate route id {id}");
            assert!(
                DIMENSIONS.iter().any(|(d, _)| r["dimension"] == *d),
                "{id}: unknown dimension"
            );
            for key in ["topic", "use", "mcp", "cli", "notes"] {
                assert!(r[key].is_string(), "{id}: {key} must be a string");
            }
            assert!(
                !r["keywords"].as_array().unwrap().is_empty(),
                "{id}: no keywords"
            );
        }
    }

    #[test]
    fn routing_md_matches_the_rendered_json() {
        let checked_in = include_str!("../../ROUTING.md");
        assert!(
            checked_in == markdown(),
            "ROUTING.md is stale; run: cd code-intel && cargo run -q --bin rosslabs-lsp -- route --markdown > ../ROUTING.md"
        );
    }

    #[test]
    fn every_registered_language_server_has_a_language_route() {
        let doc = index();
        let routes = doc["routes"].as_array().unwrap();
        for server in crate::registry::SERVERS {
            assert!(
                routes.iter().any(|r| r["dimension"] == "language"
                    && r["use"].as_str().unwrap().contains(server.command)),
                "no language route names {}",
                server.command
            );
        }
    }

    #[test]
    fn every_compat_section_has_a_component_route() {
        let doc = index();
        let routes = doc["routes"].as_array().unwrap();
        for section in crate::compat::SECTIONS {
            assert!(
                routes
                    .iter()
                    .any(|r| r["cli"].as_str().unwrap().contains(section)),
                "no route covers compat section {section}"
            );
        }
    }

    #[test]
    fn lookup_ranks_the_obvious_route_first() {
        let first = |q: &str| lookup(q)["matches"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(first("who calls this function"), "references");
        assert_eq!(first("rust"), "rust");
        assert_eq!(first("hooks"), "agent-hooks");
        assert_eq!(first("codex"), "codex");
        assert_eq!(first("search config yaml"), "text-search");
    }

    #[test]
    fn empty_query_returns_everything_and_nonsense_returns_a_hint() {
        let all = lookup("");
        assert_eq!(
            all["matches"].as_array().unwrap().len(),
            index()["routes"].as_array().unwrap().len()
        );
        let none = lookup("zzzqqq");
        assert!(none["matches"].as_array().unwrap().is_empty());
        assert!(none["hint"].is_string());
    }
}

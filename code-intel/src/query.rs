// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! The shared core. Both adapters — CLI and MCP — call only this.
//!
//! Keeping every query here is what makes the two surfaces one tool rather
//! than two: adding a third adapter later costs a translation layer, not a
//! second implementation.

use crate::daemon;
use crate::lsp::{self, Client};
use crate::registry;
use crate::selector::{self, Selector};
use crate::ui;
use serde_json::{json, Value};
use std::time::Duration;

/// How long a server may index before a positional query gives up waiting.
/// rust-analyzer on a cold `target/` and jdtls on a large classpath are the
/// cases this exists for.
const INDEX_BUDGET: Duration = Duration::from_secs(60);

/// Queries the tool can answer. One variant per LSP request we expose.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Definition,
    References,
    Hover,
    Symbols,
    Diagnostics,
    Ui,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "def" | "definition" => Kind::Definition,
            "refs" | "references" => Kind::References,
            "hover" => Kind::Hover,
            "symbols" => Kind::Symbols,
            "diag" | "diagnostics" => Kind::Diagnostics,
            "ui" | "ui-contract" => Kind::Ui,
            _ => return None,
        })
    }
    /// Position-taking queries need line:col; whole-file ones do not.
    pub fn needs_position(self) -> bool {
        matches!(self, Kind::Definition | Kind::References | Kind::Hover)
    }
}

/// Find the workspace root by walking up for a project marker.
///
/// The server is spawned here, so getting it wrong means the server indexes
/// the wrong tree and answers about the wrong code.
pub fn workspace_root(path: &std::path::Path) -> String {
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "pyproject.toml",
        "package.json",
        "go.mod",
        "Package.swift",
        "compile_commands.json",
    ];
    let mut dir = if path.is_dir() {
        Some(path)
    } else {
        path.parent()
    };
    while let Some(d) = dir {
        if MARKERS.iter().any(|m| d.join(m).exists()) {
            return d.to_string_lossy().into_owned();
        }
        dir = d.parent();
    }
    path.parent()
        .unwrap_or(std::path::Path::new("."))
        .to_string_lossy()
        .into_owned()
}

/// Run one query and return a JSON envelope.
///
/// The envelope is identical for both adapters — the CLI prints it, the MCP
/// adapter wraps it in a tool result. Neither reformats it.
pub fn run(kind: Kind, file: &str, sel: Selector, raw: bool) -> Result<Value, String> {
    let abs = std::fs::canonicalize(file)
        .map_err(|e| format!("cannot resolve {file}: {e}"))?
        .to_string_lossy()
        .into_owned();

    if kind == Kind::Ui {
        return Ok(json!({
            "file": abs,
            "query": "ui",
            "ready": true,
            "result": ui::inspect(&abs)?,
        }));
    }

    let server = registry::for_path(&abs)
        .ok_or_else(|| format!("no language server registered for {abs}"))?;
    if !registry::is_installed(server) {
        return Err(format!(
            "{} is not installed — install it with: {}",
            server.command, server.install
        ));
    }

    let root = workspace_root(std::path::Path::new(&abs));
    if daemon::is_poolable(server) {
        if let Some(result) = daemon::try_run(kind, &abs, &sel, &root, server.command, raw)? {
            return Ok(result);
        }
    }
    let mut client = Client::start(server, &root)?;
    run_with_client(kind, &abs, &sel, &root, server, &mut client, raw)
}

/// Directories never worth scanning for a symbol: VCS data, build output,
/// dependency trees. Hidden directories are skipped as well.
const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "build",
    "dist",
    "out",
    "vendor",
    "__pycache__",
    "DerivedData",
];
const MAX_SCAN_FILES: usize = 20_000;
const MAX_SCAN_BYTES: u64 = 1024 * 1024;
const MAX_CANDIDATES: usize = 40;

/// Resolve a symbol by name without a file, for shell-only hosts.
///
/// `path` may be a file (resolved exactly like MCP's `{file, symbol}`) or a
/// directory, default the current one. For a directory, files of a registered
/// language that contain the name as a whole word are candidates; each
/// candidate's own symbol tree decides whether it *defines* the name, using
/// the same `selector::resolve_symbol` the MCP path uses. Exactly one
/// defining file is required: several are refused with the list, never
/// guessed.
pub fn run_named(kind: Kind, name: &str, path: Option<&str>, raw: bool) -> Result<Value, String> {
    let symbol_path: Vec<String> = name
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let leaf = symbol_path
        .last()
        .cloned()
        .ok_or("--symbol needs a non-empty name")?;
    let base = leaf.split('(').next().unwrap_or(&leaf).trim().to_string();
    let target = path.unwrap_or(".");
    let meta = std::fs::metadata(target).map_err(|e| format!("cannot resolve {target}: {e}"))?;
    if meta.is_file() {
        return run(
            kind,
            target,
            Selector::Symbol {
                path: symbol_path.clone(),
            },
            raw,
        );
    }

    let root_dir = std::fs::canonicalize(target).map_err(|e| e.to_string())?;
    let mut files = Vec::new();
    let mut scanned = 0usize;
    scan_candidates(&root_dir, &base, &mut files, &mut scanned);
    if files.is_empty() {
        return Err(format!(
            "no file of a supported language under {} mentions '{base}' (scanned {scanned} files); try rg -w '{base}'",
            root_dir.display()
        ));
    }
    files.truncate(MAX_CANDIDATES);

    // One client per server, reused across that server's candidate files.
    let mut clients: Vec<(&'static str, Client)> = Vec::new();
    let mut defining: Vec<(String, &'static registry::Server)> = Vec::new();
    let mut ambiguous: Vec<String> = Vec::new();
    let mut last_err = String::new();
    for file in &files {
        let Some(server) = registry::for_path(file) else {
            continue;
        };
        if !registry::is_installed(server) {
            last_err = format!("{} is not installed — {}", server.command, server.install);
            continue;
        }
        let idx = match clients.iter().position(|(c, _)| *c == server.command) {
            Some(i) => i,
            None => {
                let root = workspace_root(std::path::Path::new(file));
                match Client::start(server, &root) {
                    Ok(c) => {
                        clients.push((server.command, c));
                        clients.len() - 1
                    }
                    Err(e) => {
                        last_err = e;
                        continue;
                    }
                }
            }
        };
        let client = &mut clients[idx].1;
        let Ok(uri) = client.open(file, server.language_id) else {
            continue;
        };
        let tree = client
            .request(
                "textDocument/documentSymbol",
                json!({"textDocument": {"uri": uri}}),
            )
            .unwrap_or(Value::Null);
        match selector::resolve_symbol(&tree, &symbol_path) {
            Ok(_) => defining.push((file.clone(), server)),
            Err(e) if e.contains("ambiguous") => ambiguous.push(format!("{file}: {e}")),
            Err(_) => {}
        }
    }

    match (defining.len(), ambiguous.len()) {
        (1, 0) => {
            let (file, server) = defining.remove(0);
            if daemon::is_poolable(server) {
                // Hand off to the warm pool: the discovery client never
                // indexed the workspace, the pooled one may have.
                drop(clients);
                return run(kind, &file, Selector::Symbol { path: symbol_path }, raw);
            }
            let root = workspace_root(std::path::Path::new(&file));
            let idx = clients
                .iter()
                .position(|(c, _)| *c == server.command)
                .expect("client exists for the defining file");
            let sel = Selector::Symbol { path: symbol_path };
            run_with_client(kind, &file, &sel, &root, server, &mut clients[idx].1, raw)
        }
        (0, 0) => Err(format!(
            "no file defines '{name}' among {} candidate files that mention it{}; pass --path <file> or use rg -w '{base}'",
            files.len(),
            if last_err.is_empty() {
                String::new()
            } else {
                format!(" (last error: {last_err})")
            }
        )),
        _ => {
            let mut list: Vec<String> = defining.into_iter().map(|(f, _)| f).collect();
            list.extend(ambiguous);
            Err(format!(
                "'{name}' is defined in more than one place: {}. Pass --path <file> or qualify it as Parent/{leaf}",
                list.join("; ")
            ))
        }
    }
}

fn scan_candidates(dir: &std::path::Path, word: &str, out: &mut Vec<String>, scanned: &mut usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if *scanned >= MAX_SCAN_FILES {
            return;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            scan_candidates(&path, word, out, scanned);
        } else if ft.is_file() {
            let p = path.to_string_lossy().into_owned();
            if registry::for_path(&p).is_none() {
                continue;
            }
            *scanned += 1;
            if entry.metadata().is_ok_and(|m| m.len() > MAX_SCAN_BYTES) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                if contains_word(&text, word) {
                    out.push(p);
                }
            }
        }
    }
}

/// True when `word` occurs in `text` bounded by non-identifier characters.
fn contains_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut start = 0;
    while let Some(i) = text[start..].find(word) {
        let at = start + i;
        let end = at + word.len();
        let before = text[..at].chars().next_back();
        let after = text[end..].chars().next();
        if !before.is_some_and(is_ident) && !after.is_some_and(is_ident) {
            return true;
        }
        start = end;
    }
    false
}

/// Run against an initialized client. The direct CLI/MCP path and daemon path
/// intentionally share this function so pooling cannot change output shape.
pub(crate) fn run_with_client(
    kind: Kind,
    abs: &str,
    sel: &Selector,
    root: &str,
    server: &registry::Server,
    client: &mut Client,
    raw: bool,
) -> Result<Value, String> {
    let uri = client.open(abs, server.language_id)?;

    // Whether the server produced a usable answer rather than an early empty.
    let mut ready = true;
    let result = match kind {
        Kind::Symbols => client.request(
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
        )?,
        Kind::Diagnostics => client.wait_diagnostics(&uri, Duration::from_secs(30))?,
        _ => {
            // Resolve the selector to a concrete position. A symbol path is
            // looked up in this file's own symbol tree, so it stays correct
            // when unrelated lines above it move.
            let position = match sel {
                Selector::Position { line, col } => {
                    let text = std::fs::read_to_string(abs).map_err(|e| e.to_string())?;
                    lsp::to_lsp_position(&text, *line, *col)?
                }
                Selector::Symbol { path } => {
                    let tree = client.request(
                        "textDocument/documentSymbol",
                        json!({"textDocument": {"uri": uri}}),
                    )?;
                    selector::resolve_symbol(&tree, path)?
                }
                Selector::Whole => return Err("this query needs a symbol or position".into()),
            };
            let (method, extra) = match kind {
                Kind::Definition => ("textDocument/definition", json!({})),
                Kind::References => (
                    "textDocument/references",
                    json!({"context": {"includeDeclaration": false}}),
                ),
                Kind::Hover => ("textDocument/hover", json!({})),
                _ => unreachable!("position kinds are exhaustive here"),
            };
            let mut params = json!({
                "textDocument": {"uri": uri},
                "position": position,
            });
            if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
                for (k, v) in e {
                    p.insert(k.clone(), v.clone());
                }
            }
            // Retry until the server produces an answer or the budget runs out.
            //
            // Progress notifications alone are not a sufficient readiness
            // signal: rust-analyzer answers `[]` in milliseconds before it has
            // begun reporting progress at all, so a one-shot query silently
            // returns a false negative. Asking again until it answers is the
            // only signal that works across every server.
            let deadline = std::time::Instant::now() + INDEX_BUDGET;
            let answer;
            loop {
                client.wait_until_idle(Duration::from_secs(2));
                let attempt = client.request(method, params.clone());
                let a = match attempt {
                    Ok(a) => a,
                    Err(e) if is_retryable(&e) => {
                        if std::time::Instant::now() >= deadline {
                            return Err(format!("{e} (server never finished loading)"));
                        }
                        std::thread::sleep(Duration::from_millis(400));
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                let empty = matches!(&a, Value::Null) || a.as_array().is_some_and(|x| x.is_empty());
                if !empty || std::time::Instant::now() >= deadline {
                    ready = !empty;
                    if !empty {
                        client.mark_proven_ready();
                    }
                    answer = a;
                    break;
                }
                std::thread::sleep(Duration::from_millis(400));
            }
            answer
        }
    };

    // Name the symbol containing each hit. "registry.rs:107" tells a caller
    // where; "tests/maps_extension_to_server" tells them what — which is the
    // difference between a line number and a fact they can act on.
    let result = if matches!(kind, Kind::References | Kind::Definition) {
        annotate_containers(client, server, &result)
    } else {
        result
    };

    let empty = match &result {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        _ => false,
    };

    // Compact by default. A caller is an agent paying for every token of this
    // payload, and raw LSP locations carry a full URI plus a four-number range
    // per hit where `path:line` and the containing symbol are what get acted
    // on. Measured on this repo: raw output cost 3x what grep cost for the
    // same question; compacting is the difference between the tool saving
    // context and spending it. `--raw` keeps the untouched LSP shape.
    let result = if raw { result } else { compact(&result, root) };

    let mut envelope = json!({
        "query": format!("{kind:?}").to_lowercase(),
        "file": abs,
        "server": server.command,
        "root": root,
        "ready": ready,
        "result": result,
    });
    // An empty answer from a server that never finished indexing is unknown,
    // not negative. Say so rather than letting a caller treat it as fact.
    if empty && !ready {
        envelope["warning"] = json!(
            "server was still indexing when queried; an empty result here means \"unknown\", not \"none\""
        );
    }
    Ok(envelope)
}

/// Reduce LSP locations to what a caller acts on: a repo-relative path, a
/// 1-based line, and the symbol containing the hit.
///
/// Anything not recognised as a location list passes through untouched, so
/// hover and diagnostics keep their own shapes.
fn compact(result: &Value, root: &str) -> Value {
    let Some(items) = result.as_array() else {
        return result.clone();
    };
    let prefix = format!("file://{root}/");
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let uri = item
            .get("uri")
            .or_else(|| item.get("targetUri"))
            .and_then(|v| v.as_str());
        let range = item
            .get("range")
            .or_else(|| item.get("targetSelectionRange"));
        let (Some(uri), Some(range)) = (uri, range) else {
            out.push(item.clone());
            continue;
        };
        let line = range
            .get("start")
            .and_then(|s| s.get("line"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            + 1;
        let path = percent_decode(uri.strip_prefix(&prefix).unwrap_or(uri));
        let mut o = serde_json::Map::new();
        o.insert("at".into(), json!(format!("{path}:{line}")));
        if let Some(sym) = item.get("in_symbol") {
            o.insert("in".into(), sym.clone());
        }
        out.push(Value::Object(o));
    }
    Value::Array(out)
}

/// Add `in_symbol` to each location by asking each referenced file for its
/// symbol tree and finding the innermost span containing the hit.
///
/// Best-effort: a file we cannot open simply keeps its bare location rather
/// than failing the whole query.
fn annotate_containers(client: &mut Client, server: &registry::Server, result: &Value) -> Value {
    let Some(items) = result.as_array() else {
        return result.clone();
    };
    let mut trees: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    let mut out = Vec::with_capacity(items.len());

    for item in items {
        let mut item = item.clone();
        let uri = item
            .get("uri")
            .or_else(|| item.get("targetUri"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let line = item
            .get("range")
            .or_else(|| item.get("targetSelectionRange"))
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get("line"))
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        if let (Some(uri), Some(line)) = (uri, line) {
            if !trees.contains_key(&uri) {
                let path = uri.strip_prefix("file://").unwrap_or(&uri);
                let decoded = percent_decode(path);
                let opened = client.open(&decoded, server.language_id).is_ok();
                let tree = if opened {
                    client
                        .request(
                            "textDocument/documentSymbol",
                            json!({"textDocument": {"uri": uri}}),
                        )
                        .unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                trees.insert(uri.clone(), tree);
            }
            if let Some(tree) = trees.get(&uri) {
                if let Some(name) = selector::containing_symbol(tree, line) {
                    if let Some(o) = item.as_object_mut() {
                        o.insert("in_symbol".into(), json!(name));
                    }
                }
            }
        }
        out.push(item);
    }
    Value::Array(out)
}

/// Reverse `path_to_uri`'s percent-encoding.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// LSP's own "not ready, ask again" codes: ContentModified (-32801) and
/// ServerCancelled (-32802). rust-analyzer returns the former for the entire
/// window in which it is loading a workspace.
fn is_retryable(err: &str) -> bool {
    err.starts_with("[-32801]") || err.starts_with("[-32802]")
}

/// Report which registered servers are installed. Used by `doctor` and by the
/// MCP adapter so a host can see coverage without running a query.
pub fn doctor() -> Value {
    let servers: Vec<Value> = registry::SERVERS
        .iter()
        .map(|s| {
            let path = registry::which(s.command);
            let installed = path.is_some();
            let fallback = (s.language_id == "typescript")
                .then(registry::typescript_fallback_path)
                .flatten();
            let ready = installed && (s.language_id != "typescript" || fallback.is_some());
            json!({
                "language": s.language_id,
                "command": s.command,
                "extensions": s.extensions,
                "installed": installed,
                "ready": ready,
                "path": path.map(|p| p.to_string_lossy().into_owned()),
                "install": s.install,
                "runtime": if s.language_id == "typescript" {
                    json!({
                        "fallback_path": fallback.as_ref().map(|p| p.to_string_lossy().into_owned()),
                        "fallback_ready": fallback.is_some(),
                        "hint": registry::typescript_setup_hint(),
                    })
                } else { Value::Null },
            })
        })
        .collect();
    let installed = servers.iter().filter(|s| s["installed"] == true).count();
    let ready = servers.iter().filter(|s| s["ready"] == true).count();
    json!({
        "build": build_info(),
        "installed": installed,
        "ready": ready,
        "total": servers.len(),
        "servers": servers,
    })
}

/// Which source this binary was built from, so a stale install is visible.
pub fn build_info() -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "commit": env!("ROSSLABS_LSP_BUILD_COMMIT"),
        "date": env!("ROSSLABS_LSP_BUILD_DATE"),
        "id": env!("ROSSLABS_LSP_BUILD_ID"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_short_and_long_query_names() {
        assert_eq!(Kind::parse("def"), Some(Kind::Definition));
        assert_eq!(Kind::parse("references"), Some(Kind::References));
        assert_eq!(Kind::parse("nope"), None);
    }

    #[test]
    fn only_point_queries_need_a_position() {
        assert!(Kind::Definition.needs_position());
        assert!(!Kind::Symbols.needs_position());
        assert!(!Kind::Diagnostics.needs_position());
    }

    #[test]
    fn workspace_root_finds_the_marker_dir() {
        let here = std::env::current_dir().unwrap();
        let root = workspace_root(&here.join("src").join("query.rs"));
        assert!(std::path::Path::new(&root).join("Cargo.toml").exists());
    }

    #[test]
    fn doctor_reports_every_registered_server() {
        let d = doctor();
        assert_eq!(d["total"], registry::SERVERS.len());
        assert!(d["servers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["command"].is_string()));
        assert!(d["servers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["ready"].is_boolean()));
    }

    #[test]
    fn doctor_reports_its_build() {
        let b = &doctor()["build"];
        for key in ["version", "commit", "date", "id"] {
            assert!(
                b[key].as_str().is_some_and(|s| !s.is_empty()),
                "build.{key} missing"
            );
        }
    }

    #[test]
    fn contains_word_respects_identifier_boundaries() {
        assert!(contains_word("fn run_named(x)", "run_named"));
        assert!(!contains_word("fn run_named_x()", "run_named"));
        assert!(!contains_word("xrun_named", "run_named"));
        assert!(contains_word("run_named", "run_named"));
    }

    #[test]
    fn named_lookup_without_any_mention_is_refused_before_spawning() {
        // Built at runtime so this source file does not itself mention it.
        let name = format!("{}{}", "zz_no_such", "_symbol_qq");
        let err = run_named(Kind::References, &name, Some("src"), false).unwrap_err();
        assert!(err.contains("mentions"), "got: {err}");
    }

    #[test]
    fn unknown_extension_is_rejected_before_spawning() {
        let err = run(Kind::Symbols, "Cargo.toml", Selector::Whole, false).unwrap_err();
        assert!(err.contains("no language server"), "got: {err}");
    }
}

// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! MCP stdio adapter — the same queries as the CLI, exposed as tools.
//!
//! Implements the four exit paths from ~/dev/docs/standards/mcp-lifecycle/SPEC.md
//! (contract v1, frozen 2026-08-12). EOF is one of four, never the only one:
//! a host that pools conversations holds the pipe open forever, and a server
//! that waits only for EOF accumulates until it is measured in gigabytes.
//!
//! Note the framing differs from LSP: MCP stdio is newline-delimited JSON,
//! not Content-Length framed.

use crate::query::{self, Kind};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const PROTOCOL_VERSION: &str = "2025-06-18";

/// Millis since process start when the last inbound frame arrived (L4).
static LAST_FRAME_MS: AtomicU64 = AtomicU64::new(0);

pub fn serve() -> ! {
    install_signal_handlers(); // L2
    let start = Instant::now();
    spawn_watchdog(start); // L3 + L4

    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // treated as EOF
        };
        LAST_FRAME_MS.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(out, "{}", error_frame(Value::Null, -32700, &format!("parse error: {e}")));
                let _ = out.flush();
                continue;
            }
        };
        if let Some(resp) = handle(&req) {
            let _ = writeln!(out, "{resp}");
            let _ = out.flush();
        }
    }
    // L1 — stdin EOF: flush and exit 0.
    let _ = out.flush();
    std::process::exit(0)
}

/// Returns None for notifications, which take no response.
fn handle(req: &Value) -> Option<Value> {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_none() {
        return None; // notification (e.g. notifications/initialized)
    }
    let id = id.unwrap();

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "code-intel", "version": env!("CARGO_PKG_VERSION")}
        })),
        "tools/list" => Ok(json!({"tools": tool_list()})),
        "tools/call" => call_tool(req.get("params").unwrap_or(&Value::Null)),
        "ping" => Ok(json!({})),
        other => Err(format!("unknown method: {other}")),
    };

    Some(match result {
        Ok(r) => json!({"jsonrpc":"2.0","id":id,"result":r}),
        Err(e) => error_frame(id, -32603, &e),
    })
}

fn call_tool(params: &Value) -> Result<Value, String> {
    let name = params.get("name").and_then(|v| v.as_str()).ok_or("missing tool name")?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    if name == "doctor" {
        return Ok(tool_result(&query::doctor(), false));
    }
    let kind = Kind::parse(name).ok_or_else(|| format!("unknown tool: {name}"))?;
    let file = args.get("file").and_then(|v| v.as_str()).ok_or("missing 'file'")?;
    let line = args.get("line").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
    let col = args.get("column").and_then(|v| v.as_u64()).unwrap_or(1) as usize;

    // A failed query is a tool error, not a protocol error: the host should
    // show the model what went wrong so it can correct, not drop the turn.
    Ok(match query::run(kind, file, line, col) {
        Ok(v) => tool_result(&v, false),
        Err(e) => tool_result(&json!({"error": e}), true),
    })
}

fn tool_result(payload: &Value, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": serde_json::to_string_pretty(payload).unwrap_or_default()}],
        "isError": is_error
    })
}

fn tool_list() -> Vec<Value> {
    let point = |name: &str, desc: &str| {
        json!({
            "name": name,
            "description": desc,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": {"type": "string", "description": "Path to the source file"},
                    "line": {"type": "integer", "description": "1-based line number"},
                    "column": {"type": "integer", "description": "1-based column number"}
                },
                "required": ["file", "line", "column"]
            }
        })
    };
    let whole = |name: &str, desc: &str| {
        json!({
            "name": name,
            "description": desc,
            "inputSchema": {
                "type": "object",
                "properties": {"file": {"type": "string", "description": "Path to the source file"}},
                "required": ["file"]
            }
        })
    };
    vec![
        point("def", "Resolve the definition of the symbol at a position. Compiler-accurate, unlike text search."),
        point("refs", "Find every reference to the symbol at a position, across files."),
        point("hover", "Inferred type and documentation for the symbol at a position."),
        whole("symbols", "Outline every top-level symbol in a file."),
        whole("diag", "Type and compile diagnostics for a file."),
        json!({
            "name": "doctor",
            "description": "Report which language servers are registered and installed.",
            "inputSchema": {"type": "object", "properties": {}}
        }),
    ]
}

fn error_frame(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

// ---------------------------------------------------------------- lifecycle

extern "C" fn on_signal(_sig: i32) {
    // Only async-signal-safe calls are legal here, so exit immediately rather
    // than unwinding. Exit 0, not 128+n, so supervisors do not respawn us.
    unsafe { libc::_exit(0) }
}

/// L2 — terminate on SIGTERM / SIGINT / SIGHUP.
fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_signal as *const () as libc::sighandler_t);
    }
}

/// L3 — parent-death watchdog, and L4 — idle timeout.
fn spawn_watchdog(start: Instant) {
    let idle_ms: Option<u64> = std::env::var("MCP_IDLE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0);

    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(5));

        // L3: reparented to init/launchd means the spawning host died.
        if unsafe { libc::getppid() } == 1 {
            std::process::exit(0);
        }

        // L4: implemented always, active only when the host opts in.
        if let Some(limit) = idle_ms {
            let now = start.elapsed().as_millis() as u64;
            let last = LAST_FRAME_MS.load(Ordering::Relaxed);
            if now.saturating_sub(last) >= limit {
                std::process::exit(0);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_reports_tools_capability() {
        let r = handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize"})).unwrap();
        assert_eq!(r["result"]["serverInfo"]["name"], "code-intel");
        assert!(r["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn notifications_get_no_response() {
        assert!(handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());
    }

    #[test]
    fn every_tool_has_a_schema_and_matches_a_query_kind() {
        for t in tool_list() {
            let name = t["name"].as_str().unwrap();
            assert!(t["inputSchema"]["type"] == "object", "{name} has no object schema");
            assert!(
                name == "doctor" || Kind::parse(name).is_some(),
                "tool {name} maps to no query kind"
            );
        }
    }

    #[test]
    fn unknown_method_is_an_error_frame_not_a_panic() {
        let r = handle(&json!({"jsonrpc":"2.0","id":7,"method":"bogus"})).unwrap();
        assert_eq!(r["error"]["code"], -32603);
        assert_eq!(r["id"], 7);
    }

    #[test]
    fn failed_query_is_a_tool_error_not_a_protocol_error() {
        let r = call_tool(&json!({"name":"symbols","arguments":{"file":"Cargo.toml"}})).unwrap();
        assert_eq!(r["isError"], true, "expected an isError tool result");
    }
}

// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! code-intel — one binary, two surfaces, any agent.
//!
//! `code-intel def src/x.rs:42:11`  → CLI, for anything that can run a command
//! `code-intel mcp`                 → MCP stdio server, for MCP-capable hosts
//!
//! Both go through query.rs. Neither knows the other exists.

mod daemon;
mod lsp;
mod mcp;
mod query;
mod registry;
mod selector;
mod ui;

use query::Kind;

const USAGE: &str = "\
code-intel — compiler-grade code intelligence for any agent

USAGE
  code-intel <query> <file>[:<line>:<column>]
  code-intel mcp
  code-intel daemon
  code-intel doctor

OPTIONS
  --raw     emit untouched LSP payloads instead of compact locations

QUERIES
  def       definition of the symbol at a position
  refs      every reference to the symbol at a position
  hover     inferred type and docs at a position
  symbols   outline of a whole file
  diag      diagnostics for a whole file
  ui        static JavaScript-to-CSS, icon, text, and accessibility contract

EXAMPLES
  code-intel def src/query.rs:42:11
  code-intel symbols scripts/build.py
  code-intel doctor

MODES
  mcp       serve the same queries over MCP stdio (for Claude Code, Codex, ...)
  daemon    keep eligible language servers warm over a local Unix socket

Output is JSON on stdout. Errors go to stderr with a non-zero exit.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(out) => println!("{out}"),
        Err(e) => {
            eprintln!("code-intel: {e}");
            std::process::exit(1);
        }
    }
}

fn run(args: &[String]) -> Result<String, String> {
    let first = args.first().map(String::as_str).unwrap_or("");
    match first {
        "" | "-h" | "--help" | "help" => Ok(USAGE.trim_end().to_string()),
        "--version" | "-V" => Ok(format!("code-intel {}", env!("CARGO_PKG_VERSION"))),
        "mcp" => mcp::serve(),
        "daemon" => {
            daemon::serve()?;
            Ok(String::new())
        }
        "doctor" => pretty(&query::doctor()),
        verb => {
            let kind =
                Kind::parse(verb).ok_or_else(|| format!("unknown query '{verb}' (try --help)"))?;
            let target = args.get(1).ok_or("missing <file>[::symbol | :line:col]")?;
            let raw = args.iter().any(|a| a == "--raw");
            let (file, sel) = selector::Selector::parse(target);
            if kind.needs_position() && !sel.is_point() {
                return Err(format!(
                    "{verb} needs a symbol or position, e.g. {file}::my_function or {file}:42:11"
                ));
            }
            pretty(&query::run(kind, &file, sel, raw)?)
        }
    }
}

fn pretty(v: &serde_json::Value) -> Result<String, String> {
    serde_json::to_string_pretty(v).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_and_version_do_not_error() {
        assert!(run(&["--help".into()]).unwrap().contains("code-intel"));
        assert!(run(&["--version".into()]).unwrap().contains("0.1.0"));
    }

    #[test]
    fn point_query_without_position_is_rejected() {
        let err = run(&["def".into(), "src/main.rs".into()]).unwrap_err();
        assert!(err.contains("needs a symbol or position"), "got: {err}");
    }

    #[test]
    fn unknown_verb_is_rejected() {
        assert!(run(&["frobnicate".into()]).is_err());
    }
}

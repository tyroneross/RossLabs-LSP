// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! RossLabs LSP — one implementation, two commands, two surfaces, any agent.

mod compat;
mod daemon;
mod lsp;
mod mcp;
mod query;
mod registry;
mod selector;
mod ui;

use query::Kind;

const USAGE: &str = "\
RossLabs LSP — compiler-grade code intelligence for any agent

USAGE
  rosslabs-lsp <query> <file>[:<line>:<column>]
  rosslabs-lsp mcp
  rosslabs-lsp daemon
  rosslabs-lsp doctor
  rosslabs-lsp compat record <registry.json> <record.json>
  rosslabs-lsp compat sections
  rosslabs-lsp compat <registry.json> [--section NAME] [--component NAME]

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
  rosslabs-lsp def src/query.rs:42:11
  rosslabs-lsp symbols scripts/build.py
  rosslabs-lsp doctor

MODES
  mcp       serve the same queries over MCP stdio (for Claude Code, Codex, ...)
  daemon    keep eligible language servers warm over a local Unix socket

The legacy code-intel command remains available as a compatibility alias.
Output is JSON on stdout. Errors go to stderr with a non-zero exit.
";

pub fn main_entry() {
    let mut process_args = std::env::args();
    let invoked_as = process_args
        .next()
        .and_then(|value| {
            std::path::Path::new(&value)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "rosslabs-lsp".to_string());
    let args: Vec<String> = process_args.collect();
    match run_as(&invoked_as, &args) {
        Ok(out) => println!("{out}"),
        Err(e) => {
            eprintln!("{invoked_as}: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
fn run(args: &[String]) -> Result<String, String> {
    run_as("rosslabs-lsp", args)
}

fn run_as(invoked_as: &str, args: &[String]) -> Result<String, String> {
    let first = args.first().map(String::as_str).unwrap_or("");
    match first {
        "" | "-h" | "--help" | "help" => Ok(USAGE.trim_end().to_string()),
        "--version" | "-V" => Ok(format!("{invoked_as} {}", env!("CARGO_PKG_VERSION"))),
        "mcp" => mcp::serve(),
        "daemon" => {
            daemon::serve()?;
            Ok(String::new())
        }
        "doctor" => pretty(&query::doctor()),
        "compat" => pretty(&compat::cli(&args[1..])?),
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
    fn help_and_version_use_the_rosslabs_lsp_name() {
        assert!(run(&["--help".into()]).unwrap().contains("RossLabs LSP"));
        assert_eq!(run(&["--version".into()]).unwrap(), "rosslabs-lsp 0.1.0");
    }

    #[test]
    fn help_documents_the_compatibility_alias() {
        assert!(run(&["--help".into()])
            .unwrap()
            .contains("legacy code-intel command"));
    }

    #[test]
    fn compatibility_alias_preserves_its_version_prefix() {
        assert_eq!(
            run_as("code-intel", &["--version".into()]).unwrap(),
            "code-intel 0.1.0"
        );
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

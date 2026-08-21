// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! code-intel — one binary, two surfaces, any agent.
//!
//! `code-intel def src/x.rs:42:11`  → CLI, for anything that can run a command
//! `code-intel mcp`                 → MCP stdio server, for MCP-capable hosts
//!
//! Both go through query.rs. Neither knows the other exists.

mod lsp;
mod mcp;
mod query;
mod registry;

use query::Kind;

const USAGE: &str = "\
code-intel — compiler-grade code intelligence for any agent

USAGE
  code-intel <query> <file>[:<line>:<column>]
  code-intel mcp
  code-intel doctor

QUERIES
  def       definition of the symbol at a position
  refs      every reference to the symbol at a position
  hover     inferred type and docs at a position
  symbols   outline of a whole file
  diag      diagnostics for a whole file

EXAMPLES
  code-intel def src/query.rs:42:11
  code-intel symbols scripts/build.py
  code-intel doctor

MODES
  mcp       serve the same queries over MCP stdio (for Claude Code, Codex, ...)

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
        "doctor" => pretty(&query::doctor()),
        verb => {
            let kind = Kind::parse(verb)
                .ok_or_else(|| format!("unknown query '{verb}' (try --help)"))?;
            let target = args.get(1).ok_or("missing <file>[:line:column]")?;
            let (file, line, col) = split_target(target)?;
            if kind.needs_position() && (line, col) == (0, 0) {
                return Err(format!("{verb} needs a position, e.g. {file}:42:11"));
            }
            let (line, col) = if (line, col) == (0, 0) { (1, 1) } else { (line, col) };
            pretty(&query::run(kind, &file, line, col)?)
        }
    }
}

/// Split `path/to/file.rs:42:11` into its parts.
///
/// Splits from the right so absolute Windows-style or colon-bearing paths do
/// not lose their prefix, and returns (0, 0) when no position was supplied.
fn split_target(target: &str) -> Result<(String, usize, usize), String> {
    let parts: Vec<&str> = target.rsplitn(3, ':').collect();
    if parts.len() == 3 {
        if let (Ok(c), Ok(l)) = (parts[0].parse::<usize>(), parts[1].parse::<usize>()) {
            return Ok((parts[2].to_string(), l, c));
        }
    }
    Ok((target.to_string(), 0, 0))
}

fn pretty(v: &serde_json::Value) -> Result<String, String> {
    serde_json::to_string_pretty(v).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_file_line_column() {
        let (f, l, c) = split_target("src/x.rs:42:11").unwrap();
        assert_eq!((f.as_str(), l, c), ("src/x.rs", 42, 11));
    }

    #[test]
    fn bare_path_has_no_position() {
        let (f, l, c) = split_target("src/x.rs").unwrap();
        assert_eq!((f.as_str(), l, c), ("src/x.rs", 0, 0));
    }

    #[test]
    fn non_numeric_suffix_is_part_of_the_path() {
        // A path containing colons must not be mistaken for a position.
        let (f, l, c) = split_target("weird:name.rs").unwrap();
        assert_eq!((f.as_str(), l, c), ("weird:name.rs", 0, 0));
    }

    #[test]
    fn help_and_version_do_not_error() {
        assert!(run(&["--help".into()]).unwrap().contains("code-intel"));
        assert!(run(&["--version".into()]).unwrap().contains("0.1.0"));
    }

    #[test]
    fn point_query_without_position_is_rejected() {
        let err = run(&["def".into(), "src/main.rs".into()]).unwrap_err();
        assert!(err.contains("needs a position"), "got: {err}");
    }

    #[test]
    fn unknown_verb_is_rejected() {
        assert!(run(&["frobnicate".into()]).is_err());
    }
}

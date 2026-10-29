// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! RossLabs LSP — one implementation, two commands, two surfaces, any agent.

mod acp;
mod compat;
mod daemon;
mod hooks;
mod lsp;
mod mcp;
mod query;
mod registry;
mod route;
mod selector;
mod ui;

use query::Kind;

const USAGE: &str = "\
RossLabs LSP — compiler-grade code intelligence for any agent

USAGE
  rosslabs-lsp <query> <file>[:<line>:<column> | ::<symbol>]
  rosslabs-lsp def|refs|hover --symbol <name> [--path <root-or-file>]
  rosslabs-lsp route [<question-or-topic>] [--json | --markdown]
  rosslabs-lsp mcp
  rosslabs-lsp daemon
  rosslabs-lsp doctor
  rosslabs-lsp acp doctor [--agent ID]... [--cwd DIR] [--timeout SECS] [--json] [--first-token] [--registry PATH|URL]
  rosslabs-lsp compat record <registry.json> <record.json>
  rosslabs-lsp compat sections
  rosslabs-lsp compat refresh [<registry.json>] --section agent-hooks --host ID [--dry-run]
  rosslabs-lsp compat [<registry.json>] [--section NAME] [--component NAME] [--stale-days N]
  rosslabs-lsp hooks doctor [--host ID] [--json] [--drift] [--stale-days N] [--cwd DIR] [--registry PATH]

OPTIONS
  --raw     emit untouched LSP payloads instead of compact locations
  --symbol  name a symbol instead of a position; with --path DIR (default .)
            the defining file is found by name, with --path FILE it is exact

ROUTING
  route     which tool answers a question: by question, language, component
            (hooks, ACP, terminal input, ...) or host. Offline keyword match
            over the built-in routing index (routing.json / ROUTING.md).

QUERIES
  def       definition of the symbol at a position
  refs      every reference to the symbol at a position
  hover     inferred type and docs at a position
  symbols   outline of a whole file
  diag      diagnostics for a whole file
  ui        static JavaScript-to-CSS, icon, text, and accessibility contract

EXAMPLES
  rosslabs-lsp def src/query.rs:42:11
  rosslabs-lsp refs --symbol workspace_root
  rosslabs-lsp route 'who calls this function'
  rosslabs-lsp symbols scripts/build.py
  rosslabs-lsp doctor

ACP
  acp doctor   probe installed ACP agents (claude, gemini, cursor, opencode,
               codex, goose, kiro): version vs registry, initialize and
               session/new timings. --first-token also times one tiny prompt
               (costs tokens; never run by default).

HOOKS
  hooks doctor read agent hook configs (claude, codex, gemini, cursor, copilot,
               antigravity) and check events, outputs and versions against the
               agent-hooks compatibility records. Read-only; secrets redacted.
               --drift lists installed vs recorded host versions and the
               records to re-verify.

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
        "acp" => {
            acp::install_signal_handlers();
            acp::cli(&args[1..])
        }
        "compat" => pretty(&compat::cli(&args[1..])?),
        "route" => route_cli(&args[1..]),
        "hooks" => hooks::cli(&args[1..]),
        verb => {
            let kind =
                Kind::parse(verb).ok_or_else(|| format!("unknown query '{verb}' (try --help)"))?;
            let raw = args.iter().any(|a| a == "--raw");
            if let Some(name) = flag_value(args, "--symbol")? {
                if !kind.needs_position() {
                    return Err(format!(
                        "--symbol applies to def, refs and hover; {verb} takes a file"
                    ));
                }
                let path = flag_value(args, "--path")?;
                return pretty(&query::run_named(kind, &name, path.as_deref(), raw)?);
            }
            let target = args.get(1).ok_or("missing <file>[::symbol | :line:col]")?;
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

/// Value of `--flag VALUE` or `--flag=VALUE`. A flag with no value is an error.
fn flag_value(args: &[String], flag: &str) -> Result<Option<String>, String> {
    let prefix = format!("{flag}=");
    for (i, a) in args.iter().enumerate() {
        if a == flag {
            return match args.get(i + 1) {
                Some(v) if !v.starts_with("--") => Ok(Some(v.clone())),
                _ => Err(format!("{flag} needs a value")),
            };
        }
        if let Some(v) = a.strip_prefix(&prefix) {
            return Ok(Some(v.to_string()));
        }
    }
    Ok(None)
}

fn route_cli(args: &[String]) -> Result<String, String> {
    if args.iter().any(|a| a == "--markdown") {
        return Ok(route::markdown().trim_end().to_string());
    }
    let json = args.iter().any(|a| a == "--json");
    if let Some(bad) = args.iter().find(|a| a.starts_with("--") && *a != "--json") {
        return Err(format!("unknown route option {bad}"));
    }
    let query: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
        .collect();
    let result = route::lookup(&query.join(" "));
    if json {
        pretty(&result)
    } else {
        Ok(route::text(&result))
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
    fn symbol_flag_needs_a_value_and_a_point_query() {
        let err = run(&["refs".into(), "--symbol".into()]).unwrap_err();
        assert!(err.contains("needs a value"), "got: {err}");
        let err = run(&["symbols".into(), "--symbol".into(), "x".into()]).unwrap_err();
        assert!(err.contains("applies to def"), "got: {err}");
    }

    #[test]
    fn route_prints_text_json_and_markdown() {
        let text = run(&["route".into(), "rust".into()]).unwrap();
        assert!(text.contains("rust-analyzer"), "got: {text}");
        let json: serde_json::Value =
            serde_json::from_str(&run(&["route".into(), "hooks".into(), "--json".into()]).unwrap())
                .unwrap();
        assert_eq!(json["matches"][0]["id"], "agent-hooks");
        assert!(run(&["route".into(), "--markdown".into()])
            .unwrap()
            .starts_with("<!--"));
        assert!(run(&["route".into(), "--bogus".into()]).is_err());
    }

    #[test]
    fn unknown_verb_is_rejected() {
        assert!(run(&["frobnicate".into()]).is_err());
    }
}

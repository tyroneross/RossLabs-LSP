// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! `hooks doctor`: read installed agent hosts' hook configs and cross-check
//! them against the `agent-hooks` compatibility records.
//!
//! Read-only. Never writes a config, never contacts the network, and never
//! prints environment values: commands are redacted and home paths become `~`.
//! The only processes spawned are `<host> --version`, bounded by a timeout.

use crate::compat;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Registry compiled into the binary; `--registry PATH` overrides it.
use crate::compat::EMBEDDED_REGISTRY;
const DEFAULT_STALE_DAYS: u64 = 30;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

pub const USAGE: &str =
    "usage: hooks doctor [--host ID] [--json] [--stale-days N] [--cwd DIR] [--registry PATH]";

#[derive(Clone, Copy)]
enum Loc {
    /// File under the home directory.
    Home(&'static str),
    /// Every `*.json` in a directory under the home directory.
    HomeDir(&'static str),
    /// File under the project directory (`--cwd`).
    Project(&'static str),
    /// Every `*.json` in a directory under the project directory.
    ProjectDir(&'static str),
    /// Absolute system path.
    System(&'static str),
}

struct Host {
    id: &'static str,
    /// Component name used in registry records.
    component: &'static str,
    binaries: &'static [&'static str],
    /// Paths whose presence means the host is in use even without a binary.
    markers: &'static [&'static str],
    configs: &'static [Loc],
    /// Claude Code event name -> this host's nearest event (None = no equivalent).
    claude_aliases: &'static [(&'static str, Option<&'static str>)],
}

const HOSTS: &[Host] = &[
    Host {
        id: "claude",
        component: "Claude Code",
        binaries: &["claude"],
        markers: &[".claude"],
        configs: &[
            Loc::Home(".claude/settings.json"),
            Loc::Project(".claude/settings.json"),
            Loc::Project(".claude/settings.local.json"),
        ],
        claude_aliases: &[],
    },
    Host {
        id: "codex",
        component: "Codex CLI",
        binaries: &["codex"],
        markers: &[".codex"],
        configs: &[
            Loc::Home(".codex/hooks.json"),
            Loc::Project(".codex/hooks.json"),
        ],
        claude_aliases: &[],
    },
    Host {
        id: "gemini",
        component: "Gemini CLI",
        binaries: &["gemini"],
        markers: &[".gemini/settings.json"],
        configs: &[
            Loc::Home(".gemini/settings.json"),
            Loc::Project(".gemini/settings.json"),
            Loc::System("/etc/gemini-cli/settings.json"),
        ],
        claude_aliases: &[
            ("UserPromptSubmit", Some("BeforeAgent")),
            ("Stop", Some("AfterAgent")),
            ("SubagentStop", None),
            ("PreCompact", Some("PreCompress")),
            ("PreToolUse", Some("BeforeTool")),
            ("PostToolUse", Some("AfterTool")),
        ],
    },
    Host {
        id: "cursor",
        component: "Cursor",
        binaries: &["cursor-agent", "cursor"],
        markers: &[".cursor"],
        configs: &[
            Loc::Home(".cursor/hooks.json"),
            Loc::Project(".cursor/hooks.json"),
        ],
        claude_aliases: &[
            ("SessionStart", Some("sessionStart")),
            ("UserPromptSubmit", Some("beforeSubmitPrompt")),
            ("Stop", Some("stop")),
            ("PreCompact", Some("preCompact")),
            ("SessionEnd", Some("sessionEnd")),
            ("PreToolUse", Some("beforeShellExecution")),
            ("PostToolUse", Some("afterFileEdit")),
        ],
    },
    Host {
        id: "copilot",
        component: "Copilot CLI",
        binaries: &["copilot"],
        markers: &[".copilot"],
        configs: &[
            Loc::HomeDir(".copilot/hooks"),
            Loc::Home(".copilot/settings.json"),
            Loc::ProjectDir(".github/hooks"),
            Loc::Project(".github/copilot/settings.json"),
            Loc::Project(".github/copilot/settings.local.json"),
            Loc::Project(".claude/settings.json"),
            Loc::Project(".claude/settings.local.json"),
        ],
        claude_aliases: &[],
    },
    Host {
        id: "antigravity",
        component: "Antigravity",
        binaries: &["agy"],
        markers: &[".gemini/config/hooks.json"],
        configs: &[
            Loc::Home(".gemini/config/hooks.json"),
            Loc::Project(".agents/hooks.json"),
        ],
        claude_aliases: &[
            ("UserPromptSubmit", Some("PreInvocation")),
            ("PreCompact", None),
            ("SessionEnd", None),
        ],
    },
];

/// Hook-author capability served by each event name, across hosts.
fn capability_of(event: &str) -> Option<&'static str> {
    Some(match event {
        "SessionStart" | "sessionStart" => "session-start-context-inject",
        "UserPromptSubmit" | "BeforeAgent" | "userPromptSubmitted" | "beforeSubmitPrompt" => {
            "prompt-submit-context-inject"
        }
        "Stop" | "AfterAgent" | "agentStop" | "stop" => "turn-end-block-and-continue",
        "PreCompact" | "PreCompress" | "preCompact" => "precompact-event",
        "SessionEnd" | "sessionEnd" => "session-end-event",
        _ => return None,
    })
}

/// Words in a turn-end command that suggest the author expected session exit.
const SESSION_EXIT_WORDS: &[&str] = &[
    "session-end",
    "session_end",
    "sessionend",
    "end-session",
    "on-exit",
    "on_exit",
    "shutdown",
    "teardown",
    "cleanup",
    "logout",
];

pub struct Env {
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub path: Option<OsString>,
    pub today: i64,
}

pub struct Options {
    pub host: Option<String>,
    pub json: bool,
    pub stale_days: u64,
    pub registry: Option<String>,
}

pub fn cli(args: &[String]) -> Result<String, String> {
    if args.first().map(String::as_str) != Some("doctor") {
        return Err(USAGE.into());
    }
    let mut o = Options {
        host: None,
        json: false,
        stale_days: DEFAULT_STALE_DAYS,
        registry: None,
    };
    let mut cwd = None;
    let mut seen = Vec::new();
    let mut it = args[1..].iter();
    while let Some(flag) = it.next() {
        if flag != "--json" && seen.contains(flag) {
            return Err(format!("duplicate option: {flag}"));
        }
        seen.push(flag.clone());
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("missing value for {flag}\n{USAGE}"))
        };
        match flag.as_str() {
            "--json" => o.json = true,
            "--host" => o.host = Some(value()?),
            "--registry" => o.registry = Some(value()?),
            "--cwd" => cwd = Some(PathBuf::from(value()?)),
            "--stale-days" => {
                let v = value()?;
                o.stale_days = v
                    .parse()
                    .map_err(|_| format!("--stale-days needs a whole number, got '{v}'"))?;
            }
            other => return Err(format!("unknown hooks doctor option: {other}\n{USAGE}")),
        }
    }
    let env = Env {
        home: std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is not set")?,
        cwd: match cwd {
            Some(c) => c,
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        },
        path: std::env::var_os("PATH"),
        today: compat::today_days(),
    };
    let report = doctor(&env, &o)?;
    if o.json {
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
    } else {
        Ok(render(&report))
    }
}

/// MCP `hooks_doctor` arguments. Uses the server's HOME, PATH and cwd.
pub fn mcp(args: &Value) -> Result<Value, String> {
    let obj = args
        .as_object()
        .ok_or("hooks_doctor arguments must be an object")?;
    for k in obj.keys() {
        if !["host", "stale_days", "cwd", "registry"].contains(&k.as_str()) {
            return Err(format!("unknown hooks_doctor argument: {k}"));
        }
    }
    let text = |k: &str| -> Result<Option<String>, String> {
        match obj.get(k) {
            None => Ok(None),
            Some(v) => v
                .as_str()
                .map(|s| Some(s.to_string()))
                .ok_or(format!("{k} must be a string")),
        }
    };
    let stale_days = match obj.get("stale_days") {
        None => DEFAULT_STALE_DAYS,
        Some(v) => v.as_u64().ok_or("stale_days must be a whole number")?,
    };
    let env = Env {
        home: std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is not set")?,
        cwd: match text("cwd")? {
            Some(c) => PathBuf::from(c),
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        },
        path: std::env::var_os("PATH"),
        today: compat::today_days(),
    };
    doctor(
        &env,
        &Options {
            host: text("host")?,
            json: true,
            stale_days,
            registry: text("registry")?,
        },
    )
}

pub fn doctor(env: &Env, o: &Options) -> Result<Value, String> {
    if let Some(h) = &o.host {
        if !HOSTS.iter().any(|x| x.id == h) {
            let ids: Vec<_> = HOSTS.iter().map(|x| x.id).collect();
            return Err(format!(
                "unknown host '{h}'; expected one of {}",
                ids.join(", ")
            ));
        }
    }
    let (bytes, source) = match &o.registry {
        Some(p) => (read_bounded(Path::new(p))?, tilde(Path::new(p), &env.home)),
        None => (
            EMBEDDED_REGISTRY.as_bytes().to_vec(),
            "embedded compatibility/registry.json".to_string(),
        ),
    };
    let doc: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid registry JSON: {e}"))?;
    compat::validate(&doc)?;
    let records: Vec<Value> = doc["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["section"] == "agent-hooks")
        .cloned()
        .collect();

    let mut hosts = Vec::new();
    let mut totals = BTreeMap::from([("error", 0), ("warning", 0), ("info", 0)]);
    for host in HOSTS
        .iter()
        .filter(|h| o.host.as_deref().is_none_or(|id| id == h.id))
    {
        let r = check_host(host, env, &records, o.stale_days);
        for f in r["findings"].as_array().unwrap() {
            *totals
                .entry(match f["severity"].as_str().unwrap() {
                    "error" => "error",
                    "warning" => "warning",
                    _ => "info",
                })
                .or_default() += 1;
        }
        hosts.push(r);
    }
    Ok(json!({
        "schema_version": 1,
        "registry": source,
        "registry_sha256": format!("{:x}", Sha256::digest(&bytes)),
        "agent_hooks_records": records.len(),
        "stale_days": o.stale_days,
        "live_verified": false,
        "read_only": true,
        "hosts": hosts,
        "summary": totals,
    }))
}

fn check_host(host: &Host, env: &Env, records: &[Value], stale_days: u64) -> Value {
    let mine: Vec<&Value> = records
        .iter()
        .filter(|r| host_version(r, host.component).is_some())
        .collect();
    let binary = host
        .binaries
        .iter()
        .find_map(|b| which_in(env.path.as_ref(), b));
    let marker = host
        .markers
        .iter()
        .any(|m| fs::symlink_metadata(env.home.join(m)).is_ok());
    let version = binary.as_ref().and_then(|p| {
        crate::acp::run_version(
            &[p.to_string_lossy().into_owned(), "--version".into()],
            VERSION_TIMEOUT,
        )
    });
    let mut findings = Vec::new();
    let mut configs = Vec::new();
    let detected = binary.is_some() || marker;

    if detected {
        let mut paths = config_paths(host, env);
        if host.id == "codex" {
            paths.extend(codex_plugin_hooks(&env.home, &mut findings));
        }
        let events = event_set(&mine);
        let mut seen = Vec::new();
        for (path, scope) in paths {
            let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
            if seen.contains(&canon) {
                continue;
            }
            seen.push(canon);
            if let Some(c) = check_config(host, &path, scope, env, &events, &mine, &mut findings) {
                configs.push(c);
            }
        }
        drift_and_freshness(
            host,
            version.as_deref(),
            &mine,
            stale_days,
            env.today,
            &mut findings,
        );
    }

    json!({
        "id": host.id,
        "component": host.component,
        "detected": detected,
        "binary": binary.as_ref().map(|p| tilde(p, &env.home)),
        "version": version,
        "records": mine.len(),
        "configs": configs,
        "findings": findings,
    })
}

/// Version recorded for `component` in a record, if the record names it.
fn host_version<'a>(record: &'a Value, component: &str) -> Option<&'a str> {
    record["components"].as_array()?.iter().find_map(|c| {
        c["name"]
            .as_str()
            .filter(|n| n.eq_ignore_ascii_case(component))
            .and(c["version"].as_str())
    })
}

fn support(records: &[&Value], capability: &str) -> Option<(String, String)> {
    let r = records.iter().find(|r| r["capability"] == capability)?;
    let s = r["validity"]["capabilities"]
        .as_array()?
        .iter()
        .find_map(|c| c.as_str()?.strip_prefix("support:"))?;
    Some((s.to_string(), r["id"].as_str()?.to_string()))
}

/// Documented event names (`event:` entries of the hook-config record).
fn event_set(records: &[&Value]) -> Option<(Vec<String>, String)> {
    let r = records
        .iter()
        .find(|r| r["capability"] == "hook-config-location")?;
    let events = r["validity"]["capabilities"]
        .as_array()?
        .iter()
        .filter_map(|c| c.as_str()?.strip_prefix("event:").map(str::to_string))
        .collect();
    Some((events, r["id"].as_str()?.to_string()))
}

fn config_paths(host: &Host, env: &Env) -> Vec<(PathBuf, &'static str)> {
    let mut out = Vec::new();
    let mut dir = |d: PathBuf, scope| {
        if let Ok(entries) = fs::read_dir(&d) {
            let mut files: Vec<_> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect();
            files.sort();
            out.extend(files.into_iter().map(|p| (p, scope)));
        }
    };
    let mut singles = Vec::new();
    for loc in host.configs {
        match *loc {
            Loc::Home(p) => singles.push((env.home.join(p), "user")),
            Loc::Project(p) => singles.push((env.cwd.join(p), "project")),
            Loc::System(p) => singles.push((PathBuf::from(p), "system")),
            Loc::HomeDir(p) => dir(env.home.join(p), "user"),
            Loc::ProjectDir(p) => dir(env.cwd.join(p), "project"),
        }
    }
    out.extend(singles);
    out
}

/// Hooks of plugins enabled in `~/.codex/config.toml`. Minimal line parser:
/// `[plugins."name@marketplace"]` followed by `enabled = true|false`.
fn codex_plugin_hooks(home: &Path, findings: &mut Vec<Value>) -> Vec<(PathBuf, &'static str)> {
    let Ok(text) = fs::read_to_string(home.join(".codex/config.toml")) else {
        return Vec::new();
    };
    let mut enabled = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            // `[hooks.state."…"]` holds trust-review state, not hook definitions.
            let inline =
                (line == "[hooks]" || line.starts_with("[hooks.") || line.starts_with("[[hooks"))
                    && !line.starts_with("[hooks.state");
            if inline
                && !findings
                    .iter()
                    .any(|f| f["kind"] == "inline-hooks-not-parsed")
            {
                findings.push(finding(
                    "info",
                    "inline-hooks-not-parsed",
                    "~/.codex/config.toml has an inline [hooks] table; this doctor reads hooks.json files only.",
                    &[],
                ));
            }
            current = line
                .strip_prefix("[plugins.\"")
                .and_then(|r| r.strip_suffix("\"]"))
                .and_then(|r| r.rsplit_once('@'))
                .map(|(n, m)| (n.to_string(), m.to_string()));
            continue;
        }
        if let Some((name, market)) = &current {
            if let Some(v) = line.strip_prefix("enabled") {
                if v.trim_start().strip_prefix('=').map(str::trim) == Some("true") {
                    enabled.push((name.clone(), market.clone()));
                }
            }
        }
    }
    let mut out = Vec::new();
    for (name, market) in enabled {
        let base = home.join(".codex/plugins/cache").join(&market).join(&name);
        // Several cached versions can coexist; the newest directory is the one in use.
        let newest = fs::read_dir(&base).ok().and_then(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
                .map(|e| e.path())
        });
        if let Some(dir) = newest {
            let hooks = dir.join("hooks/hooks.json");
            if hooks.is_file() {
                out.push((hooks, "plugin"));
            }
        }
    }
    out
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("not a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err("exceeds 1 MiB limit".into());
    }
    Ok(bytes)
}

fn check_config(
    host: &Host,
    path: &Path,
    scope: &str,
    env: &Env,
    events: &Option<(Vec<String>, String)>,
    records: &[&Value],
    findings: &mut Vec<Value>,
) -> Option<Value> {
    if !path.exists() {
        return None;
    }
    let shown = tilde(path, &env.home);
    let doc: Value = match read_bounded(path)
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| format!("invalid JSON: {e}")))
    {
        Ok(d) => d,
        Err(e) => {
            findings.push(with(
                finding(
                    "warning",
                    "config-unreadable",
                    &format!("{shown}: {e}"),
                    &[],
                ),
                "config",
                &shown,
            ));
            return Some(json!({"path": shown, "scope": scope, "error": e}));
        }
    };
    let mut counts = Map::new();
    let Some(hooks) = doc.get("hooks").and_then(Value::as_object) else {
        return Some(json!({"path": shown, "scope": scope, "events": counts}));
    };
    for (event, groups) in hooks {
        let Some(groups) = groups.as_array() else {
            continue; // e.g. Gemini's non-event settings under `hooks`
        };
        let commands = commands_of(groups);
        counts.insert(event.clone(), json!(commands.len()));
        let at = |f: Value| with(with(f, "config", &shown), "event", event);

        if let Some((known, rid)) = events {
            if !known.iter().any(|k| k == event) {
                findings.push(at(unknown_event(host, event, rid)));
                continue;
            }
        }
        if let Some(cap) = capability_of(event) {
            if let Some((s, rid)) = support(records, cap) {
                let msg = match (cap, s.as_str()) {
                    ("prompt-submit-context-inject", "no") => Some(format!(
                        "{} cannot inject context from {event}; output other than its documented fields is ignored. Measure or inject at another event.",
                        host.component
                    )),
                    (_, "no") => Some(format!(
                        "{} records {cap} as unsupported; this {event} hook cannot do what the capability names.",
                        host.component
                    )),
                    (_, "partial") => Some(format!(
                        "{} supports {cap} only partially for {event}; see the record result.",
                        host.component
                    )),
                    _ => None,
                };
                if let Some(m) = msg {
                    let sev = if s == "no" { "warning" } else { "info" };
                    findings.push(at(finding(sev, "output-not-honoured", &m, &[&rid])));
                }
            }
            if cap == "turn-end-block-and-continue" {
                let rid = support(records, cap).map(|(_, r)| r);
                let ids: Vec<&str> = rid.iter().map(String::as_str).collect();
                for c in &commands {
                    let lower = c.to_ascii_lowercase();
                    if let Some(w) = SESSION_EXIT_WORDS.iter().find(|w| lower.contains(*w)) {
                        findings.push(with(at(finding(
                            "warning",
                            "turn-end-assumes-session-exit",
                            &format!("{event} runs at the end of every turn, not at session exit, but the command mentions '{w}'. Move exit work to a session-end event or make it idempotent per turn."),
                            &ids,
                        )), "command", &redact(c, &env.home)));
                    }
                }
                let json_only = records.iter().any(|r| {
                    r["capability"] == cap
                        && r["validity"]["capabilities"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|v| v == "stop-requires-json"))
                });
                if json_only && !commands.is_empty() {
                    findings.push(at(finding(
                        "info",
                        "turn-end-requires-json",
                        &format!(
                            "{} {event} hooks must print JSON on stdout; plain text is invalid.",
                            host.component
                        ),
                        &ids,
                    )));
                }
            }
        }
        for c in &commands {
            if let Some(f) = npx_finding(c) {
                findings.push(with(at(f), "command", &redact(c, &env.home)));
            }
        }
    }
    Some(json!({"path": shown, "scope": scope, "events": counts}))
}

fn unknown_event(host: &Host, event: &str, rid: &str) -> Value {
    if let Some((_, alias)) = host.claude_aliases.iter().find(|(c, _)| *c == event) {
        let hint = match alias {
            Some(a) => format!("use {a}"),
            None => "no equivalent event is documented".into(),
        };
        return finding(
            "error",
            "unsupported-event",
            &format!(
                "{} has no {event} event ({hint}); this hook never fires.",
                host.component
            ),
            &[rid],
        );
    }
    // An event name another host documents (e.g. Gemini's BeforeAgent in a
    // Codex file), or a role event in the wrong dialect, never fires here.
    let owner = HOSTS
        .iter()
        .find(|h| h.id != host.id && h.claude_aliases.iter().any(|(_, a)| *a == Some(event)));
    if let Some(o) = owner {
        return finding(
            "error",
            "unsupported-event",
            &format!(
                "{event} is a {} event name; {} does not document it, so this hook never fires.",
                o.component, host.component
            ),
            &[rid],
        );
    }
    if capability_of(event).is_some() {
        return finding(
            "error",
            "unsupported-event",
            &format!(
                "{} does not document {event}; this hook never fires.",
                host.component
            ),
            &[rid],
        );
    }
    finding(
        "warning",
        "unrecognized-event",
        &format!(
            "{event} is not in the documented {} event list; check the host's docs.",
            host.component
        ),
        &[rid],
    )
}

fn npx_finding(command: &str) -> Option<Value> {
    let toks: Vec<&str> = command.split_whitespace().collect();
    let at = toks.iter().enumerate().find_map(|(i, t)| {
        let base = t.rsplit('/').next().unwrap_or(t);
        match base {
            "npx" | "bunx" => Some(i + 1),
            "npm" | "pnpm" if toks.get(i + 1).is_some_and(|n| *n == "exec" || *n == "dlx") => {
                Some(i + 2)
            }
            _ => None,
        }
    })?;
    let package = toks[at..].iter().find(|t| !t.starts_with('-'))?;
    // `@scope/name@1.2.3` or `name@1.2.3` pins; a bare name or a dist-tag does not.
    let pinned = package
        .trim_start_matches('@')
        .split_once('@')
        .is_some_and(|(_, v)| v.starts_with(|c: char| c.is_ascii_digit()));
    Some(if pinned {
        finding(
            "info",
            "npx-package",
            &format!("Runs {package} through a package runner with a pinned version; first run needs the network."),
            &[],
        )
    } else {
        finding(
            "warning",
            "npx-package",
            &format!("Runs {package} through a package runner without a pinned version; it may run a stale cached copy or change on the next release."),
            &[],
        )
    })
}

fn drift_and_freshness(
    host: &Host,
    installed: Option<&str>,
    records: &[&Value],
    stale_days: u64,
    today: i64,
    findings: &mut Vec<Value>,
) {
    if records.is_empty() {
        findings.push(finding(
            "info",
            "no-records",
            &format!(
                "No agent-hooks records name {}; coverage is unknown.",
                host.component
            ),
            &[],
        ));
        return;
    }
    if let Some(v) = installed {
        let drifted: Vec<&str> = records
            .iter()
            .filter(|r| {
                host_version(r, host.component).is_some_and(|rv| {
                    rv != "unknown" && crate::acp::extract_version(rv).as_deref().unwrap_or(rv) != v
                })
            })
            .filter_map(|r| r["id"].as_str())
            .collect();
        if !drifted.is_empty() {
            findings.push(finding(
                "warning",
                "version-drift",
                &format!("Installed {} {v} is outside the version these records observed; re-check before relying on them.", host.component),
                &drifted,
            ));
        }
    }
    let unscoped = records
        .iter()
        .filter(|r| host_version(r, host.component) == Some("unknown"))
        .count();
    if unscoped > 0 {
        findings.push(finding(
            "info",
            "unscoped-records",
            &format!("{unscoped} of {} {} records have no host version scope (documentation evidence); they cannot detect drift.", records.len(), host.component),
            &[],
        ));
    }
    let mut copies: Vec<Value> = records.iter().map(|r| (*r).clone()).collect();
    compat::annotate_stale(&mut copies, stale_days, today);
    let stale: Vec<&str> = copies
        .iter()
        .filter(|r| r["freshness"]["stale"] == true)
        .filter_map(|r| r["id"].as_str())
        .collect();
    if !stale.is_empty() {
        findings.push(finding(
            "info",
            "stale-records",
            &format!(
                "{} {} records have no evidence newer than {stale_days} days.",
                stale.len(),
                host.component
            ),
            &stale,
        ));
    }
}

fn commands_of(groups: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    let mut take = |h: &Value| {
        for k in ["command", "bash", "powershell"] {
            if let Some(c) = h.get(k).and_then(Value::as_str) {
                out.push(c.to_string());
                return;
            }
        }
    };
    for g in groups {
        match g.get("hooks").and_then(Value::as_array) {
            Some(inner) => inner.iter().for_each(&mut take),
            None => take(g),
        }
    }
    out
}

fn finding(severity: &str, kind: &str, message: &str, records: &[&str]) -> Value {
    json!({"severity": severity, "kind": kind, "message": message, "record_ids": records})
}

fn with(mut f: Value, key: &str, value: &str) -> Value {
    f[key] = json!(value);
    f
}

fn which_in(path: Option<&OsString>, bin: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path?).map(|d| d.join(bin)).find(|p| {
        fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Shorten and redact a hook command for display. Values of `NAME=value`
/// assignments, values after secret-looking flags, and long opaque tokens are
/// replaced; the home directory becomes `~`.
pub fn redact(command: &str, home: &Path) -> String {
    let home = home.to_string_lossy();
    let secretish = |t: &str| {
        let l = t.to_ascii_lowercase();
        [
            "token",
            "key",
            "secret",
            "password",
            "passwd",
            "auth",
            "bearer",
            "credential",
        ]
        .iter()
        .any(|w| l.contains(w))
    };
    let mut out = Vec::new();
    let mut hide_next = false;
    for tok in command.split_whitespace() {
        if hide_next {
            out.push("<redacted>".to_string());
            hide_next = false;
            continue;
        }
        let tok = if home.len() > 1 {
            tok.replace(home.as_ref(), "~")
        } else {
            tok.to_string()
        };
        if let Some((name, value)) = tok.split_once('=') {
            let ident = !name.is_empty()
                && name
                    .trim_start_matches('-')
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            if ident && !value.is_empty() {
                out.push(format!("{name}=<redacted>"));
                continue;
            }
        }
        if tok.starts_with('-') && secretish(&tok) {
            hide_next = true;
            out.push(tok);
            continue;
        }
        let opaque = tok.len() >= 24
            && !tok.contains('/')
            && tok
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.+=".contains(c));
        let bearer = [
            "sk-",
            "ghp_",
            "gho_",
            "github_pat_",
            "xoxb-",
            "xoxp-",
            "AKIA",
        ]
        .iter()
        .any(|p| tok.trim_matches(|c| c == '"' || c == '\'').starts_with(p));
        out.push(if opaque || bearer {
            "<redacted>".into()
        } else {
            tok
        });
    }
    let joined = out.join(" ");
    if joined.chars().count() > 160 {
        format!("{}…", joined.chars().take(160).collect::<String>())
    } else {
        joined
    }
}

fn render(report: &Value) -> String {
    let mut s = String::new();
    for h in report["hosts"].as_array().unwrap() {
        let state = if h["detected"] == true {
            format!(
                "{} {}",
                h["binary"].as_str().unwrap_or("(no binary)"),
                h["version"].as_str().unwrap_or("version unknown")
            )
        } else {
            "not detected".into()
        };
        s.push_str(&format!(
            "{} ({}): {state}\n",
            h["id"].as_str().unwrap(),
            h["component"].as_str().unwrap()
        ));
        for c in h["configs"].as_array().unwrap() {
            let events: Vec<String> = c["events"]
                .as_object()
                .map(|m| m.iter().map(|(k, v)| format!("{k}×{v}")).collect())
                .unwrap_or_default();
            s.push_str(&format!(
                "  [{}] {}  {}\n",
                c["scope"].as_str().unwrap_or("?"),
                c["path"].as_str().unwrap_or("?"),
                events.join(" ")
            ));
        }
        for f in h["findings"].as_array().unwrap() {
            s.push_str(&format!(
                "  {:7} {}: {}\n",
                f["severity"].as_str().unwrap(),
                f["kind"].as_str().unwrap(),
                f["message"].as_str().unwrap()
            ));
            if let Some(c) = f["command"].as_str() {
                s.push_str(&format!("          command: {c}\n"));
            }
        }
    }
    s.push_str(&format!(
        "summary: {} error, {} warning, {} info · records {} · stale after {} days · read-only, live_verified false",
        report["summary"]["error"],
        report["summary"]["warning"],
        report["summary"]["info"],
        report["agent_hooks_records"],
        report["stale_days"]
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Fixture {
        root: PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn fake_bin(dir: &Path, name: &str, out: &str) {
        let p = dir.join(name);
        write(&p, &format!("#!/bin/sh\necho '{out}'\n"));
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn fixture(tag: &str) -> (Fixture, Env) {
        let root = std::env::temp_dir().join(format!("hooks-doctor-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let cwd = root.join("project");
        let bin = root.join("bin");
        fs::create_dir_all(&cwd).unwrap();
        fake_bin(&bin, "claude", "2.1.306 (Claude Code)");
        fake_bin(&bin, "codex", "codex-cli 0.165.0");
        fake_bin(&bin, "gemini", "0.64.0");
        write(
            &home.join(".claude/settings.json"),
            r#"{"hooks":{
              "Stop":[{"hooks":[{"type":"command","command":"bash ~/scripts/session-cleanup.sh"}]}],
              "UserPromptSubmit":[{"hooks":[{"type":"command","command":"API_TOKEN=abc123 npx some-hook-pkg check"}]}],
              "SessionStart":[{"hooks":[{"type":"command","command":"npx @scope/pinned@1.2.3 --key hunter2"}]}]
            }}"#,
        );
        write(
            &home.join(".codex/hooks.json"),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"bookmark restore --host codex"}]}],
                       "Stop":[{"hooks":[{"type":"command","command":"bookmark stop --host codex"}]}]}}"#,
        );
        write(
            &home.join(".codex/config.toml"),
            "[plugins.\"demo@local\"]\nenabled = true\n\n[plugins.\"off@local\"]\nenabled = false\n\n[hooks.state.\"a\"]\ntrusted = true\n[hooks.state.\"b\"]\ntrusted = true\n",
        );
        write(
            &home.join(".codex/plugins/cache/local/demo/1.0.0/hooks/hooks.json"),
            r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"demo-guard"}]}]}}"#,
        );
        write(
            &home.join(".codex/plugins/cache/local/off/1.0.0/hooks/hooks.json"),
            r#"{"hooks":{"Bogus":[{"hooks":[{"type":"command","command":"x"}]}]}}"#,
        );
        write(
            &home.join(".gemini/settings.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"handoff"}]}],
                         "SessionStart":[{"hooks":[{"type":"command","command":"restore"}]}]}}"#,
        );
        let env = Env {
            home,
            cwd,
            path: Some(bin.into_os_string()),
            today: compat::civil_days("2026-10-28").unwrap(),
        };
        (Fixture { root }, env)
    }

    fn opts(host: Option<&str>) -> Options {
        Options {
            host: host.map(str::to_string),
            json: true,
            stale_days: 30,
            registry: None,
        }
    }

    fn host<'a>(report: &'a Value, id: &str) -> &'a Value {
        report["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["id"] == id)
            .unwrap()
    }

    fn kinds(h: &Value, severity: &str) -> Vec<String> {
        h["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["severity"] == severity)
            .map(|f| f["kind"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn fixture_hosts_produce_expected_findings() {
        let (_fx, env) = fixture("all");
        let report = doctor(&env, &opts(None)).unwrap();
        let text = serde_json::to_string(&report).unwrap();
        // Read-only output never leaks the temp home, secrets or env values.
        assert!(!text.contains(env.home.to_str().unwrap()));
        assert!(!text.contains("abc123") && !text.contains("hunter2"));

        let claude = host(&report, "claude");
        assert_eq!(claude["version"], "2.1.306");
        assert_eq!(claude["configs"][0]["path"], "~/.claude/settings.json");
        let warn = kinds(claude, "warning");
        assert!(
            warn.contains(&"turn-end-assumes-session-exit".to_string()),
            "{warn:?}"
        );
        assert_eq!(
            warn.iter().filter(|k| *k == "npx-package").count(),
            1,
            "{warn:?}"
        );
        assert!(kinds(claude, "info").contains(&"npx-package".to_string()));
        assert!(kinds(claude, "error").is_empty());

        let gemini = host(&report, "gemini");
        let errs: Vec<&Value> = gemini["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["severity"] == "error")
            .collect();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0]["kind"], "unsupported-event");
        assert_eq!(errs[0]["event"], "Stop");
        assert!(errs[0]["message"].as_str().unwrap().contains("AfterAgent"));

        let codex = host(&report, "codex");
        assert_eq!(codex["version"], "0.165.0");
        assert!(
            kinds(codex, "error").is_empty() && kinds(codex, "warning").is_empty(),
            "{codex}"
        );
        let scopes: Vec<_> = codex["configs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["scope"].clone())
            .collect();
        assert!(
            scopes.contains(&json!("plugin")),
            "enabled plugin hooks are read"
        );
        assert!(!text.contains("Bogus"), "disabled plugin hooks are skipped");
        assert!(
            !text.contains("inline-hooks-not-parsed"),
            "hooks.state is trust state"
        );

        assert_eq!(host(&report, "cursor")["detected"], false);
        assert_eq!(report["live_verified"], false);
    }

    #[test]
    fn drift_and_staleness_come_from_records() {
        let (fx, mut env) = fixture("drift");
        fake_bin(&fx.root.join("bin"), "codex", "codex-cli 0.170.0");
        let codex = &doctor(&env, &opts(Some("codex"))).unwrap()["hosts"][0];
        assert!(kinds(codex, "warning").contains(&"version-drift".to_string()));
        env.today = compat::civil_days("2027-01-01").unwrap();
        let codex = &doctor(&env, &opts(Some("codex"))).unwrap()["hosts"][0];
        assert!(kinds(codex, "info").contains(&"stale-records".to_string()));
    }

    #[test]
    fn prompt_submit_on_cursor_is_flagged_and_bad_args_rejected() {
        let (_fx, env) = fixture("cursor");
        write(
            &env.home.join(".cursor/hooks.json"),
            r#"{"version":1,"hooks":{"beforeSubmitPrompt":[{"command":"inject.sh"}],"UserPromptSubmit":[{"command":"x"}]}}"#,
        );
        let cursor = &doctor(&env, &opts(Some("cursor"))).unwrap()["hosts"][0];
        assert!(kinds(cursor, "warning").contains(&"output-not-honoured".to_string()));
        assert!(kinds(cursor, "error").contains(&"unsupported-event".to_string()));
        assert!(doctor(&env, &opts(Some("nobody"))).is_err());
        assert!(cli(&["doctor".into(), "--stale-days".into(), "x".into()]).is_err());
        assert!(cli(&["doctor".into(), "--bogus".into()]).is_err());
        assert!(mcp(&json!({"x": 1})).is_err());
        assert!(mcp(&json!({"stale_days": "3"})).is_err());
    }

    #[test]
    fn redaction_hides_values_and_home() {
        let home = Path::new("/Users/someone");
        let r = redact(
            "FOO=bar /Users/someone/bin/x --api-key s3cr3t --token=zzz sk-abcdefghijklmnop ghp_123 aaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            home,
        );
        assert_eq!(
            r,
            "FOO=<redacted> ~/bin/x --api-key <redacted> --token=<redacted> <redacted> <redacted> <redacted>"
        );
    }

    #[test]
    fn embedded_registry_has_hook_config_records_for_every_scanned_host() {
        let doc: Value = serde_json::from_str(EMBEDDED_REGISTRY).unwrap();
        compat::validate(&doc).unwrap();
        let recs: Vec<Value> = doc["records"].as_array().unwrap().clone();
        for h in HOSTS {
            let mine: Vec<&Value> = recs
                .iter()
                .filter(|r| host_version(r, h.component).is_some())
                .collect();
            assert!(
                event_set(&mine).is_some(),
                "{} lacks a hook-config-location record",
                h.id
            );
        }
    }
}

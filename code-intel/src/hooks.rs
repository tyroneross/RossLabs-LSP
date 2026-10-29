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

pub const USAGE: &str = "usage: hooks doctor [--host ID] [--json] [--drift] [--stale-days N] [--cwd DIR] [--registry PATH]";

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
    /// Report only installed-vs-observed host versions and the records to re-verify.
    pub drift: bool,
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
        drift: false,
        stale_days: DEFAULT_STALE_DAYS,
        registry: None,
    };
    let mut cwd = None;
    let mut seen = Vec::new();
    let mut it = args[1..].iter();
    while let Some(flag) = it.next() {
        if flag != "--json" && flag != "--drift" && seen.contains(flag) {
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
            "--drift" => o.drift = true,
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
    } else if o.drift {
        Ok(render_drift(&report))
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
        if !["host", "stale_days", "cwd", "registry", "drift"].contains(&k.as_str()) {
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
    let drift = match obj.get("drift") {
        None => false,
        Some(v) => v.as_bool().ok_or("drift must be a boolean")?,
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
            drift,
            stale_days,
            registry: text("registry")?,
        },
    )
}

fn check_host_id(host: Option<&str>) -> Result<(), String> {
    if let Some(h) = host {
        if !HOSTS.iter().any(|x| x.id == h) {
            let ids: Vec<_> = HOSTS.iter().map(|x| x.id).collect();
            return Err(format!(
                "unknown host '{h}'; expected one of {}",
                ids.join(", ")
            ));
        }
    }
    Ok(())
}

/// Registry bytes, display label, and the full validated document.
fn load_registry(registry: Option<&str>, home: &Path) -> Result<(Vec<u8>, String, Value), String> {
    let (bytes, source) = match registry {
        Some(p) => (read_bounded(Path::new(p))?, tilde(Path::new(p), home)),
        None => (
            EMBEDDED_REGISTRY.as_bytes().to_vec(),
            "embedded compatibility/registry.json".to_string(),
        ),
    };
    let doc: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid registry JSON: {e}"))?;
    compat::validate(&doc)?;
    Ok((bytes, source, doc))
}

/// Current (not superseded) agent-hooks records.
fn hook_records(doc: &Value) -> Vec<Value> {
    let section: Vec<Value> = doc["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["section"] == "agent-hooks")
        .cloned()
        .collect();
    compat::current_records(&section)
}

/// Binary path, home-marker presence, and `--version` output for a host.
fn detect(host: &Host, env: &Env) -> (Option<PathBuf>, bool, Option<String>) {
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
    (binary, marker, version)
}

pub fn doctor(env: &Env, o: &Options) -> Result<Value, String> {
    check_host_id(o.host.as_deref())?;
    let (bytes, source, doc) = load_registry(o.registry.as_deref(), &env.home)?;
    let records = hook_records(&doc);
    if o.drift {
        return Ok(drift_report(env, o, &records, source, &bytes));
    }

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
    let (binary, marker, version) = detect(host, env);
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

/// Normalised host version a record observed; None when unscoped (`unknown`).
fn observed_version(record: &Value, component: &str) -> Option<String> {
    let rv = host_version(record, component).filter(|v| *v != "unknown")?;
    Some(crate::acp::extract_version(rv).unwrap_or_else(|| rv.to_string()))
}

/// Version-scoped records whose observed host version differs from `installed`.
fn drifted_ids<'a>(component: &str, installed: &str, records: &[&'a Value]) -> Vec<&'a str> {
    records
        .iter()
        .filter(|r| observed_version(r, component).is_some_and(|v| v != installed))
        .filter_map(|r| r["id"].as_str())
        .collect()
}

/// `--drift`: per detected host, installed version vs the versions the
/// records observed, and the record ids to re-verify. Skips config parsing.
fn drift_report(env: &Env, o: &Options, records: &[Value], source: String, bytes: &[u8]) -> Value {
    let mut hosts = Vec::new();
    let mut not_detected = Vec::new();
    for host in HOSTS
        .iter()
        .filter(|h| o.host.as_deref().is_none_or(|id| id == h.id))
    {
        let (binary, marker, installed) = detect(host, env);
        if binary.is_none() && !marker {
            not_detected.push(host.id);
            continue;
        }
        let mine: Vec<&Value> = records
            .iter()
            .filter(|r| host_version(r, host.component).is_some())
            .collect();
        let mut observed: Vec<String> = mine
            .iter()
            .filter_map(|r| observed_version(r, host.component))
            .collect();
        observed.sort();
        observed.dedup();
        let reverify = installed
            .as_deref()
            .map(|v| drifted_ids(host.component, v, &mine))
            .unwrap_or_default();
        let status = match (&installed, observed.is_empty()) {
            (None, _) => "version-unknown",
            (_, true) => "unscoped",
            _ if reverify.is_empty() => "current",
            _ => "drift",
        };
        hosts.push(json!({
            "id": host.id,
            "component": host.component,
            "installed": installed,
            "observed_versions": observed,
            "status": status,
            "reverify": reverify,
            "refresh": (status == "drift").then(|| format!(
                "rosslabs-lsp compat refresh <registry.json> --section agent-hooks --host {}",
                host.id
            )),
        }));
    }
    let drifted = hosts.iter().filter(|h| h["status"] == "drift").count();
    json!({
        "schema_version": 1,
        "mode": "drift",
        "registry": source,
        "registry_sha256": format!("{:x}", Sha256::digest(bytes)),
        "live_verified": false,
        "read_only": true,
        "hosts": hosts,
        "not_detected": not_detected,
        "drifted_hosts": drifted,
    })
}

fn render_drift(report: &Value) -> String {
    let mut s = String::new();
    for h in report["hosts"].as_array().unwrap() {
        let observed: Vec<&str> = h["observed_versions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        s.push_str(&format!(
            "{} ({}): installed {} · records observed {} · {}\n",
            h["id"].as_str().unwrap(),
            h["component"].as_str().unwrap(),
            h["installed"].as_str().unwrap_or("unknown"),
            if observed.is_empty() {
                "no version".to_string()
            } else {
                observed.join(", ")
            },
            h["status"].as_str().unwrap()
        ));
        for id in h["reverify"].as_array().unwrap() {
            s.push_str(&format!("  re-verify {}\n", id.as_str().unwrap()));
        }
        if let Some(cmd) = h["refresh"].as_str() {
            s.push_str(&format!("  refresh: {cmd}\n"));
        }
    }
    s.push_str(&format!(
        "drift: {} host(s) need re-verification · read-only, live_verified false",
        report["drifted_hosts"]
    ));
    s
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
        let drifted = drifted_ids(host.component, v, records);
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

// ------------------------------------------------------------- refresh

pub const REFRESH_USAGE: &str =
    "usage: compat refresh [<registry.json>] --section agent-hooks --host ID [--dry-run]";
/// Newest transcripts examined when looking for one written by the installed version.
const TRANSCRIPT_SCAN_FILES: usize = 50;
/// Bytes read from the start of one transcript.
const TRANSCRIPT_READ_BYTES: u64 = 8 * 1024 * 1024;

/// One capability re-observed from a local transcript.
struct Fact {
    capability: &'static str,
    support: &'static str,
    result: String,
}

struct Probe {
    source: &'static str,
    files_scanned: usize,
    facts: Vec<Fact>,
}

/// `compat refresh`: re-read local evidence (installed `--version` and
/// transcript field presence) and append revision records for `verified`
/// records whose host version drifted. Never edits or deletes a record,
/// never rewrites `reported` (documentation) records, never uses the network.
pub fn refresh_cli(args: &[String]) -> Result<Value, String> {
    let path = args
        .first()
        .filter(|s| !s.starts_with("--"))
        .map(String::as_str);
    let mut section = None;
    let mut host = None;
    let mut dry_run = false;
    let mut it = args[usize::from(path.is_some())..].iter();
    while let Some(flag) = it.next() {
        let slot = match flag.as_str() {
            "--dry-run" => {
                dry_run = true;
                continue;
            }
            "--section" => &mut section,
            "--host" => &mut host,
            other => {
                return Err(format!(
                    "unknown compat refresh option: {other}\n{REFRESH_USAGE}"
                ))
            }
        };
        if slot.is_some() {
            return Err(format!("duplicate option: {flag}"));
        }
        *slot = Some(
            it.next()
                .ok_or_else(|| format!("missing value for {flag}\n{REFRESH_USAGE}"))?
                .clone(),
        );
    }
    let section = section.ok_or_else(|| format!("--section is required\n{REFRESH_USAGE}"))?;
    if !compat::SECTIONS.contains(&section.as_str()) {
        return Err(compat::unknown_section(&section));
    }
    if section != "agent-hooks" {
        return Err(format!(
            "refresh supports only the agent-hooks section; {section} has no local probe"
        ));
    }
    let host = host.ok_or_else(|| format!("--host is required\n{REFRESH_USAGE}"))?;
    if !dry_run && path.is_none() {
        return Err(format!(
            "refresh writes need a registry path; the embedded registry is read-only (add --dry-run to preview)\n{REFRESH_USAGE}"
        ));
    }
    let env = Env {
        home: std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is not set")?,
        cwd: std::env::current_dir().map_err(|e| e.to_string())?,
        path: std::env::var_os("PATH"),
        today: compat::today_days(),
    };
    refresh(&env, path, &host, dry_run)
}

pub fn refresh(
    env: &Env,
    registry: Option<&str>,
    host_id: &str,
    dry_run: bool,
) -> Result<Value, String> {
    check_host_id(Some(host_id))?;
    let host = HOSTS.iter().find(|h| h.id == host_id).unwrap();
    let (_, source, doc) = load_registry(registry, &env.home)?;
    let mut ids: Vec<String> = doc["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["id"].as_str().map(str::to_string))
        .collect();
    let records = hook_records(&doc);
    let mine: Vec<&Value> = records
        .iter()
        .filter(|r| host_version(r, host.component).is_some())
        .collect();
    let (binary, _, installed) = detect(host, env);
    let installed = installed.ok_or_else(|| {
        format!(
            "{} --version gave no version; refresh needs the installed version",
            host.component
        )
    })?;
    let bin_name = binary
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| host.id.to_string());
    let probe = probe_transcripts(host, &env.home, &installed);
    let date = compat::civil_date(env.today);
    let mut fresh: Vec<Value> = mine.iter().map(|r| (*r).clone()).collect();
    compat::annotate_stale(&mut fresh, DEFAULT_STALE_DAYS, env.today);

    let mut actions = Vec::new();
    for (r, f) in mine.iter().zip(&fresh) {
        let id = r["id"].as_str().unwrap();
        let act =
            |action: &str, reason: String| json!({"id": id, "action": action, "reason": reason});
        if r["status"] == "reported" {
            actions.push(if f["freshness"]["stale"] == true {
                act("flag-stale", format!("documentation evidence older than {DEFAULT_STALE_DAYS} days; re-read the docs by hand (refresh never rewrites reported records)"))
            } else {
                act("keep", "documentation evidence; refresh never rewrites reported records".into())
            });
            continue;
        }
        let Some(observed) = observed_version(r, host.component) else {
            actions.push(act(
                "skip-unscoped",
                "no host version scope; nothing to compare".into(),
            ));
            continue;
        };
        if observed == installed {
            actions.push(act(
                "current",
                format!("observed {observed} matches installed"),
            ));
            continue;
        }
        if r["status"] != "verified" {
            actions.push(act(
                "needs-manual",
                format!(
                    "status {} is re-verified by hand",
                    r["status"].as_str().unwrap_or("?")
                ),
            ));
            continue;
        }
        let capability = r["capability"].as_str().unwrap_or("");
        let fact = match &probe {
            Err(e) => {
                actions.push(act("needs-manual", e.clone()));
                continue;
            }
            Ok(p) => p.facts.iter().find(|x| x.capability == capability),
        };
        let Some(fact) = fact else {
            actions.push(act(
                "needs-manual",
                format!("no local probe for {capability}"),
            ));
            continue;
        };
        let recorded = support(&[*r], capability).map(|(s, _)| s);
        if recorded.as_deref() != Some(fact.support) {
            actions.push(act("contradicted", format!(
                "{} {installed} transcript shows support:{}, record says support:{}; re-verify by hand",
                host.component, fact.support, recorded.as_deref().unwrap_or("none")
            )));
            continue;
        }
        let base = id.split('@').next().unwrap_or(id);
        let mut new_id = format!("{base}@{installed}");
        if ids.contains(&new_id) {
            new_id = format!("{new_id}~{date}");
        }
        if ids.contains(&new_id) {
            actions.push(act(
                "needs-manual",
                format!("revision id {new_id} already exists"),
            ));
            continue;
        }
        let evidence = json!({
            "source": probe.as_ref().map(|p| p.source).unwrap_or("local transcript"),
            "observed_at": date,
            "scope": format!(
                "First {} MiB of one {} {installed} session transcript on one Mac; `{bin_name} --version` reported {installed}; recorded by `compat refresh`",
                TRANSCRIPT_READ_BYTES / (1024 * 1024),
                host.component
            ),
            "result": fact.result,
        });
        let revision = revise(r, host, &new_id, &installed, evidence);
        if !dry_run {
            compat::record_value(registry.unwrap(), &revision)?;
        }
        ids.push(new_id.clone());
        let action = if dry_run {
            "would-refresh"
        } else {
            "refreshed"
        };
        actions.push(json!({"id": id, "action": action, "revision": new_id,
            "reason": format!("{} {installed} transcript re-confirms support:{}", host.component, fact.support)}));
    }
    let refreshed = actions
        .iter()
        .filter(|a| a["action"] == "refreshed" || a["action"] == "would-refresh")
        .count();
    Ok(json!({
        "schema_version": 1,
        "mode": "refresh",
        "host": host.id,
        "component": host.component,
        "installed": installed,
        "registry": source,
        "dry_run": dry_run,
        "probe": match &probe {
            Ok(p) => json!({"source": p.source, "files_scanned": p.files_scanned}),
            Err(e) => json!({"error": e}),
        },
        "actions": actions,
        "refreshed": refreshed,
        "written": !dry_run && refreshed > 0,
        "network": false,
        "live_verified": false,
    }))
}

/// A new revision of `old` at the installed version: old evidence kept, the
/// new observation appended, and `supersedes` naming the record it replaces.
fn revise(old: &Value, host: &Host, new_id: &str, installed: &str, evidence: Value) -> Value {
    let mut n = old.clone();
    n["id"] = json!(new_id);
    n["supersedes"] = old["id"].clone();
    for c in n["components"].as_array_mut().unwrap() {
        if c["name"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case(host.component))
        {
            c["version"] = json!(installed);
        }
    }
    let date = evidence["observed_at"].as_str().unwrap_or("").to_string();
    n["evidence"].as_array_mut().unwrap().push(evidence);
    n["validity"]["snapshot"] = json!(format!("local files read {date}"));
    n
}

/// Find the newest local transcript written by `installed` and read which
/// fields it carries. Only field presence leaves this function.
fn probe_transcripts(host: &Host, home: &Path, installed: &str) -> Result<Probe, String> {
    let (root, depth, source) = match host.id {
        "claude" => (
            home.join(".claude/projects"),
            1,
            "local transcript under the Claude projects directory",
        ),
        "codex" => (
            home.join(".codex/sessions"),
            3,
            "local Codex rollout JSONL under the Codex sessions directory",
        ),
        _ => return Err(format!("no local transcript probe for {}", host.component)),
    };
    let mut files = Vec::new();
    jsonl_files(&root, depth, &mut files);
    files.sort_by_key(|a| std::cmp::Reverse(a.0));
    files.truncate(TRANSCRIPT_SCAN_FILES);
    let mut scanned = 0;
    for (_, path) in &files {
        scanned += 1;
        if let Some(facts) = scan_transcript(host.id, path, installed) {
            return Ok(Probe {
                source,
                files_scanned: scanned,
                facts,
            });
        }
    }
    Err(format!(
        "none of the {scanned} newest transcripts was written by {} {installed}",
        host.component
    ))
}

/// `*.jsonl` files exactly `depth` directories below `dir`, with mtimes.
fn jsonl_files(dir: &Path, depth: usize, out: &mut Vec<(std::time::SystemTime, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(Result::ok) {
        let Ok(meta) = fs::symlink_metadata(e.path()) else {
            continue;
        };
        if depth == 0 {
            if meta.is_file() && e.path().extension().is_some_and(|x| x == "jsonl") {
                if let Ok(m) = meta.modified() {
                    out.push((m, e.path()));
                }
            }
        } else if meta.is_dir() {
            jsonl_files(&e.path(), depth - 1, out);
        }
    }
}

/// Whether any object at or below `v` has one of `keys` (bounded depth).
fn has_key(v: &Value, keys: &[&str], depth: usize) -> bool {
    const MAX_DEPTH: usize = 32;
    if depth > MAX_DEPTH {
        return false;
    }
    match v {
        Value::Object(m) => m
            .iter()
            .any(|(k, x)| keys.contains(&k.as_str()) || has_key(x, keys, depth + 1)),
        Value::Array(a) => a.iter().any(|x| has_key(x, keys, depth + 1)),
        _ => false,
    }
}

fn scan_transcript(host_id: &str, path: &Path, installed: &str) -> Option<Vec<Fact>> {
    use std::io::BufRead;
    let file = fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file.take(TRANSCRIPT_READ_BYTES));
    let mut version: Option<String> = None;
    let (mut usage, mut window) = (false, None::<String>);
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line).ok()? > 0 {
        let text = String::from_utf8_lossy(&line);
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            match host_id {
                "claude" => {
                    if version.is_none() {
                        version = v["version"].as_str().map(str::to_string);
                    }
                    let u = &v["message"]["usage"];
                    usage |= v["type"] == "assistant"
                        && [
                            "input_tokens",
                            "output_tokens",
                            "cache_read_input_tokens",
                            "cache_creation_input_tokens",
                        ]
                        .iter()
                        .all(|k| !u[k].is_null());
                    if window.is_none() && has_key(&v, &["context_window", "max_input_tokens"], 0) {
                        window = Some(String::new());
                    }
                }
                _ => {
                    let p = &v["payload"];
                    if version.is_none() && v["type"] == "session_meta" {
                        version = p["cli_version"].as_str().map(str::to_string);
                    }
                    if v["type"] == "event_msg" && p["type"] == "token_count" {
                        let info = &p["info"];
                        usage |= !info["last_token_usage"].is_null()
                            && !info["total_token_usage"].is_null();
                        if window.is_none() {
                            window = info["model_context_window"].as_u64().map(|n| n.to_string());
                        }
                    }
                }
            }
        }
        // A transcript from another version is not evidence for this one.
        if version.as_deref().is_some_and(|v| v != installed) {
            return None;
        }
        line.clear();
    }
    if version.as_deref() != Some(installed) {
        return None;
    }
    let yes_no = |b: bool| if b { "yes" } else { "no" };
    Some(match host_id {
        "claude" => vec![
            Fact {
                capability: "transcript-token-usage",
                support: yes_no(usage),
                result: if usage {
                    "Assistant message records carry message.usage with input_tokens, output_tokens, cache_read_input_tokens and cache_creation_input_tokens.".into()
                } else {
                    "No assistant message record carries all four message.usage token fields."
                        .into()
                },
            },
            Fact {
                capability: "transcript-context-window",
                support: yes_no(window.is_some()),
                result: if window.is_some() {
                    "A context_window or max_input_tokens field appears in the transcript.".into()
                } else {
                    "No context_window or max_input_tokens field appears in the transcript; window must come from a table, catalog or override.".into()
                },
            },
        ],
        _ => vec![
            Fact {
                capability: "transcript-token-usage",
                support: yes_no(usage),
                result: if usage {
                    "Rollout event_msg records with payload.type token_count carry info.last_token_usage and info.total_token_usage.".into()
                } else {
                    "No token_count record carries info.last_token_usage and info.total_token_usage.".into()
                },
            },
            Fact {
                capability: "transcript-context-window",
                support: yes_no(window.is_some()),
                result: match &window {
                    Some(n) => {
                        format!("token_count info.model_context_window is present (observed {n}).")
                    }
                    None => "No token_count record carries info.model_context_window.".into(),
                },
            },
        ],
    })
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
            drift: false,
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
    fn drift_mode_lists_records_to_reverify_and_honours_supersedes() {
        let (fx, env) = fixture("drift-mode");
        fake_bin(&fx.root.join("bin"), "claude", "2.1.312 (Claude Code)");
        let mut o = opts(Some("claude"));
        o.drift = true;
        o.registry = Some(base_registry(&fx.root));
        let report = doctor(&env, &o).unwrap();
        assert_eq!(report["mode"], "drift");
        let h = &report["hosts"][0];
        assert_eq!(h["installed"], "2.1.312");
        assert_eq!(h["status"], "drift");
        assert!(h["observed_versions"]
            .as_array()
            .unwrap()
            .contains(&json!("2.1.306")));
        let ids: Vec<&str> = h["reverify"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            ids.contains(&"agent-hooks.claude.transcript-token-usage"),
            "{ids:?}"
        );
        assert!(render_drift(&report).contains("re-verify agent-hooks.claude"));

        // A revision record that supersedes every drifted record clears drift.
        let mut doc: Value =
            serde_json::from_slice(&fs::read(o.registry.as_ref().unwrap()).unwrap()).unwrap();
        let recs = doc["records"].as_array_mut().unwrap();
        let mut revisions = Vec::new();
        for r in recs
            .iter()
            .filter(|r| ids.contains(&r["id"].as_str().unwrap()))
        {
            let mut n = r.clone();
            n["supersedes"] = r["id"].clone();
            n["id"] = json!(format!("{}@2.1.312", r["id"].as_str().unwrap()));
            n["components"][0]["version"] = json!("2.1.312");
            revisions.push(n);
        }
        recs.extend(revisions);
        let path = fx.root.join("registry.json");
        fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        o.registry = Some(path.to_string_lossy().into_owned());
        let h = &doctor(&env, &o).unwrap()["hosts"][0];
        assert_eq!(h["status"], "current", "{h}");
        assert!(mcp(&json!({"drift": "yes"})).is_err());
        assert!(cli(&[
            "doctor".into(),
            "--drift".into(),
            "--host".into(),
            "nobody".into()
        ])
        .is_err());
    }

    /// The embedded registry without refresh revisions, so tests start from
    /// the 2.1.306 observations whatever later refreshes appended.
    fn base_registry(dir: &Path) -> String {
        let mut doc: Value = serde_json::from_str(EMBEDDED_REGISTRY).unwrap();
        doc["records"]
            .as_array_mut()
            .unwrap()
            .retain(|r| r["supersedes"].is_null());
        let path = dir.join("base-registry.json");
        let mut bytes = serde_json::to_vec_pretty(&doc).unwrap();
        bytes.push(b'\n');
        fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn claude_transcript(env: &Env, name: &str, version: &str, usage: bool) {
        let u = if usage {
            r#"{"input_tokens":1,"output_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":4}"#
        } else {
            r#"{"input_tokens":1}"#
        };
        write(
            &env.home.join(".claude/projects/p").join(name),
            &format!(
                "{{\"type\":\"user\",\"version\":\"{version}\",\"message\":{{\"content\":\"mentions \\\"context_window\\\" in text\"}}}}\n{{\"type\":\"assistant\",\"version\":\"{version}\",\"message\":{{\"usage\":{u}}}}}\n"
            ),
        );
    }

    #[test]
    fn refresh_appends_revisions_for_verified_records_only() {
        let (fx, env) = fixture("refresh");
        fake_bin(&fx.root.join("bin"), "claude", "2.1.312 (Claude Code)");
        let base = base_registry(&fx.root);
        let original = fs::read_to_string(&base).unwrap();
        let reg = PathBuf::from(&base);
        let path = reg.to_str().unwrap();
        // A transcript from another version is not evidence.
        claude_transcript(&env, "old.jsonl", "2.1.306", true);
        let out = refresh(&env, Some(path), "claude", false).unwrap();
        assert_eq!(out["refreshed"], 0, "{out}");
        assert!(out["probe"]["error"].as_str().unwrap().contains("2.1.312"));
        assert_eq!(fs::read_to_string(&reg).unwrap(), original);

        claude_transcript(&env, "new.jsonl", "2.1.312", true);
        let before = fs::read_to_string(&reg).unwrap();
        let dry = refresh(&env, Some(path), "claude", true).unwrap();
        assert_eq!(dry["refreshed"], 2, "{dry}");
        assert_eq!(
            fs::read_to_string(&reg).unwrap(),
            before,
            "dry run writes nothing"
        );

        let out = refresh(&env, Some(path), "claude", false).unwrap();
        assert_eq!(out["refreshed"], 2, "{out}");
        let doc: Value = serde_json::from_str(&fs::read_to_string(&reg).unwrap()).unwrap();
        let old: Value = serde_json::from_str(&original).unwrap();
        let recs = doc["records"].as_array().unwrap();
        // Append-only: every original record is byte-identical and still present.
        assert_eq!(
            &recs[..old["records"].as_array().unwrap().len()],
            old["records"].as_array().unwrap().as_slice()
        );
        let rev = recs
            .iter()
            .find(|r| r["id"] == "agent-hooks.claude.transcript-token-usage@2.1.312")
            .unwrap();
        assert_eq!(
            rev["supersedes"],
            "agent-hooks.claude.transcript-token-usage"
        );
        assert_eq!(rev["components"][0]["version"], "2.1.312");
        assert_eq!(rev["evidence"].as_array().unwrap().len(), 2);
        assert_eq!(rev["evidence"][1]["observed_at"], "2026-10-28");
        let cw = recs
            .iter()
            .find(|r| r["id"] == "agent-hooks.claude.transcript-context-window@2.1.312")
            .unwrap();
        assert!(cw["evidence"][1]["result"]
            .as_str()
            .unwrap()
            .starts_with("No context_window"));
        let text = serde_json::to_string(&doc).unwrap();
        assert!(
            !text.contains(env.home.to_str().unwrap()),
            "no home path in evidence"
        );

        // Reported records were kept, not rewritten; a second run is a no-op.
        assert!(out["actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["action"] == "keep"));
        let again = refresh(&env, Some(path), "claude", false).unwrap();
        assert_eq!(again["refreshed"], 0, "{again}");
        let mut o = opts(Some("claude"));
        o.drift = true;
        o.registry = Some(path.into());
        assert_eq!(doctor(&env, &o).unwrap()["hosts"][0]["status"], "current");
    }

    #[test]
    fn refresh_never_certifies_a_contradiction_and_rejects_bad_args() {
        let (fx, env) = fixture("refresh-contra");
        fake_bin(&fx.root.join("bin"), "claude", "2.1.312 (Claude Code)");
        claude_transcript(&env, "new.jsonl", "2.1.312", false);
        let base = base_registry(&fx.root);
        let out = refresh(&env, Some(&base), "claude", true).unwrap();
        let usage = out["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == "agent-hooks.claude.transcript-token-usage")
            .unwrap();
        assert_eq!(usage["action"], "contradicted", "{usage}");
        assert_eq!(out["refreshed"], 1, "context-window still re-confirms");
        assert!(refresh(&env, None, "nobody", true).is_err());
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(
            refresh_cli(&args(&["--section", "agent-hooks", "--host", "claude"]))
                .unwrap_err()
                .contains("read-only")
        );
        assert!(refresh_cli(&args(&[
            "--section",
            "protocols",
            "--host",
            "claude",
            "--dry-run"
        ]))
        .is_err());
        assert!(refresh_cli(&args(&[
            "--section",
            "nope",
            "--host",
            "claude",
            "--dry-run"
        ]))
        .unwrap_err()
        .contains("reinstall"));
        assert!(refresh_cli(&args(&["--host", "claude", "--dry-run"])).is_err());
        assert!(refresh_cli(&args(&[
            "--section",
            "agent-hooks",
            "--host",
            "claude",
            "--bogus"
        ]))
        .is_err());
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

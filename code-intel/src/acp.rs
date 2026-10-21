// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! `acp doctor`: probe installed Agent Client Protocol (ACP v1) agents.
//!
//! For each agent: resolve the launch command, compare its version with the
//! registry, then spawn it in its own process group and time `initialize` and
//! `session/new`. `--first-token` (opt-in, costs tokens) also times the first
//! `agent_message_chunk` of a one-word prompt.
//!
//! The client is a small hand-written JSON-RPC 2.0 stdio client on std
//! threads. The official `agent-client-protocol` crate is async and needs a
//! newer MSRV than this crate; five methods do not justify either.
//!
//! Invariants: every step has a deadline, every agent→client request gets an
//! answer (an unanswered request hangs some agents), and every spawned process
//! group is killed before the agent's result is returned.

use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PROTOCOL_VERSION: u64 = 1;
const VENDORED_REGISTRY: &str = include_str!("../data/acp-registry.json");
pub const DEFAULT_TIMEOUT_S: f64 = 20.0;
/// Grace per shutdown stage, matching the ACP TCK's 2 s.
const GRACE: Duration = Duration::from_secs(2);
const MAX_LINE: usize = 4 << 20;
const STDERR_TAIL: usize = 20;
const SAMPLE_LINES: usize = 3;
const FIRST_TOKEN_PROMPT: &str = "Reply with the single word OK";

/// Process group of the agent currently being probed, so a signal handler can
/// kill it instead of orphaning it. 0 = none.
static ACTIVE_PGID: AtomicI32 = AtomicI32::new(0);

pub struct Builtin {
    pub id: &'static str,
    pub registry_id: Option<&'static str>,
    pub bin: &'static str,
    pub args: &'static [&'static str],
}

/// Launch commands, verified against the ACP registry snapshot (see tests).
/// Kiro is native ACP but not in the registry.
pub const BUILTINS: &[Builtin] = &[
    Builtin {
        id: "claude",
        registry_id: Some("claude-acp"),
        bin: "claude-agent-acp",
        args: &[],
    },
    Builtin {
        id: "gemini",
        registry_id: Some("gemini"),
        bin: "gemini",
        args: &["--acp"],
    },
    Builtin {
        id: "cursor",
        registry_id: Some("cursor"),
        bin: "cursor-agent",
        args: &["acp"],
    },
    Builtin {
        id: "opencode",
        registry_id: Some("opencode"),
        bin: "opencode",
        args: &["acp"],
    },
    Builtin {
        id: "codex",
        registry_id: Some("codex-acp"),
        bin: "codex-acp",
        args: &[],
    },
    Builtin {
        id: "goose",
        registry_id: Some("goose"),
        bin: "goose",
        args: &["acp"],
    },
    Builtin {
        id: "kiro",
        registry_id: None,
        bin: "kiro-cli",
        args: &["acp"],
    },
];

/// One agent to probe. Built-ins resolve from PATH; tests construct their own.
#[derive(Clone, Debug)]
pub struct AgentSpec {
    pub id: String,
    pub registry_id: Option<String>,
    pub bin: String,
    pub program: Option<PathBuf>,
    pub args: Vec<String>,
    /// Command whose stdout carries the version; None = use `agentInfo`.
    pub version_cmd: Option<Vec<String>>,
}

impl AgentSpec {
    fn from_builtin(b: &Builtin) -> Self {
        let program = which(b.bin);
        AgentSpec {
            id: b.id.into(),
            registry_id: b.registry_id.map(str::to_string),
            bin: b.bin.into(),
            version_cmd: program
                .as_ref()
                .map(|p| vec![p.to_string_lossy().into_owned(), "--version".into()]),
            program,
            args: b.args.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub agents: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub timeout: Duration,
    pub first_token: bool,
    pub registry: Option<String>,
    pub json: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            agents: Vec::new(),
            cwd: None,
            timeout: Duration::from_secs_f64(DEFAULT_TIMEOUT_S),
            first_token: false,
            registry: None,
            json: false,
        }
    }
}

// ------------------------------------------------------------------ entry

pub const USAGE: &str = "usage: acp doctor [--agent ID]... [--cwd DIR] [--timeout SECS] [--json] [--first-token] [--registry PATH|URL]";

/// `rosslabs-lsp acp ...`. Err only on bad usage.
pub fn cli(args: &[String]) -> Result<String, String> {
    if args.first().map(String::as_str) != Some("doctor") {
        return Err(USAGE.into());
    }
    let opts = parse_cli(&args[1..])?;
    let report = doctor(&opts)?;
    if opts.json {
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
    } else {
        Ok(render_table(&report))
    }
}

pub fn parse_cli(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = args.iter();
    let mut seen_timeout = false;
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("missing value for {flag}"))
        };
        match flag.as_str() {
            "--agent" => o.agents.push(value()?),
            "--cwd" => {
                if o.cwd.is_some() {
                    return Err("duplicate option: --cwd".into());
                }
                o.cwd = Some(PathBuf::from(value()?));
            }
            "--timeout" => {
                if seen_timeout {
                    return Err("duplicate option: --timeout".into());
                }
                seen_timeout = true;
                o.timeout = parse_timeout(&value()?)?;
            }
            "--registry" => {
                if o.registry.is_some() {
                    return Err("duplicate option: --registry".into());
                }
                o.registry = Some(value()?);
            }
            "--json" => o.json = true,
            "--first-token" => o.first_token = true,
            other => return Err(format!("unknown acp doctor option: {other}\n{USAGE}")),
        }
    }
    validate_agents(&o.agents)?;
    Ok(o)
}

/// MCP `acp_doctor` arguments → Options. Err is a tool error.
pub fn parse_mcp(args: &Value) -> Result<Options, String> {
    let obj = args
        .as_object()
        .ok_or("acp_doctor arguments must be an object")?;
    let allowed = ["agents", "cwd", "timeout", "first_token", "registry"];
    if let Some(k) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("unknown acp_doctor argument: {k}"));
    }
    let mut o = Options {
        json: true,
        ..Options::default()
    };
    if let Some(a) = obj.get("agents") {
        let list = a.as_array().ok_or("agents must be an array of strings")?;
        for v in list {
            o.agents.push(
                v.as_str()
                    .ok_or("agents must be an array of strings")?
                    .into(),
            );
        }
    }
    if let Some(v) = obj.get("cwd") {
        o.cwd = Some(PathBuf::from(v.as_str().ok_or("cwd must be a string")?));
    }
    if let Some(v) = obj.get("timeout") {
        let secs = v.as_f64().ok_or("timeout must be a number of seconds")?;
        o.timeout = parse_timeout(&secs.to_string())?;
    }
    if let Some(v) = obj.get("first_token") {
        o.first_token = v.as_bool().ok_or("first_token must be a boolean")?;
    }
    if let Some(v) = obj.get("registry") {
        o.registry = Some(v.as_str().ok_or("registry must be a string")?.into());
    }
    validate_agents(&o.agents)?;
    Ok(o)
}

fn parse_timeout(s: &str) -> Result<Duration, String> {
    let secs: f64 = s
        .parse()
        .map_err(|_| format!("--timeout needs seconds, got '{s}'"))?;
    if !(secs > 0.0 && secs <= 600.0) {
        return Err("--timeout must be between 0 and 600 seconds".into());
    }
    Ok(Duration::from_secs_f64(secs))
}

fn validate_agents(ids: &[String]) -> Result<(), String> {
    for id in ids {
        if !BUILTINS.iter().any(|b| b.id == id) {
            let known: Vec<_> = BUILTINS.iter().map(|b| b.id).collect();
            return Err(format!(
                "unknown agent '{id}' (known: {})",
                known.join(", ")
            ));
        }
    }
    Ok(())
}

/// Run the check over built-in agents. Err only on bad usage (bad cwd/registry path).
pub fn doctor(opts: &Options) -> Result<Value, String> {
    let specs: Vec<AgentSpec> = BUILTINS
        .iter()
        .filter(|b| opts.agents.is_empty() || opts.agents.iter().any(|a| a == b.id))
        .map(AgentSpec::from_builtin)
        .collect();
    doctor_specs(&specs, opts)
}

pub fn doctor_specs(specs: &[AgentSpec], opts: &Options) -> Result<Value, String> {
    let cwd = match &opts.cwd {
        Some(dir) => {
            let abs = dir
                .canonicalize()
                .map_err(|e| format!("--cwd {}: {e}", dir.display()))?;
            if !abs.is_dir() {
                return Err(format!("--cwd {}: not a directory", dir.display()));
            }
            Some(abs)
        }
        None => None,
    };
    let (registry, registry_info) = load_registry(opts.registry.as_deref())?;
    let agents: Vec<Value> = specs
        .iter()
        .map(|s| check_agent(s, &registry, opts, cwd.as_deref()))
        .collect();
    let count = |st: &str| agents.iter().filter(|a| a["status"] == st).count();
    let not_installed = count("not_installed");
    let ok = count("ok");
    Ok(json!({
        "schema_version": 1,
        "protocol_version": PROTOCOL_VERSION,
        "registry": registry_info,
        "cwd": cwd.map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "fresh temp dir per agent".into()),
        "timeout_s": opts.timeout.as_secs_f64(),
        "first_token": opts.first_token,
        "summary": {"checked": agents.len() - not_installed, "ok": ok, "not_installed": not_installed,
                    "failed": agents.len() - not_installed - ok},
        "agents": agents,
    }))
}

// --------------------------------------------------------------- registry

fn load_registry(src: Option<&str>) -> Result<(Value, Value), String> {
    let vendored = || -> Value {
        serde_json::from_str(VENDORED_REGISTRY).expect("vendored registry is valid JSON")
    };
    let snapshot = |v: &Value| v["_snapshot"]["fetched"].clone();
    match src {
        None => {
            let v = vendored();
            let info = json!({"source": "vendored", "snapshot_fetched": snapshot(&v)});
            Ok((v, info))
        }
        Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
            match fetch(url).and_then(|b| parse_registry(&b)) {
                Ok(v) => Ok((v, json!({"source": url}))),
                Err(e) => {
                    let v = vendored();
                    let info = json!({"source": "vendored", "snapshot_fetched": snapshot(&v),
                        "warning": format!("could not load {url} ({e}); using vendored snapshot")});
                    Ok((v, info))
                }
            }
        }
        Some(path) => {
            let bytes = std::fs::read(path).map_err(|e| format!("--registry {path}: {e}"))?;
            let v = parse_registry(&bytes).map_err(|e| format!("--registry {path}: {e}"))?;
            Ok((v, json!({"source": path})))
        }
    }
}

fn parse_registry(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > 8 << 20 {
        return Err("registry exceeds 8 MiB".into());
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("invalid JSON: {e}"))?;
    if !v["agents"].is_array() {
        return Err("registry has no 'agents' array".into());
    }
    Ok(v)
}

/// Network is optional, so shell out to curl rather than add an HTTP stack.
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "10",
            "--max-filesize",
            "8388608",
            url,
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl exited {}", out.status));
    }
    Ok(out.stdout)
}

fn registry_entry<'a>(registry: &'a Value, id: &str) -> Option<&'a Value> {
    registry["agents"]
        .as_array()?
        .iter()
        .find(|a| a["id"] == id)
}

fn platform_key() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

/// The registry's launch command and whether it agrees with ours.
fn registry_launch(entry: &Value, bin: &str, args: &[String]) -> (Value, Option<bool>) {
    let dist = &entry["distribution"];
    let strs = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    if let Some(npx) = dist.get("npx") {
        let rargs = strs(&npx["args"]);
        let mut cmd = vec![
            "npx".to_string(),
            npx["package"].as_str().unwrap_or("").to_string(),
        ];
        cmd.extend(rargs.iter().cloned());
        // The npm package's bin name is not in the registry; compare args only.
        return (json!(cmd), Some(rargs == args));
    }
    if let Some(bins) = dist.get("binary").and_then(Value::as_object) {
        let target = bins
            .get(&platform_key())
            .or_else(|| bins.values().next())
            .cloned()
            .unwrap_or(Value::Null);
        let rcmd = target["cmd"].as_str().unwrap_or("").to_string();
        let rargs = strs(&target["args"]);
        let base = Path::new(&rcmd)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut cmd = vec![rcmd];
        cmd.extend(rargs.iter().cloned());
        return (json!(cmd), Some(base == bin && rargs == args));
    }
    (Value::Null, None)
}

// ---------------------------------------------------------------- version

/// The first token that looks like a version, e.g. "goose 1.2.3" → "1.2.3".
fn extract_version(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|t| t.trim_matches(|c: char| c == ',' || c == '(' || c == ')'))
        .find(|t| {
            let t = t.strip_prefix('v').unwrap_or(t);
            t.starts_with(|c: char| c.is_ascii_digit()) && t.contains('.')
        })
        .map(str::to_string)
}

fn numeric_parts(v: &str) -> Option<Vec<u64>> {
    let v = v.strip_prefix('v').unwrap_or(v);
    let core: String = v
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let parts: Option<Vec<u64>> = core
        .split('.')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().ok())
        .collect();
    parts.filter(|p| !p.is_empty())
}

/// current | behind | ahead | unknown. Pre-release/build suffixes are ignored.
pub fn compare_versions(installed: Option<&str>, registry: Option<&str>) -> &'static str {
    let (Some(a), Some(b)) = (
        installed.and_then(numeric_parts),
        registry.and_then(numeric_parts),
    ) else {
        return "unknown";
    };
    let n = a.len().max(b.len());
    let pad = |v: &[u64]| -> Vec<u64> { (0..n).map(|i| v.get(i).copied().unwrap_or(0)).collect() };
    match pad(&a).cmp(&pad(&b)) {
        std::cmp::Ordering::Equal => "current",
        std::cmp::Ordering::Less => "behind",
        std::cmp::Ordering::Greater => "ahead",
    }
}

fn run_version(cmd: &[String], timeout: Duration) -> Option<String> {
    let (prog, rest) = cmd.split_first()?;
    let mut child = Command::new(prog)
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let pgid = child.id() as i32;
    ACTIVE_PGID.store(pgid, Ordering::SeqCst);
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout).take(64 * 1024).read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let out = rx.recv_timeout(timeout).ok();
    reap_group(&mut child, pgid, Duration::from_millis(500));
    extract_version(&String::from_utf8_lossy(&out?))
}

// ------------------------------------------------------------ connection

#[derive(Debug)]
enum StepError {
    Timeout,
    Exited,
    Rpc { code: i64, message: String },
}

enum Pumped {
    Response(Result<Value, StepError>),
    Chunk,
}

struct Conn {
    child: Child,
    pgid: i32,
    stdin: Option<ChildStdin>,
    rx: Receiver<Value>,
    next_id: u64,
    non_json: Arc<AtomicUsize>,
    samples: Arc<Mutex<Vec<String>>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    first_chunk: Option<Instant>,
    agent_requests: Vec<Value>,
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

/// Read one line of at most MAX_LINE bytes. Ok(None) = EOF; Ok(Some(true)) = overlong.
fn read_capped_line<R: BufRead>(r: &mut R, buf: &mut Vec<u8>) -> std::io::Result<Option<bool>> {
    buf.clear();
    let n = r.by_ref().take(MAX_LINE as u64).read_until(b'\n', buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.len() == MAX_LINE && buf.last() != Some(&b'\n') {
        let mut sink = Vec::new();
        loop {
            sink.clear();
            let m = r
                .by_ref()
                .take(MAX_LINE as u64)
                .read_until(b'\n', &mut sink)?;
            if m == 0 || sink.last() == Some(&b'\n') {
                break;
            }
        }
        return Ok(Some(true));
    }
    Ok(Some(false))
}

impl Conn {
    fn spawn(program: &Path, args: &[String], cwd: &Path) -> std::io::Result<Conn> {
        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
        let pgid = child.id() as i32;
        ACTIVE_PGID.store(pgid, Ordering::SeqCst);
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr_pipe = child.stderr.take().expect("piped stderr");
        let (tx, rx) = mpsc::channel();
        let non_json = Arc::new(AtomicUsize::new(0));
        let samples = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        {
            let (non_json, samples) = (non_json.clone(), samples.clone());
            std::thread::spawn(move || {
                let mut r = BufReader::new(stdout);
                let mut buf = Vec::new();
                while let Ok(Some(overlong)) = read_capped_line(&mut r, &mut buf) {
                    let line = String::from_utf8_lossy(&buf);
                    let t = line.trim();
                    if t.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Value>(t) {
                        Ok(v) if v.is_object() && !overlong => {
                            if tx.send(v).is_err() {
                                break;
                            }
                        }
                        _ => {
                            non_json.fetch_add(1, Ordering::Relaxed);
                            let mut s = samples.lock().unwrap();
                            if s.len() < SAMPLE_LINES {
                                s.push(truncate(t, 200));
                            }
                        }
                    }
                }
            });
        }
        {
            let stderr = stderr.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(stderr_pipe);
                let mut buf = Vec::new();
                while let Ok(Some(_)) = read_capped_line(&mut r, &mut buf) {
                    let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                    let mut q = stderr.lock().unwrap();
                    if q.len() == STDERR_TAIL {
                        q.pop_front();
                    }
                    q.push_back(truncate(&line, 500));
                }
            });
        }
        Ok(Conn {
            child,
            pgid,
            stdin,
            rx,
            next_id: 0,
            non_json,
            samples,
            stderr,
            first_chunk: None,
            agent_requests: Vec::new(),
        })
    }

    fn write(&mut self, msg: &Value) -> Result<(), StepError> {
        let stdin = self.stdin.as_mut().ok_or(StepError::Exited)?;
        let mut line = msg.to_string();
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|_| StepError::Exited)
    }

    fn send_request(&mut self, method: &str, params: Value) -> Result<u64, StepError> {
        self.next_id += 1;
        let id = self.next_id;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), StepError> {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, StepError> {
        let id = self.send_request(method, params)?;
        match self.pump(id, Instant::now() + timeout, false)? {
            Pumped::Response(r) => r,
            Pumped::Chunk => unreachable!("chunk stop disabled"),
        }
    }

    /// Process inbound messages until the response to `id` arrives (or, with
    /// `stop_on_chunk`, the first agent_message_chunk). Answers every
    /// agent→client request on the way.
    fn pump(
        &mut self,
        id: u64,
        deadline: Instant,
        stop_on_chunk: bool,
    ) -> Result<Pumped, StepError> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(StepError::Timeout);
            }
            let msg = match self.rx.recv_timeout(left) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => return Err(StepError::Timeout),
                Err(RecvTimeoutError::Disconnected) => return Err(StepError::Exited),
            };
            if let Some(method) = msg.get("method").and_then(Value::as_str) {
                if let Some(req_id) = msg.get("id") {
                    self.answer(method, req_id.clone())?;
                } else if method == "session/update"
                    && msg["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                    && self.first_chunk.is_none()
                {
                    self.first_chunk = Some(Instant::now());
                    if stop_on_chunk {
                        return Ok(Pumped::Chunk);
                    }
                }
                continue;
            }
            if msg.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = msg.get("error") {
                    return Ok(Pumped::Response(Err(StepError::Rpc {
                        code: err["code"].as_i64().unwrap_or(0),
                        message: err["message"].as_str().unwrap_or("").to_string(),
                    })));
                }
                return Ok(Pumped::Response(Ok(msg
                    .get("result")
                    .cloned()
                    .unwrap_or(Value::Null))));
            }
            // A stale response to an earlier, timed-out request: ignore.
        }
    }

    fn answer(&mut self, method: &str, id: Value) -> Result<(), StepError> {
        let (reply, answered) = if method == "session/request_permission" {
            (
                json!({"jsonrpc": "2.0", "id": id, "result": {"outcome": {"outcome": "cancelled"}}}),
                json!("cancelled"),
            )
        } else {
            (
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("Method not found: {method}")}}),
                json!(-32601),
            )
        };
        self.agent_requests
            .push(json!({"method": method, "answered": answered}));
        self.write(&reply)
    }

    fn stderr_tail(&self) -> Vec<String> {
        self.stderr.lock().unwrap().iter().cloned().collect()
    }

    /// session/close (if advertised) → close stdin → grace → SIGTERM group →
    /// grace → SIGKILL group. Returns whether session/close succeeded.
    fn shutdown(mut self, close_session: Option<&str>, timeout: Duration) -> bool {
        let mut closed = false;
        if let Some(sid) = close_session {
            closed = self
                .request(
                    "session/close",
                    json!({"sessionId": sid}),
                    timeout.min(Duration::from_secs(5)),
                )
                .is_ok();
        }
        drop(self.stdin.take());
        reap_group(&mut self.child, self.pgid, GRACE);
        closed
    }
}

fn group_alive(pgid: i32) -> bool {
    unsafe { libc::kill(-pgid, 0) == 0 }
}

/// Wait up to `grace` for the leader to exit, then terminate and kill the
/// whole group. Runs every time: a leader can exit and leave children behind.
fn reap_group(child: &mut Child, pgid: i32, grace: Duration) {
    let wait_until = |child: &mut Child, cond: &dyn Fn(&mut Child) -> bool, limit: Duration| {
        let end = Instant::now() + limit;
        while Instant::now() < end {
            if cond(child) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        cond(child)
    };
    wait_until(
        child,
        &|c: &mut Child| matches!(c.try_wait(), Ok(Some(_))),
        grace,
    );
    if group_alive(pgid) {
        unsafe { libc::kill(-pgid, libc::SIGTERM) };
        let gone = wait_until(
            child,
            &|c: &mut Child| {
                let _ = c.try_wait();
                !group_alive(pgid)
            },
            GRACE,
        );
        if !gone {
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
    }
    let _ = child.wait();
    let _ = ACTIVE_PGID.compare_exchange(pgid, 0, Ordering::SeqCst, Ordering::SeqCst);
}

/// Called from signal handlers: kill the agent being probed. Async-signal-safe.
pub fn kill_active_group() {
    let pgid = ACTIVE_PGID.load(Ordering::SeqCst);
    if pgid > 0 {
        unsafe { libc::kill(-pgid, libc::SIGKILL) };
    }
}

extern "C" fn on_signal(_sig: i32) {
    kill_active_group();
    unsafe { libc::_exit(130) }
}

/// CLI only: Ctrl-C must not orphan an agent running in its own process group.
pub fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_signal as *const () as libc::sighandler_t);
    }
}

// ------------------------------------------------------------------ check

fn ms(since: Instant) -> u64 {
    since.elapsed().as_millis() as u64
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(id: &str) -> std::io::Result<TempDir> {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("rosslabs-lsp-acp-{id}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(TempDir(dir.canonicalize()?))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn check_agent(spec: &AgentSpec, registry: &Value, opts: &Options, cwd: Option<&Path>) -> Value {
    let entry = spec
        .registry_id
        .as_deref()
        .and_then(|id| registry_entry(registry, id));
    let registry_version = entry
        .and_then(|e| e["version"].as_str())
        .map(str::to_string);
    let (registry_cmd, launch_matches) = entry
        .map(|e| registry_launch(e, &spec.bin, &spec.args))
        .unwrap_or((Value::Null, None));
    let mut command = vec![spec.bin.clone()];
    command.extend(spec.args.iter().cloned());
    let mut r = json!({
        "id": spec.id,
        "command": command,
        "registry_id": spec.registry_id,
        "registry_command": registry_cmd,
        "launch_matches_registry": launch_matches,
        "installed": spec.program.is_some(),
        "path": spec.program.as_ref().map(|p| p.to_string_lossy().into_owned()),
        "installed_version": null,
        "version_source": null,
        "registry_version": registry_version,
        "version_status": "unknown",
        "status": "not_installed",
        "failed_step": null,
        "error": null,
        "init_ms": null,
        "session_ms": null,
        "first_token_ms": null,
        "protocol_version": null,
        "agent_info": null,
        "agent_capabilities": null,
        "auth_methods": null,
        "session_closed": false,
        "agent_requests": [],
        "non_json_stdout_lines": 0,
        "non_json_samples": [],
        "stderr_tail": [],
        "notes": [],
    });
    let mut notes: Vec<String> = Vec::new();
    if spec.registry_id.is_none() || entry.is_none() {
        notes.push("not in ACP registry".into());
    } else if launch_matches == Some(false) {
        notes.push("launch command differs from registry".into());
    }
    let Some(program) = spec.program.clone() else {
        r["notes"] = json!(notes);
        return r;
    };

    if let Some(cmd) = &spec.version_cmd {
        if let Some(v) = run_version(cmd, opts.timeout.min(Duration::from_secs(10))) {
            r["installed_version"] = json!(v);
            r["version_source"] = json!("--version");
        }
    }

    let temp;
    let cwd: PathBuf = match cwd {
        Some(c) => c.to_path_buf(),
        None => match TempDir::new(&spec.id) {
            Ok(t) => {
                temp = t;
                temp.0.clone()
            }
            Err(e) => {
                r["status"] = json!("error");
                r["failed_step"] = json!("spawn");
                r["error"] = json!(format!("temp dir: {e}"));
                return finish(r, notes, registry_version.as_deref());
            }
        },
    };

    let started = Instant::now();
    let mut conn = match Conn::spawn(&program, &spec.args, &cwd) {
        Ok(c) => c,
        Err(e) => {
            r["status"] = json!("error");
            r["failed_step"] = json!("spawn");
            r["error"] = json!(e.to_string());
            return finish(r, notes, registry_version.as_deref());
        }
    };

    let mut session_id: Option<String> = None;
    let mut close_supported = false;
    let outcome: Result<(), (&str, StepError)> = (|| {
        let init = conn
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
                    "clientInfo": {"name": "rosslabs-lsp", "version": env!("CARGO_PKG_VERSION")}
                }),
                opts.timeout,
            )
            .map_err(|e| ("initialize", e))?;
        r["init_ms"] = json!(ms(started));
        r["protocol_version"] = init["protocolVersion"].clone();
        r["agent_info"] = init["agentInfo"].clone();
        r["agent_capabilities"] = init["agentCapabilities"].clone();
        r["auth_methods"] = init.get("authMethods").cloned().unwrap_or(json!([]));
        close_supported = init["agentCapabilities"]["sessionCapabilities"]["close"].is_object();
        if init["protocolVersion"].as_u64() != Some(PROTOCOL_VERSION) {
            return Err((
                "initialize",
                StepError::Rpc {
                    code: 0,
                    message: format!("agent speaks protocol {}", init["protocolVersion"]),
                },
            ));
        }

        let t = Instant::now();
        let session = conn
            .request(
                "session/new",
                json!({"cwd": cwd.to_string_lossy(), "mcpServers": []}),
                opts.timeout,
            )
            .map_err(|e| ("session/new", e))?;
        r["session_ms"] = json!(ms(t));
        let sid = session["sessionId"]
            .as_str()
            .ok_or((
                "session/new",
                StepError::Rpc {
                    code: 0,
                    message: "response has no sessionId".into(),
                },
            ))?
            .to_string();
        session_id = Some(sid.clone());

        if opts.first_token {
            let t = Instant::now();
            let id = conn
                .send_request(
                    "session/prompt",
                    json!({"sessionId": sid, "prompt": [{"type": "text", "text": FIRST_TOKEN_PROMPT}]}),
                )
                .map_err(|e| ("session/prompt", e))?;
            match conn
                .pump(id, t + opts.timeout, true)
                .map_err(|e| ("first token", e))?
            {
                Pumped::Chunk => {
                    r["first_token_ms"] = json!(conn
                        .first_chunk
                        .map(|c| c.duration_since(t).as_millis() as u64));
                    // Stop the turn; keep answering requests until it ends.
                    let _ = conn.notify("session/cancel", json!({"sessionId": sid}));
                    let _ = conn.pump(
                        id,
                        Instant::now() + opts.timeout.min(Duration::from_secs(5)),
                        false,
                    );
                }
                Pumped::Response(Ok(res)) => {
                    notes.push(format!(
                        "turn ended ({}) with no agent_message_chunk",
                        res["stopReason"]
                    ));
                }
                Pumped::Response(Err(e)) => return Err(("session/prompt", e)),
            }
        }
        Ok(())
    })();

    if let Err((step, err)) = outcome {
        r["failed_step"] = json!(step);
        let (status, msg) = match &err {
            StepError::Timeout => ("timeout", None),
            StepError::Exited => ("exited", None),
            StepError::Rpc { code, message }
                if step == "session/new"
                    && (*code == -32000 || message.to_ascii_lowercase().contains("auth")) =>
            {
                ("needs_sign_in", Some(format!("{code}: {message}")))
            }
            StepError::Rpc { code: 0, message } if step == "initialize" => {
                ("protocol_mismatch", Some(message.clone()))
            }
            StepError::Rpc { code, message } => ("error", Some(format!("{code}: {message}"))),
        };
        r["status"] = json!(status);
        r["error"] = json!(msg);
    } else {
        r["status"] = json!("ok");
    }

    let failed = r["status"] != "ok";
    let close = if close_supported {
        session_id.as_deref()
    } else {
        None
    };
    let non_json = conn.non_json.clone();
    let samples = conn.samples.clone();
    let agent_requests = std::mem::take(&mut conn.agent_requests);
    let stderr_tail = conn.stderr_tail();
    r["session_closed"] = json!(conn.shutdown(close, opts.timeout));
    r["agent_requests"] = json!(agent_requests);
    let bad = non_json.load(Ordering::Relaxed);
    r["non_json_stdout_lines"] = json!(bad);
    r["non_json_samples"] = json!(samples.lock().unwrap().clone());
    if bad > 0 {
        notes.push(format!("{bad} non-JSON stdout line(s)"));
    }
    if failed {
        r["stderr_tail"] = json!(stderr_tail);
    }
    finish(r, notes, registry_version.as_deref())
}

fn finish(mut r: Value, mut notes: Vec<String>, registry_version: Option<&str>) -> Value {
    if r["installed_version"].is_null() {
        if let Some(v) = r["agent_info"]["version"].as_str() {
            r["installed_version"] = json!(v);
            r["version_source"] = json!("agentInfo");
        }
    }
    let status = compare_versions(r["installed_version"].as_str(), registry_version);
    r["version_status"] = json!(status);
    if status == "behind" {
        notes.insert(
            0,
            format!("behind registry {}", registry_version.unwrap_or("")),
        );
    }
    if r["status"] == "needs_sign_in" {
        let ids: Vec<&str> = r["auth_methods"]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m["id"].as_str()).collect())
            .unwrap_or_default();
        if !ids.is_empty() {
            notes.push(format!("auth methods: {}", ids.join(", ")));
        }
    }
    r["notes"] = json!(notes);
    r
}

fn which(bin: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| {
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

// ------------------------------------------------------------------- table

fn status_label(a: &Value) -> String {
    let step = a["failed_step"].as_str().unwrap_or("?");
    match a["status"].as_str().unwrap_or("") {
        "ok" => "ok".into(),
        "not_installed" => "not installed".into(),
        "needs_sign_in" => "needs sign-in".into(),
        "timeout" => format!("timeout at {step}"),
        "exited" => format!("exited at {step}"),
        "protocol_mismatch" => "protocol mismatch".into(),
        _ => format!("error at {step}"),
    }
}

pub fn render_table(report: &Value) -> String {
    let dash = || "—".to_string();
    let num = |v: &Value| v.as_u64().map(|n| n.to_string()).unwrap_or_else(dash);
    let text = |v: &Value| v.as_str().map(str::to_string).unwrap_or_else(dash);
    let header = [
        "agent",
        "installed",
        "registry",
        "status",
        "init ms",
        "session ms",
        "first token ms",
        "notes",
    ];
    let mut rows: Vec<Vec<String>> = vec![header.iter().map(|s| s.to_string()).collect()];
    for a in report["agents"].as_array().into_iter().flatten() {
        let mut notes: Vec<String> = a["notes"]
            .as_array()
            .map(|n| {
                n.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(e) = a["error"].as_str() {
            notes.push(truncate(e, 80));
        }
        if a["status"] != "ok" && a["status"] != "not_installed" {
            if let Some(last) = a["stderr_tail"]
                .as_array()
                .and_then(|t| t.last())
                .and_then(Value::as_str)
            {
                notes.push(format!("stderr: {}", truncate(last, 80)));
            }
        }
        rows.push(vec![
            text(&a["id"]),
            text(&a["installed_version"]),
            text(&a["registry_version"]),
            status_label(a),
            num(&a["init_ms"]),
            num(&a["session_ms"]),
            num(&a["first_token_ms"]),
            if notes.is_empty() {
                dash()
            } else {
                notes.join("; ")
            },
        ]);
    }
    let cols = header.len();
    let widths: Vec<usize> = (0..cols)
        .map(|c| rows.iter().map(|r| r[c].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in &rows {
        let line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(c, cell)| {
                if c == cols - 1 {
                    cell.clone()
                } else {
                    format!("{cell}{}", " ".repeat(widths[c] - cell.chars().count()))
                }
            })
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    let reg = &report["registry"];
    out.push_str(&format!(
        "\nregistry: {}{}  ·  protocol v{}  ·  timeout {}s per step",
        reg["source"].as_str().unwrap_or("?"),
        reg["snapshot_fetched"]
            .as_str()
            .map(|d| format!(" (snapshot {d})"))
            .unwrap_or_default(),
        PROTOCOL_VERSION,
        report["timeout_s"]
    ));
    if let Some(w) = reg["warning"].as_str() {
        out.push_str(&format!("\nwarning: {w}"));
    }
    out
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/fake_acp_agent.py"
    );

    fn python() -> PathBuf {
        which("python3").expect("python3 on PATH for the fake ACP agent")
    }

    /// A fake agent; `mode` is passed as an argument the script reads.
    fn fake(mode: &str, extra: &[&str]) -> AgentSpec {
        let mut args = vec!["-I".to_string(), FAKE.to_string(), mode.to_string()];
        args.extend(extra.iter().map(|s| s.to_string()));
        AgentSpec {
            id: format!("fake-{mode}"),
            registry_id: Some("claude-acp".into()),
            bin: "python3".into(),
            program: Some(python()),
            args,
            version_cmd: None,
        }
    }

    fn opts(timeout_ms: u64, first_token: bool) -> Options {
        Options {
            timeout: Duration::from_millis(timeout_ms),
            first_token,
            ..Options::default()
        }
    }

    fn run(spec: AgentSpec, o: &Options) -> Value {
        let report = doctor_specs(&[spec], o).unwrap();
        report["agents"][0].clone()
    }

    #[test]
    fn normal_handshake_records_timings_capabilities_and_closes_session() {
        let a = run(fake("normal", &[]), &opts(10_000, false));
        assert_eq!(a["status"], "ok", "{a:#}");
        assert!(a["init_ms"].is_u64() && a["session_ms"].is_u64());
        assert!(a["first_token_ms"].is_null(), "first token must be opt-in");
        assert_eq!(a["protocol_version"], 1);
        assert_eq!(a["agent_info"]["name"], "fake-agent");
        assert_eq!(a["installed_version"], "0.94.0");
        assert_eq!(a["version_source"], "agentInfo");
        assert_eq!(a["version_status"], "current");
        assert_eq!(a["session_closed"], true);
        assert_eq!(a["non_json_stdout_lines"], 0);
        assert_eq!(
            a["stderr_tail"],
            json!([]),
            "stderr is only kept on failure"
        );
    }

    #[test]
    fn garbage_stdout_is_counted_not_fatal() {
        let a = run(fake("garbage", &[]), &opts(10_000, false));
        assert_eq!(a["status"], "ok", "{a:#}");
        assert_eq!(a["non_json_stdout_lines"], 3);
        assert_eq!(a["non_json_samples"][0], "Loading agent...");
        assert!(a["notes"].to_string().contains("3 non-JSON stdout line(s)"));
    }

    #[test]
    fn silent_agent_times_out_and_its_whole_group_is_killed() {
        let pidfile = std::env::temp_dir().join(format!("fake-acp-hang-{}", std::process::id()));
        let _ = std::fs::remove_file(&pidfile);
        let started = Instant::now();
        let a = run(
            fake("hang", &[pidfile.to_str().unwrap()]),
            &opts(800, false),
        );
        assert_eq!(a["status"], "timeout", "{a:#}");
        assert_eq!(a["failed_step"], "initialize");
        assert!(
            a["stderr_tail"].to_string().contains("fake agent hanging"),
            "{a:#}"
        );
        // 0.8 s step + 2 s stdin grace + 2 s SIGTERM grace (ignored) + kill.
        assert!(started.elapsed() < Duration::from_secs(10));
        let pids = std::fs::read_to_string(&pidfile).unwrap();
        let _ = std::fs::remove_file(&pidfile);
        for pid in pids.split_whitespace().map(|p| p.parse::<i32>().unwrap()) {
            let end = Instant::now() + Duration::from_secs(2);
            while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(50));
            }
            assert_ne!(
                unsafe { libc::kill(pid, 0) },
                0,
                "process {pid} was orphaned"
            );
        }
    }

    #[test]
    fn auth_required_on_session_new_is_needs_sign_in() {
        let a = run(fake("auth", &[]), &opts(10_000, false));
        assert_eq!(a["status"], "needs_sign_in", "{a:#}");
        assert_eq!(a["failed_step"], "session/new");
        assert!(a["init_ms"].is_u64());
        assert!(a["notes"].to_string().contains("fake-login"));
    }

    #[test]
    fn first_token_answers_unknown_requests_and_permissions() {
        let a = run(fake("normal", &[]), &opts(10_000, true));
        assert_eq!(a["status"], "ok", "{a:#}");
        assert!(a["first_token_ms"].is_u64(), "{a:#}");
        assert_eq!(
            a["agent_requests"],
            json!([
                {"method": "_fake/ask_question", "answered": -32601},
                {"method": "session/request_permission", "answered": "cancelled"}
            ])
        );
    }

    #[test]
    fn agent_that_exits_reports_exited() {
        let a = run(fake("exit", &[]), &opts(10_000, false));
        assert_eq!(a["status"], "exited", "{a:#}");
        assert_eq!(a["failed_step"], "initialize");
    }

    #[test]
    fn missing_binary_is_not_installed_not_an_error() {
        let mut spec = fake("normal", &[]);
        spec.program = None;
        let report = doctor_specs(&[spec], &opts(1000, false)).unwrap();
        assert_eq!(report["agents"][0]["status"], "not_installed");
        assert_eq!(report["summary"]["not_installed"], 1);
        assert_eq!(report["summary"]["failed"], 0);
        assert!(render_table(&report).contains("not installed"));
    }

    #[test]
    fn builtin_launch_commands_match_the_vendored_registry() {
        let (registry, info) = load_registry(None).unwrap();
        assert_eq!(info["source"], "vendored");
        for b in BUILTINS {
            let args: Vec<String> = b.args.iter().map(|s| s.to_string()).collect();
            match b.registry_id {
                Some(id) => {
                    let entry =
                        registry_entry(&registry, id).unwrap_or_else(|| panic!("{id} missing"));
                    assert_eq!(
                        registry_launch(entry, b.bin, &args).1,
                        Some(true),
                        "{} launch drifted",
                        b.id
                    );
                }
                None => assert_eq!(b.id, "kiro"),
            }
        }
    }

    #[test]
    fn versions_compare_numerically_and_ignore_build_suffixes() {
        assert_eq!(compare_versions(Some("0.94.0"), Some("0.94.0")), "current");
        assert_eq!(compare_versions(Some("0.63.0"), Some("0.94.0")), "behind");
        assert_eq!(compare_versions(Some("1.4.3"), Some("1.19.3")), "behind");
        assert_eq!(
            compare_versions(Some("2026.10.19-6bbf2c4"), Some("2026.10.19")),
            "current"
        );
        assert_eq!(compare_versions(Some("v2.0"), Some("1.9.9")), "ahead");
        assert_eq!(compare_versions(Some("nightly"), Some("1.0")), "unknown");
        assert_eq!(compare_versions(None, Some("1.0")), "unknown");
        assert_eq!(extract_version("goose 1.55.0\n").as_deref(), Some("1.55.0"));
        assert_eq!(extract_version("0.63.0").as_deref(), Some("0.63.0"));
        assert_eq!(extract_version("no version here"), None);
    }

    #[test]
    fn cli_rejects_bad_usage() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(cli(&s(&[])).is_err());
        assert!(cli(&s(&["frob"])).is_err());
        assert!(parse_cli(&s(&["--agent", "nobody"])).is_err());
        assert!(parse_cli(&s(&["--agent"])).is_err());
        assert!(parse_cli(&s(&["--timeout", "0"])).is_err());
        assert!(parse_cli(&s(&["--timeout", "abc"])).is_err());
        assert!(parse_cli(&s(&["--bogus"])).is_err());
        let o = parse_cli(&s(&[
            "--agent",
            "claude",
            "--agent",
            "gemini",
            "--timeout",
            "2.5",
            "--json",
        ]))
        .unwrap();
        assert_eq!(o.agents, ["claude", "gemini"]);
        assert_eq!(o.timeout, Duration::from_millis(2500));
        assert!(o.json && !o.first_token);
        assert!(doctor(&Options {
            cwd: Some("/nonexistent/dir".into()),
            ..Options::default()
        })
        .is_err());
        assert!(doctor(&Options {
            registry: Some("/nonexistent/r.json".into()),
            ..Options::default()
        })
        .is_err());
    }

    #[test]
    fn mcp_arguments_are_validated() {
        assert!(
            parse_mcp(&json!({"agents": ["claude"], "timeout": 3, "first_token": false})).is_ok()
        );
        assert!(parse_mcp(&json!({"agents": "claude"})).is_err());
        assert!(parse_mcp(&json!({"agents": ["nobody"]})).is_err());
        assert!(parse_mcp(&json!({"first_token": "yes"})).is_err());
        assert!(parse_mcp(&json!({"bogus": 1})).is_err());
    }
}

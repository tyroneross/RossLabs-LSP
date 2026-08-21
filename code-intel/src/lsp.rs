// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! A blocking LSP client: spawn a server, handshake, ask one thing, shut down.
//!
//! Scope is deliberate. This drives a server for the lifetime of one query
//! batch and then reaps it. It does not pool, cache, or stay resident — a
//! resident pool is the v0.2 daemon, and until it exists a short-lived client
//! that always reaps its child cannot accumulate the orphaned `SourceKitService`
//! population documented in easy-terminal/docs/lsp-memory-rca.md.

use crate::registry::Server;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

pub struct Client {
    child: Child,
    stdin: ChildStdin,
    /// Frames arrive on a thread so the main path can wait with a timeout.
    /// A blocking read cannot express "nothing arrived", which is exactly the
    /// state we must distinguish from "the answer is empty".
    rx: Receiver<Value>,
    next_id: i64,
    /// Diagnostics seen so far, keyed by document URI.
    diagnostics: Vec<(String, Value)>,
    /// Outstanding `$/progress` tokens. Non-empty means the server is still
    /// working (rust-analyzer indexing, clangd parsing) and any empty answer
    /// it gives is premature rather than authoritative.
    progress: Vec<String>,
    /// Whether the server has ever reported progress. Servers that never do
    /// must not be waited on.
    saw_progress: bool,
    /// Set once this server has returned a usable answer. A pooled server that
    /// has already answered is known-ready, so the readiness grace below is
    /// pure latency on every subsequent query — it was the whole 1.5s floor on
    /// warm daemon queries until this flag existed.
    proven_ready: bool,
    /// Open document versions. Pooled clients update an existing buffer with
    /// `didChange`; re-sending `didOpen` is invalid LSP and can return stale
    /// diagnostics after a file changes.
    opened: HashMap<String, i64>,
    /// A pooled server gets a small forked parent-death guard. It kills the
    /// whole server process group if the daemon dies unexpectedly.
    parent_guard: Option<libc::pid_t>,
    pooled: bool,
}

/// Wall-clock ceiling for one client session. A server that indexes forever
/// (rust-analyzer on a cold target/, jdtls on a big classpath) must not hang
/// the caller; it gets killed and the caller is told, rather than blocking.
const DEADLINE: Duration = Duration::from_secs(90);

impl Client {
    /// Spawn `server` rooted at `root` and complete the initialize handshake.
    pub fn start(server: &Server, root: &str) -> Result<Self, String> {
        Self::start_inner(server, root, false)
    }

    /// Start a daemon-owned client. Its process group and parent-death guard
    /// ensure a pooled server cannot survive an unclean daemon exit.
    pub fn start_pooled(server: &Server, root: &str) -> Result<Self, String> {
        Self::start_inner(server, root, true)
    }

    fn start_inner(server: &Server, root: &str, pooled: bool) -> Result<Self, String> {
        let mut command = Command::new(server.command);
        command
            .args(server.args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if pooled {
            // Children of this LSP inherit the group, so one kill reaps the
            // server and workers such as SourceKitService together.
            unsafe {
                command.pre_exec(|| {
                    if libc::setpgid(0, 0) == -1 {
                        Err(std::io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", server.command))?;

        let pid = child.id();
        // Fork before starting this client's reader/timer threads. The guard's
        // child branch uses only async-signal-safe libc calls, which remains
        // safe even if the daemon already owns other pooled reader threads.
        let parent_guard = if pooled {
            spawn_parent_guard(std::process::id(), pid)
        } else {
            None
        };
        std::thread::spawn(move || {
            std::thread::sleep(DEADLINE);
            // Kills only if still alive; a finished pid has already been reaped.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        });

        let stdin = child.stdin.take().ok_or("no stdin on server")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout on server")?);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || read_frames(stdout, tx));
        let mut c = Client {
            child,
            stdin,
            rx,
            next_id: 0,
            diagnostics: Vec::new(),
            progress: Vec::new(),
            proven_ready: false,
            saw_progress: false,
            opened: HashMap::new(),
            parent_guard,
            pooled,
        };

        let init = c.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": path_to_uri(root),
                "capabilities": {
                    "textDocument": {
                        "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                        "publishDiagnostics": {},
                        "definition": {}, "references": {}, "hover": {}
                    }
                }
            }),
        )?;
        c.notify("initialized", json!({}))?;
        let _ = init;
        Ok(c)
    }

    /// Send `didOpen` so the server has the buffer contents we are asking about.
    pub fn open(&mut self, path: &str, language_id: &str) -> Result<String, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        let uri = path_to_uri(path);
        if let Some(version) = self.opened.get_mut(&uri) {
            *version += 1;
            let next_version = *version;
            self.diagnostics.retain(|(known_uri, _)| known_uri != &uri);
            self.notify(
                "textDocument/didChange",
                json!({"textDocument": {"uri": uri, "version": next_version},
                    "contentChanges": [{"text": text}]}),
            )?;
        } else {
            self.notify(
                "textDocument/didOpen",
                json!({"textDocument": {
                    "uri": uri, "languageId": language_id, "version": 1, "text": text }}),
            )?;
            self.opened.insert(uri.clone(), 1);
        }
        Ok(uri)
    }

    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        loop {
            let msg = self.recv(DEADLINE)?;
            self.absorb_notification(&msg);
            if msg.get("id").and_then(|v| v.as_i64()) == Some(id) {
                if let Some(err) = msg.get("error") {
                    let m = err
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
                    // -32801 ContentModified / -32802 ServerCancelled are the
                    // protocol's own "ask again" signals, not failures. Keep the
                    // code so the caller can retry instead of reporting an error.
                    return Err(format!("[{code}] {method} failed: {m}"));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}))
    }

    /// Block until the server publishes diagnostics for `uri`, or `budget` elapses.
    ///
    /// Diagnostics are pushed, not requested, so "none yet" and "none at all"
    /// are indistinguishable without a wait. Returning an empty list on timeout
    /// would report a clean file that was merely slow.
    pub fn wait_diagnostics(&mut self, uri: &str, budget: Duration) -> Result<Value, String> {
        let deadline = std::time::Instant::now() + budget;
        loop {
            if let Some((_, d)) = self.diagnostics.iter().find(|(u, _)| u == uri) {
                return Ok(d.clone());
            }
            if std::time::Instant::now() >= deadline {
                return Err("timed out waiting for diagnostics".into());
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            match self.recv(remaining) {
                Ok(msg) => self.absorb_notification(&msg),
                Err(_) => return Err("timed out waiting for diagnostics".into()),
            }
        }
    }

    /// Block until the server has no outstanding `$/progress` work, or `budget`
    /// elapses. Returns whether the server actually reached idle.
    ///
    /// Without this, a query issued while rust-analyzer is still indexing comes
    /// back `[]` in milliseconds and is indistinguishable from a genuine "no
    /// results" — a false negative an agent would act on.
    pub fn wait_until_idle(&mut self, budget: Duration) -> bool {
        // A server that has already answered cannot be mid-load. Waiting again
        // buys nothing and costs the full grace on every warm query.
        if self.proven_ready {
            return true;
        }
        let deadline = Instant::now() + budget;
        // Give a server that reports progress a moment to announce it. One that
        // never reports is idle by definition and must not be waited on.
        let grace = Instant::now() + Duration::from_millis(1500);
        loop {
            if self.saw_progress && self.progress.is_empty() {
                return true;
            }
            if !self.saw_progress && Instant::now() >= grace {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let wait = deadline.min(grace).saturating_duration_since(now);
            match self.recv(wait.max(Duration::from_millis(50))) {
                Ok(msg) => self.absorb_notification(&msg),
                Err(_) => {
                    if !self.saw_progress {
                        return true;
                    }
                }
            }
        }
    }

    /// Record that this server produced a usable answer.
    pub fn mark_proven_ready(&mut self) {
        self.proven_ready = true;
    }

    fn absorb_notification(&mut self, msg: &Value) {
        if msg.get("method").and_then(|v| v.as_str()) == Some("$/progress") {
            if let Some(p) = msg.get("params") {
                let token = p.get("token").map(|t| t.to_string()).unwrap_or_default();
                match p
                    .get("value")
                    .and_then(|v| v.get("kind"))
                    .and_then(|k| k.as_str())
                {
                    Some("begin") => {
                        self.saw_progress = true;
                        self.progress.push(token);
                    }
                    Some("end") => self.progress.retain(|t| *t != token),
                    _ => {}
                }
            }
            return;
        }
        if msg.get("method").and_then(|v| v.as_str()) == Some("textDocument/publishDiagnostics") {
            if let Some(p) = msg.get("params") {
                if let Some(uri) = p.get("uri").and_then(|v| v.as_str()) {
                    let items = p.get("diagnostics").cloned().unwrap_or(json!([]));
                    self.diagnostics.retain(|(u, _)| u != uri);
                    self.diagnostics.push((uri.to_string(), items));
                }
            }
        }
    }

    fn send(&mut self, msg: Value) -> Result<(), String> {
        let body = serde_json::to_vec(&msg).map_err(|e| e.to_string())?;
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len()).map_err(|e| e.to_string())?;
        self.stdin.write_all(&body).map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())
    }

    fn recv(&mut self, timeout: Duration) -> Result<Value, String> {
        match self.rx.recv_timeout(timeout) {
            Ok(v) => Ok(v),
            Err(RecvTimeoutError::Timeout) => Err("timed out waiting for server".into()),
            Err(RecvTimeoutError::Disconnected) => {
                Err("language server closed the connection".into())
            }
        }
    }
}

/// Reader thread: parse Content-Length framed JSON off the server's stdout
/// until the pipe closes. Errors end the thread, which disconnects the channel
/// and surfaces to the caller as a closed connection.
fn read_frames(mut out: BufReader<std::process::ChildStdout>, tx: mpsc::Sender<Value>) {
    loop {
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            match out.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let t = line.trim_end();
            if t.is_empty() {
                break;
            }
            if let Some(v) = t.strip_prefix("Content-Length:") {
                match v.trim().parse() {
                    Ok(n) => len = n,
                    Err(_) => return,
                }
            }
        }
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u8; len];
        if out.read_exact(&mut buf).is_err() {
            return;
        }
        match serde_json::from_slice(&buf) {
            Ok(v) => {
                if tx.send(v).is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

impl Drop for Client {
    /// Always reap. Without this, every CLI invocation would leave a server
    /// behind and the tool would become the leak it exists to avoid.
    fn drop(&mut self) {
        let _ = self.notify("shutdown", json!(null));
        let _ = self.notify("exit", json!(null));
        if self.pooled {
            unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(guard) = self.parent_guard.take() {
            unsafe {
                libc::kill(guard, libc::SIGTERM);
                libc::waitpid(guard, std::ptr::null_mut(), 0);
            }
        }
    }
}

/// Fork an allocation-free guard process for one daemon-owned server group.
/// If the daemon is killed (including SIGKILL), the guard observes reparenting
/// and kills the group. This is necessary because child processes do not die
/// automatically when their parent exits on macOS or Linux.
fn spawn_parent_guard(parent_pid: u32, group_leader: u32) -> Option<libc::pid_t> {
    unsafe {
        let guard = libc::fork();
        if guard > 0 {
            return Some(guard);
        }
        if guard < 0 {
            return None;
        }
        loop {
            if libc::getppid() != parent_pid as libc::pid_t {
                libc::kill(-(group_leader as libc::pid_t), libc::SIGKILL);
                libc::_exit(0);
            }
            if libc::kill(-(group_leader as libc::pid_t), 0) == -1 {
                libc::_exit(0);
            }
            libc::usleep(100_000);
        }
    }
}

/// Convert a filesystem path to a `file://` URI, percent-encoding what must be.
pub fn path_to_uri(path: &str) -> String {
    let mut out = String::from("file://");
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Convert a 1-based (line, character) pair into the 0-based, **UTF-16**
/// position LSP requires.
///
/// LSP counts characters in UTF-16 code units, not bytes and not chars. On any
/// line containing non-ASCII text the three disagree, and a byte offset silently
/// points at the wrong symbol rather than failing — so this is where a wrong
/// answer would come from if it were skipped.
pub fn to_lsp_position(text: &str, line_1: usize, char_1: usize) -> Result<Value, String> {
    let line_0 = line_1.checked_sub(1).ok_or("line is 1-based")?;
    let char_0 = char_1.checked_sub(1).ok_or("column is 1-based")?;
    let line = text
        .lines()
        .nth(line_0)
        .ok_or_else(|| format!("line {line_1} is past end of file"))?;
    let utf16: usize = line.chars().take(char_0).map(|c| c.len_utf16()).sum();
    Ok(json!({"line": line_0, "character": utf16}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_position_is_unchanged() {
        let p = to_lsp_position("fn main() {}\n", 1, 4).unwrap();
        assert_eq!(p["line"], 0);
        assert_eq!(p["character"], 3);
    }

    #[test]
    fn astral_chars_count_as_two_utf16_units() {
        // "let 🦀 = 1" — the crab is 1 char, 4 bytes, 2 UTF-16 units.
        let p = to_lsp_position("let 🦀 = 1", 1, 7).unwrap();
        assert_eq!(p["character"], 7, "6 chars before col 7: 4 ascii + crab(2)");
    }

    #[test]
    fn bmp_non_ascii_counts_as_one_unit_not_its_bytes() {
        // "é" is 2 bytes but 1 UTF-16 unit; a byte-offset impl would say 2.
        let p = to_lsp_position("é = 1", 1, 3).unwrap();
        assert_eq!(p["character"], 2);
    }

    #[test]
    fn rejects_zero_and_overrun() {
        assert!(to_lsp_position("a\n", 0, 1).is_err());
        assert!(to_lsp_position("a\n", 1, 0).is_err());
        assert!(to_lsp_position("a\n", 9, 1).is_err());
    }

    #[test]
    fn uri_encodes_spaces_but_not_separators() {
        assert_eq!(path_to_uri("/a b/c.py"), "file:///a%20b/c.py");
        assert_eq!(path_to_uri("/x/y.rs"), "file:///x/y.rs");
    }
}

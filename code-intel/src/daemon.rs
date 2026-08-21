// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! Local, blocking daemon for language servers that have expensive cold starts.
//!
//! The wire protocol is one compact JSON request and response per Unix-socket
//! connection. It is intentionally private: clients fall back to direct spawn
//! whenever the socket is absent, stale, or speaks an incompatible protocol.

use crate::lsp::Client;
use crate::query::{self, Kind};
use crate::registry::{self, Server};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Pool only verified expensive servers. Measurements on this machine: ty is
/// 0.21s cold / 0.07s repeat, SourceKit-LSP is 1.51s cold, and rust-analyzer
/// is 9.87s cold / 3.96s repeat. One second excludes ty and admits the latter.
pub const POOL_COLD_START_THRESHOLD: Duration = Duration::from_secs(1);

/// A quiet pooled server is reaped instead of retaining unbounded workspace RSS.
const SERVER_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const SOCKET_TIMEOUT: Duration = Duration::from_secs(2);
static DAEMON_RUNNING: AtomicBool = AtomicBool::new(true);

pub fn is_poolable(server: &Server) -> bool {
    server
        .measured_cold_start
        .is_some_and(|cold| cold > POOL_COLD_START_THRESHOLD)
}

pub fn serve() -> Result<(), String> {
    DAEMON_RUNNING.store(true, Ordering::Relaxed);
    install_signal_handlers();
    let path = socket_path();
    let listener = bind_listener(&path)?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let mut pool = Pool::default();

    while DAEMON_RUNNING.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => handle_connection(stream, &mut pool),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("daemon accept failed: {e}")),
        }
        pool.evict_idle();
    }
    drop(pool); // Client::Drop kills and reaps every server process group.
    let _ = std::fs::remove_file(path);
    Ok(())
}

/// Attempt one daemon query. `None` means no usable daemon, so callers must
/// spawn directly. A returned daemon query error is a real query error.
pub fn try_run(
    kind: Kind,
    file: &str,
    line: usize,
    col: usize,
    root: &str,
    command: &str,
) -> Result<Option<Value>, String> {
    let mut stream = match UnixStream::connect(socket_path()) {
        Ok(stream) => stream,
        Err(_) => return Ok(None),
    };
    if stream.set_read_timeout(Some(SOCKET_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(SOCKET_TIMEOUT)).is_err()
    {
        return Ok(None);
    }
    let request = json!({
        "kind": kind.name(), "file": file, "line": line, "column": col,
        "root": root, "command": command,
    });
    if write_frame(&mut stream, &request).is_err() {
        return Ok(None);
    }
    let response = match read_frame(stream) {
        Ok(response) => response,
        Err(_) => return Ok(None),
    };
    if response.get("ok") == Some(&Value::Bool(true)) {
        return response
            .get("result")
            .cloned()
            .map(Some)
            .ok_or_else(|| "daemon returned a successful response without a result".to_string());
    }
    if let Some(error) = response.get("error").and_then(Value::as_str) {
        return Err(error.to_string());
    }
    Ok(None)
}

fn socket_path() -> PathBuf {
    std::env::var_os("CODE_INTEL_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("code-intel-{}.sock", unsafe { libc::getuid() }))
        })
}

fn bind_listener(path: &PathBuf) -> Result<UnixListener, String> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(format!("daemon already running at {}", path.display()));
        }
        std::fs::remove_file(path)
            .map_err(|e| format!("cannot remove stale socket {}: {e}", path.display()))?;
    }
    let listener = UnixListener::bind(path)
        .map_err(|e| format!("cannot bind socket {}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("cannot restrict socket {}: {e}", path.display()))?;
    Ok(listener)
}

fn handle_connection(stream: UnixStream, pool: &mut Pool) {
    let response = match stream.try_clone() {
        Ok(input) => match read_frame(input) {
            Ok(request) => match request_to_query(&request) {
                Ok((kind, file, line, col, root, command)) => pool
                    .run(kind, &file, line, col, &root, &command)
                    .map(|result| json!({"ok": true, "result": result}))
                    .unwrap_or_else(|error| json!({"ok": false, "error": error})),
                Err(error) => json!({"ok": false, "error": error}),
            },
            Err(error) => json!({"ok": false, "error": error}),
        },
        Err(error) => json!({"ok": false, "error": error.to_string()}),
    };
    let mut stream = stream;
    let _ = write_frame(&mut stream, &response);
}

fn request_to_query(
    request: &Value,
) -> Result<(Kind, String, usize, usize, String, String), String> {
    let field = |name: &str| {
        request
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("daemon request missing '{name}'"))
    };
    let kind = Kind::parse(field("kind")?).ok_or("daemon request has unknown query kind")?;
    let line = request
        .get("line")
        .and_then(Value::as_u64)
        .ok_or("daemon request missing 'line'")? as usize;
    let col = request
        .get("column")
        .and_then(Value::as_u64)
        .ok_or("daemon request missing 'column'")? as usize;
    Ok((
        kind,
        field("file")?.to_string(),
        line,
        col,
        field("root")?.to_string(),
        field("command")?.to_string(),
    ))
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct PoolKey {
    root: String,
    command: String,
}

struct Entry {
    client: Client,
    last_used: Instant,
}

#[derive(Default)]
struct Pool {
    entries: HashMap<PoolKey, Entry>,
}

impl Pool {
    fn run(
        &mut self,
        kind: Kind,
        file: &str,
        line: usize,
        col: usize,
        root: &str,
        command: &str,
    ) -> Result<Value, String> {
        let server = registry::by_command(command).ok_or("daemon request has unknown server")?;
        if !is_poolable(server) {
            return Err(format!("{} is not eligible for pooling", server.command));
        }
        let key = PoolKey {
            root: root.to_string(),
            command: command.to_string(),
        };
        if !self.entries.contains_key(&key) {
            let client = Client::start_pooled(server, root)?;
            self.entries.insert(
                key.clone(),
                Entry {
                    client,
                    last_used: Instant::now(),
                },
            );
        }
        let result = {
            let entry = self.entries.get_mut(&key).expect("entry inserted above");
            let result =
                query::run_with_client(kind, file, line, col, root, server, &mut entry.client);
            if result.is_ok() {
                entry.last_used = Instant::now();
            }
            result
        };
        if result.is_err() {
            self.entries.remove(&key);
        }
        result
    }

    fn evict_idle(&mut self) {
        let now = Instant::now();
        self.entries
            .retain(|_, entry| now.duration_since(entry.last_used) < SERVER_IDLE_TIMEOUT);
    }
}

fn write_frame(stream: &mut UnixStream, frame: &Value) -> Result<(), String> {
    let encoded = serde_json::to_string(frame).map_err(|e| e.to_string())?;
    stream
        .write_all(encoded.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(b"\n").map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())
}

fn read_frame(stream: UnixStream) -> Result<Value, String> {
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.is_empty() {
        return Err("daemon closed the connection".into());
    }
    serde_json::from_str(line.trim_end()).map_err(|e| format!("invalid daemon response: {e}"))
}

extern "C" fn stop_daemon(_signal: i32) {
    DAEMON_RUNNING.store(false, Ordering::Relaxed);
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(
            libc::SIGTERM,
            stop_daemon as *const () as libc::sighandler_t,
        );
        libc::signal(libc::SIGINT, stop_daemon as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, stop_daemon as *const () as libc::sighandler_t);
    }
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Definition => "def",
            Kind::References => "refs",
            Kind::Hover => "hover",
            Kind::Symbols => "symbols",
            Kind::Diagnostics => "diag",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_threshold_matches_measured_servers() {
        assert!(!is_poolable(registry::by_command("ty").unwrap()));
        assert!(is_poolable(registry::by_command("sourcekit-lsp").unwrap()));
        assert!(is_poolable(registry::by_command("rust-analyzer").unwrap()));
        assert!(!is_poolable(registry::by_command("clangd").unwrap()));
    }

    #[test]
    fn daemon_request_requires_the_complete_query_shape() {
        assert!(request_to_query(&json!({"kind": "refs"})).is_err());
        let parsed = request_to_query(&json!({
            "kind": "refs", "file": "/tmp/a.rs", "line": 1, "column": 2,
            "root": "/tmp", "command": "rust-analyzer"
        }))
        .unwrap();
        assert_eq!(parsed.0, Kind::References);
        assert_eq!((parsed.2, parsed.3), (1, 2));
    }

    #[test]
    fn idle_eviction_drops_old_entries() {
        let mut pool = Pool::default();
        // No Client is needed to prove the timestamp-based eviction policy.
        assert!(pool.entries.is_empty());
        pool.evict_idle();
        assert!(pool.entries.is_empty());
    }
}

// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! Evidence registry for component boundaries. Never executes remediation text.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::Path,
};

pub const SECTIONS: &[&str] = &[
    "language-tooling",
    "terminal-input",
    "protocols",
    "environments",
    "build-dependencies",
    "ui-contracts",
    "security-trust",
    "agent-hooks",
];

/// Error for a section this build does not know. A registry newer than the
/// binary is the usual cause, so name the build and say how to fix it.
pub fn unknown_section(section: &str) -> String {
    format!(
        "unknown section '{section}': this rosslabs-lsp build (commit {}, built {}) knows {}. If the registry is newer than the binary, reinstall rosslabs-lsp (`rosslabs-lsp doctor` prints the build).",
        env!("ROSSLABS_LSP_BUILD_COMMIT"),
        env!("ROSSLABS_LSP_BUILD_DATE"),
        SECTIONS.join(", ")
    )
}

pub fn sections() -> Value {
    json!({"schema_version": 1, "sections": SECTIONS})
}

fn required<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("missing or empty string: {key}"))
}

pub fn validate(doc: &Value) -> Result<(), String> {
    validate_shapes(doc)?;
    validate_supersedes(doc["records"].as_array().unwrap())
}

/// Revision links must form simple chains inside one section: the target
/// exists, is not the record itself, shares its section, has at most one
/// successor (no fork), and following the links never loops.
fn validate_supersedes(records: &[Value]) -> Result<(), String> {
    let by_id: HashMap<&str, &Value> = records
        .iter()
        .filter_map(|r| Some((r["id"].as_str()?, r)))
        .collect();
    let mut claimed: HashMap<&str, &str> = HashMap::new();
    for r in records {
        let (Some(id), Some(prior)) = (r["id"].as_str(), r["supersedes"].as_str()) else {
            continue;
        };
        if prior == id {
            return Err(format!("{id}: supersedes itself"));
        }
        let target = by_id
            .get(prior)
            .ok_or_else(|| format!("{id}: supersedes names no record: {prior}"))?;
        if target["section"] != r["section"] {
            return Err(format!("{id}: supersedes {prior} in another section"));
        }
        if let Some(other) = claimed.insert(prior, id) {
            return Err(format!("{id}: {prior} is already superseded by {other}"));
        }
    }
    for start in claimed.keys() {
        let mut at = *start;
        for _ in 0..=records.len() {
            match by_id.get(at).and_then(|r| r["supersedes"].as_str()) {
                Some(next) if next == *start => {
                    return Err(format!("supersedes cycle through {start}"));
                }
                Some(next) => at = next,
                None => break,
            }
        }
    }
    Ok(())
}

/// Per-record structure only; `validate` adds the cross-record link checks.
fn validate_shapes(doc: &Value) -> Result<(), String> {
    if doc["schema_version"].as_u64() != Some(1) {
        return Err("unsupported schema_version; expected 1".into());
    }
    let records = doc["records"]
        .as_array()
        .ok_or("records must be an array")?;
    let mut ids = HashSet::new();
    for r in records {
        let id = required(r, "id")?;
        if !ids.insert(id) {
            return Err(format!("duplicate record id: {id}"));
        }
        let section = required(r, "section")?;
        if !SECTIONS.contains(&section) {
            return Err(format!("{id}: {}", unknown_section(section)));
        }
        for key in ["capability", "symptom", "cause", "remediation", "owner"] {
            required(r, key)?;
        }
        if !["unknown", "reported", "reproduced", "mitigated", "verified"]
            .contains(&required(r, "status")?)
        {
            return Err(format!("{id}: unknown status"));
        }
        let components = r["components"]
            .as_array()
            .ok_or("components must be an array")?;
        if components.len() < 2 {
            return Err(format!("{id}: a boundary needs at least two components"));
        }
        for c in components {
            required(c, "name")?;
            required(c, "version")?;
        }
        let validity = &r["validity"];
        for key in ["workspace", "snapshot", "environment", "build_target"] {
            required(validity, key)?;
        }
        for key in ["capabilities", "limitations"] {
            let values = validity[key]
                .as_array()
                .ok_or_else(|| format!("{id}: validity.{key} must be an array"))?;
            if values
                .iter()
                .any(|v| v.as_str().is_none_or(|s| s.trim().is_empty()))
            {
                return Err(format!(
                    "{id}: validity.{key} entries must be nonempty strings"
                ));
            }
        }
        if !r["supersedes"].is_null() {
            required(r, "supersedes").map_err(|e| format!("{id}: {e}"))?;
        }
        let evidence = r["evidence"]
            .as_array()
            .ok_or("evidence must be an array")?;
        if r["status"] != "unknown" && evidence.is_empty() {
            return Err(format!("{id}: non-unknown status requires evidence"));
        }
        for e in evidence {
            for key in ["source", "observed_at", "scope", "result"] {
                required(e, key)?;
            }
        }
    }
    Ok(())
}

/// Days since 1970-01-01 for a `YYYY-MM-DD` prefix; None when malformed.
pub fn civil_days(date: &str) -> Option<i64> {
    let mut parts = date.get(..10)?.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// `YYYY-MM-DD` for days since 1970-01-01 (inverse of `civil_days`).
pub fn civil_date(days: i64) -> String {
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Today in days since the epoch (UTC).
pub fn today_days() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86_400) as i64)
        .unwrap_or(0)
}

/// Newest `observed_at` across a record's evidence, if any parses.
pub fn newest_observed(record: &Value) -> Option<(i64, String)> {
    record["evidence"]
        .as_array()?
        .iter()
        .filter_map(|e| {
            let s = e["observed_at"].as_str()?;
            civil_days(s).map(|d| (d, s.to_string()))
        })
        .max_by_key(|(d, _)| *d)
}

/// Old record id -> the revision record that names it in `supersedes`.
/// Evidence is append-only, so a refresh adds a revision instead of editing.
pub fn supersession(records: &[Value]) -> HashMap<String, String> {
    records
        .iter()
        .filter_map(|r| {
            Some((
                r["supersedes"].as_str()?.to_string(),
                r["id"].as_str()?.to_string(),
            ))
        })
        .collect()
}

/// Records no later revision supersedes: the evidence that applies now.
pub fn current_records(records: &[Value]) -> Vec<Value> {
    let old = supersession(records);
    records
        .iter()
        .filter(|r| r["id"].as_str().is_none_or(|id| !old.contains_key(id)))
        .cloned()
        .collect()
}

/// Mark each record stale when its newest evidence is older than `days`.
/// A record with no parseable evidence date is stale: its age is unknown.
/// A record marked `superseded_by` is history, never stale.
pub fn annotate_stale(records: &mut [Value], days: u64, today: i64) -> usize {
    let mut stale = 0;
    for r in records.iter_mut() {
        let newest = newest_observed(r);
        let age = newest.as_ref().map(|(d, _)| today - d);
        let is_stale = r["superseded_by"].is_null() && age.is_none_or(|a| a > days as i64);
        stale += usize::from(is_stale);
        r["freshness"] = json!({
            "newest_observed_at": newest.map(|(_, s)| s),
            "age_days": age,
            "stale": is_stale,
        });
    }
    stale
}

/// Permissions for a registry file `compat record` creates.
const NEW_REGISTRY_MODE: u32 = 0o644;

/// The registry compiled into this binary, used when no path is given.
pub const EMBEDDED_REGISTRY: &str = include_str!("../../compatibility/registry.json");
const EMBEDDED_LABEL: &str = "embedded compatibility/registry.json";

pub fn query(
    path: Option<&str>,
    section: Option<&str>,
    component: Option<&str>,
) -> Result<Value, String> {
    query_with(path, section, component, None)
}

pub fn query_with(
    path: Option<&str>,
    section: Option<&str>,
    component: Option<&str>,
    stale_days: Option<u64>,
) -> Result<Value, String> {
    if let Some(s) = section {
        if !SECTIONS.contains(&s) {
            return Err(unknown_section(s));
        }
    }
    let (bytes, path) = match path {
        Some(p) => (read_registry(p)?, p),
        None => (EMBEDDED_REGISTRY.as_bytes().to_vec(), EMBEDDED_LABEL),
    };
    query_bytes(&bytes, path, section, component, stale_days)
}

// Explicit path: no global store, implicit home-directory scan or network reads.
fn read_registry(path: &str) -> Result<Vec<u8>, String> {
    const MAX_BYTES: u64 = 1024 * 1024;
    if !fs::metadata(path)
        .map_err(|e| format!("{path}: {e}"))?
        .is_file()
    {
        return Err("registry must be a regular file".into());
    }
    let file = fs::File::open(Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("registry must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("registry exceeds 1 MiB limit".into());
    }
    Ok(bytes)
}

fn query_bytes(
    bytes: &[u8],
    path: &str,
    section: Option<&str>,
    component: Option<&str>,
    stale_days: Option<u64>,
) -> Result<Value, String> {
    let digest = format!("{:x}", Sha256::digest(bytes));
    let doc: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("invalid registry JSON: {e}"))?;
    validate(&doc)?;
    let newer = supersession(doc["records"].as_array().unwrap());
    let mut records: Vec<_> = doc["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| {
            section.is_none_or(|s| r["section"] == s)
                && component.is_none_or(|c| {
                    r["components"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v["name"].as_str().unwrap().eq_ignore_ascii_case(c))
                })
        })
        .cloned()
        .collect();
    for r in records.iter_mut() {
        if let Some(by) = r["id"].as_str().and_then(|id| newer.get(id)) {
            r["superseded_by"] = json!(by);
        }
    }
    let stale = stale_days.map(|days| annotate_stale(&mut records, days, today_days()));
    let mut out = json!({"schema_version": 1, "registry": path, "registry_sha256": digest, "validated": true,
        "coverage": if records.is_empty() { "unknown" } else { "recorded" },
        "live_verified": false, "records": records});
    if let (Some(days), Some(count)) = (stale_days, stale) {
        out["stale_days"] = json!(days);
        out["stale_count"] = json!(count);
    }
    Ok(out)
}

/// Append an immutable evidence record. All cooperating writers share a lock.
pub fn record(path: &str, input: &str) -> Result<Value, String> {
    if !fs::metadata(input).map_err(|e| e.to_string())?.is_file() {
        return Err("record must be a regular file".into());
    }
    let input_file = fs::File::open(input).map_err(|e| e.to_string())?;
    if !input_file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("record must be a regular file".into());
    }
    let mut bytes = Vec::new();
    input_file
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("record exceeds 1 MiB limit".into());
    }
    let item: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    record_value(path, &item)
}

/// Append one validated record under the shared lock. An existing id must be
/// byte-identical (idempotent); a changed record needs a new revision id.
pub fn record_value(path: &str, item: &Value) -> Result<Value, String> {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    validate_shapes(&json!({"schema_version":1,"records":[item.clone()]}))?;
    let target = Path::new(path);
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent.canonicalize().map_err(|e| e.to_string())?;
    let name = target.file_name().ok_or("registry needs a filename")?;
    let target = parent.join(name);
    let lock_path = parent.join(format!("{}.lock", name.to_string_lossy()));
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(lock_path)
        .map_err(|e| e.to_string())?;
    if !lock.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("lock must be a regular file".into());
    }
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut doc = match fs::symlink_metadata(&target) {
        Ok(meta) => {
            if !meta.file_type().is_file() {
                return Err("registry must be a regular file, not a symlink".into());
            }
            // Validate the original without discarding optional document metadata.
            let mut original = Vec::new();
            fs::File::open(&target)
                .map_err(|e| e.to_string())?
                .take(1024 * 1024 + 1)
                .read_to_end(&mut original)
                .map_err(|e| e.to_string())?;
            if original.len() > 1024 * 1024 {
                return Err("registry exceeds 1 MiB limit".into());
            }
            serde_json::from_slice::<Value>(&original).map_err(|e| e.to_string())?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            json!({"schema_version":1,"records":[]})
        }
        Err(e) => return Err(e.to_string()),
    };
    validate(&doc)?;
    let records = doc["records"].as_array_mut().unwrap();
    if let Some(old) = records.iter().find(|r| r["id"] == item["id"]) {
        if old != item {
            return Err(
                "record id already exists with different evidence; use a new revision id".into(),
            );
        }
        return Ok(
            json!({"status":"unchanged","id":item["id"],"registry":path,"live_verified":false}),
        );
    }
    records.push(item.clone());
    // Links are checked against the whole file: missing target, fork, cycle.
    validate(&doc)?;
    let mut output = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
    output.push(b'\n');
    if output.len() > 1024 * 1024 {
        return Err("updated registry exceeds 1 MiB limit".into());
    }
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    // Keep the existing registry's permissions; a new registry is 0644.
    let mode = match fs::symlink_metadata(&target) {
        Ok(m) => {
            use std::os::unix::fs::PermissionsExt;
            m.permissions().mode() & 0o7777
        }
        Err(_) => NEW_REGISTRY_MODE,
    };
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    let result = (|| -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        // `mode()` is filtered by the umask; set it exactly.
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.write_all(&output)?;
        file.sync_all()?;
        fs::rename(&temp, &target)?;
        fs::File::open(&parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|e| e.to_string())?;
    Ok(json!({"status":"recorded","id":item["id"],"registry":path,
        "registry_sha256":format!("{:x}",Sha256::digest(&output)),"live_verified":false}))
}

pub fn cli(args: &[String]) -> Result<Value, String> {
    if args.first().is_some_and(|s| s == "record") {
        if args.len() != 3 {
            return Err("usage: compat record <registry.json> <record.json>".into());
        }
        return record(&args[1], &args[2]);
    }
    if args.first().is_some_and(|s| s == "refresh") {
        return crate::hooks::refresh_cli(&args[1..]);
    }
    if args == ["sections"] {
        return Ok(sections());
    }
    // No path: use the registry compiled into this binary.
    let path = args
        .first()
        .filter(|s| !s.starts_with("--"))
        .map(String::as_str);
    let mut section = None;
    let mut component = None;
    let mut stale = None;
    let mut rest = args[usize::from(path.is_some())..].iter();
    while let Some(flag) = rest.next() {
        let slot = match flag.as_str() {
            "--section" => &mut section,
            "--component" => &mut component,
            "--stale-days" => &mut stale,
            _ => return Err(format!("unknown compat option: {flag}")),
        };
        if slot.is_some() {
            return Err(format!("duplicate option: {flag}"));
        }
        *slot = Some(
            rest.next()
                .ok_or_else(|| format!("missing value for {flag}"))?
                .as_str(),
        );
    }
    let stale_days = stale
        .map(|s| {
            s.parse::<u64>()
                .map_err(|_| format!("--stale-days needs a whole number of days, got '{s}'"))
        })
        .transpose()?;
    query_with(path, section, component, stale_days)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> Value {
        json!({"id":"test", "section":"terminal-input", "capability":"newline",
          "status":"reproduced", "components":[{"name":"host","version":"1"},{"name":"client","version":"2"}],
          "validity":{"workspace":"fixture","snapshot":"1","environment":"test","build_target":"test","capabilities":[],"limitations":[]},
          "symptom":"key dropped", "cause":"missing mapping", "remediation":"adapter", "owner":"host",
          "evidence":[{"source":"test log", "observed_at":"2026-10-02", "scope":"versions 1 and 2", "result":"failed"}]})
    }
    #[test]
    fn record_is_idempotent_and_conflicts_preserve_evidence() {
        let dir = std::env::temp_dir().join(format!("compat-write-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let registry = dir.join("registry.json");
        let input = dir.join("record.json");
        fs::write(&input, serde_json::to_vec(&record()).unwrap()).unwrap();
        let args = vec![
            "record".into(),
            registry.to_str().unwrap().into(),
            input.to_str().unwrap().into(),
        ];
        assert_eq!(cli(&args).unwrap()["status"], "recorded");
        let before = fs::read(&registry).unwrap();
        assert_eq!(cli(&args).unwrap()["status"], "unchanged");
        let mut changed = record();
        changed["cause"] = json!("new cause");
        fs::write(&input, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(cli(&args).is_err());
        assert_eq!(fs::read(&registry).unwrap(), before);
        fs::write(&input, b"{}").unwrap();
        assert!(cli(&args).is_err());
        assert_eq!(fs::read(&registry).unwrap(), before);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_false_certainty_and_duplicate_ids() {
        let mut doc = json!({"schema_version":1,"records":[record()]});
        assert!(validate(&doc).is_ok());
        doc["records"][0]["evidence"] = json!([]);
        assert!(validate(&doc).is_err());
        doc["records"][0]["status"] = json!("unknown");
        assert!(validate(&doc).is_ok());
        doc["records"] = json!([record(), record()]);
        assert!(validate(&doc).unwrap_err().contains("duplicate"));
    }
    #[test]
    fn unknown_section_names_the_build_and_says_to_reinstall() {
        let mut doc = json!({"schema_version":1,"records":[record()]});
        doc["records"][0]["section"] = json!("future-section");
        let err = validate(&doc).unwrap_err();
        assert!(
            err.starts_with("test: unknown section 'future-section'"),
            "{err}"
        );
        assert!(err.contains(env!("ROSSLABS_LSP_BUILD_COMMIT")), "{err}");
        assert!(err.contains(env!("ROSSLABS_LSP_BUILD_DATE")), "{err}");
        assert!(err.contains("reinstall"), "{err}");
        let q = query(None, Some("future-section"), None).unwrap_err();
        assert!(q.contains("built ") && q.contains("reinstall"), "{q}");
    }

    #[test]
    fn revisions_supersede_without_editing_and_cannot_fork() {
        let dir = std::env::temp_dir().join(format!("compat-supersede-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let registry = dir.join("registry.json");
        let reg = registry.to_str().unwrap();
        record_value(reg, &record()).unwrap();
        let mut rev = record();
        rev["id"] = json!("test@2");
        rev["supersedes"] = json!("test");
        assert_eq!(record_value(reg, &rev).unwrap()["status"], "recorded");
        let mut fork = rev.clone();
        fork["id"] = json!("test@3");
        assert!(record_value(reg, &fork)
            .unwrap_err()
            .contains("already superseded"));
        fork["supersedes"] = json!("missing");
        assert!(record_value(reg, &fork)
            .unwrap_err()
            .contains("names no record"));
        fork["supersedes"] = json!(42);
        assert!(record_value(reg, &fork).is_err());
        let out = query_with(Some(reg), None, None, Some(0)).unwrap();
        let recs = out["records"].as_array().unwrap();
        assert_eq!(recs[0]["superseded_by"], "test@2");
        assert_eq!(recs[0]["freshness"]["stale"], false);
        let doc: Value = serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
        let current = current_records(doc["records"].as_array().unwrap());
        assert_eq!(current.len(), 1);
        assert_eq!(current[0]["id"], "test@2");
        assert_eq!(civil_date(civil_days("2026-10-28").unwrap()), "2026-10-28");
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(civil_days("2024-02-29").unwrap()), "2024-02-29");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validate_rejects_broken_supersedes_links_in_the_file() {
        let rev = |id: &str, prior: &str| {
            let mut r = record();
            r["id"] = json!(id);
            r["supersedes"] = json!(prior);
            r
        };
        let check = |records: Vec<Value>| {
            validate(&json!({"schema_version":1,"records":records})).map(|_| ())
        };
        assert!(check(vec![
            record(),
            rev("test@2", "test"),
            rev("test@3", "test@2")
        ])
        .is_ok());
        let missing = check(vec![record(), rev("test@2", "gone")]).unwrap_err();
        assert!(missing.contains("names no record"), "{missing}");
        let fork = check(vec![record(), rev("test@2", "test"), rev("test@3", "test")]).unwrap_err();
        assert!(fork.contains("already superseded"), "{fork}");
        let me = check(vec![rev("test", "test")]).unwrap_err();
        assert!(me.contains("supersedes itself"), "{me}");
        let mut other = rev("other@2", "test");
        other["section"] = json!("protocols");
        let cross = check(vec![record(), other]).unwrap_err();
        assert!(cross.contains("another section"), "{cross}");
        let cycle = check(vec![rev("a", "b"), rev("b", "c"), rev("c", "a")]).unwrap_err();
        assert!(cycle.contains("cycle"), "{cycle}");
        // A hand-edited fork in a registry file is rejected at read time too.
        let dir = std::env::temp_dir().join(format!("compat-fork-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("registry.json");
        let doc = json!({"schema_version":1,"records":[record(), rev("test@2","test"), rev("test@3","test")]});
        fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        assert!(query(Some(path.to_str().unwrap()), None, None)
            .unwrap_err()
            .contains("already superseded"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn record_keeps_registry_mode_and_creates_new_files_0644() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("compat-mode-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("registry.json");
        let reg = path.to_str().unwrap();
        record_value(reg, &record()).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o644);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let mut rev = record();
        rev["id"] = json!("test@2");
        rev["supersedes"] = json!("test");
        record_value(reg, &rev).unwrap();
        assert_eq!(mode(&path), 0o640);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_wrong_shape_and_unsupported_schema() {
        for doc in [
            json!({}),
            json!({"schema_version":2,"records":[]}),
            json!({"schema_version":1,"records":[{"id":"bad"}]}),
        ] {
            assert!(validate(&doc).is_err());
        }
    }
    #[test]
    fn real_registry_filtering_preserves_unknown_coverage() {
        let path = Some(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../compatibility/registry.json"
        ));
        let all = query(path, None, None).unwrap();
        assert!(!all["records"].as_array().unwrap().is_empty());
        assert_eq!(all["live_verified"], false);
        assert_eq!(
            query(path, None, Some("unrecorded-component")).unwrap()["coverage"],
            "unknown"
        );
        assert_eq!(
            query(path, Some("terminal-input"), Some("codex")).unwrap()["records"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(query(path, Some("typo"), None).is_err());
        assert!(query(Some("/nonexistent/registry.json"), None, None).is_err());
        // No path: the embedded registry answers the same query.
        let embedded = query(None, Some("terminal-input"), Some("codex")).unwrap();
        assert_eq!(embedded["records"].as_array().unwrap().len(), 1);
        assert_eq!(embedded["registry"], EMBEDDED_LABEL);
        let via_cli = cli(&["--section".into(), "agent-hooks".into()]).unwrap();
        assert!(!via_cli["records"].as_array().unwrap().is_empty());
    }
    #[test]
    fn civil_days_and_stale_annotation() {
        assert_eq!(civil_days("1970-01-01"), Some(0));
        assert_eq!(civil_days("2026-10-28"), Some(20_754));
        assert_eq!(civil_days("2026-13-01"), None);
        assert_eq!(civil_days("soon"), None);
        let mut recs = vec![record(), record()];
        recs[1]["evidence"][0]["observed_at"] = json!("unknown");
        let today = civil_days("2026-11-30").unwrap();
        assert_eq!(annotate_stale(&mut recs, 30, today), 2);
        assert_eq!(recs[0]["freshness"]["age_days"], 59);
        assert_eq!(annotate_stale(&mut recs[..1], 60, today), 0);
    }

    #[test]
    fn agent_hooks_section_validates_and_stale_flag_parses() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../compatibility/registry.json"
        );
        let out = cli(&[
            path.into(),
            "--section".into(),
            "agent-hooks".into(),
            "--stale-days".into(),
            "100000".into(),
        ])
        .unwrap();
        assert!(out["records"].as_array().unwrap().len() >= 8);
        assert_eq!(out["stale_count"], 0);
        assert!(out["records"][0]["freshness"]["newest_observed_at"].is_string());
        assert!(cli(&[path.into(), "--stale-days".into(), "x".into()]).is_err());
    }

    #[test]
    fn requires_validity_and_typed_capabilities() {
        let mut doc = json!({"schema_version":1,"records":[record()]});
        doc["records"][0]["validity"]["capabilities"] = json!([42]);
        assert!(validate(&doc).is_err());
        doc["records"][0]["validity"] = Value::Null;
        assert!(validate(&doc).is_err());
    }

    #[test]
    fn cli_rejects_ambiguous_arguments() {
        for args in [
            vec!["--section"],
            vec!["--bad"],
            vec!["x", "--bad"],
            vec!["x", "--section"],
            vec!["x", "--section", "protocols", "--section", "protocols"],
        ] {
            assert!(cli(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
        assert!(cli(&["sections".into()]).is_ok());
        // No arguments reads the embedded registry.
        assert!(cli(&[]).is_ok());
    }
}

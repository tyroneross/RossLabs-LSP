// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! Evidence registry for component boundaries. Never executes remediation text.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fs, io::Read, path::Path};

pub const SECTIONS: &[&str] = &[
    "language-tooling",
    "terminal-input",
    "protocols",
    "environments",
    "build-dependencies",
    "ui-contracts",
    "security-trust",
];

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
        if !SECTIONS.contains(&required(r, "section")?) {
            return Err(format!("{id}: unknown section"));
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

pub fn query(path: &str, section: Option<&str>, component: Option<&str>) -> Result<Value, String> {
    if let Some(s) = section {
        if !SECTIONS.contains(&s) {
            return Err(format!("unknown section: {s}"));
        }
    }
    // Explicit path: no global store, implicit home-directory scan or network reads.
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
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let doc: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid registry JSON: {e}"))?;
    validate(&doc)?;
    let records: Vec<_> = doc["records"]
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
    Ok(
        json!({"schema_version": 1, "registry": path, "registry_sha256": digest, "validated": true,
        "coverage": if records.is_empty() { "unknown" } else { "recorded" },
        "live_verified": false, "records": records}),
    )
}

/// Append an immutable evidence record. All cooperating writers share a lock.
pub fn record(path: &str, input: &str) -> Result<Value, String> {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
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
    validate(&json!({"schema_version":1,"records":[item.clone()]}))?;
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
        if old != &item {
            return Err(
                "record id already exists with different evidence; use a new revision id".into(),
            );
        }
        return Ok(
            json!({"status":"unchanged","id":item["id"],"registry":path,"live_verified":false}),
        );
    }
    records.push(item.clone());
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
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    let result = (|| -> std::io::Result<()> {
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
    if args == ["sections"] {
        return Ok(sections());
    }
    let path = args.first().filter(|s| !s.starts_with("--")).ok_or(
        "usage: compat sections | compat <registry.json> [--section NAME] [--component NAME]",
    )?;
    let mut section = None;
    let mut component = None;
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        let slot = match flag.as_str() {
            "--section" => &mut section,
            "--component" => &mut component,
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
    query(path, section, component)
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
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../compatibility/registry.json"
        );
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
        assert!(query("/nonexistent/registry.json", None, None).is_err());
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
            vec![],
            vec!["x", "--bad"],
            vec!["x", "--section"],
            vec!["x", "--section", "protocols", "--section", "protocols"],
        ] {
            assert!(cli(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
        assert!(cli(&["sections".into()]).is_ok());
    }
}

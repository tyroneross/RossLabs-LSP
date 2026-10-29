// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! Stamp a source fingerprint into the binary.
//!
//! The daemon gate used CARGO_PKG_VERSION alone, which stays "0.1.0" across
//! rebuilds: after `cargo install`, a daemon started from the previous binary
//! kept answering with the previous behaviour and nothing said so. Hashing the
//! sources gives every behaviour-changing build a distinct identity.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run git and return trimmed stdout, or None when git or the repo is absent
/// (a source tarball, a sandboxed build).
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Stamp the commit and build date that `doctor` prints, so an install that
/// predates a feature is visible instead of failing with "unknown query".
fn stamp_git() {
    // Re-run when HEAD moves; otherwise cargo would keep a stale commit.
    for path in [
        git(&["rev-parse", "--git-path", "HEAD"]),
        git(&["symbolic-ref", "-q", "HEAD"]).and_then(|r| git(&["rev-parse", "--git-path", &r])),
        git(&["rev-parse", "--git-path", "packed-refs"]),
    ]
    .into_iter()
    .flatten()
    {
        println!("cargo:rerun-if-changed={path}");
    }
    let commit = git(&["rev-parse", "--short=12", "HEAD"])
        .map(|c| {
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some();
            if dirty {
                format!("{c}-dirty")
            } else {
                c
            }
        })
        .unwrap_or_else(|| "unknown".into());
    let date = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=ROSSLABS_LSP_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=ROSSLABS_LSP_BUILD_DATE={date}");
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    stamp_git();
    let mut files = Vec::new();
    collect(Path::new("src"), &mut files);
    files.sort();
    let mut hasher = DefaultHasher::new();
    for file in &files {
        file.hash(&mut hasher);
        std::fs::read(file).unwrap_or_default().hash(&mut hasher);
    }
    std::fs::read("Cargo.toml")
        .unwrap_or_default()
        .hash(&mut hasher);
    println!(
        "cargo:rustc-env=ROSSLABS_LSP_BUILD_ID={}+{:016x}",
        std::env::var("CARGO_PKG_VERSION").unwrap_or_default(),
        hasher.finish()
    );
}

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

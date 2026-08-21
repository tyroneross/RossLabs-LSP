// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! Extension -> language server mapping.
//!
//! Deliberately a table, not a config file. A server is only useful if its
//! binary is on PATH, so the table is the union of what we know how to drive
//! and `doctor` reports which half is actually installed.

/// One language server we know how to launch.
pub struct Server {
    /// LSP `languageId` sent in `didOpen`. Servers reject unknown ids.
    pub language_id: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    /// Extensions this server claims, leading dot included.
    pub extensions: &'static [&'static str],
    /// Where to get it, shown by `doctor` when the binary is missing.
    pub install: &'static str,
}

pub const SERVERS: &[Server] = &[
    Server {
        language_id: "python",
        command: "ty",
        args: &["server"],
        extensions: &[".py", ".pyi"],
        install: "uv tool install ty",
    },
    Server {
        language_id: "rust",
        command: "rust-analyzer",
        args: &[],
        extensions: &[".rs"],
        install: "brew install rust-analyzer",
    },
    Server {
        language_id: "swift",
        command: "sourcekit-lsp",
        args: &[],
        extensions: &[".swift"],
        install: "ships with Xcode / the Swift toolchain",
    },
    Server {
        language_id: "typescript",
        command: "typescript-language-server",
        args: &["--stdio"],
        extensions: &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"],
        install: "npm i -g typescript-language-server typescript",
    },
    Server {
        language_id: "c",
        command: "clangd",
        args: &[],
        extensions: &[".c", ".h", ".cc", ".cpp", ".hpp", ".m", ".mm"],
        install: "ships with Xcode / llvm",
    },
];

/// Pick the server that claims `path`'s extension.
pub fn for_path(path: &str) -> Option<&'static Server> {
    let ext = path.rfind('.').map(|i| &path[i..])?.to_ascii_lowercase();
    SERVERS
        .iter()
        .find(|s| s.extensions.iter().any(|e| *e == ext))
}

/// True when the server's binary resolves on PATH.
pub fn is_installed(server: &Server) -> bool {
    which(server.command).is_some()
}

/// Minimal `which`: PATH scan for an executable file. Avoids a dependency.
pub fn which(bin: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if let Ok(meta) = std::fs::metadata(&candidate) {
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_extension_to_server() {
        assert_eq!(for_path("a/b/c.py").unwrap().command, "ty");
        assert_eq!(for_path("main.rs").unwrap().command, "rust-analyzer");
        assert_eq!(for_path("View.swift").unwrap().language_id, "swift");
    }

    #[test]
    fn extension_match_is_case_insensitive() {
        assert!(for_path("Main.PY").is_some());
    }

    #[test]
    fn unknown_and_extensionless_paths_are_none() {
        assert!(for_path("notes.txt").is_none());
        assert!(for_path("Makefile").is_none());
    }

    #[test]
    fn every_extension_is_claimed_once() {
        let mut seen = std::collections::HashSet::new();
        for s in SERVERS {
            for e in s.extensions {
                assert!(seen.insert(*e), "extension {e} claimed twice");
                assert!(e.starts_with('.'), "extension {e} missing leading dot");
            }
        }
    }
}

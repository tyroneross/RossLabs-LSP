// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! Smoke test for `scripts/weekly_freshness.sh` against the built binary and
//! fixture binaries. The script must always exit 0 and print one summary line.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/weekly_freshness.sh")
}

fn run(bin: &Path, home: &Path) -> (bool, String) {
    let out = Command::new("/bin/bash")
        .arg(script())
        .arg(bin)
        .env("HOME", home)
        // No agent hosts on PATH: the drift report detects only markers.
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn weekly_freshness_summarises_and_always_exits_zero() {
    let root = std::env::temp_dir().join(format!("weekly-freshness-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();

    let (ok, out) = run(Path::new(env!("CARGO_BIN_EXE_rosslabs-lsp")), &home);
    assert!(ok, "{out}");
    assert!(out.contains("weekly-freshness: stale "), "{out}");
    assert!(out.contains("current records"), "{out}");
    assert!(out.contains("drift 0 host(s)"), "{out}");

    // A failing binary: stderr is reported, stdout JSON is never mixed with it.
    let bad = root.join("bad-lsp");
    fs::write(&bad, "#!/bin/sh\necho 'boom on stderr' >&2\nexit 3\n").unwrap();
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o755)).unwrap();
    let (ok, out) = run(&bad, &home);
    assert!(ok, "{out}");
    assert!(out.contains("compat failed: boom on stderr"), "{out}");

    // A binary that warns on stderr but prints valid JSON still summarises.
    let real = env!("CARGO_BIN_EXE_rosslabs-lsp");
    let noisy = root.join("noisy-lsp");
    fs::write(
        &noisy,
        format!("#!/bin/sh\necho 'warning: noise' >&2\nexec '{real}' \"$@\"\n"),
    )
    .unwrap();
    fs::set_permissions(&noisy, fs::Permissions::from_mode(0o755)).unwrap();
    let (ok, out) = run(&noisy, &home);
    assert!(ok && out.contains("weekly-freshness: stale "), "{out}");

    let (ok, out) = run(&root.join("missing"), &home);
    assert!(ok && out.contains("not found; skipped"), "{out}");
    fs::remove_dir_all(&root).unwrap();
}

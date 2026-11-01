// SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
// SPDX-License-Identifier: Apache-2.0

//! Real-server regressions. Requires `typescript-language-server` and TypeScript
//! on PATH. Run explicitly with `cargo test --test javascript -- --ignored
//! --test-threads=1`. Ignored by default so an absent optional server does not
//! make the ordinary offline suite depend on a machine's npm installation.

use serde_json::Value;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "RossLabs JavaScript project {} {}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let project = Self(root);
        project.write(
            "package.json",
            r#"{"name":"lsp-javascript-regression","private":true}"#,
        );
        project
    }

    fn write(&self, name: &str, contents: &str) {
        fs::write(self.0.join(name), contents).unwrap();
    }

    fn query(&self, verb: &str, target: &str) -> Value {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let stdout_path = self.0.join(format!("query-{serial}.stdout"));
        let stderr_path = self.0.join(format!("query-{serial}.stderr"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_rosslabs-lsp"));
        command
            .args([verb, target])
            .current_dir(&self.0)
            .stdin(Stdio::null())
            .stdout(fs::File::create(&stdout_path).unwrap())
            .stderr(fs::File::create(&stderr_path).unwrap());
        // Own the CLI and its language-server descendants as one group, so a
        // failed regression can terminate only its own processes with SIGTERM.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let started = Instant::now();
        let budget = Duration::from_secs(if verb == "diag" { 40 } else { 25 });
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() >= budget {
                unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGTERM) };
                child.wait().unwrap();
                panic!(
                    "{verb} {target} exceeded {budget:?}; stderr: {}",
                    fs::read_to_string(&stderr_path).unwrap()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let stdout = fs::read_to_string(stdout_path).unwrap();
        let stderr = fs::read_to_string(stderr_path).unwrap();
        assert!(status.success(), "{verb} {target}: {stderr}\n{stdout}");
        let result: Value = serde_json::from_str(&stdout).expect("CLI must emit one JSON envelope");
        assert_eq!(result["server"], "typescript-language-server", "{result}");
        assert_eq!(result["ready"], true, "{result}");
        result
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn assert_location(result: &Value, file: &str, line: u64) {
    let locations = result["result"].as_array().expect("location array");
    // A path containing spaces may be absolute when the encoded URI does not
    // share the compact formatter's literal root prefix. Both forms must still
    // identify the actual file and line, rather than merely be nonempty.
    let suffix = format!("{file}:{line}");
    assert!(
        locations
            .iter()
            .any(|hit| hit["at"].as_str().is_some_and(|at| at.ends_with(&suffix))),
        "expected {suffix}: {result}"
    );
}

#[test]
#[ignore = "requires installed typescript-language-server and TypeScript"]
fn commonjs_import_definition_and_cross_file_references() {
    let project = Project::new();
    // References in an unopened file require that file to belong to the
    // configured project; an inferred project may contain only the open file.
    project.write(
        "jsconfig.json",
        r#"{"compilerOptions":{"allowJs":true},"include":["library.js","consumer.js"]}"#,
    );
    project.write(
        "library.js",
        "exports.greet = function(name) { return `Hello ${name}`; };\n",
    );
    project.write(
        "consumer.js",
        "const { greet } = require('./library');\ngreet('Planner');\n",
    );
    assert_location(&project.query("def", "consumer.js:2:2"), "library.js", 1);
    // The exported property and the destructured local binding are distinct
    // TypeScript symbols: inspect each at its own semantic boundary.
    assert_location(&project.query("refs", "library.js:1:9"), "consumer.js", 1);
    assert_location(&project.query("refs", "consumer.js:2:2"), "consumer.js", 2);
}

#[test]
#[ignore = "requires installed typescript-language-server and TypeScript"]
fn completed_empty_javascript_answers_are_ready_without_indexing_retry() {
    let project = Project::new();
    project.write("unused.js", "function unusedHere() { return 42; }\n");
    project.write(
        "external.js",
        "const external = require('package-that-does-not-exist');\nexternal.absent();\n",
    );
    // Each command is capped below the former 60-second empty-result retry.
    for (verb, target) in [("refs", "unused.js:1:11"), ("def", "external.js:2:11")] {
        let answer = project.query(verb, target);
        assert!(
            answer["result"].is_null() || answer["result"].as_array().is_some_and(Vec::is_empty),
            "{answer}"
        );
        assert!(answer.get("warning").is_none(), "{answer}");
    }
}

#[test]
#[ignore = "requires installed typescript-language-server and TypeScript"]
fn jsx_is_parsed_as_jsx_and_resolves_embedded_javascript() {
    let project = Project::new();
    project.write("view.jsx", "const label = 'Planner';\nconst view = <section aria-label={label}>{label}</section>;\nmodule.exports = view;\n");
    assert_location(&project.query("def", "view.jsx:2:35"), "view.jsx", 1);
    let diagnostics = project.query("diag", "view.jsx");
    // checkJs may report absent JSX typings; valid JSX must never produce
    // TypeScript parser errors such as '>' expected or unterminated regexp.
    for diagnostic in diagnostics["result"].as_array().expect("diagnostics array") {
        if let Some(code) = diagnostic["code"].as_u64() {
            assert!(
                !(1000..2000).contains(&code),
                "JSX parser error: {diagnostic}"
            );
        }
    }
}

#[test]
#[ignore = "requires installed typescript-language-server and TypeScript"]
fn mjs_and_cjs_resolve_real_imported_definitions() {
    let project = Project::new();
    project.write("library.mjs", "export function esmValue() { return 7; }\n");
    project.write(
        "consumer.mjs",
        "import { esmValue } from './library.mjs';\nesmValue();\n",
    );
    project.write(
        "library.cjs",
        "function cjsValue() { return 8; }\nmodule.exports = { cjsValue };\n",
    );
    project.write(
        "consumer.cjs",
        "const { cjsValue } = require('./library.cjs');\ncjsValue();\n",
    );
    assert_location(&project.query("def", "consumer.mjs:2:2"), "library.mjs", 1);
    assert_location(&project.query("def", "consumer.cjs:2:2"), "library.cjs", 1);
}

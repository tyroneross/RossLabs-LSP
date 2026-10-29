# SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
# SPDX-License-Identifier: Apache-2.0
"""Run: python3 -m unittest discover -s plugins/lsp-routing-hint/tests"""

import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "..", "scripts", "lsp_routing_hint.py")
sys.path.insert(0, os.path.join(HERE, "..", "scripts"))

import lsp_routing_hint as hint  # noqa: E402


class Identifier(unittest.TestCase):
    def test_bare_identifiers_match(self):
        for p in ["workspace_root", "MyType", "_private", r"\bfor_path\b", r"\<run\>"]:
            self.assertIsNotNone(hint.identifier(p), p)

    def test_non_identifiers_do_not_match(self):
        for p in ["fn run", "a.b", "foo(", "ab", "9lives", "TODO", "error: .*", "", None, "x|y"]:
            self.assertIsNone(hint.identifier(p), p)

    def test_wrappers_are_stripped(self):
        self.assertEqual(hint.identifier(r"\bfor_path\b"), "for_path")


class ShellParsing(unittest.TestCase):
    def test_rg_pattern_and_path(self):
        self.assertEqual(hint.parse_search_command("rg -n workspace_root src"), ("workspace_root", "src"))

    def test_value_options_are_skipped(self):
        self.assertEqual(
            hint.parse_search_command("rg -g '*.rs' -t rust --max-count 3 for_path"), ("for_path", None)
        )

    def test_e_flag_and_pipelines(self):
        self.assertEqual(hint.parse_search_command("cd x && grep -rn -e run_named . | head"), ("run_named", "."))

    def test_non_search_commands_are_ignored(self):
        self.assertIsNone(hint.parse_search_command("cargo test workspace_root"))
        self.assertIsNone(hint.parse_search_command("echo 'unterminated"))


class Decide(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = self.tmp.name
        os.mkdir(os.path.join(root, ".git"))
        os.mkdir(os.path.join(root, "src"))
        with open(os.path.join(root, "src", "lib.rs"), "w") as f:
            f.write("pub fn for_path() {}\n")
        self.root = root
        # Pretend every server binary is installed so the test is host-independent.
        self.which = mock.patch.object(hint.shutil, "which", return_value="/bin/true")
        self.which.start()

    def tearDown(self):
        self.which.stop()
        self.tmp.cleanup()

    def test_grep_identifier_in_repo_hints(self):
        text = hint.decide({"tool_name": "Grep", "tool_input": {"pattern": "for_path"}, "cwd": self.root})
        self.assertIn("rosslabs-lsp refs --symbol for_path", text)
        self.assertIn("rust", text)

    def test_bash_rg_hints(self):
        text = hint.decide({"tool_name": "Bash", "tool_input": {"command": "rg for_path src"}, "cwd": self.root})
        self.assertIn("--path", text)

    def test_regex_pattern_is_silent(self):
        self.assertIsNone(hint.decide({"tool_name": "Grep", "tool_input": {"pattern": "fn .*path"}, "cwd": self.root}))

    def test_non_code_glob_is_silent(self):
        payload = {"tool_name": "Grep", "tool_input": {"pattern": "for_path", "glob": "*.md"}, "cwd": self.root}
        self.assertIsNone(hint.decide(payload))

    def test_outside_git_is_silent(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertIsNone(hint.decide({"tool_name": "Grep", "tool_input": {"pattern": "for_path"}, "cwd": d}))

    def test_no_ready_server_is_silent(self):
        with mock.patch.object(hint.shutil, "which", return_value=None):
            self.assertIsNone(hint.decide({"tool_name": "Grep", "tool_input": {"pattern": "for_path"}, "cwd": self.root}))

    def test_other_tools_are_silent(self):
        self.assertIsNone(hint.decide({"tool_name": "Read", "tool_input": {"file_path": "x"}, "cwd": self.root}))


class Process(unittest.TestCase):
    def run_script(self, stdin):
        return subprocess.run([sys.executable, SCRIPT], input=stdin, capture_output=True, text=True, timeout=10)

    def test_garbage_input_exits_zero_silently(self):
        for stdin in ["", "not json", "[]", json.dumps({"tool_name": "Grep"})]:
            r = self.run_script(stdin)
            self.assertEqual(r.returncode, 0, stdin)
            self.assertEqual(r.stdout, "", stdin)

    def test_output_is_hook_specific_json_without_a_decision(self):
        with tempfile.TemporaryDirectory() as d:
            os.mkdir(os.path.join(d, ".git"))
            with open(os.path.join(d, "a.py"), "w") as f:
                f.write("def for_path(): pass\n")
            env = dict(os.environ, PATH=d + os.pathsep + os.environ.get("PATH", ""))
            fake = os.path.join(d, "ty")
            with open(fake, "w") as f:
                f.write("#!/bin/sh\n")
            os.chmod(fake, 0o755)
            payload = {"hook_event_name": "PreToolUse", "tool_name": "Grep", "tool_input": {"pattern": "for_path"}, "cwd": d}
            r = subprocess.run([sys.executable, SCRIPT], input=json.dumps(payload), capture_output=True, text=True, env=env, timeout=10)
            self.assertEqual(r.returncode, 0)
            out = json.loads(r.stdout)["hookSpecificOutput"]
            self.assertEqual(out["hookEventName"], "PreToolUse")
            self.assertNotIn("permissionDecision", out)
            self.assertIn("python", out["additionalContext"])

    def test_opt_out(self):
        with mock.patch.dict(os.environ, {"ROSSLABS_LSP_HINT": "0"}):
            with mock.patch.object(sys, "stdin", io.StringIO("{}")):
                self.assertEqual(hint.main(), 0)


if __name__ == "__main__":
    unittest.main()

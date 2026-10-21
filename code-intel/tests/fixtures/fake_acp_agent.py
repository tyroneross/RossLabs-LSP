# SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
# SPDX-License-Identifier: Apache-2.0
"""Fake ACP v1 agent for `acp doctor` tests. Speaks just enough ACP.

usage: fake_acp_agent.py MODE [PIDFILE]
       fake_acp_agent.py --version

MODE:
  normal   handshake + session; on session/prompt sends an unknown request and
           a permission request, streams one chunk, ends on session/cancel
  garbage  like normal, but prints non-JSON lines to stdout first
  auth     session/new fails with -32000 Authentication required
  hang     never answers; spawns a child in its own process group, ignores
           SIGTERM, survives stdin EOF; writes both pids to PIDFILE
  exit     exits immediately without answering
"""
import json
import signal
import subprocess
import sys
import time

if sys.argv[1:] == ["--version"]:
    print("0.94.0")
    sys.exit(0)

MODE = sys.argv[1] if len(sys.argv) > 1 else "normal"


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def read():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    return json.loads(line)


if MODE == "exit":
    sys.exit(3)

if MODE == "hang":
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    child = subprocess.Popen(["sleep", "300"])
    with open(sys.argv[2], "w") as f:
        f.write(f"{child.pid} {__import__('os').getpid()}")
    sys.stderr.write("fake agent hanging\n")
    sys.stderr.flush()
    while True:
        time.sleep(1)

if MODE == "garbage":
    sys.stdout.write("Loading agent...\n\n{not json\n[1, 2]\n")
    sys.stdout.flush()

sys.stderr.write("fake agent started\n")
sys.stderr.flush()

while True:
    msg = read()
    method, mid = msg.get("method"), msg.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "protocolVersion": 1,
            "agentCapabilities": {"loadSession": False, "sessionCapabilities": {"close": {}}},
            "authMethods": [{"id": "fake-login", "name": "Fake login"}],
            "agentInfo": {"name": "fake-agent", "version": "0.94.0"},
        }})
    elif method == "session/new":
        if MODE == "auth":
            send({"jsonrpc": "2.0", "id": mid,
                  "error": {"code": -32000, "message": "Authentication required"}})
        else:
            send({"jsonrpc": "2.0", "id": mid, "result": {"sessionId": "sess-1"}})
    elif method == "session/prompt":
        sid = msg["params"]["sessionId"]
        send({"jsonrpc": "2.0", "id": 900, "method": "_fake/ask_question", "params": {}})
        reply = read()
        assert reply.get("id") == 900 and reply["error"]["code"] == -32601, reply
        send({"jsonrpc": "2.0", "id": 901, "method": "session/request_permission",
              "params": {"sessionId": sid, "toolCall": {"toolCallId": "t1"}, "options": []}})
        reply = read()
        assert reply.get("id") == 901 and reply["result"]["outcome"]["outcome"] == "cancelled", reply
        send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": {
            "sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "OK"}}}})
        cancel = read()
        assert cancel.get("method") == "session/cancel", cancel
        send({"jsonrpc": "2.0", "id": mid, "result": {"stopReason": "cancelled"}})
    elif method == "session/close":
        send({"jsonrpc": "2.0", "id": mid, "result": {}})
    elif mid is not None:
        send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "Method not found"}})

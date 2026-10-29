#!/bin/bash
# SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
# SPDX-License-Identifier: Apache-2.0
#
# Weekly freshness check for the agent-hooks compatibility records.
# Read-only: reports stale evidence and host-version drift, never writes the
# registry or any config, never uses the network. Always exits 0 so a calling
# job keeps running; read the printed summary (or the JSON lines) instead.
#
# Usage: scripts/weekly_freshness.sh [path/to/rosslabs-lsp]
# Default binary: `rosslabs-lsp` on PATH. Default registry: the one built into
# the binary, so reinstall after a registry change before trusting the result.
set -u

STALE_DAYS=30
SECTION="agent-hooks"
BIN="${1:-rosslabs-lsp}"

if ! command -v "$BIN" >/dev/null 2>&1; then
  echo "$(date '+%F %T') weekly-freshness: $BIN not found; skipped"
  exit 0
fi

stale_json=$("$BIN" compat --stale-days "$STALE_DAYS" --section "$SECTION" 2>&1) || {
  echo "$(date '+%F %T') weekly-freshness: compat failed: $stale_json"
  exit 0
}
drift_json=$("$BIN" hooks doctor --drift --json 2>&1) || {
  echo "$(date '+%F %T') weekly-freshness: hooks doctor --drift failed: $drift_json"
  exit 0
}

summary=$(STALE="$stale_json" DRIFT="$drift_json" /usr/bin/python3 -I -c '
import json, os
s = json.loads(os.environ["STALE"]); d = json.loads(os.environ["DRIFT"])
stale = [r["id"] for r in s["records"] if r.get("freshness", {}).get("stale")]
drift = [h for h in d["hosts"] if h["status"] == "drift"]
parts = ["stale %d/%d records (>%s days)" % (len(stale), len(s["records"]), s["stale_days"])]
parts.append("drift %d host(s)" % len(drift))
for h in drift:
    parts.append("%s installed %s vs %s: refresh %s" % (
        h["id"], h["installed"], ",".join(h["observed_versions"]), " ".join(h["reverify"])))
print("; ".join(parts))
' 2>&1) || summary="could not summarise: $summary"

echo "$(date '+%F %T') weekly-freshness: $summary"
exit 0

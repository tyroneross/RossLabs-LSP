<!--
SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com>
SPDX-License-Identifier: Apache-2.0
-->

# Third-party notices

## Agent Client Protocol registry

- **File:** `code-intel/data/acp-registry.json`
- **Source:** https://github.com/agentclientprotocol/registry, fetched from https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json
- **Fetched:** 2026-10-21 from the CDN URL above. The upstream repository's `main` was at commit `8468098d30d717b2c50b43335ecc1b3d39dde09b` that day.
- **License:** [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0). The full text is in this repository's `LICENSE`.
- **Copyright:** Agent Client Protocol registry contributors
- **NOTICE:** upstream ships no NOTICE file.

Modifications (Apache-2.0 section 4(b)):

- The file is limited to the agents in the `acp doctor` built-in table.
- A top-level `_snapshot` provenance object was added.
- The agent entries are otherwise unmodified.

The file is compiled into the `rosslabs-lsp` binary via `include_str!`.

The agents the registry lists are separate software under their own licenses. The registry only describes them.

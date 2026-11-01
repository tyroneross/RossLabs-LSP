<!-- SPDX-FileCopyrightText: 2026 Tyrone Ross, Jr <46267523+tyroneross@users.noreply.github.com> | SPDX-License-Identifier: Apache-2.0 -->
# JavaScript support and protocol libraries

RossLabs LSP uses TypeScript Language Server for JavaScript semantics. It is a
client of language servers, so adopting a framework for writing another server
would not repair its adapter.

## Document languages and completed answers

The executable is shared, but each opened file needs its own document language:
`.js`, `.mjs` and `.cjs` use `javascript`; `.jsx` uses `javascriptreact`; `.ts` uses
`typescript`; `.tsx` uses `typescriptreact`. This applies both to the requested
file and to definition/reference target files opened for containing-symbol labels.

TypeScript Language Server awaits tsserver's definition/reference request before
returning its result. An empty result after the client observes idle is therefore
an authoritative absence, including an external API with no available type
information. It must not incur the cold-indexer retry loop. Explicit loading
progress and retryable server errors still retain that loop. Rust's cold indexing
policy remains unchanged. These semantics follow the upstream
[definition/reference handlers](https://github.com/typescript-language-server/typescript-language-server/blob/master/src/lsp-server.ts)
and [supported document language IDs](https://github.com/typescript-language-server/typescript-language-server/blob/master/docs/configuration.md).

An empty definition for `require("obsidian").Plugin.registerView` without Obsidian
typings does not establish a broken runtime function. Runtime registration,
installed plugin files and mobile Sync receipt require separate evidence.

## Project scope

`jsconfig.json` and `tsconfig.json` define project roots, including nested plugins
inside a larger vault. For cross-file references to unopened callers, declare the
JavaScript project with a `jsconfig.json` that includes those files. A standalone
inferred project can resolve imports it loads, but is not a complete search of
every unopened consumer in a repository. RossLabs does not create configuration
files or change project membership automatically.

## Library review, 2026-11-01

| Library | Role | Disposition |
|---|---|---|
| [typescript-language-server](https://github.com/typescript-language-server/typescript-language-server) | JavaScript/TypeScript definitions, references, symbols and diagnostics through tsserver | Retained existing backend; corrected adapter document IDs and empty-answer handling. |
| [lsp-server](https://docs.rs/lsp-server/0.10.0/lsp_server/enum.Message.html) | Standard synchronous LSP messages and Content-Length framing | Added version 0.10; use `Message::read` and `Message::write` in the existing client. Client process ownership, timeouts and query dispatch remain RossLabs responsibilities. |
| [vscode-languageserver-node](https://github.com/microsoft/vscode-languageserver-node) | Node protocol types, client/server and JSON-RPC implementation | Relevant upstream ecosystem for the existing TS backend; not added to this Rust client. |
| [tower-lsp-server](https://github.com/tower-lsp-community/tower-lsp-server) | Async Rust server implementation and service layer | Evaluated; no replacement of the existing client with a server framework. |
| [gen-lsp-types](https://github.com/ribru17/gen-lsp-types) | Rust protocol types generated from the official metamodel | Candidate if broader typed protocol coverage is added; not needed for this message-framing integration. |

The installed TS backend used for live validation is recorded with test evidence;
these links describe upstream behavior, not the phone's current installation.

## Verification

Run the ordinary suite with `cargo test --locked --manifest-path code-intel/Cargo.toml`.
Real-server tests require the optional TypeScript Language Server and TypeScript:

```sh
cargo test --locked --manifest-path code-intel/Cargo.toml --test javascript -- --ignored --test-threads=1
```

Those tests exercise CommonJS and ES modules, JSX, cross-file definitions and
references, and authoritative empty answers. Their temporary project paths
contain spaces. Explicit child deadlines terminate owned test groups normally
if the old minute-long retry recurs.

Validation on 2026-11-01 used Rust/Cargo 1.99.1 (the current stable channel,
confirmed with `rustup check`), TypeScript Language Server 5.3.0 and TypeScript
5.9.3. A separate `cargo +1.85.0 check --locked` passed for the declared minimum
compiler version. The minimum compatibility check does not select the compiler
used for the installed build.

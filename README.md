# NOVA

**Normalized Object and Value Architecture** — one precise semantic model with several interchangeable representations, each optimized for a different consumer:

- **Human text** for people editing configuration.
- **Compact text** with tables for LLM context.
- **Binary frames** for services and storage.
- **Queries and conditional patches** for agents that read and change *parts* of a document.
- **Templates** for composition, rendered to plain values before anything else sees them.

📄 **[Read the specification →](SPEC.md)**

This repository contains the draft specification (v0.1) and a reference implementation in Rust.

## A taste

The human text representation:

```
@schema infrastructure@1.0.0

environment: production
deployed_at: 2026-11-17T16:00:00Z   # a string, until a schema resolves it
ratio: 22/7                         # an exact rational

services:
  - id: auth
    port: 8080
    replicas: 3
  - id: api
    port: 3000
    replicas: 5
```

The `services` list in the compact representation:

```
services[2]{id,port,replicas}:
  auth,8080,3
  api,3000,5
```

An agent reads only what it needs…

```
$ nova get infrastructure.nova 'services[id=api].{replicas,port}'
@revision sha256:91cf16db2f4a403ed7a586e82fc2618093f7c1d87fc0183e54ea1c9b936bd65b
@scope services[id=api]
replicas: 5
port: 3000
```

…and writes back a conditional, atomically applied patch:

```
@patch sha256:91cf16db
@scope services[id=api]
replicas: 5 -> 8
```

A template is typed, total, and operates on values rather than text, so there is no Helm-style `nindent`:

```
@template
@param env: string
@param services: list<string>
@param metrics: bool = false
@let defaults = {enabled: true, replicas: 2}

services:
  @for name in $services:
    - ...$defaults
      id: $name
      image: "ghcr.io/example/${name}:v2"
  @if $metrics and $env == production:
    - id: metrics
```

## Serde

```rust
#[derive(Serialize, Deserialize)]
enum Status { Active, Failed(String) }

#[derive(Serialize, Deserialize)]
struct Service { id: String, port: u16, deployed_at: nova_value::Timestamp, status: Status }

let text = nova_serde::to_string(&svc)?;      // status: !Failed(oom), deployed_at: @timestamp("…")
let svc: Service = nova_serde::from_str(&text)?;
let bytes = nova_serde::to_vec(&svc)?;         // binary frame
```

## Workspace

| Crate | Purpose |
| --- | --- |
| [`nova-value`](crates/nova-value) | Value model (incl. decimals, rationals, timestamps, durations), canonical encoding, revisions |
| [`nova-binary`](crates/nova-binary) | Framed binary representation, CRC-32, recovering stream reader |
| [`nova-text`](crates/nova-text) | Text parser; human, compact and canonical writers; paths; source spans; template syntax; token heuristics; [miette](https://docs.rs/miette) diagnostics (feature `miette`) |
| [`nova-schema`](crates/nova-schema) | Schema language, schema-directed resolution, `@key`, compatibility checks, fingerprints |
| [`nova-patch`](crates/nova-patch) | `@get` queries and projections; the patch language; atomic apply; diff |
| [`nova-template`](crates/nova-template) | Template rendering with typed parameters, output budget and sandboxed imports |
| [`nova-json`](crates/nova-json) | JSON and JSONL mapping with explicit loss reporting |
| [`nova-formats`](crates/nova-formats) | YAML, TOML and CBOR adapters with explicit loss reporting |
| [`nova-proto`](crates/nova-proto) | `.proto` generation and Protobuf encoding/decoding driven by NOVA schemas (`@id` = field number) |
| [`nova-facet`](crates/nova-facet) | [facet](https://github.com/facet-rs/facet) reflection binding, same mapping and API as `nova-serde` |
| [`nova-serde`](crates/nova-serde) | Serde integration: `to_string`/`from_str`, `to_vec`/`from_slice`, `to_value`/`from_value` |
| [`nova-lint`](crates/nova-lint) | Conventions and lints with fixes (`nova lint`, also shown by the language server) |
| [`nova-cli`](crates/nova-cli) | The `nova` command |
| [`nova-lsp`](crates/nova-lsp) | Language server: diagnostics with workspace schemas, outline, hover, completion, formatting |

## CLI

```bash
cargo install --path crates/nova-cli
```

```bash
cd examples
nova fmt infrastructure.nova --to compact
nova validate infrastructure.nova --schema infrastructure.novs --resolve
nova get infrastructure.nova 'services[*].{id,replicas}'
nova get infrastructure.nova 'services[?replicas>3].id' -r     # jq-style filter, raw output
curl -s https://api.example.com/items | nova get - 'items[?price<10].{name,price}' --to json
nova patch infrastructure.nova scale-api.patch --schema infrastructure.novs
nova diff before.nova after.nova
nova render service.tpl.nova --set env=production --set 'services=[auth, api]' --set metrics=true --schema infrastructure.novs
nova convert infrastructure.nova --to binary -o infrastructure.novb
nova convert config.yaml --to nova                 # also toml, cbor; --to yaml/toml/cbor
nova convert infrastructure.nova --to protobuf --schema infrastructure.novs -o infra.pb
nova schema proto infrastructure.novs              # the matching .proto
nova convert infrastructure.nova --to positional --schema infrastructure.novs   # field names omitted
nova hash infrastructure.novb          # same revision as the text form
nova stats infrastructure.nova         # bytes and estimated tokens per representation
nova schema check old.novs new.novs    # exits 1 on breaking changes
```

## Conventions and `nova lint`

JSON never settled on naming conventions; NOVA has them: `snake_case` keys and variants, and `@id` on all fields or on none. `nova lint` checks documents and schemas, and `--fix` applies the safe fixes (renaming keys, typing timestamps) without disturbing comments. Data that mirrors an external JSON, YAML or TOML contract is marked `@compat json` (added automatically by `nova convert`). For such data, names are left alone and anything the format cannot represent is flagged. See [spec Appendix E](SPEC.md#appendix-e--conventions-and-lints).

```bash
nova lint config.nova examples/*.novs
nova lint config.nova --fix
nova lint config.nova --format json       # one finding per line, for tools and agents
```

## Coding agents

When `nova` runs under a coding agent (detected with [`detect-coding-agent`](https://crates.io/crates/detect-coding-agent): Claude Code, Codex, Copilot, Cursor, Gemini CLI, …), errors are printed as plain, narratable text instead of box drawings, and `--help` points to `nova guide`, a one-screen workflow for reading projections and writing conditional patches. Override with `--agent on|off` or `NOVA_AGENT=on|off`.

## Editor support

`nova-lsp` speaks the Language Server Protocol over stdio:

```bash
cargo install --path crates/nova-lsp
```

It reports syntax errors in documents, templates, patches and `.novs` schemas. A document that declares `@schema name@version` is validated against the newest `.novs` file in the workspace that can read that version. Hover shows the path, value and declared type. Completion offers missing record fields and enum variants. Formatting is only offered for documents without comments.

Helix (`languages.toml`):

```toml
[language-server.nova-lsp]
command = "nova-lsp"

[[language]]
name = "nova"
scope = "source.nova"
file-types = ["nova", "novs", "patch"]
comment-token = "#"
indent = { tab-width = 2, unit = "  " }
language-servers = ["nova-lsp"]
```

Neovim (0.11+):

```lua
vim.filetype.add({ extension = { nova = "nova", novs = "nova" } })
vim.lsp.config("nova_lsp", { cmd = { "nova-lsp" }, filetypes = { "nova" }, root_markers = { ".git" } })
vim.lsp.enable("nova_lsp")
```

A VS Code extension, with a TextMate grammar for syntax highlighting, is not written yet.

## Status

This is a draft. Formatting and patching are **document-preserving**: `nova fmt` keeps comments, blank lines and spellings, and `nova patch -w` edits only the lines it changes. See [SPEC.md §14](SPEC.md#14-reference-implementation) and [Appendix C](SPEC.md#appendix-c--open-questions-and-future-work).

## License

Licensed under either of, at your option:

- European Union Public Licence v1.2 ([LICENSE-EUPL](LICENSE-EUPL) or <https://interoperable-europe.ec.europa.eu/collection/eupl/eupl-text-eupl-12>)
- Mozilla Public License 2.0 ([LICENSE-MPL](LICENSE-MPL) or <https://mozilla.org/MPL/2.0/>)

SPDX-License-Identifier: `EUPL-1.2 OR MPL-2.0`

The example files in [`examples/`](examples/) are dedicated to the public domain under [CC0-1.0](examples/LICENSE) (SPDX-License-Identifier: `CC0-1.0`): copy them into your own projects without conditions.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work shall be dual-licensed as above, without any additional terms or conditions.

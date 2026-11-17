# NOVA

**Notation for Objects, Values & Agents** — one precise semantic model with several interchangeable representations, each optimized for a different consumer:

- **Human text** for people editing configuration.
- **Compact text** with tables for LLM context.
- **Binary frames** for services and storage.
- **Queries and conditional patches** for agents that read and change *parts* of a document.
- **Templates** for composition, rendered to plain values before anything else sees them.

📄 **[Read the specification →](https://github.com/novafile/spec/blob/main/SPEC.md)**

This repository is the Rust reference implementation. The specification, design proposals and the cross-implementation conformance suite live in [novafile/spec](https://github.com/novafile/spec); editor support in [novafile/vscode-novafile](https://github.com/novafile/vscode-novafile) and [novafile/tree-sitter-nova](https://github.com/novafile/tree-sitter-nova). The format is called NOVA (files `.nova`, schemas `.novs`); the Rust packages are published as `novafile-*`, and the command-line tool is `nvf`.

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
$ nvf get infrastructure.nova 'services[id=api].{replicas,port}'
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
struct Service { id: String, port: u16, deployed_at: novafile_value::Timestamp, status: Status }

let text = novafile_serde::to_string(&svc)?;      // status: !Failed(oom), deployed_at: @timestamp("…")
let svc: Service = novafile_serde::from_str(&text)?;
let bytes = novafile_serde::to_vec(&svc)?;         // binary frame
```

Schemas can be derived from the same types:

```rust
#[derive(Serialize, Deserialize, NovaSchema)]          // novafile-schema = { features = ["derive"] }
#[nova(name = "infrastructure", version = "1.0.0")]
struct Infrastructure {
    #[nova(id = 1)] environment: String,
    #[nova(id = 5)] services: Vec<Service>,
}

#[derive(Serialize, Deserialize, NovaSchema)]
struct Service {
    #[nova(key, id = 1)] id: String,
    #[nova(id = 2, range = "1..65535")] port: u16,
    #[nova(id = 3, default = "true")] enabled: bool,
}

let schema = Infrastructure::novafile_schema();             // == the hand-written .novs
```

## Workspace

| Crate | Purpose |
| --- | --- |
| [`novafile`](crates/novafile) | Umbrella crate re-exporting the pieces below (features: `serde`, `derive`, `facet`, `formats`, `proto`, `template`, `lint`, `schemars`, `full`) |
| [`novafile-value`](crates/novafile-value) | Value model (incl. decimals, rationals, timestamps, durations), canonical encoding, revisions |
| [`novafile-binary`](crates/novafile-binary) | Framed binary representation, CRC-32, recovering stream reader |
| [`novafile-text`](crates/novafile-text) | Text parser; human, compact and canonical writers; paths; source spans; template syntax; token heuristics; [miette](https://docs.rs/miette) diagnostics (feature `miette`) |
| [`novafile-schema`](crates/novafile-schema) | Schema language, schema-directed resolution, `@key`, compatibility checks, fingerprints |
| [`novafile-patch`](crates/novafile-patch) | `@get` queries and projections; the patch language; atomic apply; diff |
| [`novafile-template`](crates/novafile-template) | Template rendering with typed parameters, output budget and sandboxed imports |
| [`novafile-json`](crates/novafile-json) | JSON and JSONL mapping with explicit loss reporting |
| [`novafile-formats`](crates/novafile-formats) | YAML, TOML and CBOR adapters with explicit loss reporting |
| [`novafile-proto`](crates/novafile-proto) | `.proto` generation and Protobuf encoding/decoding driven by NOVA schemas (`@id` = field number) |
| [`novafile-facet`](crates/novafile-facet) | [facet](https://github.com/facet-rs/facet) reflection binding, same mapping and API as `novafile-serde` |
| [`novafile-derive`](crates/novafile-derive) | `#[derive(NovaSchema)]`: NOVA schemas from Rust types, honouring `#[nova]` and `#[serde]` attributes |
| [`novafile-schemars`](crates/novafile-schemars) | NOVA schemas from Rust types (`#[derive(JsonSchema)]`), and NOVA schema ↔ JSON Schema |
| [`novafile-serde`](crates/novafile-serde) | Serde integration: `to_string`/`from_str`, `to_vec`/`from_slice`, `to_value`/`from_value` |
| [`novafile-lint`](crates/novafile-lint) | Conventions and lints with fixes (`nvf lint`, also shown by the language server) |
| [`novafile-conformance`](crates/novafile-conformance) | Generates and runs the [conformance suite](https://github.com/novafile/spec/tree/main/conformance) (unpublished) |
| [`novafile-cli`](crates/novafile-cli) | The `nvf` command |
| [`novafile-lsp`](crates/novafile-lsp) | Language server: diagnostics with workspace schemas, outline, hover, completion, formatting |

## CLI

```bash
cargo install --path crates/novafile-cli
```

```bash
cd examples
nvf fmt infrastructure.nova --to compact
nvf validate infrastructure.nova --schema infrastructure.novs --resolve
nvf get infrastructure.nova 'services[*].{id,replicas}'
nvf get infrastructure.nova 'services[?replicas>3].id' -r     # jq-style filter, raw output
curl -s https://api.example.com/items | nvf get - 'items[?price<10].{name,price}' --to json
nvf patch infrastructure.nova scale-api.patch --schema infrastructure.novs
nvf diff before.nova after.nova
nvf render service.tpl.nova --set env=production --set 'services=[auth, api]' --set metrics=true --schema infrastructure.novs
nvf convert infrastructure.nova --to binary -o infrastructure.novb
nvf convert config.yaml --to nova                 # also toml, cbor; --to yaml/toml/cbor
nvf convert infrastructure.nova --to protobuf --schema infrastructure.novs -o infra.pb
nvf schema proto infrastructure.novs              # the matching .proto
nvf schema json-schema infrastructure.novs        # JSON Schema 2020-12 (and back: schema from-json-schema)
nvf get infrastructure.novs 'defs[name=Service].record.fields[*].name'   # schemas are data too
nvf schema to-nova infrastructure.novs           # the schema's descriptor (validated by `nvf schema meta`)
nvf convert infrastructure.nova --to positional --schema infrastructure.novs   # field names omitted
nvf hash infrastructure.novb          # same revision as the text form
nvf stats infrastructure.nova         # bytes and estimated tokens per representation
nvf schema check old.novs new.novs    # exits 1 on breaking changes
```

## Conventions and `nvf lint`

JSON never settled on naming conventions; NOVA has them: `snake_case` keys and variants, and `@id` on all fields or on none. `nvf lint` checks documents and schemas, and `--fix` applies the safe fixes (renaming keys, typing timestamps) without disturbing comments. Data that mirrors an external JSON, YAML or TOML contract is marked `@compat json` (added automatically by `nvf convert`). For such data, names are left alone and anything the format cannot represent is flagged. See [spec Appendix E](https://github.com/novafile/spec/blob/main/SPEC.md#appendix-e--conventions-and-lints).

```bash
nvf lint config.nova examples/*.novs
nvf lint config.nova --fix
nvf lint config.nova --format json       # one finding per line, for tools and agents
```

## Coding agents

When `nvf` runs under a coding agent (detected with [`detect-coding-agent`](https://crates.io/crates/detect-coding-agent): Claude Code, Codex, Copilot, Cursor, Gemini CLI, …), errors are printed as plain, narratable text instead of box drawings, and `--help` points to `nvf guide`, a one-screen workflow for reading projections and writing conditional patches. Override with `--agent on|off` or `NOVA_AGENT=on|off`.

## Editor support

`nvf-lsp` speaks the Language Server Protocol over stdio:

```bash
cargo install --path crates/novafile-lsp
```

It reports syntax errors in documents, templates, patches and `.novs` schemas. A document that declares `@schema name@version` is validated against the newest `.novs` file in the workspace that can read that version. Hover shows the path, value and declared type. Completion offers missing record fields and enum variants. Formatting is only offered for documents without comments.

Helix (`languages.toml`):

```toml
[language-server.nvf-lsp]
command = "nvf-lsp"

[[language]]
name = "nova"
scope = "source.nova"
file-types = ["nova", "novs", "patch"]
comment-token = "#"
indent = { tab-width = 2, unit = "  " }
language-servers = ["nvf-lsp"]
```

Neovim (0.11+):

```lua
vim.filetype.add({ extension = { nova = "nova", novs = "nova" } })
vim.lsp.config("novafile_lsp", { cmd = { "nvf-lsp" }, filetypes = { "nova" }, root_markers = { ".git" } })
vim.lsp.enable("novafile_lsp")
```

A VS Code extension, with a TextMate grammar for syntax highlighting, is not written yet.

## Status

This is a draft. Formatting and patching are **document-preserving**: `nvf fmt` keeps comments, blank lines and spellings, and `nvf patch -w` edits only the lines it changes. See [SPEC.md §14](https://github.com/novafile/spec/blob/main/SPEC.md#14-reference-implementation) and [Appendix C](https://github.com/novafile/spec/blob/main/SPEC.md#appendix-c--open-questions-and-future-work).

## License

Licensed under either of, at your option:

- European Union Public Licence v1.2 ([LICENSE-EUPL](LICENSE-EUPL) or <https://interoperable-europe.ec.europa.eu/collection/eupl/eupl-text-eupl-12>)
- Mozilla Public License 2.0 ([LICENSE-MPL](LICENSE-MPL) or <https://mozilla.org/MPL/2.0/>)

SPDX-License-Identifier: `EUPL-1.2 OR MPL-2.0`

Exceptions, which keep the license of the format they belong to:

| Files | License |
| --- | --- |
| [`crates/novafile-schema/meta/nova-schema.novs`](crates/novafile-schema/meta/nova-schema.novs), a verbatim copy of the [specification's meta-schema](https://github.com/novafile/spec/blob/main/meta/nova-schema.novs) | Apache-2.0 ([LICENSE-APACHE](crates/novafile-schema/LICENSE-APACHE)) |
| [`examples/`](examples/) | CC0-1.0 ([LICENSE](examples/LICENSE)) |

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work shall be dual-licensed as above, without any additional terms or conditions.

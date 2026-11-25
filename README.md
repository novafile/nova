# NOVA

**Notation for Objects, Values & Agents** — one precise semantic model with several interchangeable representations, each optimized for a different consumer:

- **Human text** for people editing configuration.
- **Compact text** with tables for LLM context.
- **Binary frames** for services and storage.
- **Queries and conditional patches** for agents that read and change *parts* of a document.
- **Templates** for composition, rendered to plain values before anything else sees them.

🌐 **[novafile.dev](https://novafile.dev/)** · 📄 **[Read the specification →](https://github.com/novafile/spec/blob/main/SPEC.md)**

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
@template 0.2
@param env: string
@param services: list<string>
@param replicas: u16 = 2
@param metrics: bool = false
@let defaults = {enabled: true, replicas: $replicas}

@def dns_label(s: string): string = $trim_suffix($slice($lower($s), 0, 63), "-")

services:
  @for name in $services:
    - ...$defaults
      id: $dns_label($name)
      image: "ghcr.io/example/${lower($name)}:v2"
  @if $metrics and $replicas > 1:
    - id: metrics
      ports: $([for p in [9090, 9091]: {port: $p, name: "m-${p}"}])
```

Template language 0.2 adds expressions (comparisons, `+`, `-`, `*`, list and record comprehensions), user functions with `@def`, and 40 built-in functions for strings, collections, numbers, conversions and digests (`revision` turns Helm's `checksum/config` into a hash of the configuration value). A function can only call functions defined above it, loops only iterate over existing values, and every render has an output budget and a step budget, so every render terminates. Records iterate by key and sets and maps in canonical order, so equal arguments render to equal values.

## Serde

```rust
use novafile::value::Timestamp;

#[derive(Serialize, Deserialize)]
enum Status { Active, Failed(String) }

#[derive(Serialize, Deserialize)]
struct Service { id: String, port: u16, deployed_at: Timestamp, status: Status }

let text = novafile::serde::to_string(&svc)?;      // status: !Failed(oom), deployed_at: @timestamp("…")
let svc: Service = novafile::serde::from_str(&text)?;
let bytes = novafile::serde::to_vec(&svc)?;         // binary frame
```

Schemas can be derived from the same types:

```rust
use novafile::NovaSchema;                               // cargo add novafile --features derive

#[derive(Serialize, Deserialize, NovaSchema)]
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

Put `#[nova(name = …, version = …)]` on the root type. It gives the schema its identity (`@schema infrastructure@1.0.0`), which documents name in their `@schema` line and which version checks use. Without it the schema is anonymous: it still validates, but cannot check a document's `@schema` reference. Other types need no attribute.

Fields take `#[nova(secret)]` / `#[nova(personal)]` to classify them for redaction, and `#[nova(type = "string @len(1..)")]` to give a schema type for foreign types without a `NovaSchema` impl (`url::Url`, `secrecy::SecretString`, …). `PathBuf` and the `std::net` address types map to `string`.

## Workspace

| Crate | Purpose |
| --- | --- |
| [`novafile`](crates/novafile) | Umbrella crate re-exporting the pieces below (features: `serde`, `derive`, `facet`, `formats`, `proto`, `template`, `lint`, `schemars`, `full`) |
| [`novafile-value`](crates/novafile-value) | Value model (incl. decimals, rationals, timestamps, durations), canonical encoding, revisions |
| [`novafile-binary`](crates/novafile-binary) | Framed binary representation, CRC-32, recovering stream reader, record streams with header and end frames |
| [`novafile-text`](crates/novafile-text) | Text parser; human, compact and canonical writers; paths; source spans; template syntax; NOVA Lines (`.novl`) record streams; token heuristics; [miette](https://docs.rs/miette) diagnostics (feature `miette`) |
| [`novafile-schema`](crates/novafile-schema) | Schema language, schema-directed resolution, `@key`, compatibility checks, fingerprints |
| [`novafile-patch`](crates/novafile-patch) | `@get` queries and projections; the patch language; atomic apply; diff |
| [`novafile-template`](crates/novafile-template) | Template rendering with typed parameters, expressions, `@def` functions, a standard library, output and step budgets and sandboxed imports |
| [`novafile-json`](crates/novafile-json) | JSON and JSONL mapping with explicit loss reporting |
| [`novafile-formats`](crates/novafile-formats) | YAML, TOML and CBOR adapters with explicit loss reporting |
| [`novafile-proto`](crates/novafile-proto) | `.proto` generation and Protobuf encoding/decoding driven by NOVA schemas (`@id` = field number) |
| [`novafile-facet`](crates/novafile-facet) | [facet](https://github.com/facet-rs/facet) reflection binding, same mapping and API as `novafile-serde` |
| [`novafile-derive`](crates/novafile-derive) | `#[derive(NovaSchema)]`: NOVA schemas from Rust types, honouring `#[nova]` and `#[serde]` attributes |
| [`novafile-schemars`](crates/novafile-schemars) | NOVA schemas from Rust types (`#[derive(JsonSchema)]`), and NOVA schema ↔ JSON Schema |
| [`novafile-serde`](crates/novafile-serde) | Serde integration: `to_string`/`from_str`, `to_vec`/`from_slice`, `to_value`/`from_value` |
| [`novafile-lint`](crates/novafile-lint) | Conventions and lints with fixes (`nvf lint`, also shown by the language server) |
| [`novafile-conformance`](crates/novafile-conformance) | Generates and runs the [conformance suite](https://github.com/novafile/spec/tree/main/conformance) (unpublished) |
| [`novafile-umbrella-tests`](crates/novafile-umbrella-tests) | Checks that the umbrella crate works on its own, e.g. `#[derive(NovaSchema)]` through `novafile` (unpublished) |
| [`novafile-cli`](crates/novafile-cli) | The `nvf` command |
| [`novafile-lsp`](crates/novafile-lsp) | Language server: diagnostics with workspace schemas, outline, hover, completion, formatting |

## CLI

```bash
cargo install novafile-cli                 # from crates.io
cargo install --path crates/novafile-cli   # from a checkout
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
nvf convert events.jsonl --to novl -o events.novl   # NOVA Lines: one record per line, a table when columns fit
nvf convert events.novl --to binary-stream -o events.novb   # header frame, record batches, end frame
nvf hash infrastructure.novb          # same revision as the text form
nvf stats infrastructure.nova         # bytes and estimated tokens per representation
nvf schema check old.novs new.novs    # exits 1 on breaking changes
```

`nvf fmt` formats NOVA text (and `.novs` schemas) only. Other input, detected by extension or magic bytes (`.json`, `.jsonl`, `.novl`, `.yaml`/`.yml`, `.toml`, `.cbor`, `.pb`, binary frames), is refused: convert it with `nvf convert config.yaml --to nova -o config.nova`.

`nvf patch` prints the result, or with `-w` writes it back, in the input's format. NOVA text keeps its comments and layout. JSON, YAML, TOML, CBOR and binary documents are rendered in the same format, so `nvf patch x.json p.patch` prints JSON (YAML and TOML comments are not kept). `.novs` schemas are rewritten in canonical form. JSON Lines, NOVA Lines, binary record streams and Protobuf are not written back: their result is printed as NOVA text, and `-w` refuses them. For another output format, pipe the result through `nvf convert`.

### Finding schemas

Schema names are either local (`infrastructure`) or qualified by a domain you control (`acme.example/billing/invoice`), the way Go module paths are. A `nova-catalog.nova` file tells the tools where the `.novs` files live, so `nvf validate` can look up a document's `@schema` reference without `--schema`:

```
@schema novafile.dev/meta/catalog@0.1.0
schemas:
  - path: schemas/
pins:
  - schema: acme.example/billing/invoice@2.1.0
    revision: "sha256:…"
```

```bash
nvf validate invoice.nova                          # @schema looked up in nova-catalog.nova
nvf schema resolve acme.example/billing/invoice@2.1.0   # where it resolves from, and its revision
nvf schema fetch acme.example/billing/invoice@2.1.0 --pin   # HTTPS, into the user cache, pinned
```

Lookup is offline: built-in schemas, then catalogs (`--catalog`, `NOVA_CATALOG`, the nearest `nova-catalog.nova`, the user catalog), then the cache. Conflicting files and pin mismatches are errors. Fetching qualified names over HTTPS only happens when you ask for it (`--fetch`, `NOVA_FETCH=1`, or `fetch: true` in your user catalog); a project catalog can say where to fetch from, but what it redirects is never put in the shared cache. See [spec §6.9](https://github.com/novafile/spec/blob/main/SPEC.md).

`nvf patch -w` and `nvf lint --fix` lock the file while they read, change and rewrite it, so parallel patches are not lost. The new contents are written to a temporary file and renamed over the old one. On Unix the lock is on the file itself. On Windows it is a `<file>.lock` file next to it, which stays on disk: ignore it in version control with patterns such as `*.nova.lock` (not `*.lock`, which also matches `Cargo.lock`).

### Imports and the standard library

A schema can import the definitions of another schema version and refer to them as `alias.Name` ([proposal 0008](https://github.com/novafile/spec/blob/main/proposals/0008-schema-imports-and-standard-library.md)). Imports name an exact version; the tools fill in its fingerprint, so a schema's fingerprint pins what it imports. The standard library `novafile.dev/std` is bundled and never fetched. It holds fixed, regular string types: `Uuid`, `Uri`, `UriReference`, `Urn`, `Email`, `E164`, `Name`, `NonBlank`, `PortablePath`, `SemVer`, `Sha256Hex`, `CountryCode`, `CurrencyCode`, and the int type `Port`.

```
@schema acme.example/crm@1.0.0
@import novafile.dev/std@0.1.0 as std

record Contact:
  id: std.Uuid @key
  name: std.Name @len(..200)       # refines the std type: both constraints hold
  files?: list<std.PortablePath>
```

Constraints apply along the whole chain of aliases, so a refinement adds to the type it refines. `nvf schema check` compares the definitions that imports bring in, so bumping an import is breaking only if a type the schema uses changed.

### Classified values

A schema can mark types `@secret` (credentials, keys) or `@personal` (data about people), also on whole records ([proposal 0009](https://github.com/novafile/spec/blob/main/proposals/0009-classified-values-and-redaction.md)). Every command that prints values then redacts them by default; the stored document is unchanged:

```
$ nvf get ops.nova 'services[id=api]'
@revision k:3f9c0e5d…
@scope services[id=api]
id: api
db_url: @redacted
owner: @redacted("p:7c1e9302a4b1")
```

- `get`, `convert`, `diff`, `fmt --to …`, `hash`, `stats`, `validate --resolve`, printed `patch` results, `render` to stdout, and the language server's hover and outline all redact. `--reveal` shows everything, `--reveal=personal` personal data only, and `--reveal=PATH,…` the values at those paths.
- Personal values get a keyed pseudonym (`p:…`), so equal values stay recognisable. Revisions of documents with hidden values print as opaque `k:` tokens, which `nvf patch` accepts as a base. The key comes from `NOVA_REDACT_KEY` (64 hex digits) or from `redact.key` next to the user schema cache, made on first use and readable only by you. Without a key, `nvf hash` refuses instead of printing a token that changes on every run.
- Filters that compare a hidden value, patches that write a placeholder back, and templates that branch on a classified value are errors. Validation and conflict messages never quote classified values.
- `nvf render -o FILE` writes the values in clear, since rendering for deployment needs them; output to stdout is redacted.
- If a document's `@schema` cannot be found or the document does not validate, nothing can be classified. In agent mode the commands then refuse, unless you pass `--reveal` or `--no-schema`; otherwise they warn and show the values.

## Conventions and `nvf lint`

JSON never settled on naming conventions; NOVA has them: `snake_case` keys and variants, and `@id` on all fields or on none. `nvf lint` checks documents and schemas, and `--fix` applies the safe fixes (renaming keys, typing timestamps) without disturbing comments. Data that mirrors an external JSON, YAML or TOML contract is marked `@compat json` (added automatically by `nvf convert`). For such data, names are left alone and anything the format cannot represent is flagged. Schemas get rules for classified values (`classified-default`, an error: a default would put the value in the schema; `classified-hint` for fields such as `password` without a class) and for the standard library (`std-restated` for a local alias that repeats a std type; `std-hint`, off unless `@x-lint enable=std-hint`). Templates get their own rules: a `@def` that shadows a built-in, a bare word passed in an interpolation where a `$` reference was probably meant, patterns that are not literals, and functions that are never called. See [spec Appendix E](https://github.com/novafile/spec/blob/main/SPEC.md#appendix-e--conventions-and-lints).

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
cargo install novafile-lsp                 # from crates.io
cargo install --path crates/novafile-lsp   # from a checkout
```

It reports syntax errors in documents, templates, patches and `.novs` schemas. A document that declares `@schema name@version` is validated against a `.novs` file that can read that version. With a `nova-catalog.nova` above the document, that file comes from the catalogs (conflicts and pin mismatches are reported); otherwise it is the one closest to the document in the directory tree (its own folder first), and the newest among equally close ones. Files opened without a workspace folder use the schemas in their own folder. Hover shows the path, value and declared type. Completion offers missing record fields and enum variants. Formatting is only offered for documents without comments.

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

VS Code: [novafile/vscode-novafile](https://github.com/novafile/vscode-novafile) adds syntax highlighting for documents, templates, patches and schemas, and starts `nvf-lsp` (configurable via `novafile.server.path`).

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
| [`crates/novafile-schema/meta/schema.novs`](crates/novafile-schema/meta/schema.novs) and [`catalog.novs`](crates/novafile-schema/meta/catalog.novs), verbatim copies of the [specification's meta-schemas](https://github.com/novafile/spec/tree/main/meta) | Apache-2.0 ([LICENSE-APACHE](crates/novafile-schema/LICENSE-APACHE)) |
| [`examples/`](examples/) | CC0-1.0 ([LICENSE](examples/LICENSE)) |

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work shall be dual-licensed as above, without any additional terms or conditions.

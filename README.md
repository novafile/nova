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
@schema infrastructure/v1

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

## Workspace

| Crate | Purpose |
| --- | --- |
| [`nova-value`](crates/nova-value) | Value model (incl. decimals, rationals, timestamps, durations), canonical encoding, revisions |
| [`nova-binary`](crates/nova-binary) | Framed binary representation, CRC-32, recovering stream reader |
| [`nova-text`](crates/nova-text) | Text parser; human, compact and canonical writers; paths; template syntax; token heuristics |
| [`nova-schema`](crates/nova-schema) | Schema language, schema-directed resolution, `@key`, compatibility checks, fingerprints |
| [`nova-patch`](crates/nova-patch) | `@get` queries and projections; the patch language; atomic apply; diff |
| [`nova-template`](crates/nova-template) | Template rendering with typed parameters, output budget and sandboxed imports |
| [`nova-json`](crates/nova-json) | JSON and JSONL mapping with explicit loss reporting |
| [`nova-cli`](crates/nova-cli) | The `nova` command |

## CLI

```bash
cargo install --path crates/nova-cli
```

```bash
cd examples
nova fmt infrastructure.nova --to compact
nova validate infrastructure.nova --schema infrastructure.novs --resolve
nova get infrastructure.nova 'services[*].{id,replicas}'
nova patch infrastructure.nova scale-api.patch --schema infrastructure.novs
nova diff before.nova after.nova
nova render service.tpl.nova --set env=production --set 'services=[auth, api]' --set metrics=true --schema infrastructure.novs
nova convert infrastructure.nova --to binary -o infrastructure.novb
nova hash infrastructure.novb          # same revision as the text form
nova stats infrastructure.nova         # bytes and estimated tokens per representation
nova schema check old.novs new.novs    # exits 1 on breaking changes
```

## Status

This is a draft. The reference implementation is **value-preserving**: comments are not yet retained by `nova fmt`. There is no Serde integration and there are no YAML, TOML or CBOR adapters yet. See [SPEC.md §14](SPEC.md#14-reference-implementation) and [Appendix C](SPEC.md#appendix-c--open-questions-and-future-work).

## License

MIT OR Apache-2.0

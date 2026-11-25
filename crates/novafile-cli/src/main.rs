//! `nvf`, the NOVA command-line tool: format, convert, query, patch, validate, render and lint
//! NOVA documents and schemas.

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use novafile_binary::{Frame, FrameOptions, MAGIC, decode_frame, encode_frame};
use novafile_schema::{ClassMark, RedactKey, Reveal, Schema, check_versions};
use novafile_text::diagnostic::SourceDiagnostic;
use novafile_text::lines::{Header, Positional, Segment, Status, WriteMode};
use novafile_text::tokens::{HeuristicCounter, TokenCounter};
use novafile_text::{Profile, document_to_text, parse_document};
use novafile_value::{Record, Value, revision};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Command-line arguments of `nvf`.
#[derive(Parser)]
#[command(
    name = "nvf",
    version,
    about = "NOVA, the Notation for Objects, Values & Agents: one semantic model, several representations"
)]
struct Cli {
    /// Agent mode: plain, narratable errors and agent hints. `auto` detects coding
    /// agents (Claude Code, Codex, Copilot, Cursor, …); also settable via `NOVA_AGENT`.
    #[arg(long, value_enum, global = true, env = "NOVA_AGENT", default_value = "auto")]
    agent: AgentMode,
    /// The subcommand to run.
    #[command(subcommand)]
    cmd: Cmd,
}

/// Whether to switch on agent mode (`--agent` / `NOVA_AGENT`).
#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum AgentMode {
    /// Detect coding agents from the environment.
    Auto,
    /// Always use agent mode.
    On,
    /// Never use agent mode.
    Off,
}

/// Text printed by `nvf guide`: the read-narrow, patch-small workflow for coding agents.
const AGENT_GUIDE: &str = r#"NOVA for agents — read narrow, write small, never rewrite whole files.

1. Read only what you need (compact output, with the document revision):
     nvf get FILE 'services[id=api].{replicas,port}'
     nvf get FILE 'services[?replicas>3].id' -r        # one value per line
     nvf get FILE 'services[*].{id,port}' --to json     # JSON if you prefer
   Paths: a.b  [0]  [-1]  [id=api] (exactly one)  [*]  [?x>3] (filter; = != < <= > >= in)  [?x] [?!x]  .{a,b}

2. Change it with a conditional patch (atomic; comments and layout are preserved):
     cat > change.patch <<'P'
     @patch sha256:<first 8+ hex of the revision from step 1>
     @scope services[id=api]
     replicas: 5 -> 8
     P
     nvf patch FILE change.patch --schema SCHEMA.novs -w
   Ops:  path: old -> new   = path: v   + path: v (append/insert/add field)   - path[: old]   ? path[: v]
   On "conflict" or "stale base": re-run step 1 and rebuild the patch; do not force.

3. Validate:  nvf validate FILE --schema SCHEMA.novs
   Secrets and personal data print as @redacted; use the @revision (also a k:… token) as the patch base.
   Syntax:    key: value | - item | [a, b] | {k: v} | (tuple,) | !variant | @timestamp("…") | 22/7
   Quote strings that contain ": " or " #", or that start with a reserved character.
"#;

/// Output representation for `--to`.
#[derive(Clone, Copy, ValueEnum)]
enum Format {
    /// Human text (block layout).
    Nova,
    /// Compact text (tables for uniform records).
    Compact,
    /// Canonical text (sorted, deterministic).
    Canonical,
    /// JSON, pretty-printed (lossy for non-JSON types; warnings on stderr).
    Json,
    /// JSON on one line.
    JsonCompact,
    /// Framed binary.
    Binary,
    /// Framed binary in canonical form.
    BinaryCanonical,
    /// YAML 1.2 (lossy for NOVA-only types; warnings on stderr).
    Yaml,
    /// TOML 1.0 (root must be a record; no null).
    Toml,
    /// CBOR (RFC 8949).
    Cbor,
    /// CBOR, core deterministic encoding.
    CborDeterministic,
    /// Protobuf wire format (needs --schema with @id on every field).
    Protobuf,
    /// Positional text: field values in schema order, no names (needs --schema).
    Positional,
    /// NOVA Lines: one record per line, as a table when columns fit, else one value per line (root must be a list).
    Novl,
    /// NOVA Lines with one flow value per line (root must be a list).
    NovlValues,
    /// NOVA Lines in the positional profile (needs --schema with a list @root).
    NovlPositional,
    /// Binary record stream: a header frame, batches of records, an end frame (root must be a list).
    BinaryStream,
}

/// Input representation for `--from`.
#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum InputFormat {
    /// Binary frames by magic; `.json`, `.jsonl`, `.novl`, `.yaml`/`.yml`, `.toml`, `.cbor`, `.pb` by extension; otherwise JSON if it parses, else NOVA text.
    Auto,
    /// NOVA text.
    Nova,
    /// JSON.
    Json,
    /// JSON Lines (one value per line, read as a list).
    Jsonl,
    /// Framed binary: one frame, or a record stream starting with a header frame.
    Binary,
    /// NOVA Lines (one record per line, read as a list).
    Novl,
    /// YAML 1.2.
    Yaml,
    /// TOML 1.0.
    Toml,
    /// CBOR (RFC 8949).
    Cbor,
    /// Protobuf wire format (needs --schema).
    Protobuf,
}

/// `nvf` subcommands.
#[derive(Subcommand)]
enum Cmd {
    /// Reformat a NOVA text document.
    ///
    /// Only NOVA text (and `.novs` schemas) is formatted. Other input (JSON, JSON Lines, YAML,
    /// NOVA Lines, TOML, CBOR, Protobuf, binary) is refused; convert it with `nvf convert FILE --to nova`.
    Fmt {
        /// Document or schema to reformat.
        file: PathBuf,
        /// Output format.
        #[arg(long, value_enum, default_value = "nova")]
        to: Format,
        /// Rewrite the file in place.
        #[arg(short, long)]
        write: bool,
        /// How to treat classified values when the output is another format than NOVA text.
        #[command(flatten)]
        classified: Classified,
    },
    /// Convert between NOVA text and binary, JSON/JSONL, YAML, TOML, CBOR and Protobuf (input format is detected unless `--from` is given).
    Convert {
        /// Input file.
        file: PathBuf,
        /// Output format.
        #[arg(long, value_enum)]
        to: Format,
        /// Input format (detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        from: InputFormat,
        /// Schema, required for Protobuf input/output.
        #[arg(long)]
        schema: Option<PathBuf>,
        /// Output file (default: stdout).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Fail instead of warning when the conversion loses type information or precision.
        #[arg(long)]
        strict: bool,
        /// How to treat classified values.
        #[command(flatten)]
        classified: Classified,
    },
    /// Print the document's revision (SHA-256 of the canonical encoding).
    ///
    /// For a document with classified values this prints the opaque `k:` token instead, since
    /// a hash lets a reader test guesses of a hidden value; `--reveal` prints the revision.
    /// Without a redaction key the token would change on every run, so this then fails.
    Hash {
        /// Document to hash.
        file: PathBuf,
        /// How to treat classified values.
        #[command(flatten)]
        classified: Classified,
    },
    /// Validate against a schema; with --resolve print the resolved document.
    Validate {
        /// Document to validate.
        file: PathBuf,
        /// Schema to validate against. Without it, the document's `@schema`
        /// reference is looked up in catalogs (spec §6.9).
        #[arg(long)]
        schema: Option<PathBuf>,
        /// Extra catalog to look the reference up in (repeatable; also `NOVA_CATALOG`).
        #[arg(long)]
        catalog: Vec<PathBuf>,
        /// Fetch a qualified schema over HTTPS if no catalog has it (also `NOVA_FETCH=1`).
        #[arg(long)]
        fetch: bool,
        /// Print the resolved document instead of a status line.
        #[arg(long)]
        resolve: bool,
        /// Show classified values in the resolved document: all of them (no value), `personal`,
        /// or a comma-separated list of paths.
        #[arg(long, num_args = 0..=1, default_missing_value = "", require_equals = true, value_name = "personal|PATHS")]
        reveal: Option<String>,
    },
    /// Answer a query such as `services[id=api].{replicas,port}`.
    ///
    /// Paths: `a.b`, `[0]`, `[-1]`, `[id=api]` (unique match), `[*]` (all),
    /// `[?replicas>3]` (filter; also = != < <= >=, chainable), `.{a,b}` (projection).
    /// Use `-` as FILE to read stdin; JSON and JSON Lines are detected.
    Get {
        /// Input document (`-` for stdin).
        file: PathBuf,
        /// Query path, e.g. `services[id=api].replicas`.
        path: String,
        /// Output format.
        #[arg(long, value_enum, default_value = "compact")]
        to: Format,
        /// Input format (detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        from: InputFormat,
        /// Raw output: strings without quotes, and one line per result when the
        /// path selects many (`[*]`, filters), like `jq -r`.
        #[arg(short, long)]
        raw: bool,
        /// Omit the `@revision`/`@scope` header.
        #[arg(long)]
        bare: bool,
        /// Exit with status 1 if the result is null, false, or an empty list.
        #[arg(short, long)]
        exit_status: bool,
        /// How to treat classified values.
        #[command(flatten)]
        classified: Classified,
    },
    /// Apply a patch atomically.
    ///
    /// The result is printed, or with `-w` written back, in the input's format: NOVA text keeps
    /// its comments and layout; JSON, YAML, TOML, CBOR and binary are rendered in the same format
    /// (YAML and TOML comments are not kept); `.novs` schemas are rewritten in canonical form.
    /// JSON Lines and Protobuf have no writer: their result prints as NOVA text, and `-w` refuses
    /// them. For another output format, pipe the result through `nvf convert`.
    Patch {
        /// Document to patch.
        file: PathBuf,
        /// Patch file.
        patch: PathBuf,
        /// Schema the patched document must satisfy.
        #[arg(long)]
        schema: Option<PathBuf>,
        /// Rewrite the file in place.
        #[arg(short, long)]
        write: bool,
    },
    /// Print a conditional patch that turns A into B.
    ///
    /// A change to a classified value prints as `= path: @redacted`; the output is then a report,
    /// not a patch to apply.
    Diff {
        /// Original document.
        a: PathBuf,
        /// Target document.
        b: PathBuf,
        /// How to treat classified values.
        #[command(flatten)]
        classified: Classified,
    },
    /// Render a @template document.
    Render {
        /// Template document.
        file: PathBuf,
        /// Parameter as NAME=FLOW-VALUE (e.g. `replicas=3`, `env=prod`, `tags=[a,b]`).
        #[arg(long = "set", value_name = "NAME=VALUE")]
        set: Vec<String>,
        /// A NOVA record supplying parameters.
        #[arg(long)]
        values: Option<PathBuf>,
        /// Schema passed to the template; also validates the rendered document.
        #[arg(long)]
        schema: Option<PathBuf>,
        /// Output format.
        #[arg(long, value_enum, default_value = "nova")]
        to: Format,
        /// Output file (default: stdout). A file gets the values in clear unless `--reveal`
        /// says otherwise: rendering for deployment writes files.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Values from imported documents that their schemas classify print as `@redacted`
        /// on stdout unless revealed (proposal 0009.7).
        #[arg(long, num_args = 0..=1, default_missing_value = "", require_equals = true, value_name = "personal|PATHS")]
        reveal: Option<String>,
    },
    /// Compare sizes of every representation (bytes and estimated tokens).
    Stats {
        /// Document to measure.
        file: PathBuf,
        /// How to treat classified values: sizes are those of the redacted view unless revealed.
        #[command(flatten)]
        classified: Classified,
    },
    /// Print a short guide for AI coding agents (read projections, write conditional patches).
    Guide,
    /// Check documents (.nova) and schemas (.novs) against NOVA conventions; exits 1 on warnings and errors.
    ///
    /// Rules: key-case (`snake_case` keys; fixable), key-collision, quoted-key, untyped-timestamp
    /// (fixable), mixed-list, compat-loss (with `@compat json|yaml|toml`), field-case,
    /// variant-case, partial-ids, union-order. Configure in a file with `@x-lint case=camel allow=rule,…`.
    Lint {
        /// Documents (.nova) and schemas (.novs) to check.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Naming convention: snake (default), camel or kebab.
        #[arg(long)]
        case: Option<String>,
        /// Disable a rule (repeatable).
        #[arg(long)]
        allow: Vec<String>,
        /// Apply fixes in place.
        #[arg(long)]
        fix: bool,
        /// Output: text (diagnostics) or json (one finding per line).
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Schema tools.
    Schema {
        /// Schema subcommand to run.
        #[command(subcommand)]
        cmd: SchemaCmd,
    },
}

/// `nvf schema` subcommands.
#[derive(Subcommand)]
enum SchemaCmd {
    /// Print the canonical form of a schema.
    Fmt {
        /// Schema file.
        file: PathBuf,
    },
    /// Print `name#fingerprint`.
    Fingerprint {
        /// Schema file.
        file: PathBuf,
    },
    /// Classify changes between two schema versions and verify the `SemVer` bump; exits 1 if insufficient.
    Check {
        /// Previous schema version.
        old: PathBuf,
        /// New schema version.
        new: PathBuf,
    },
    /// Print the proto3 `.proto` file for a schema (fields need `@id`).
    Proto {
        /// Schema file.
        file: PathBuf,
    },
    /// Print a schema as NOVA data (its descriptor, validated by `nvf schema meta`).
    ToNova {
        /// Schema file.
        file: PathBuf,
    },
    /// Convert a schema descriptor (NOVA data) back to the schema language.
    FromNova {
        /// Schema descriptor (NOVA data).
        file: PathBuf,
    },
    /// Where a reference such as `acme.example/invoice@2.1.0` resolves from (spec §6.9).
    Resolve {
        /// The reference, `name@version[#fingerprint]`.
        reference: String,
        /// Extra catalog to search first (repeatable; also `NOVA_CATALOG`).
        #[arg(long)]
        catalog: Vec<PathBuf>,
        /// Fetch it over HTTPS if no catalog has it (also `NOVA_FETCH=1`).
        #[arg(long)]
        fetch: bool,
    },
    /// Fetch exactly the version a reference names into the user cache.
    Fetch {
        /// The reference, `name@version[#fingerprint]`.
        reference: String,
        /// Extra catalog for remote locations and pins (repeatable).
        #[arg(long)]
        catalog: Vec<PathBuf>,
        /// Record its revision as a pin in the project catalog (nova-catalog.nova).
        #[arg(long)]
        pin: bool,
    },
    /// Print the meta-schema `novafile.dev/meta/schema@0.3.4`, which describes schema descriptors.
    Meta,
    /// Print a JSON Schema (2020-12) describing the JSON form of valid documents.
    JsonSchema {
        /// Schema file.
        file: PathBuf,
    },
    /// Convert a JSON Schema (2020-12 or draft-07) into a NOVA schema.
    FromJsonSchema {
        /// JSON Schema file.
        file: PathBuf,
        /// Schema name for `@schema name@version`.
        #[arg(long)]
        name: Option<String>,
        /// `SemVer` version (default 0.1.0).
        #[arg(long)]
        version: Option<String>,
    },
}

/// How a command that prints document values treats classified values (proposal 0009).
///
/// Values that the document's schema marks `@secret` or `@personal` print as `@redacted`
/// unless `--reveal` shows them.
#[derive(clap::Args)]
struct Classified {
    /// Show classified values: all of them (no value), `personal`, or a comma-separated list of
    /// paths such as `services[id=api].db_url`.
    #[arg(long, num_args = 0..=1, default_missing_value = "", require_equals = true, value_name = "personal|PATHS")]
    reveal: Option<String>,
    /// Continue without classification when the document's schema cannot be found or the
    /// document is not valid against it. Then no value is redacted.
    #[arg(long)]
    no_schema: bool,
}

/// CLI error: a miette report, so diagnostics with source spans render nicely.
struct E(miette::Report);

impl From<String> for E {
    fn from(s: String) -> Self {
        E(miette::miette!("{s}"))
    }
}

impl From<&str> for E {
    fn from(s: &str) -> Self {
        E(miette::miette!("{s}"))
    }
}

impl From<SourceDiagnostic> for E {
    fn from(d: SourceDiagnostic) -> Self {
        E(miette::Report::new(d))
    }
}

/// Result of a CLI step.
type R<T> = Result<T, E>;

/// Display name of a path, for diagnostics.
fn name_of(p: &Path) -> String {
    p.display().to_string()
}

/// Reads a file, or stdin when the path is `-`.
fn read_bytes(p: &Path) -> R<Vec<u8>> {
    if p.as_os_str() == "-" {
        let mut b = Vec::new();
        std::io::stdin().read_to_end(&mut b).map_err(|e| e.to_string())?;
        return Ok(b);
    }

    Ok(std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?)
}

/// Reads a file (or stdin for `-`) as UTF-8 text.
fn read_text(p: &Path) -> R<String> {
    Ok(String::from_utf8(read_bytes(p)?).map_err(|_| format!("{}: not UTF-8", p.display()))?)
}

/// A loaded document: its directives and its value.
struct Loaded {
    /// Document directives as `(name, argument)` pairs, e.g. `("schema", "app@1.0.0")`.
    directives: Vec<(String, String)>,
    /// The document's value.
    value: Value,
}

/// Loads a document, detecting its input format.
fn load(p: &Path) -> R<Loaded> {
    load_as(p, InputFormat::Auto)
}

/// Wraps a value without any directives.
fn json_loaded(value: Value) -> Loaded {
    Loaded { directives: vec![], value }
}

/// Data from another format keeps that format's names: mark it `@compat` (spec §4.2).
fn compat_loaded(value: Value, format: &str) -> Loaded {
    Loaded {
        directives: vec![("compat".into(), format.into())],
        value,
    }
}

/// Loads a document in the given input format (`Auto` detects it).
fn load_as(p: &Path, from: InputFormat) -> R<Loaded> {
    load_with(p, from, None, false)
}

/// Schema reference of the meta-schema that describes schema descriptors.
const META_REF: &str = "novafile.dev/meta/schema@0.3.4";

/// Loads a document in the given input format.
///
/// `.novs` files load as their schema descriptor. Protobuf input and positional text need
/// `schema`; everything else ignores it. With `strict`, a lossy import (a JSON number too
/// precise for NOVA) is an error instead of a warning on stderr.
fn load_with(p: &Path, from: InputFormat, schema: Option<&Schema>, strict: bool) -> R<Loaded> {
    decode(p, read_bytes(p)?, from, schema, strict)
}

/// Resolves `Auto` by magic bytes or by the extension of `p`; stays `Auto` when neither decides.
fn detect(p: &Path, bytes: &[u8], from: InputFormat) -> InputFormat {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    match from {
        InputFormat::Auto if bytes.starts_with(MAGIC) => InputFormat::Binary,
        InputFormat::Auto if ext == "jsonl" || ext == "ndjson" => InputFormat::Jsonl,
        InputFormat::Auto if ext == "novl" => InputFormat::Novl,
        InputFormat::Auto if ext == "json" => InputFormat::Json,
        InputFormat::Auto if ext == "yaml" || ext == "yml" => InputFormat::Yaml,
        InputFormat::Auto if ext == "toml" => InputFormat::Toml,
        InputFormat::Auto if ext == "cbor" => InputFormat::Cbor,
        InputFormat::Auto if ext == "pb" || ext == "binpb" => InputFormat::Protobuf,
        f => f,
    }
}

/// Decodes the already read contents of `p`, like [`load_with`].
fn decode(p: &Path, bytes: Vec<u8>, from: InputFormat, schema: Option<&Schema>, strict: bool) -> R<Loaded> {
    let ctx = |e: String| format!("{}: {e}", p.display());
    let json = |v: Value, w: Vec<novafile_json::Warning>| -> R<Loaded> {
        lossy(&format!("NOVA ({})", p.display()), &w, strict)?;
        Ok(compat_loaded(v, "json"))
    };
    if p.extension().is_some_and(|e| e == "novs") && from == InputFormat::Auto {
        // Schema files load as their descriptor (spec §6.8), so every data tool works on them.
        let src = String::from_utf8(bytes).map_err(|_| ctx("not UTF-8".into()))?;
        return Ok(Loaded {
            directives: vec![("schema".into(), META_REF.into())],
            value: schema_from_text(p, &src)?.to_value(),
        });
    }

    let from = detect(p, &bytes, from);

    match from {
        InputFormat::Cbor => return Ok(json_loaded(novafile_formats::cbor::from_slice(&bytes).map_err(ctx)?)),
        InputFormat::Protobuf => {
            let s = schema.ok_or_else(|| ctx("reading Protobuf needs --schema".into()))?;
            return Ok(Loaded {
                directives: s.reference().map(|r| vec![("schema".to_owned(), r.to_string())]).unwrap_or_default(),
                value: novafile_proto::decode(&bytes, s).map_err(ctx)?,
            });
        }
        _ => {}
    }

    if from == InputFormat::Binary && novafile_binary::stream::is_stream(&bytes) {
        let r = novafile_binary::stream::read_stream(&bytes, true, schema.map(|s| s as &dyn Positional));
        let errors = r.errors.iter().map(|(at, m)| format!("byte {at}: {m}")).collect();
        return stream_loaded(p, r.records, errors, &r.segments, strict);
    }

    if from == InputFormat::Binary {
        let (f, n) = decode_frame(&bytes).map_err(|e| ctx(e.to_string()))?;
        if n != bytes.len() {
            return Err(ctx("trailing bytes after the first frame".into()).into());
        }

        let directives = f.schema.map(|s| vec![("schema".to_owned(), s)]).unwrap_or_default();
        return Ok(Loaded { directives, value: f.value });
    }

    let text = String::from_utf8(bytes).map_err(|_| ctx("not UTF-8".into()))?;
    match from {
        InputFormat::Novl => {
            let r = novafile_text::lines::read_str(&text, schema.map(|s| s as &dyn Positional));
            let errors = r.errors.iter().map(ToString::to_string).collect();
            return stream_loaded(p, r.records, errors, &r.segments, strict);
        }
        InputFormat::Jsonl => {
            let (v, w) = novafile_json::from_jsonl_str_lossy(&text).map_err(ctx)?;
            return json(Value::List(v), w);
        }
        InputFormat::Json => {
            let (v, w) = novafile_json::from_json_str_lossy(&text).map_err(ctx)?;
            return json(v, w);
        }
        InputFormat::Yaml => return Ok(compat_loaded(novafile_formats::yaml::from_str(&text).map_err(ctx)?, "yaml")),
        InputFormat::Toml => return Ok(compat_loaded(novafile_formats::toml::from_str(&text).map_err(ctx)?, "toml")),
        InputFormat::Auto => {
            // Sniff JSON (e.g. on stdin): a whole JSON value, or JSON Lines.
            let t = text.trim_start();
            if t.starts_with('{') || t.starts_with('[') || t.starts_with('"') {
                if let Ok((v, w)) = novafile_json::from_json_str_lossy(&text) {
                    return json(v, w);
                }

                let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
                if lines.len() > 1
                    && lines.iter().all(|l| l.trim_start().starts_with('{') || l.trim_start().starts_with('['))
                    && let Ok((v, w)) = novafile_json::from_jsonl_str_lossy(&text)
                {
                    return json(Value::List(v), w);
                }
            }
        }
        _ => {}
    }

    let doc = parse_document(&text).map_err(|e| e.to_diagnostic(&name_of(p), &text))?;
    if doc.is_template() {
        return Err(ctx("this is a @template; use `nvf render`".into()).into());
    }

    let mut value = doc.body.to_value().map_err(ctx)?;
    if doc.directive("mode") == Some("positional") {
        // Positional text is only meaningful with exactly the schema it was written for.
        let s = schema.ok_or_else(|| ctx("this document uses @mode positional; pass --schema".into()))?;
        let want = doc.directive("schema").unwrap_or("");
        if want != s.fingerprint() {
            return Err(ctx(format!("positional document was written for {want:?}, but the schema is {}", s.fingerprint())).into());
        }

        value = s.from_positional(&value).map_err(ctx)?;
        let directives = vec![("schema".to_owned(), s.reference().map(|r| r.to_string()).unwrap_or_default())];
        return Ok(Loaded { directives, value });
    }

    Ok(Loaded {
        directives: doc.directives.into_iter().map(|d| (d.name, d.arg)).collect(),
        value,
    })
}

/// A record stream (NOVA Lines or binary) as a list. Skipped records and corrupt segments
/// are errors with `strict`, warnings otherwise; an unfinished segment is a note.
fn stream_loaded(p: &Path, records: Vec<Value>, mut problems: Vec<String>, segments: &[Segment], strict: bool) -> R<Loaded> {
    for (i, seg) in segments.iter().enumerate() {
        let n = i + 1;
        match &seg.status {
            Status::Complete => {}
            Status::Open if seg.count == 0 && segments.len() == 1 => {}
            Status::Open => eprintln!("note: {}: segment {n} has no end marker; the producer may still be writing", p.display()),
            Status::Corrupt(m) => problems.push(format!("segment {n}: {m}")),
        }

        if let Some(r) = &seg.revision {
            eprintln!("note: {}: segment {n} revision {r} is not verified", p.display());
        }
    }

    if !problems.is_empty() {
        let msg = problems.iter().map(|m| format!("  {m}")).collect::<Vec<_>>().join("\n");
        if strict {
            return Err(format!("{}: invalid records:\n{msg}", p.display()).into());
        }

        eprintln!("warning: {}: skipped invalid records:\n{msg}", p.display());
    }

    let directives = match segments {
        [only] => only.header.schema.iter().map(|s| ("schema".to_owned(), s.clone())).collect(),
        _ => Vec::new(),
    };
    Ok(Loaded {
        directives,
        value: Value::List(records),
    })
}

/// Is this file NOVA text (rather than another format by extension, or binary)?
fn is_nova_text(p: &Path, text: &str) -> bool {
    matches!(detect(p, text.as_bytes(), InputFormat::Auto), InputFormat::Auto | InputFormat::Nova) && novafile_text::parse(text).is_ok()
}

/// Whether fetching is enabled by `--fetch` or `NOVA_FETCH=1`; the user catalog's
/// `fetch: true` is checked by the resolver (spec §6.9).
fn fetch_requested(flag: bool) -> bool {
    flag || std::env::var("NOVA_FETCH").is_ok_and(|v| v == "1")
}

/// Look up `reference` in built-in schemas, catalogs (explicit, then the project
/// catalog above `start`, then the user catalog) and the cache, and fetch it over
/// HTTPS if that finds nothing and fetching is enabled. Returns the schema and a
/// description of where it came from.
fn lookup(reference: &str, catalogs: &[PathBuf], start: Option<PathBuf>, fetch: bool) -> R<(Schema, String)> {
    use novafile_schema::catalog::{LookupError, Options, Resolver};
    let r = Resolver::new(&Options::from_env(catalogs.to_vec(), start))?;
    for p in &r.problems {
        eprintln!("warning: {p}");
    }

    match r.resolve(reference) {
        Ok(found) => {
            for w in &found.warnings {
                eprintln!("warning: {w}");
            }

            Ok((found.schema, found.origin.to_string()))
        }
        Err(LookupError::NotFound(m)) if fetch_requested(fetch) || r.fetch_enabled() => {
            let sr = novafile_schema::SchemaRef::parse(reference)?;
            let fetch = r.fetch_url(&sr).map_err(|why| format!("{m}; {why}"))?;
            let text = http_get(&fetch.url)?;
            let s = r.accept_fetched(&sr, &text).map_err(|e| e.to_string())?;
            if fetch.cacheable {
                let path = r.store(&s, &text).map_err(|e| e.to_string())?;
                eprintln!("fetched {sr} from {} into {}", fetch.url, path.display());
            } else {
                eprintln!("fetched {sr} from {} (the project catalog's location; not cached)", fetch.url);
            }

            Ok((s, fetch.url))
        }
        Err(e @ LookupError::NotFound(_)) => Err(format!("{e}; pass --schema, add it to a catalog, or allow --fetch").into()),
        Err(e) => Err(format!("{} ({})", e, e.kind()).into()),
    }
}

/// GET an `https:` URL, following only HTTPS redirects, with the body bounded by
/// [`novafile_schema::catalog::MAX_FETCH_BYTES`].
fn http_get(url: &str) -> R<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(5)
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .build()
        .into();
    let mut resp = agent.get(url).call().map_err(|e| format!("fetching {url}: {e}"))?;
    let text = resp
        .body_mut()
        .with_config()
        .limit(novafile_schema::catalog::MAX_FETCH_BYTES)
        .read_to_string()
        .map_err(|e| format!("fetching {url}: {e}"))?;
    Ok(text)
}

/// `nvf schema fetch`: fetch exactly `reference` into the cache and, with `pin`,
/// record its revision in the project catalog.
fn schema_fetch(reference: &str, catalogs: &[PathBuf], pin: bool) -> R<()> {
    use novafile_schema::catalog::{CATALOG_FILE, Options, Resolver, project_catalog};
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let r = Resolver::new(&Options::from_env(catalogs.to_vec(), Some(cwd.clone())))?;
    let sr = novafile_schema::SchemaRef::parse(reference)?;
    let fetch = r.fetch_url(&sr)?;
    let text = http_get(&fetch.url)?;
    let s = r.accept_fetched(&sr, &text).map_err(|e| e.to_string())?;
    println!("{sr}  {}\n{}", fetch.url, s.revision());
    if fetch.cacheable {
        println!("cached at {}", r.store(&s, &text).map_err(|e| e.to_string())?.display());
    } else {
        println!("not cached: the location comes from the project catalog, and the cache is shared by all projects");
    }
    if !pin {
        return Ok(());
    }

    // Trust on first use, then pinned (spec §6.9): add the pin, keeping the catalog's comments.
    let catalog = project_catalog(&cwd).unwrap_or_else(|| cwd.join(CATALOG_FILE));
    // Held until the pin is written, so two `--pin` runs cannot drop each other's pin.
    // shortcut: a catalog that does not exist yet is created without a lock; two first pins at once can race.
    let _lock = catalog.exists().then(|| lock(&catalog)).transpose()?;
    // Only a missing catalog starts fresh: on any other read error, overwriting would lose its pins.
    let src = match std::fs::read_to_string(&catalog) {
        Ok(src) => src,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => format!("@schema {}@0.1.0\n", novafile_schema::META_CATALOG_NAME),
        Err(e) => return Err(format!("{}: {e}", catalog.display()).into()),
    };
    let value = novafile_text::parse(&src).map_err(|e| format!("{}: {e}", catalog.display()))?;
    let key = format!("{}@{}", sr.name, sr.version);
    let pins = value.as_record().and_then(|r| r.get("pins")).and_then(Value::as_list);
    if let Some(p) = pins
        .into_iter()
        .flatten()
        .filter_map(Value::as_record)
        .find(|p| p.get("schema").and_then(Value::as_str) == Some(&key))
    {
        let have = p.get("revision").and_then(Value::as_str).unwrap_or("");
        if have == s.revision() {
            println!("already pinned in {}", catalog.display());
            return Ok(());
        }

        return Err(format!("{}: {key} is pinned to {have}, but the fetched file has {}", catalog.display(), s.revision()).into());
    }

    let entry = format!("{{schema: {}, revision: {}}}", novafile_text::quote(&key), novafile_text::quote(&s.revision()));
    let op = if pins.is_some() {
        format!("+ pins: {entry}")
    } else {
        format!("= pins: [{entry}]")
    };
    let patch = novafile_patch::parse_patch(&format!("@patch\n{op}\n")).map_err(|e| e.to_string())?;
    let meta = novafile_schema::meta_catalog();
    let check = |v: &Value| meta.validate(v).map_err(|i| i.iter().map(ToString::to_string).collect());
    let edit = novafile_patch::apply_to_source(&src, &patch, &check).map_err(|e| format!("{}: {e}", catalog.display()))?;
    write_atomic(&catalog, edit.text.as_bytes())?;
    println!("pinned in {}", catalog.display());
    Ok(())
}

/// Loads a schema from the schema language or from a schema descriptor (NOVA data).
fn load_schema(p: &Path) -> R<Schema> {
    schema_from_text(p, &read_text(p)?)
}

/// Parses `src`, the contents of the schema file `p`, like [`load_schema`].
fn schema_from_text(p: &Path, src: &str) -> R<Schema> {
    // A schema descriptor (NOVA data, spec §6.8) is accepted wherever a schema is.
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext != "novs"
        && let Ok(doc) = novafile_text::parse_document(src)
        && doc.directive("schema").is_some_and(|r| {
            r.starts_with(&format!("{}@", novafile_schema::META_SCHEMA_NAME)) || r.starts_with(&format!("{}@", novafile_schema::LEGACY_META_SCHEMA_NAME))
        })
    {
        let v = doc.body.to_value().map_err(|e| format!("{}: {e}", p.display()))?;
        return Ok(Schema::from_value(&v).map_err(|e| format!("{}: {e}", p.display()))?);
    }

    // Imports resolve through the catalogs that apply to the schema's directory (proposal 0008).
    let start = p.parent().filter(|_| p.as_os_str() != "-").map(Path::to_path_buf);
    let parsed = match novafile_schema::catalog::Resolver::new(&novafile_schema::catalog::Options::from_env(vec![], start)) {
        Ok(r) => r.load_text(src),
        Err(e) => {
            eprintln!("warning: {e}; only imports of the standard library resolve");
            novafile_schema::parse_schema(src)
        }
    };

    parsed.map_err(|e| {
        if e.line == 0 {
            E::from(format!("{}: {}", p.display(), e.message))
        } else {
            E::from(SourceDiagnostic::new("nova::schema", e.message.clone(), &name_of(p), src).with_line_col(e.line, 1, "in this definition"))
        }
    })
}

/// Report lossy mappings: an error with `strict`, warnings otherwise.
fn lossy<W: std::fmt::Display>(what: &str, warnings: &[W], strict: bool) -> R<()> {
    if warnings.is_empty() {
        return Ok(());
    }

    let msg = warnings.iter().map(|w| format!("  {w}")).collect::<Vec<_>>().join("\n");
    if strict {
        return Err(format!("conversion to {what} is lossy:\n{msg}").into());
    }

    eprintln!("warning: conversion to {what} is lossy:\n{msg}");
    Ok(())
}

/// Renders a loaded document in the given format, without a schema.
fn render_as(l: &Loaded, f: Format, strict: bool) -> R<Vec<u8>> {
    render_with(l, f, strict, None)
}

/// Renders a loaded document in the given format.
///
/// `schema` is required for Protobuf and positional output. With `strict`, a lossy
/// conversion is an error instead of a warning on stderr.
fn render_with(l: &Loaded, f: Format, strict: bool, schema: Option<&Schema>) -> R<Vec<u8>> {
    let directives: Vec<(&str, &str)> = l.directives.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let text = |p| document_to_text(&directives, &l.value, p).into_bytes();
    Ok(match f {
        Format::Nova => text(Profile::Human),
        Format::Compact => text(Profile::Compact),
        Format::Canonical => text(Profile::Canonical),
        Format::Json | Format::JsonCompact => {
            let (s, w) = novafile_json::to_json_string(&l.value, matches!(f, Format::Json));
            lossy("JSON", &w, strict)?;
            (s + "\n").into_bytes()
        }
        Format::Yaml => {
            let (s, w) = novafile_formats::yaml::to_string(&l.value);
            lossy("YAML", &w, strict)?;
            s.into_bytes()
        }
        Format::Toml => {
            let (s, w) = novafile_formats::toml::to_string(&l.value)?;
            lossy("TOML", &w, strict)?;
            s.into_bytes()
        }
        Format::Cbor | Format::CborDeterministic => {
            let (b, w) = novafile_formats::cbor::to_vec(&l.value, matches!(f, Format::CborDeterministic));
            lossy("CBOR", &w, strict)?;
            b
        }
        Format::Protobuf => {
            let s = schema.ok_or("writing Protobuf needs --schema")?;
            novafile_proto::encode(&l.value, s)?
        }
        Format::Positional => {
            let s = schema.ok_or("writing positional text needs --schema")?;
            let pos = s.to_positional(&l.value)?;
            let fp = s.fingerprint();
            document_to_text(&[("schema", fp.as_str()), ("mode", "positional")], &pos, Profile::Compact).into_bytes()
        }
        Format::Novl | Format::NovlValues | Format::NovlPositional => {
            let Value::List(records) = &l.value else {
                return Err(format!("NOVA Lines writes a list of records; the root is {}", l.value.kind()).into());
            };

            let mode = match f {
                Format::Novl => WriteMode::Table,
                Format::NovlValues => WriteMode::Values,
                _ => WriteMode::Positional(schema.ok_or("writing positional NOVA Lines needs --schema")?),
            };
            let declared = l.directives.iter().find(|(n, _)| n == "schema").map(|(_, s)| s.as_str());
            novafile_text::lines::write_string(records, mode, None, declared, None)
                .map_err(|e| e.to_string())?
                .into_bytes()
        }
        Format::BinaryStream => {
            let Value::List(records) = &l.value else {
                return Err(format!("a binary record stream writes a list of records; the root is {}", l.value.kind()).into());
            };

            let header = Header {
                schema: l.directives.iter().find(|(n, _)| n == "schema").map(|(_, s)| s.clone()),
                ..Header::default()
            };
            novafile_binary::stream::write_stream(records, &header, None, FrameOptions::default())?
        }
        Format::Binary | Format::BinaryCanonical => {
            let canonical = matches!(f, Format::BinaryCanonical);
            let schema = l.directives.iter().find(|(n, _)| n == "schema").map(|(_, s)| s.clone());
            encode_frame(
                &Frame {
                    schema,
                    value: l.value.clone(),
                    meta: false,
                },
                FrameOptions {
                    canonical,
                    dedupe_strings: !canonical,
                    tables: !canonical,
                    checksum: true,
                },
            )
        }
    })
}

/// Writes bytes to the given file, or to stdout.
fn out(bytes: &[u8], to: Option<&Path>) -> R<()> {
    match to {
        // Devices and pipes (`-o /dev/stdout`) cannot be replaced by a rename.
        Some(p) if std::fs::metadata(p).is_ok_and(|m| !m.is_file()) => Ok(std::fs::write(p, bytes).map_err(|e| format!("{}: {e}", p.display()))?),
        Some(p) => write_atomic(p, bytes),
        None => Ok(std::io::stdout().write_all(bytes).map_err(|e| e.to_string())?),
    }
}

/// Replaces the file `p` with `bytes` atomically: a temporary file in the same directory,
/// synced, then renamed over `p`, so readers never see a partial file.
///
/// shortcut: the new file gets the old one's permissions, but not its owner, group or xattrs;
/// copy them here if `nvf` ever edits files owned by another user (e.g. run as root).
fn write_atomic(p: &Path, bytes: &[u8]) -> R<()> {
    let err = |e: std::io::Error| format!("{}: {e}", p.display());
    // Write through a symlink to its target instead of replacing the link,
    // also when the target does not exist yet (then `canonicalize` fails).
    let p = &std::fs::canonicalize(p).unwrap_or_else(|_| match std::fs::read_link(p) {
        Ok(target) => p.parent().unwrap_or(Path::new("")).join(target),
        Err(_) => p.to_path_buf(),
    });
    let name = p.file_name().ok_or_else(|| format!("{}: not a file name", p.display()))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = p.with_file_name(tmp_name);
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        if let Ok(m) = std::fs::metadata(p) {
            f.set_permissions(m.permissions())?;
        }

        f.sync_all()?;
        std::fs::rename(&tmp, p)?;
        // Persist the rename itself; best effort, as some platforms (Windows) and filesystems cannot sync a directory.
        #[cfg(unix)]
        let _ = std::fs::File::open(p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."))).and_then(|d| d.sync_all());
        Ok(())
    };

    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        err(e).into()
    })
}

/// Takes an exclusive lock for a read-modify-write of `p`; it lasts until the file is dropped.
///
/// On Unix the lock is on `p` itself, so no lock file is left behind.
#[cfg(unix)]
fn lock(p: &Path) -> R<std::fs::File> {
    use std::os::unix::fs::MetadataExt as _;
    let err = |e: std::io::Error| format!("{}: {e}", p.display());
    loop {
        let f = std::fs::File::open(p).map_err(err)?;
        f.lock().map_err(err)?;
        // `write_atomic` renames a new inode over `p`: whoever waited on the old one must lock the new one.
        let (held, now) = (f.metadata().map_err(err)?, std::fs::metadata(p).map_err(err)?);
        if (held.dev(), held.ino()) == (now.dev(), now.ino()) {
            return Ok(f);
        }
    }
}

/// Takes an exclusive lock for a read-modify-write of `p`; it lasts until the file is dropped.
///
/// Windows locks are mandatory (they would block reading `p` through another handle), so this
/// locks a sidecar `<file>.lock` instead.
#[cfg(not(unix))]
fn lock(p: &Path) -> R<std::fs::File> {
    // A sidecar, since the atomic rename replaces the target's inode; it stays, as deleting it races other holders.
    let mut name = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).into_os_string();
    name.push(".lock");
    let err = |e: std::io::Error| format!("{}: {e}", Path::new(&name).display());
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&name).map_err(err)?;
    f.lock().map_err(err)?;
    Ok(f)
}

/// Formats validation issues as an indented list, one per line.
fn issues(i: &[novafile_schema::Issue]) -> String {
    i.iter().map(|i| format!("  {i}")).collect::<Vec<_>>().join("\n")
}

/// The `--reveal` mode: [`Reveal::Redact`] without the flag.
fn reveal_mode(arg: Option<&str>) -> R<Reveal> {
    arg.map_or(Ok(Reveal::Redact), |a| Ok(Reveal::parse(a)?))
}

/// The directory where catalog lookup for the document `file` starts; none for stdin.
fn start_of(file: &Path) -> Option<PathBuf> {
    file.parent().filter(|_| file.as_os_str() != "-").map(Path::to_path_buf)
}

/// A document prepared for output (proposal 0009).
struct View {
    /// The value to print: the stored value, with classified values replaced when
    /// something is hidden. Defaults and conversions are not applied.
    value: Value,
    /// How many values became placeholders.
    hidden: usize,
    /// The key for pseudonyms and revision tokens, if one was needed and found.
    key: Option<RedactKey>,
}

impl View {
    /// The stored value of `l`, with nothing hidden.
    fn plain(l: &Loaded) -> View {
        View {
            value: l.value.clone(),
            hidden: 0,
            key: None,
        }
    }

    /// The opaque token (`k:…`) for `rev` when the view hides values, so the revision
    /// cannot test guesses of a hidden value (proposal 0009.6.5).
    ///
    /// Without a stored key, a key for this process only is used: the token then cannot
    /// serve as a patch base, but the real revision is never shown.
    fn token(&self, rev: &novafile_value::Revision) -> Option<String> {
        if self.hidden == 0 {
            return None;
        }

        let key = self.key.clone().or_else(|| {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).ok().map(|()| RedactKey::new(bytes))
        });
        Some(key.map_or_else(|| "k:unavailable".to_owned(), |k| k.token(rev)))
    }
}

/// Classifies the document `l` (read from `file`) against `schema`, or else against the schema
/// its `@schema` line names, and redacts what `reveal` does not show.
///
/// A document without a schema has no classified values. If the schema cannot be found, or
/// the document is not valid against it, agent mode fails, so that a missing schema does not
/// quietly send values to an agent; `--no-schema` continues there, and `--reveal` needs no schema.
/// Outside agent mode this prints a warning and shows the values.
fn view(l: &Loaded, file: &Path, schema: Option<&Schema>, reveal: &Reveal, no_schema: bool, agent: bool) -> R<View> {
    if *reveal == Reveal::All {
        return Ok(View::plain(l));
    }

    let refuse = |why: String| -> R<View> {
        if no_schema || !agent {
            eprintln!("warning: {why}; values are shown without classification");
            return Ok(View::plain(l));
        }

        Err(
            format!("{why}\nthe document can have classified values; pass --reveal to show all values, or --no-schema to continue without classification")
                .into(),
        )
    };

    let found;
    let s = if let Some(s) = schema {
        s
    } else {
        let Some(reference) = l
            .directives
            .iter()
            .find(|(n, _)| n == "schema")
            .map(|(_, r)| r.as_str())
            .filter(|r| !r.is_empty())
        else {
            return Ok(View::plain(l));
        };

        match lookup(reference, &[], start_of(file), false) {
            Ok((s, _)) => {
                found = s;
                &found
            }
            Err(E(e)) => return refuse(format!("{}: cannot find the schema {reference}: {e}", file.display())),
        }
    };

    // Source paths: the view keeps the stored form, so a patch based on it matches the file.
    let marks: Vec<ClassMark> = match s.classify_source(&l.value) {
        Ok(c) => c,
        Err(i) => {
            return refuse(format!(
                "{} is invalid against {}, so its values cannot be classified:\n{}",
                file.display(),
                s.fingerprint(),
                issues(&i)
            ));
        }
    };

    if marks.is_empty() {
        return Ok(View::plain(l));
    }

    let key = redact_key();
    let r = novafile_schema::redact(&l.value, &marks, reveal, key.as_ref());
    if r.count() == 0 {
        return Ok(View::plain(l));
    }

    Ok(View {
        value: r.for_output().clone(),
        hidden: r.count(),
        key,
    })
}

/// Template imports read from files, with the schemas of imported documents looked up
/// in catalogs, so `nvf render` can classify what they hold (proposal 0009.7).
struct SchemaLoader {
    /// Reads the imported files.
    files: novafile_template::FsLoader,
    /// Where catalog lookup starts: the template's directory.
    start: Option<PathBuf>,
}

impl novafile_template::Loader for SchemaLoader {
    fn load(&self, path: &str, from: Option<&str>) -> Result<(String, String), String> {
        self.files.load(path, from)
    }

    fn schema(&self, reference: &str, _from: Option<&str>) -> Result<Option<Schema>, String> {
        lookup(reference, &[], self.start.clone(), false)
            .map(|(s, _)| Some(s))
            .map_err(|E(e)| e.to_string())
    }
}

/// `bytes`, the printed result of a patch, or the patched document `result` with its
/// classified values redacted when it has any. Fails when the document names a schema that
/// cannot be found or does not resolve it, since nothing could then be redacted.
fn redacted_echo(bytes: Vec<u8>, result: &Loaded, file: &Path, schema: Option<&Schema>) -> R<Vec<u8>> {
    let Some(marks) = conflict_marks(result, file, schema) else {
        return Err(format!(
            "{}: the patch applies, but the result is not shown: its schema is not found or does not classify it; use -w",
            file.display()
        )
        .into());
    };

    if marks.is_empty() {
        return Ok(bytes);
    }

    let r = novafile_schema::redact(&result.value, &marks, &Reveal::Redact, redact_key().as_ref());
    if r.count() == 0 {
        return Ok(bytes);
    }

    let shown = Loaded {
        directives: result.directives.clone(),
        value: r.for_output().clone(),
    };
    render_as(&shown, Format::Nova, false)
}

/// The classified paths of `l` (read from `file`), from `schema` or the schema its `@schema`
/// line names: empty without a schema, and `None` when the named schema cannot be found or
/// does not resolve the document, so that every conflict hides its values.
fn conflict_marks(l: &Loaded, file: &Path, schema: Option<&Schema>) -> Option<Vec<ClassMark>> {
    let found;
    let s = if let Some(s) = schema {
        s
    } else {
        let Some(reference) = l
            .directives
            .iter()
            .find(|(n, _)| n == "schema")
            .map(|(_, r)| r.as_str())
            .filter(|r| !r.is_empty())
        else {
            return Some(Vec::new());
        };
        found = lookup(reference, &[], start_of(file), false).ok()?.0;
        &found
    };

    s.classify_source(&l.value).ok()
}

/// Whether a patch conflict at `at` in `doc` must hide its values: `at` lies at, below or
/// above a classified path, or the classified paths are unknown (`marks` is `None`).
fn hides(doc: &Value, marks: Option<&[ClassMark]>, at: &novafile_text::Path) -> bool {
    let (Some(marks), Some(at)) = (marks, novafile_schema::concrete_path(doc, at)) else {
        return true;
    };

    marks.iter().any(|(m, _)| at.starts_with(&m.0) || m.0.starts_with(&at))
}

/// The key for pseudonyms and revision tokens: `NOVA_REDACT_KEY` (64 hex digits), else
/// `redact.key` next to the user schema cache, made on first use. Without a key, personal
/// values become a bare `@redacted` and revisions print in clear.
fn redact_key() -> Option<RedactKey> {
    if let Some(hex) = std::env::var_os("NOVA_REDACT_KEY").filter(|v| !v.is_empty()) {
        let key = hex.to_str().and_then(|h| RedactKey::from_hex(h).ok());
        if key.is_none() {
            eprintln!("warning: NOVA_REDACT_KEY must be 64 hex digits; pseudonyms and revision tokens are off");
        }

        return key;
    }

    let path = novafile_schema::catalog::user_cache()?.parent()?.join("redact.key");
    key_file(&path)
        .map_err(|e| eprintln!("warning: {}: {e}; pseudonyms and revision tokens are off", path.display()))
        .ok()
}

/// Reads the key file `path`, or makes it with 32 random bytes, readable only by the user.
///
/// The key is written to a temporary file first and then linked to `path`, so `path` is
/// never seen empty or half written, and of two processes that make it, the second uses
/// the key of the first.
fn key_file(path: &Path) -> Result<RedactKey, String> {
    let read = || -> Result<RedactKey, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        RedactKey::from_hex(text.trim()).map_err(|e| format!("{e}; delete the file to make a new key"))
    };
    match std::fs::metadata(path) {
        Ok(_) => return read(),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
        Err(_) => {}
    }

    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    let key = RedactKey::new(bytes);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }

    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let linked = write_new_private(&tmp, key.to_hex().as_bytes()).and_then(|()| std::fs::hard_link(&tmp, path));
    // The temporary name is ours alone; it is not needed after the link, or after a failure.
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(key),
        // Another process made it first: use its key.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => read(),
        Err(e) => Err(e.to_string()),
    }
}

/// Writes `bytes` to the new file `path`, readable only by the user, and syncs it to disk.
fn write_new_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// A placeholder as one line of raw output: `@redacted` or `@redacted(p:…)`.
fn raw_placeholder(v: &Value) -> String {
    match v {
        Value::Variant(_, Some(p)) => format!("@redacted({})", p.as_str().unwrap_or_default()),
        _ => "@redacted".into(),
    }
}

/// Whether a diff operation at `path` would show a value that the redacted `view` hides: the
/// path passes through a placeholder, or leads to a value that holds one. Where the path cannot
/// be followed (a selector that finds nothing, a map, a set), any placeholder below counts.
fn masked(view: &Value, path: &novafile_text::Path) -> bool {
    use novafile_text::Segment;
    let mut cur = view;
    for seg in &path.0 {
        if cur.is_redacted() {
            return true;
        }

        let next = match (cur, seg) {
            // A field that is not there holds no value to show.
            (Value::Record(r), Segment::Field(f)) => match r.get(f) {
                Some(v) => Some(v),
                None => return false,
            },
            (Value::Variant(t, Some(p)), Segment::Field(f)) if t == f => Some(&**p),
            (Value::List(l) | Value::Tuple(l), Segment::Index(i)) => usize::try_from(*i).ok().and_then(|i| l.get(i)),
            (Value::List(l), Segment::Match(f, want)) => l.iter().find(|e| e.as_record().and_then(|r| r.get(f)) == Some(want)),
            _ => None,
        };

        match next {
            Some(n) => cur = n,
            None => return cur.contains_redacted(),
        }
    }

    cur.contains_redacted()
}

/// Output formats that cannot hold a placeholder: binary encodings, where it must never be
/// encoded, and YAML and TOML, where it would become a plain string that can be written back.
fn refuses_placeholders(f: Format) -> bool {
    matches!(
        f,
        Format::Binary
            | Format::BinaryCanonical
            | Format::BinaryStream
            | Format::Cbor
            | Format::CborDeterministic
            | Format::Protobuf
            | Format::Yaml
            | Format::Toml
    )
}

/// Turns a patch error into a diagnostic that points into the patch source.
fn patch_error(e: &novafile_patch::PatchError, file: &Path, src: &str) -> E {
    use novafile_patch::PatchError as P;
    match e {
        P::Syntax { line, col, message } => {
            let (m, h) = message
                .split_once("; ")
                .map(|(a, b)| (a.to_owned(), Some(b.to_owned())))
                .unwrap_or((message.clone(), None));
            let mut d = SourceDiagnostic::new("nova::patch::syntax", m, &name_of(file), src).with_line_col(*line, *col, "here");
            if let Some(h) = h {
                d = d.with_help(h);
            }

            d.into()
        }
        P::Op { index, line, message, .. } => {
            let off = src.split_inclusive('\n').take(line.saturating_sub(1)).map(str::len).sum::<usize>();
            let len = src[off..].find('\n').unwrap_or(src.len() - off);
            SourceDiagnostic::new(
                "nova::patch::conflict",
                format!("patch rejected: operation {} failed; nothing was applied", index + 1),
                &name_of(file),
                src,
            )
            .with_label(off, len, message.clone())
            .into()
        }
        P::StaleBase { .. } => SourceDiagnostic::new("nova::patch::stale", e.to_string(), &name_of(file), src)
            .with_help("fetch the document again (`nvf get`) and rebase the patch on the new revision")
            .into(),
        other => other.to_string().into(),
    }
}

/// Runs the parsed command and returns the process exit code.
#[expect(clippy::too_many_lines, reason = "one dispatch arm per subcommand")]
fn run(cli: Cli) -> R<ExitCode> {
    let agent = agent_detected(cli.agent).is_some();
    match cli.cmd {
        Cmd::Guide => print!("{AGENT_GUIDE}"),
        Cmd::Lint {
            files,
            case,
            allow,
            fix,
            format,
        } => return lint(&files, case, allow, fix, &format),
        Cmd::Fmt { file, to, write, classified } => {
            // NOVA text keeps its comments, blank lines and spellings (spec §3).
            let profile = match to {
                Format::Nova => Some(Profile::Human),
                Format::Canonical => Some(Profile::Canonical),
                Format::Compact => Some(Profile::Compact),
                _ => None,
            };

            if file.extension().is_some_and(|e| e == "novs") && matches!(to, Format::Nova | Format::Canonical) {
                // The schema writer drops comments and reorders definitions, license headers included.
                if write {
                    return Err(format!(
                        "{}: -w does not rewrite schemas, as their comments would be lost; print with `nvf fmt {}` and review",
                        file.display(),
                        file.display()
                    )
                    .into());
                }

                out(load_schema(&file)?.to_source_text().as_bytes(), None)?;
                return Ok(ExitCode::SUCCESS);
            }

            if write && file.as_os_str() == "-" {
                return Err("-w needs a file to rewrite, not stdin".into());
            }

            // Hold the lock from read to write, as `patch -w` does, so no concurrent edit is undone.
            let _lock = write.then(|| lock(&file)).transpose()?;
            let mut input = read_bytes(&file)?;
            let detected = detect(&file, &input, InputFormat::Auto);
            if !matches!(detected, InputFormat::Auto | InputFormat::Nova) {
                let name = detected.to_possible_value().map(|v| v.get_name().to_owned()).unwrap_or_default();
                return Err(format!(
                    "{}: `nvf fmt` formats NOVA text only, and this is {name}; convert it with `nvf convert {} --to nova -o {}`",
                    file.display(),
                    file.display(),
                    file.with_extension("nova").display()
                )
                .into());
            }

            if let Some((fixed, n)) = std::str::from_utf8(&input).ok().and_then(novafile_text::fix_syntax) {
                eprintln!("note: {}: fixed {n} table row count{}", file.display(), if n == 1 { "" } else { "s" });
                input = fixed.into_bytes();
            }

            let text = std::str::from_utf8(&input)
                .ok()
                .filter(|t| is_nova_text(&file, t) && file.extension().is_none_or(|e| e != "novs"));
            let bytes = match (profile, text) {
                (Some(p), Some(t)) => novafile_text::format_preserving(t, p)
                    .map_err(|e| e.to_diagnostic(&name_of(&file), t))?
                    .into_bytes(),
                // Another format in place would replace the NOVA text, comments and types included.
                _ if write => {
                    return Err(format!(
                        "{}: -w rewrites NOVA text only; write other formats with `nvf convert {} --to … -o FILE`",
                        file.display(),
                        file.display()
                    )
                    .into());
                }
                // A conversion to stdout prints values like `nvf convert`, so it redacts like it.
                _ => {
                    let reveal = reveal_mode(classified.reveal.as_deref())?;
                    let l = decode(&file, input.clone(), InputFormat::Auto, None, false)?;
                    let v = view(&l, &file, None, &reveal, classified.no_schema, agent)?;
                    if v.hidden > 0 && refuses_placeholders(to) {
                        return Err(format!(
                            "{}: classified values are redacted, and this format cannot hold the placeholders; use --reveal",
                            file.display()
                        )
                        .into());
                    }

                    render_as(
                        &Loaded {
                            directives: l.directives,
                            value: v.value,
                        },
                        to,
                        false,
                    )?
                }
            };

            out(&bytes, write.then_some(file.as_path()))?;
        }

        Cmd::Convert {
            schema,
            file,
            to,
            from,
            output,
            strict,
            classified,
        } => {
            let reveal = reveal_mode(classified.reveal.as_deref())?;
            let s = schema.as_deref().map(load_schema).transpose()?;
            let l = load_with(&file, from, s.as_ref(), strict)?;
            let v = view(&l, &file, s.as_ref(), &reveal, classified.no_schema, agent)?;
            let name = to.to_possible_value().map(|v| v.get_name().to_owned()).unwrap_or_default();
            if v.hidden > 0 && refuses_placeholders(to) {
                return Err(format!(
                    "{}: {} classified value(s) are redacted, and {name} cannot hold the placeholders; use --reveal or a text format (NOVA, JSON)",
                    file.display(),
                    v.hidden
                )
                .into());
            }

            if v.hidden > 0 {
                lossy(&name, &[format!("{} classified value(s) are redacted", v.hidden)], strict)?;
            }

            let shown = Loaded {
                directives: l.directives,
                value: v.value,
            };

            out(&render_with(&shown, to, strict, s.as_ref())?, output.as_deref())?;
        }
        Cmd::Hash { file, classified } => {
            let reveal = reveal_mode(classified.reveal.as_deref())?;
            let l = load(&file)?;
            let v = view(&l, &file, None, &reveal, classified.no_schema, agent)?;
            let rev = revision(&l.value);
            if v.hidden > 0 && v.key.is_none() {
                return Err("classified values are hidden and there is no redaction key for a stable token; set NOVA_REDACT_KEY, or pass --reveal".into());
            }

            println!("{}", v.token(&rev).unwrap_or_else(|| rev.to_string()));
        }
        Cmd::Validate {
            file,
            schema,
            catalog,
            fetch,
            resolve,
            reveal,
        } => {
            let reveal = reveal_mode(reveal.as_deref())?;
            let l = load(&file)?;
            let s = if let Some(p) = &schema {
                load_schema(p)?
            } else {
                let r = l
                    .directives
                    .iter()
                    .find(|(n, _)| n == "schema")
                    .map(|(_, r)| r.as_str())
                    .ok_or_else(|| format!("{}: no @schema line to look up; pass --schema SCHEMA.novs", file.display()))?;
                let start = file.parent().filter(|_| file.as_os_str() != "-").map(Path::to_path_buf);
                lookup(r, &catalog, start, fetch)?.0
            };
            if let Some((_, r)) = l.directives.iter().find(|(n, _)| n == "schema")
                && s.reference().is_some()
            {
                s.accepts_reference(r).map_err(|e| format!("{}: {e}", file.display()))?;
            }

            match s.classify(&l.value) {
                Ok((v, marks)) => {
                    if resolve {
                        let key = (!marks.is_empty() && reveal != Reveal::All).then(redact_key).flatten();
                        let resolved = Loaded {
                            directives: vec![("schema".into(), s.fingerprint())],
                            value: novafile_schema::redact(&v, &marks, &reveal, key.as_ref()).for_output().clone(),
                        };

                        out(&render_as(&resolved, Format::Nova, false)?, None)?;
                    } else {
                        eprintln!("ok: valid against {}", s.fingerprint());
                    }
                }
                Err(i) => {
                    // Point at each offending value when the input is NOVA text, unless the
                    // schema classifies values: the snippet would show them (proposal 0009.6.2).
                    let src = read_text(&file).ok().filter(|_| !s.has_classes());
                    if let Some((_, spans)) = src.as_deref().and_then(|t| novafile_text::parse_with_spans(t).ok()) {
                        let src = src.unwrap();
                        let title = format!(
                            "{} is invalid against {} ({} issue{})",
                            file.display(),
                            s.fingerprint(),
                            i.len(),
                            if i.len() == 1 { "" } else { "s" }
                        );
                        return Err(SourceDiagnostic::from_path_issues(
                            "nova::invalid",
                            title,
                            &name_of(&file),
                            &src,
                            &spans,
                            i.iter().map(|x| (&x.path, x.message.clone())),
                        )
                        .into());
                    }

                    return Err(format!("{} is invalid:\n{}", file.display(), issues(&i)).into());
                }
            }
        }

        Cmd::Get {
            file,
            path,
            to,
            from,
            raw,
            bare,
            exit_status,
            classified,
        } => {
            let reveal = reveal_mode(classified.reveal.as_deref())?;
            let l = load_as(&file, from)?;
            // The query runs on the redacted view, so a filter cannot test a hidden value.
            let v = view(&l, &file, None, &reveal, classified.no_schema, agent)?;
            let mut r = novafile_patch::get(&v.value, &path)?;
            // The revision is always that of the stored document, never of the view.
            r.revision = revision(&l.value);
            r.token = v.token(&r.revision);
            let multi = !novafile_patch::parse_request(&path)?.is_singular();
            let json = matches!(to, Format::Json | Format::JsonCompact);
            let one_line = |v: &Value| -> String {
                match v {
                    _ if raw && v.is_redacted() => raw_placeholder(v),
                    Value::String(s) if raw => s.clone(),
                    _ if json => novafile_json::to_json_string(v, false).0,
                    _ => novafile_text::flow_string(v),
                }
            };

            if raw {
                // jq -r style: one line per result.
                match (&r.value, multi) {
                    (Value::List(items), true) => items.iter().for_each(|v| println!("{}", one_line(v))),
                    (v, _) => println!("{}", one_line(v)),
                }
            } else if json {
                let (s, warnings) = novafile_json::to_json_string(&r.value, matches!(to, Format::Json));
                for w in warnings {
                    eprintln!("warning: {w}");
                }

                println!("{s}");
            } else {
                let p = match to {
                    Format::Nova => Profile::Human,
                    Format::Canonical => Profile::Canonical,
                    _ => Profile::Compact,
                };

                if bare {
                    print!("{}", novafile_text::to_text(&r.value, p));
                } else {
                    print!("{}", r.to_text(p));
                }
            }

            let empty = matches!(&r.value, Value::Null | Value::Bool(false)) || matches!(&r.value, Value::List(l) if l.is_empty());
            if exit_status && empty {
                return Ok(ExitCode::from(1));
            }
        }
        Cmd::Patch { file, patch, schema, write } => {
            if write && file.as_os_str() == "-" {
                return Err("-w needs a file to rewrite, not stdin".into());
            }

            if file.as_os_str() == "-" && patch.as_os_str() == "-" {
                return Err("the document and the patch cannot both come from stdin".into());
            }

            // Held until the patched file is written, so a concurrent patch sees this one's result.
            let _lock = write.then(|| lock(&file)).transpose()?;
            let bytes = read_bytes(&file)?;
            // In place, a rounded number would be written back: refuse it, as for lossy output.
            let l = decode(&file, bytes.clone(), InputFormat::Auto, None, write)?;
            let directives = l.directives.clone();
            let patch_src = read_text(&patch)?;
            let mut p = novafile_patch::parse_patch(&patch_src).map_err(|e| patch_error(&e, &patch, &patch_src))?;
            // `@patch k:…` names an opaque revision token (proposal 0009.6.5), not a schema.
            if p.base.is_none() && p.schema.as_deref().is_some_and(|t| t.starts_with("k:")) {
                p.base = p.schema.take();
            }

            let key = p.base.as_deref().is_some_and(|b| b.starts_with("k:")).then(redact_key);
            if let Some(key) = &key {
                let key = key
                    .as_ref()
                    .ok_or("the patch base is a k: token, but there is no redaction key to check it with")?;
                let base = p.base.clone().unwrap_or_default();
                let current = key.token(&revision(&l.value));
                // Like a revision, a token can be cut to 8 or more hex digits.
                if base.len() < 10 || !current.starts_with(&base) {
                    return Err(SourceDiagnostic::new(
                        "nova::patch::stale",
                        format!("conflict: patch expects base {base} but the document is at {current}"),
                        &name_of(&patch),
                        &patch_src,
                    )
                    .with_help("fetch the document again (`nvf get`) and rebase the patch on the new revision")
                    .into());
                }

                p.base = Some(revision(&l.value).to_string());
            }

            let s = schema.as_deref().map(load_schema).transpose()?;
            if let (Some(s), Some(r)) = (&s, &p.schema)
                && s.reference().is_some()
            {
                s.accepts_reference(r).map_err(|e| format!("{}: {e}", patch.display()))?;
            }

            // Conflicts at classified paths do not show the stored value (proposal 0009.6.2).
            let marks = conflict_marks(&l, &file, s.as_ref());
            let hide = |at: &novafile_text::Path| hides(&l.value, marks.as_deref(), at);
            let validate = |v: &Value| match &s {
                Some(s) => s.validate(v).map_err(|i| i.iter().map(std::string::ToString::to_string).collect()),
                None => Ok(()),
            };

            let text = std::str::from_utf8(&bytes).ok().filter(|t| is_nova_text(&file, t));
            let (bytes, applied) = if let Some(src) = text {
                // Document-preserving: minimal edits to the source text.
                let r = novafile_patch::apply_to_source(src, &p, &validate).map_err(|e| patch_error(&e.hiding(&p, &hide), &patch, &patch_src))?;
                (r.text.into_bytes(), r.applied)
            } else {
                let is_schema = file.extension().is_some_and(|e| e == "novs");
                let meta = novafile_schema::meta_schema();
                let meta_check = |v: &Value| meta.validate(v).map_err(|i| i.iter().map(std::string::ToString::to_string).collect());
                let check: &dyn Fn(&Value) -> Result<(), Vec<String>> = if is_schema { &meta_check } else { &validate };
                let applied = novafile_patch::apply(&l.value, &p, check).map_err(|e| patch_error(&e.hiding(&p, &hide), &patch, &patch_src))?;
                if is_schema {
                    // Patched schema descriptors are written back in the schema language.
                    let mut s = Schema::from_value(&applied.value)?;
                    // Descriptors leave out `@x-…` directives; keep the file's own.
                    if let Ok(old) = novafile_schema::parse_schema_unlinked(&String::from_utf8_lossy(&bytes)) {
                        s.extensions = old.extensions;
                    }

                    eprintln!("note: {} is rewritten in canonical form (comments are not kept)", file.display());
                    (s.to_source_text().into_bytes(), applied)
                } else {
                    // Written back in the format it was read in.
                    let to = match detect(&file, &bytes, InputFormat::Auto) {
                        // Record streams would lose their header (`@key`, columns) if written back.
                        InputFormat::Binary if novafile_binary::stream::is_stream(&bytes) => None,
                        InputFormat::Binary => Some(Format::Binary),
                        InputFormat::Json => Some(Format::Json),
                        InputFormat::Yaml => Some(Format::Yaml),
                        InputFormat::Toml => Some(Format::Toml),
                        InputFormat::Cbor => Some(Format::Cbor),
                        InputFormat::Jsonl | InputFormat::Novl | InputFormat::Protobuf => None,
                        InputFormat::Auto | InputFormat::Nova if l.directives.iter().any(|(n, a)| n == "compat" && a == "json") => Some(Format::Json),
                        InputFormat::Auto | InputFormat::Nova => Some(Format::Nova),
                    };
                    if write && to.is_none() {
                        return Err(format!("{}: cannot write this format back; omit -w to print NOVA text", file.display()).into());
                    }

                    // These formats are rendered again from the value, so their comments are lost.
                    if write && matches!(to, Some(Format::Yaml | Format::Toml)) {
                        eprintln!("note: {} is rewritten from its values (comments are not kept)", file.display());
                    }

                    let result = Loaded {
                        directives: l.directives,
                        value: applied.value.clone(),
                    };

                    // In place, a lossy conversion would silently drop data from the file.
                    (render_as(&result, to.unwrap_or(Format::Nova), write)?, applied)
                }
            };

            // The printed result must not echo classified values (proposal 0009.6.3).
            let bytes = if write {
                bytes
            } else {
                let result = Loaded {
                    directives,
                    value: applied.value.clone(),
                };
                redacted_echo(bytes, &result, &file, s.as_ref())?
            };

            out(&bytes, write.then_some(file.as_path()))?;
            if let Some(k) = key.flatten() {
                eprintln!("{} -> {}", k.token(&applied.base), k.token(&applied.revision));
            } else {
                eprintln!("{} -> {}", applied.base, applied.revision);
            }
        }
        Cmd::Diff { a, b, classified } => {
            let reveal = reveal_mode(classified.reveal.as_deref())?;
            let (la, lb) = (load(&a)?, load(&b)?);
            let (va, vb) = (
                view(&la, &a, None, &reveal, classified.no_schema, agent)?,
                view(&lb, &b, None, &reveal, classified.no_schema, agent)?,
            );
            // Diff the stored values, then hide changes to classified values (proposal 0009.6.4):
            // a diff of two redacted views would not show that a hidden value changed.
            let mut d = novafile_patch::diff(&la.value, &lb.value);
            for op in &mut d.ops {
                if masked(&va.value, &op.path) || masked(&vb.value, &op.path) {
                    op.kind = novafile_patch::OpKind::Set(Value::Variant(novafile_value::REDACTED_TAG.into(), None));
                }
            }

            if va.hidden + vb.hidden > 0 {
                let rev = revision(&la.value);
                d.base = va.token(&rev).or_else(|| vb.key.as_ref().map(|k| k.token(&rev)));
            }

            print!("{d}");
        }
        Cmd::Render {
            file,
            set,
            values,
            schema,
            to,
            output,
            reveal,
        } => {
            let src = read_text(&file)?;
            let mut args = match values {
                Some(v) => match load(&v)?.value {
                    Value::Record(r) => r,
                    _ => return Err("--values must contain a record".into()),
                },
                None => Record::new(),
            };

            for s in set {
                let (k, v) = s.split_once('=').ok_or_else(|| format!("--set {s:?}: expected NAME=VALUE"))?;
                let v = novafile_text::parse_flow(v).map_err(|e| format!("--set {k}: {e}"))?;
                args.set(k, v);
            }

            let s = schema.as_deref().map(load_schema).transpose()?;
            let loader = SchemaLoader {
                files: novafile_template::FsLoader {
                    // A bare file name has an empty parent: the current directory.
                    root: file.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf(),
                },
                start: start_of(&file),
            };

            let opts = novafile_template::Options {
                loader: &loader,
                schema: s.as_ref(),
                limits: novafile_template::Limits::default(),
            };

            let reveal = match (&reveal, &output) {
                (None, Some(_)) => Reveal::All,
                _ => reveal_mode(reveal.as_deref())?,
            };
            let key = redact_key();
            let mode = novafile_template::Redaction { reveal, key: key.as_ref() };
            let r = novafile_template::render_redacted(&src, &args, &opts, &mode).map_err(|e| format!("{}: {e}", file.display()))?;
            if let Some(s) = &s
                && s.root.is_some()
            {
                // Placeholders would not validate: check the clear rendering instead.
                let clear = novafile_template::Redaction {
                    reveal: Reveal::All,
                    key: None,
                };
                let v = if r.count() == 0 {
                    r.for_output().clone()
                } else {
                    novafile_template::render_redacted(&src, &args, &opts, &clear)
                        .map_err(|e| format!("{}: {e}", file.display()))?
                        .for_output()
                        .clone()
                };
                s.validate(&v).map_err(|i| format!("rendered document is invalid:\n{}", issues(&i)))?;
            }

            if r.count() > 0 && refuses_placeholders(to) {
                return Err("the rendering has redacted values; use --reveal or a text format (NOVA, JSON)".into());
            }

            out(
                &render_as(
                    &Loaded {
                        directives: vec![],
                        value: r.for_output().clone(),
                    },
                    to,
                    false,
                )?,
                output.as_deref(),
            )?;
        }
        Cmd::Stats { file, classified } => {
            let reveal = reveal_mode(classified.reveal.as_deref())?;
            let stored = load(&file)?;
            let l = view(&stored, &file, None, &reveal, classified.no_schema, agent)?;
            let c = HeuristicCounter;
            println!("{:<18} {:>9} {:>9}", "representation", "bytes", "~tokens");
            for (name, f) in [
                ("nova (human)", Format::Nova),
                ("nova (compact)", Format::Compact),
                ("nova (canonical)", Format::Canonical),
                ("json (pretty)", Format::Json),
                ("json (compact)", Format::JsonCompact),
                ("yaml", Format::Yaml),
                ("toml", Format::Toml),
                ("binary", Format::Binary),
                ("binary canonical", Format::BinaryCanonical),
                ("cbor", Format::Cbor),
            ] {
                let plain = Loaded {
                    directives: vec![],
                    value: l.value.clone(),
                };

                let bytes = match f {
                    Format::Json | Format::JsonCompact => novafile_json::to_json_string(&plain.value, matches!(f, Format::Json)).0.into_bytes(),
                    Format::Yaml => novafile_formats::yaml::to_string(&plain.value).0.into_bytes(),
                    Format::Toml => match novafile_formats::toml::to_string(&plain.value) {
                        Ok((t, _)) => t.into_bytes(),
                        Err(_) => continue, // not representable
                    },
                    Format::Cbor => novafile_formats::cbor::to_vec(&plain.value, false).0,
                    _ => render_as(&plain, f, false)?,
                };

                let tokens = std::str::from_utf8(&bytes)
                    .ok()
                    .filter(|_| !matches!(f, Format::Binary | Format::BinaryCanonical | Format::Cbor))
                    .map_or_else(|| "-".into(), |t| c.count(t).to_string());
                println!("{name:<18} {:>9} {tokens:>9}", bytes.len());
            }

            eprintln!("(token counts are a tokenizer-independent heuristic; see spec §10)");
        }
        Cmd::Schema { cmd } => match cmd {
            SchemaCmd::Fmt { file } | SchemaCmd::FromNova { file } => print!("{}", load_schema(&file)?.to_source_text()),
            SchemaCmd::Fingerprint { file } => println!("{}", load_schema(&file)?.fingerprint()),
            SchemaCmd::Proto { file } => print!("{}", novafile_proto::proto_file(&load_schema(&file)?)?),
            SchemaCmd::ToNova { file } => {
                let v = load_schema(&file)?.to_value();
                print!("{}", document_to_text(&[("schema", META_REF)], &v, Profile::Human));
            }
            SchemaCmd::Meta => print!("{}", novafile_schema::META_SCHEMA_SRC),
            SchemaCmd::Resolve { reference, catalog, fetch } => {
                let cwd = std::env::current_dir().ok();
                let (s, origin) = lookup(&reference, &catalog, cwd, fetch)?;
                println!(
                    "{}  {origin}\n{}",
                    s.reference().map_or_else(|| reference.clone(), |r| r.to_string()),
                    s.revision()
                );
            }
            SchemaCmd::Fetch { reference, catalog, pin } => schema_fetch(&reference, &catalog, pin)?,
            SchemaCmd::JsonSchema { file } => {
                let j = novafile_schemars::to_json_schema(&load_schema(&file)?);
                println!("{}", serde_json::to_string_pretty(&j).map_err(|e| e.to_string())?);
            }
            SchemaCmd::FromJsonSchema { file, name, version } => {
                let j: serde_json::Value = serde_json::from_str(&read_text(&file)?).map_err(|e| format!("{}: {e}", file.display()))?;
                let c = novafile_schemars::from_json_schema(&j, name.as_deref(), version.as_deref()).map_err(|e| format!("{}: {e}", file.display()))?;
                for w in &c.warnings {
                    eprintln!("warning: {w}");
                }

                print!("{}", c.schema.to_canonical_text());
            }
            SchemaCmd::Check { old, new } => {
                let new = load_schema(&new)?;
                let r = check_versions(&load_schema(&old)?, &new)?;
                for c in &r.changes {
                    println!("{c}");
                }

                // Member order is informative (spec §6.5); it does not fail the check.
                for w in new.union_warnings() {
                    eprintln!("warning: {w}");
                }

                let kind = match r.kind {
                    novafile_schema::ChangeKind::None => "no semantic changes",
                    novafile_schema::ChangeKind::Compatible => "backward-compatible changes",
                    novafile_schema::ChangeKind::Breaking => "breaking changes",
                };

                println!("{} -> {}: {kind}; minimum version {}", r.old, r.new, r.minimum);
                if !r.problems.is_empty() {
                    for p in &r.problems {
                        eprintln!("error: {p}");
                    }

                    return Ok(ExitCode::from(1));
                }

                println!("ok: version bump is valid");
            }
        },
    }

    Ok(ExitCode::SUCCESS)
}

/// Runs `nvf lint` over files and prints findings; exits 1 if any warning or error remains.
fn lint(files: &[PathBuf], case: Option<String>, allow: Vec<String>, fix: bool, format: &str) -> R<ExitCode> {
    let mut config = novafile_lint::Config::default();
    if let Some(c) = case {
        config.case = novafile_lint::Case::parse(&c).ok_or_else(|| format!("unknown case {c:?} (snake, camel, kebab)"))?;
    }

    config.allow = allow;
    let mut failures = 0;
    for file in files {
        if fix && file.as_os_str() == "-" {
            return Err("--fix needs a file to rewrite, not stdin".into());
        }

        let _lock = fix.then(|| lock(file)).transpose()?;
        let mut src = read_text(file)?;
        let is_schema = file.extension().is_some_and(|e| e == "novs");
        // Both kinds of fix are applied in memory and written once, so a lint
        // error after the syntax fix leaves the file untouched.
        let mut changed = false;
        if fix
            && !is_schema
            && let Some((fixed, n)) = novafile_text::fix_syntax(&src)
        {
            eprintln!("{}: fixed {n} table row count{}", file.display(), if n == 1 { "" } else { "s" });
            src = fixed;
            changed = true;
        }

        let run = |src: &str| -> R<Vec<novafile_lint::Finding>> {
            if is_schema {
                // Linked through the catalogs, so imports of catalog schemas work.
                let schema = schema_from_text(file, src)?;
                Ok(novafile_lint::lint_parsed_schema(src, &schema, &config).map_err(|e| format!("{}: {e}", file.display()))?)
            } else {
                Ok(novafile_lint::lint_document(src, &config).map_err(|e| e.to_diagnostic(&name_of(file), src))?)
            }
        };

        let mut findings = run(&src)?;
        if fix && findings.iter().any(|f| f.fix.is_some()) {
            let fixed = novafile_lint::apply_fixes(&src, &findings);
            let n = findings.iter().filter(|f| f.fix.is_some()).count();
            eprintln!("{}: applied {n} fix{}", file.display(), if n == 1 { "" } else { "es" });
            src = fixed;
            findings = run(&src)?;
            changed = true;
        }

        if changed {
            write_atomic(file, src.as_bytes())?;
        }

        failures += findings.iter().filter(|f| f.level >= novafile_lint::Level::Warning).count();
        for f in &findings {
            if format == "json" {
                let (line, col) = novafile_text::spans::line_col_utf16(&src, f.start);
                let mut rec = Record::new();
                for (k, v) in [
                    ("file", Value::String(file.display().to_string())),
                    ("rule", Value::String(f.rule.into())),
                    (
                        "level",
                        Value::String(
                            match f.level {
                                novafile_lint::Level::Error => "error",
                                novafile_lint::Level::Warning => "warning",
                                novafile_lint::Level::Hint => "hint",
                            }
                            .into(),
                        ),
                    ),
                    ("message", Value::String(f.message.clone())),
                    ("line", Value::Int(i128::from(line) + 1)),
                    ("col", Value::Int(i128::from(col) + 1)),
                    ("fixable", Value::Bool(f.fix.is_some())),
                ] {
                    rec.set(k, v);
                }

                println!("{}", novafile_json::to_json_string(&Value::Record(rec), false).0);
            } else {
                let mut d = SourceDiagnostic::new("nova::lint", format!("[{}] {}", f.rule, f.message), &name_of(file), &src).with_label(
                    f.start,
                    f.end.saturating_sub(f.start),
                    if f.fix.is_some() { "fixable with --fix" } else { "here" },
                );
                d.severity = Some(match f.level {
                    novafile_lint::Level::Error => miette::Severity::Error,
                    novafile_lint::Level::Warning => miette::Severity::Warning,
                    novafile_lint::Level::Hint => miette::Severity::Advice,
                });
                eprintln!("{:?}", miette::Report::new(d));
            }
        }
    }

    Ok(if failures > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

/// Is the CLI driven by a coding agent?
fn agent_detected(mode: AgentMode) -> Option<String> {
    match mode {
        AgentMode::Off => None,
        AgentMode::On => Some("agent mode".into()),
        AgentMode::Auto => detect_coding_agent::detect().map(|a| a.name.to_string()),
    }
}

fn main() -> ExitCode {
    // Peek at the agent setting before clap prints help, so help can mention the guide.
    let mode = std::env::args()
        .skip_while(|a| a != "--agent")
        .nth(1)
        .or_else(|| std::env::var("NOVA_AGENT").ok())
        .and_then(|v| AgentMode::from_str(&v, true).ok())
        .unwrap_or(AgentMode::Auto);
    let agent = agent_detected(mode);
    let mut cmd = Cli::command();
    if let Some(name) = &agent {
        cmd = cmd.after_help(format!(
            "Detected {name}: run `nvf guide` for the agent workflow (read with `nvf get`, write with `nvf patch`)."
        ));
        // Plain, narratable diagnostics are easier for models to parse than box drawings.
        let _ = miette::set_hook(Box::new(|_| Box::new(miette::NarratableReportHandler::new())));
    }

    let cli = match Cli::from_arg_matches(&cmd.get_matches()) {
        Ok(c) => c,
        Err(e) => e.exit(),
    };

    match run(cli) {
        Ok(c) => c,
        Err(E(report)) => {
            eprintln!("{report:?}");
            ExitCode::from(2)
        }
    }
}

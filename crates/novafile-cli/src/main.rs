use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use novafile_binary::{decode_frame, encode_frame, Frame, FrameOptions, MAGIC};
use novafile_schema::{check_versions, parse_schema, Schema};
use novafile_text::diagnostic::SourceDiagnostic;
use novafile_text::tokens::{HeuristicCounter, TokenCounter};
use novafile_text::{document_to_text, parse_document, Profile};
use novafile_value::{revision, Record, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "nvf", version, about = "NOVA: one semantic model, several representations")]
struct Cli {
    /// Agent mode: plain, narratable errors and agent hints. `auto` detects coding
    /// agents (Claude Code, Codex, Copilot, Cursor, …); also settable via NOVA_AGENT.
    #[arg(long, value_enum, global = true, env = "NOVA_AGENT", default_value = "auto")]
    agent: AgentMode,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum AgentMode {
    Auto,
    On,
    Off,
}

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
   Syntax:    key: value | - item | [a, b] | {k: v} | (tuple,) | !variant | @timestamp("…") | 22/7
   Quote strings that contain ": " or " #", or that start with a reserved character.
"#;

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
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum InputFormat {
    /// Binary frames by magic; `.json`, `.jsonl`, `.yaml`/`.yml`, `.toml`, `.cbor`, `.pb` by extension; otherwise JSON if it parses, else NOVA text.
    Auto,
    Nova,
    Json,
    Jsonl,
    Binary,
    Yaml,
    Toml,
    Cbor,
    /// Protobuf wire format (needs --schema).
    Protobuf,
}

#[derive(Subcommand)]
enum Cmd {
    /// Reformat a NOVA text document.
    Fmt {
        file: PathBuf,
        #[arg(long, value_enum, default_value = "nova")]
        to: Format,
        /// Rewrite the file in place.
        #[arg(short, long)]
        write: bool,
    },
    /// Convert between NOVA text, JSON/JSONL and binary (input format is detected).
    Convert {
        file: PathBuf,
        #[arg(long, value_enum)]
        to: Format,
        /// Input format (detected by default).
        #[arg(long, value_enum, default_value = "auto")]
        from: InputFormat,
        /// Schema, required for Protobuf input/output.
        #[arg(long)]
        schema: Option<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Fail instead of warning when the conversion loses type information.
        #[arg(long)]
        strict: bool,
    },
    /// Print the document's revision (SHA-256 of the canonical encoding).
    Hash { file: PathBuf },
    /// Validate against a schema; with --resolve print the resolved document.
    Validate {
        file: PathBuf,
        #[arg(long)]
        schema: PathBuf,
        #[arg(long)]
        resolve: bool,
    },
    /// Answer a query such as `services[id=api].{replicas,port}`.
    ///
    /// Paths: `a.b`, `[0]`, `[-1]`, `[id=api]` (unique match), `[*]` (all),
    /// `[?replicas>3]` (filter; also = != < <= >=, chainable), `.{a,b}` (projection).
    /// Use `-` as FILE to read stdin; JSON and JSON Lines are detected.
    Get {
        file: PathBuf,
        path: String,
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
    },
    /// Apply a patch atomically.
    Patch {
        file: PathBuf,
        patch: PathBuf,
        #[arg(long)]
        schema: Option<PathBuf>,
        /// Rewrite the file in place.
        #[arg(short, long)]
        write: bool,
    },
    /// Print a conditional patch that turns A into B.
    Diff { a: PathBuf, b: PathBuf },
    /// Render a @template document.
    Render {
        file: PathBuf,
        /// Parameter as NAME=FLOW-VALUE (e.g. replicas=3, env=prod, tags=[a,b]).
        #[arg(long = "set", value_name = "NAME=VALUE")]
        set: Vec<String>,
        /// A NOVA record supplying parameters.
        #[arg(long)]
        values: Option<PathBuf>,
        #[arg(long)]
        schema: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "nova")]
        to: Format,
    },
    /// Compare sizes of every representation (bytes and estimated tokens).
    Stats { file: PathBuf },
    /// Print a short guide for AI coding agents (read projections, write conditional patches).
    Guide,
    /// Check documents (.nova) and schemas (.novs) against NOVA conventions; exits 1 on warnings.
    ///
    /// Rules: key-case (snake_case keys; fixable), key-collision, quoted-key, untyped-timestamp
    /// (fixable), mixed-list, compat-loss (with `@compat json|yaml|toml`), field-case,
    /// variant-case, partial-ids. Configure in a file with `@x-lint case=camel allow=rule,…`.
    Lint {
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
        #[command(subcommand)]
        cmd: SchemaCmd,
    },
}

#[derive(Subcommand)]
enum SchemaCmd {
    /// Print the canonical form of a schema.
    Fmt { file: PathBuf },
    /// Print `name#fingerprint`.
    Fingerprint { file: PathBuf },
    /// Classify changes between two schema versions and verify the SemVer bump; exits 1 if insufficient.
    Check { old: PathBuf, new: PathBuf },
    /// Print the proto3 `.proto` file for a schema (fields need `@id`).
    Proto { file: PathBuf },
    /// Print a schema as NOVA data (its descriptor, validated by `nvf schema meta`).
    ToNova { file: PathBuf },
    /// Convert a schema descriptor (NOVA data) back to the schema language.
    FromNova { file: PathBuf },
    /// Print the meta-schema `nova-schema@0.1.0`, which describes schema descriptors.
    Meta,
    /// Print a JSON Schema (2020-12) describing the JSON form of valid documents.
    JsonSchema { file: PathBuf },
    /// Convert a JSON Schema (2020-12 or draft-07) into a NOVA schema.
    FromJsonSchema {
        file: PathBuf,
        /// Schema name for `@schema name@version`.
        #[arg(long)]
        name: Option<String>,
        /// SemVer version (default 0.1.0).
        #[arg(long)]
        version: Option<String>,
    },
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

type R<T> = Result<T, E>;

fn name_of(p: &Path) -> String {
    p.display().to_string()
}

fn read_bytes(p: &Path) -> R<Vec<u8>> {
    if p.as_os_str() == "-" {
        let mut b = Vec::new();
        std::io::stdin().read_to_end(&mut b).map_err(|e| e.to_string())?;
        return Ok(b);
    }
    Ok(std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?)
}

fn read_text(p: &Path) -> R<String> {
    Ok(String::from_utf8(read_bytes(p)?).map_err(|_| format!("{}: not UTF-8", p.display()))?)
}

struct Loaded {
    directives: Vec<(String, String)>,
    value: Value,
}

fn load(p: &Path) -> R<Loaded> {
    load_as(p, InputFormat::Auto)
}

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

fn load_as(p: &Path, from: InputFormat) -> R<Loaded> {
    load_with(p, from, None)
}

const META_REF: &str = "nova-schema@0.1.0";

fn load_with(p: &Path, from: InputFormat, schema: Option<&Schema>) -> R<Loaded> {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext == "novs" && from == InputFormat::Auto {
        // Schema files load as their descriptor (spec §6.8), so every data tool works on them.
        return Ok(Loaded {
            directives: vec![("schema".into(), META_REF.into())],
            value: load_schema(p)?.to_value(),
        });
    }
    let bytes = read_bytes(p)?;
    let ctx = |e: String| format!("{}: {e}", p.display());
    let from = match from {
        InputFormat::Auto if bytes.starts_with(MAGIC) => InputFormat::Binary,
        InputFormat::Auto if ext == "jsonl" || ext == "ndjson" => InputFormat::Jsonl,
        InputFormat::Auto if ext == "json" => InputFormat::Json,
        InputFormat::Auto if ext == "yaml" || ext == "yml" => InputFormat::Yaml,
        InputFormat::Auto if ext == "toml" => InputFormat::Toml,
        InputFormat::Auto if ext == "cbor" => InputFormat::Cbor,
        InputFormat::Auto if ext == "pb" || ext == "binpb" => InputFormat::Protobuf,
        f => f,
    };
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
        InputFormat::Jsonl => return Ok(compat_loaded(Value::List(novafile_json::from_jsonl_str(&text).map_err(ctx)?), "json")),
        InputFormat::Json => return Ok(compat_loaded(novafile_json::from_json_str(&text).map_err(ctx)?, "json")),
        InputFormat::Yaml => return Ok(compat_loaded(novafile_formats::yaml::from_str(&text).map_err(ctx)?, "yaml")),
        InputFormat::Toml => return Ok(compat_loaded(novafile_formats::toml::from_str(&text).map_err(ctx)?, "toml")),
        InputFormat::Auto => {
            // Sniff JSON (e.g. on stdin): a whole JSON value, or JSON Lines.
            let t = text.trim_start();
            if t.starts_with('{') || t.starts_with('[') || t.starts_with('"') {
                if let Ok(v) = novafile_json::from_json_str(&text) {
                    return Ok(compat_loaded(v, "json"));
                }
                let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
                if lines.len() > 1 && lines.iter().all(|l| l.trim_start().starts_with('{') || l.trim_start().starts_with('[')) {
                    if let Ok(v) = novafile_json::from_jsonl_str(&text) {
                        return Ok(compat_loaded(Value::List(v), "json"));
                    }
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

/// Is this file NOVA text (rather than JSON, JSON Lines or binary)?
fn is_nova_text(p: &Path, text: &str) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    !matches!(ext, "json" | "jsonl" | "ndjson") && !text.as_bytes().starts_with(MAGIC) && novafile_text::parse(text).is_ok()
}

fn load_schema(p: &Path) -> R<Schema> {
    let src = read_text(p)?;
    // A schema descriptor (NOVA data, spec §6.8) is accepted wherever a schema is.
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext != "novs" {
        if let Ok(doc) = novafile_text::parse_document(&src) {
            if doc.directive("schema").is_some_and(|r| r.starts_with("nova-schema@")) {
                let v = doc.body.to_value().map_err(|e| format!("{}: {e}", p.display()))?;
                return Ok(Schema::from_value(&v).map_err(|e| format!("{}: {e}", p.display()))?);
            }
        }
    }
    parse_schema(&src).map_err(|e| {
        if e.line == 0 {
            E::from(format!("{}: {}", p.display(), e.message))
        } else {
            E::from(SourceDiagnostic::new("nova::schema", e.message.clone(), &name_of(p), &src).with_line_col(e.line, 1, "in this definition"))
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

fn render_as(l: &Loaded, f: Format, strict: bool) -> R<Vec<u8>> {
    render_with(l, f, strict, None)
}

fn render_with(l: &Loaded, f: Format, strict: bool, schema: Option<&Schema>) -> R<Vec<u8>> {
    let d: Vec<(&str, &str)> = l.directives.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let text = |p| document_to_text(&d, &l.value, p).into_bytes();
    Ok(match f {
        Format::Nova => text(Profile::Human),
        Format::Compact => text(Profile::Compact),
        Format::Canonical => text(Profile::Canonical),
        Format::Json | Format::JsonCompact => {
            let (s, warnings) = novafile_json::to_json_string(&l.value, matches!(f, Format::Json));
            if !warnings.is_empty() {
                let msg = warnings.iter().map(|w| format!("  {w}")).collect::<Vec<_>>().join("\n");
                if strict {
                    return Err(format!("conversion to JSON is lossy:\n{msg}").into());
                }
                eprintln!("warning: conversion to JSON is lossy:\n{msg}");
            }
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
        Format::Binary | Format::BinaryCanonical => {
            let canonical = matches!(f, Format::BinaryCanonical);
            let schema = l.directives.iter().find(|(n, _)| n == "schema").map(|(_, s)| s.clone());
            encode_frame(
                &Frame {
                    schema,
                    value: l.value.clone(),
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

fn out(bytes: &[u8], to: Option<&Path>) -> R<()> {
    match to {
        Some(p) => Ok(std::fs::write(p, bytes).map_err(|e| format!("{}: {e}", p.display()))?),
        None => Ok(std::io::stdout().write_all(bytes).map_err(|e| e.to_string())?),
    }
}

fn issues(i: Vec<novafile_schema::Issue>) -> String {
    i.iter().map(|i| format!("  {i}")).collect::<Vec<_>>().join("\n")
}

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

fn run(cli: Cli) -> R<ExitCode> {
    match cli.cmd {
        Cmd::Guide => print!("{AGENT_GUIDE}"),
        Cmd::Lint {
            files,
            case,
            allow,
            fix,
            format,
        } => return lint(files, case, allow, fix, &format),
        Cmd::Fmt { file, to, write } => {
            // NOVA text keeps its comments, blank lines and spellings (spec §3).
            let profile = match to {
                Format::Nova => Some(Profile::Human),
                Format::Canonical => Some(Profile::Canonical),
                Format::Compact => Some(Profile::Compact),
                _ => None,
            };
            let text = read_text(&file)
                .ok()
                .filter(|t| is_nova_text(&file, t) && !file.extension().is_some_and(|e| e == "novs"));
            if file.extension().is_some_and(|e| e == "novs") && matches!(to, Format::Nova | Format::Canonical) {
                out(load_schema(&file)?.to_canonical_text().as_bytes(), write.then_some(file.as_path()))?;
                return Ok(ExitCode::SUCCESS);
            }
            let bytes = match (profile, text) {
                (Some(p), Some(t)) => novafile_text::format_preserving(&t, p)
                    .map_err(|e| e.to_diagnostic(&name_of(&file), &t))?
                    .into_bytes(),
                _ => render_as(&load(&file)?, to, false)?,
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
        } => {
            let s = schema.as_deref().map(load_schema).transpose()?;
            let l = load_with(&file, from, s.as_ref())?;
            out(&render_with(&l, to, strict, s.as_ref())?, output.as_deref())?;
        }
        Cmd::Hash { file } => println!("{}", revision(&load(&file)?.value)),
        Cmd::Validate { file, schema, resolve } => {
            let s = load_schema(&schema)?;
            let l = load(&file)?;
            if let Some((_, r)) = l.directives.iter().find(|(n, _)| n == "schema") {
                if s.reference().is_some() {
                    s.accepts_reference(r).map_err(|e| format!("{}: {e}", file.display()))?;
                }
            }
            match s.resolve(&l.value) {
                Ok(v) => {
                    if resolve {
                        let resolved = Loaded {
                            directives: vec![("schema".into(), s.fingerprint())],
                            value: v,
                        };
                        out(&render_as(&resolved, Format::Nova, false)?, None)?;
                    } else {
                        eprintln!("ok: valid against {}", s.fingerprint());
                    }
                }
                Err(i) => {
                    // Point at each offending value when the input is NOVA text.
                    let src = read_text(&file).ok();
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
                    return Err(format!("{} is invalid:\n{}", file.display(), issues(i)).into());
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
        } => {
            let l = load_as(&file, from)?;
            let r = novafile_patch::get(&l.value, &path)?;
            let multi = !novafile_text::Path::parse(path.trim().trim_start_matches("@get").trim_start_matches("@select"))
                .map(|p| p.is_singular())
                .unwrap_or(true);
            let json = matches!(to, Format::Json | Format::JsonCompact);
            let one_line = |v: &Value| -> String {
                match v {
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
            let l = load(&file)?;
            let patch_src = read_text(&patch)?;
            let p = novafile_patch::parse_patch(&patch_src).map_err(|e| patch_error(&e, &patch, &patch_src))?;
            let s = schema.as_deref().map(load_schema).transpose()?;
            if let (Some(s), Some(r)) = (&s, &p.schema) {
                if s.reference().is_some() {
                    s.accepts_reference(r).map_err(|e| format!("{}: {e}", patch.display()))?;
                }
            }
            let validate = |v: &Value| match &s {
                Some(s) => s.validate(v).map_err(|i| i.iter().map(|i| i.to_string()).collect()),
                None => Ok(()),
            };
            let text = read_text(&file).ok().filter(|t| is_nova_text(&file, t));
            let (bytes, applied) = match text {
                // Document-preserving: minimal edits to the source text.
                Some(src) => {
                    let r = novafile_patch::apply_to_source(&src, &p, &validate).map_err(|e| patch_error(&e, &patch, &patch_src))?;
                    (r.text.into_bytes(), r.applied)
                }
                None => {
                    let is_schema = file.extension().is_some_and(|e| e == "novs");
                    let meta = novafile_schema::meta_schema();
                    let meta_check = |v: &Value| meta.validate(v).map_err(|i| i.iter().map(|i| i.to_string()).collect());
                    let check: &dyn Fn(&Value) -> Result<(), Vec<String>> = if is_schema { &meta_check } else { &validate };
                    let applied = novafile_patch::apply(&l.value, &p, check).map_err(|e| patch_error(&e, &patch, &patch_src))?;
                    if is_schema {
                        // Patched schema descriptors are written back in the schema language.
                        let s = Schema::from_value(&applied.value)?;
                        eprintln!("note: {} is rewritten in canonical form (comments are not kept)", file.display());
                        (s.to_canonical_text().into_bytes(), applied)
                    } else {
                        let result = Loaded {
                            directives: l.directives,
                            value: applied.value.clone(),
                        };
                        (render_as(&result, Format::Nova, false)?, applied)
                    }
                }
            };
            out(&bytes, write.then_some(file.as_path()))?;
            eprintln!("{} -> {}", applied.base, applied.revision);
        }
        Cmd::Diff { a, b } => print!("{}", novafile_patch::diff(&load(&a)?.value, &load(&b)?.value)),
        Cmd::Render { file, set, values, schema, to } => {
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
            let loader = novafile_template::FsLoader {
                root: file.parent().map(Path::to_path_buf).unwrap_or_default(),
            };
            let opts = novafile_template::Options {
                loader: &loader,
                schema: s.as_ref(),
                limits: Default::default(),
            };
            let v = novafile_template::render(&src, &args, &opts).map_err(|e| format!("{}: {e}", file.display()))?;
            if let Some(s) = &s {
                if s.root.is_some() {
                    s.validate(&v).map_err(|i| format!("rendered document is invalid:\n{}", issues(i)))?;
                }
            }
            out(&render_as(&Loaded { directives: vec![], value: v }, to, false)?, None)?;
        }
        Cmd::Stats { file } => {
            let l = load(&file)?;
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
                    .map(|t| c.count(t).to_string())
                    .unwrap_or_else(|| "-".into());
                println!("{name:<18} {:>9} {tokens:>9}", bytes.len());
            }
            eprintln!("(token counts are a tokenizer-independent heuristic; see spec §10)");
        }
        Cmd::Schema { cmd } => match cmd {
            SchemaCmd::Fmt { file } => print!("{}", load_schema(&file)?.to_canonical_text()),
            SchemaCmd::Fingerprint { file } => println!("{}", load_schema(&file)?.fingerprint()),
            SchemaCmd::Proto { file } => print!("{}", novafile_proto::proto_file(&load_schema(&file)?)?),
            SchemaCmd::ToNova { file } => {
                let v = load_schema(&file)?.to_value();
                print!("{}", document_to_text(&[("schema", META_REF)], &v, Profile::Human));
            }
            SchemaCmd::FromNova { file } => print!("{}", load_schema(&file)?.to_canonical_text()),
            SchemaCmd::Meta => print!("{}", novafile_schema::META_SCHEMA_SRC),
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
                let r = check_versions(&load_schema(&old)?, &load_schema(&new)?)?;
                for c in &r.changes {
                    println!("{c}");
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

fn lint(files: Vec<PathBuf>, case: Option<String>, allow: Vec<String>, fix: bool, format: &str) -> R<ExitCode> {
    let mut config = novafile_lint::Config::default();
    if let Some(c) = case {
        config.case = novafile_lint::Case::parse(&c).ok_or_else(|| format!("unknown case {c:?} (snake, camel, kebab)"))?;
    }
    config.allow = allow;
    let mut warnings = 0;
    for file in &files {
        let mut src = read_text(file)?;
        let is_schema = file.extension().is_some_and(|e| e == "novs");
        let run = |src: &str| -> R<Vec<novafile_lint::Finding>> {
            if is_schema {
                Ok(novafile_lint::lint_schema(src, &config).map_err(|e| format!("{}: {e}", file.display()))?)
            } else {
                Ok(novafile_lint::lint_document(src, &config).map_err(|e| e.to_diagnostic(&name_of(file), src))?)
            }
        };
        let mut findings = run(&src)?;
        if fix && findings.iter().any(|f| f.fix.is_some()) {
            let fixed = novafile_lint::apply_fixes(&src, &findings);
            let n = findings.iter().filter(|f| f.fix.is_some()).count();
            std::fs::write(file, &fixed).map_err(|e| format!("{}: {e}", file.display()))?;
            eprintln!("{}: applied {n} fix{}", file.display(), if n == 1 { "" } else { "es" });
            src = fixed;
            findings = run(&src)?;
        }
        warnings += findings.iter().filter(|f| f.level == novafile_lint::Level::Warning).count();
        for f in &findings {
            if format == "json" {
                let (line, col) = novafile_text::spans::line_col_utf16(&src, f.start);
                let mut rec = Record::new();
                for (k, v) in [
                    ("file", Value::String(file.display().to_string())),
                    ("rule", Value::String(f.rule.into())),
                    (
                        "level",
                        Value::String(if f.level == novafile_lint::Level::Warning { "warning" } else { "hint" }.into()),
                    ),
                    ("message", Value::String(f.message.clone())),
                    ("line", Value::Int(line as i128 + 1)),
                    ("col", Value::Int(col as i128 + 1)),
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
                d.severity = Some(if f.level == novafile_lint::Level::Warning {
                    miette::Severity::Warning
                } else {
                    miette::Severity::Advice
                });
                eprintln!("{:?}", miette::Report::new(d));
            }
        }
    }
    Ok(if warnings > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS })
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

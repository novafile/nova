use clap::{Parser, Subcommand, ValueEnum};
use nova_binary::{decode_frame, encode_frame, Frame, FrameOptions, MAGIC};
use nova_schema::{check_versions, parse_schema, Schema};
use nova_text::tokens::{HeuristicCounter, TokenCounter};
use nova_text::{document_to_text, parse_document, Profile};
use nova_value::{revision, Record, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "nova", version, about = "NOVA: one semantic model, several representations")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

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
    Get {
        file: PathBuf,
        path: String,
        #[arg(long, value_enum, default_value = "compact")]
        to: Format,
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
}

type R<T> = Result<T, String>;

fn read_bytes(p: &Path) -> R<Vec<u8>> {
    if p.as_os_str() == "-" {
        let mut b = Vec::new();
        std::io::stdin().read_to_end(&mut b).map_err(|e| e.to_string())?;
        return Ok(b);
    }
    std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))
}

fn read_text(p: &Path) -> R<String> {
    String::from_utf8(read_bytes(p)?).map_err(|_| format!("{}: not UTF-8", p.display()))
}

struct Loaded {
    directives: Vec<(String, String)>,
    value: Value,
}

fn load(p: &Path) -> R<Loaded> {
    let bytes = read_bytes(p)?;
    let ctx = |e: String| format!("{}: {e}", p.display());
    if bytes.starts_with(MAGIC) {
        let (f, n) = decode_frame(&bytes).map_err(|e| ctx(e.to_string()))?;
        if n != bytes.len() {
            return Err(ctx("trailing bytes after the first frame".into()));
        }
        let directives = f.schema.map(|s| vec![("schema".to_owned(), s)]).unwrap_or_default();
        return Ok(Loaded { directives, value: f.value });
    }
    let text = String::from_utf8(bytes).map_err(|_| ctx("not UTF-8".into()))?;
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext == "jsonl" || ext == "ndjson" {
        return Ok(Loaded {
            directives: vec![],
            value: Value::List(nova_json::from_jsonl_str(&text).map_err(ctx)?),
        });
    }
    if ext == "json" {
        return Ok(Loaded {
            directives: vec![],
            value: nova_json::from_json_str(&text).map_err(ctx)?,
        });
    }
    let doc = parse_document(&text).map_err(|e| ctx(e.to_string()))?;
    if doc.is_template() {
        return Err(ctx("this is a @template; use `nova render`".into()));
    }
    let value = doc.body.to_value().map_err(ctx)?;
    Ok(Loaded {
        directives: doc.directives.into_iter().map(|d| (d.name, d.arg)).collect(),
        value,
    })
}

fn load_schema(p: &Path) -> R<Schema> {
    parse_schema(&read_text(p)?).map_err(|e| format!("{}: {e}", p.display()))
}

fn render_as(l: &Loaded, f: Format, strict: bool) -> R<Vec<u8>> {
    let d: Vec<(&str, &str)> = l.directives.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let text = |p| document_to_text(&d, &l.value, p).into_bytes();
    Ok(match f {
        Format::Nova => text(Profile::Human),
        Format::Compact => text(Profile::Compact),
        Format::Canonical => text(Profile::Canonical),
        Format::Json | Format::JsonCompact => {
            let (s, warnings) = nova_json::to_json_string(&l.value, matches!(f, Format::Json));
            if !warnings.is_empty() {
                let msg = warnings.iter().map(|w| format!("  {w}")).collect::<Vec<_>>().join("\n");
                if strict {
                    return Err(format!("conversion to JSON is lossy:\n{msg}"));
                }
                eprintln!("warning: conversion to JSON is lossy:\n{msg}");
            }
            (s + "\n").into_bytes()
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
                    checksum: true,
                },
            )
        }
    })
}

fn out(bytes: &[u8], to: Option<&Path>) -> R<()> {
    match to {
        Some(p) => std::fs::write(p, bytes).map_err(|e| format!("{}: {e}", p.display())),
        None => std::io::stdout().write_all(bytes).map_err(|e| e.to_string()),
    }
}

fn issues(i: Vec<nova_schema::Issue>) -> String {
    i.iter().map(|i| format!("  {i}")).collect::<Vec<_>>().join("\n")
}

fn run(cli: Cli) -> R<ExitCode> {
    match cli.cmd {
        Cmd::Fmt { file, to, write } => {
            let l = load(&file)?;
            let bytes = render_as(&l, to, false)?;
            out(&bytes, write.then_some(file.as_path()))?;
        }
        Cmd::Convert { file, to, output, strict } => {
            let l = load(&file)?;
            out(&render_as(&l, to, strict)?, output.as_deref())?;
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
                Err(i) => return Err(format!("{} is invalid:\n{}", file.display(), issues(i))),
            }
        }
        Cmd::Get { file, path, to } => {
            let l = load(&file)?;
            let r = nova_patch::get(&l.value, &path)?;
            let p = match to {
                Format::Nova => Profile::Human,
                Format::Canonical => Profile::Canonical,
                _ => Profile::Compact,
            };
            print!("{}", r.to_text(p));
        }
        Cmd::Patch { file, patch, schema, write } => {
            let l = load(&file)?;
            let p = nova_patch::parse_patch(&read_text(&patch)?).map_err(|e| format!("{}: {e}", patch.display()))?;
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
            let applied = nova_patch::apply(&l.value, &p, &validate).map_err(|e| e.to_string())?;
            let result = Loaded {
                directives: l.directives,
                value: applied.value,
            };
            out(&render_as(&result, Format::Nova, false)?, write.then_some(file.as_path()))?;
            eprintln!("{} -> {}", applied.base, applied.revision);
        }
        Cmd::Diff { a, b } => print!("{}", nova_patch::diff(&load(&a)?.value, &load(&b)?.value)),
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
                let v = nova_text::parse_flow(v).map_err(|e| format!("--set {k}: {e}"))?;
                args.set(k, v);
            }
            let s = schema.as_deref().map(load_schema).transpose()?;
            let loader = nova_template::FsLoader {
                root: file.parent().map(Path::to_path_buf).unwrap_or_default(),
            };
            let opts = nova_template::Options {
                loader: &loader,
                schema: s.as_ref(),
                limits: Default::default(),
            };
            let v = nova_template::render(&src, &args, &opts).map_err(|e| format!("{}: {e}", file.display()))?;
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
                ("binary", Format::Binary),
                ("binary canonical", Format::BinaryCanonical),
            ] {
                let plain = Loaded {
                    directives: vec![],
                    value: l.value.clone(),
                };
                let bytes = match f {
                    Format::Json | Format::JsonCompact => nova_json::to_json_string(&plain.value, matches!(f, Format::Json)).0.into_bytes(),
                    _ => render_as(&plain, f, false)?,
                };
                let tokens = std::str::from_utf8(&bytes)
                    .ok()
                    .filter(|_| !matches!(f, Format::Binary | Format::BinaryCanonical))
                    .map(|t| c.count(t).to_string())
                    .unwrap_or_else(|| "-".into());
                println!("{name:<18} {:>9} {tokens:>9}", bytes.len());
            }
            eprintln!("(token counts are a tokenizer-independent heuristic; see spec §10)");
        }
        Cmd::Schema { cmd } => match cmd {
            SchemaCmd::Fmt { file } => print!("{}", load_schema(&file)?.to_canonical_text()),
            SchemaCmd::Fingerprint { file } => println!("{}", load_schema(&file)?.fingerprint()),
            SchemaCmd::Check { old, new } => {
                let r = check_versions(&load_schema(&old)?, &load_schema(&new)?)?;
                for c in &r.changes {
                    println!("{c}");
                }
                let kind = match r.kind {
                    nova_schema::ChangeKind::None => "no semantic changes",
                    nova_schema::ChangeKind::Compatible => "backward-compatible changes",
                    nova_schema::ChangeKind::Breaking => "breaking changes",
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

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

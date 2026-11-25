//! The NOVA conformance suite: language-neutral inputs with JSON expectations
//! (see `conformance/README.md` in <https://github.com/novafile/spec>).
//!
//! [`expectations`] computes, with this reference implementation, the
//! expected result of every case under a conformance directory. The `nova
//! -conformance` binary writes them (`generate`); the test checks that the
//! files on disk still match (`cargo test -p novafile-conformance`).

use novafile_text::lines::{Positional, Segment, Status};
use novafile_text::{Profile, to_text};
use novafile_value::{Record, Value, canonical_bytes, revision};
use serde_json::{Value as J, json};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Lowercase hex encoding of `b`.
fn hex(b: &[u8]) -> String {
    b.iter().fold(String::with_capacity(b.len() * 2), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

/// Expectation for a successfully produced value.
fn value_expect(v: &Value) -> J {
    json!({
        "canonical": to_text(v, Profile::Canonical),
        "revision": revision(v).to_string(),
        "binary": hex(&canonical_bytes(v)),
    })
}

/// Expectation for a record stream: the records as one list, the skipped lines or frames
/// (`at`: line number or byte offset), and each segment's count and status.
fn stream_expect(records: Vec<Value>, skipped: Vec<(usize, String)>, segments: &[Segment]) -> J {
    let mut e = value_expect(&Value::List(records));
    e["skipped"] = skipped.into_iter().map(|(at, m)| json!({"at": at, "message": m})).collect();
    e["segments"] = segments
        .iter()
        .map(|s| match &s.status {
            Status::Complete => json!({"count": s.count, "status": "complete"}),
            Status::Open => json!({"count": s.count, "status": "open"}),
            Status::Corrupt(m) => json!({"count": s.count, "status": "corrupt", "message": m}),
        })
        .collect();
    e
}

/// The schema beside a stream input (`case.novl` uses `case.novs`), if there is one.
fn sibling_schema(input: &Path) -> Option<novafile_schema::Schema> {
    let p = input.with_extension("novs");
    p.is_file()
        .then(|| novafile_schema::parse_schema(&read(&p)).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
}

/// Entries of `dir` that pass `keep`, sorted; empty if `dir` is missing.
fn entries(dir: &Path, keep: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => panic!("{}: {e}", dir.display()),
    };

    let mut v: Vec<PathBuf> = rd
        .map(|e| e.unwrap_or_else(|e| panic!("{}: {e}", dir.display())).path())
        .filter(|p| keep(p))
        .collect();
    v.sort();
    v
}

/// Files in `dir` with extension `ext`, sorted; empty if `dir` is missing.
fn files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    entries(dir, |p| p.extension().is_some_and(|e| e == ext))
}

/// Subdirectories of `dir`, sorted; empty if `dir` is missing.
fn dirs(dir: &Path) -> Vec<PathBuf> {
    entries(dir, Path::is_dir)
}

/// Reads a file as text, panicking with its path if that fails.
fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Expectation file for an input: `case.nova` maps to `case.expect.json` beside it.
fn expect_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().unwrap().to_string_lossy();
    input.with_file_name(format!("{stem}.expect.json"))
}

/// A resolution of a value against a schema: [`novafile_schema::Schema::resolve`] or its partial form.
type ResolveFn = fn(&novafile_schema::Schema, &Value) -> Result<Value, Vec<novafile_schema::Issue>>;

/// Expectations for the documents in `{prefix}valid/` and `{prefix}invalid/` of the
/// schema case `d`, resolved with `resolve`: the resolved value, or the issue paths
/// and messages.
///
/// # Panics
///
/// Panics if a document does not parse, a valid one does not resolve, or an
/// invalid one does.
fn resolutions(d: &Path, prefix: &str, schema: &novafile_schema::Schema, resolve: ResolveFn, out: &mut Vec<(PathBuf, J)>) {
    for f in files(&d.join(format!("{prefix}valid")), "nova") {
        let v = novafile_text::parse(&read(&f)).unwrap();
        let r = resolve(schema, &v).unwrap_or_else(|e| panic!("{}: {e:?}", f.display()));
        out.push((expect_path(&f), value_expect(&r)));
    }

    for f in files(&d.join(format!("{prefix}invalid")), "nova") {
        let v = novafile_text::parse(&read(&f)).unwrap();
        let issues = resolve(schema, &v).err().unwrap_or_else(|| panic!("{} should not resolve", f.display()));
        let paths: Vec<String> = issues.iter().map(|i| i.path.to_string()).collect();
        let msgs: Vec<String> = issues.iter().map(|i| i.message.clone()).collect();
        out.push((expect_path(&f), json!({"error": true, "issue_paths": paths, "messages": msgs})));
    }
}

/// Every `(expectation file, expected JSON)` pair under `root`.
///
/// # Panics
///
/// Panics if `root` is not a directory or holds no cases, if a case's input
/// files cannot be read, if a case contradicts
/// its directory (a `valid` input fails, an `invalid` one succeeds), or if
/// auxiliary inputs (schemas, base documents, template args) do not parse.
#[must_use]
#[expect(clippy::too_many_lines, reason = "one flat section per conformance category")]
pub fn expectations(root: &Path) -> Vec<(PathBuf, J)> {
    assert!(root.is_dir(), "{}: not a conformance directory", root.display());
    let mut out = Vec::new();

    // text: parse; valid inputs give canonical text, revision and canonical binary.
    for f in files(&root.join("text/valid"), "nova") {
        let v = novafile_text::parse(&read(&f)).unwrap_or_else(|e| panic!("{} should be valid: {e}", f.display()));
        out.push((expect_path(&f), value_expect(&v)));
    }

    for f in files(&root.join("text/invalid"), "nova") {
        let e = novafile_text::parse(&read(&f))
            .err()
            .unwrap_or_else(|| panic!("{} should be invalid", f.display()));
        out.push((expect_path(&f), json!({"error": true, "line": e.line, "col": e.col, "message": e.message})));
    }

    // binary: decode.
    for f in files(&root.join("binary/valid"), "bin") {
        let v = novafile_value::decode(&std::fs::read(&f).unwrap()).unwrap_or_else(|e| panic!("{} should decode: {e}", f.display()));
        out.push((expect_path(&f), value_expect(&v)));
    }

    for f in files(&root.join("binary/invalid"), "bin") {
        let e = novafile_value::decode(&std::fs::read(&f).unwrap())
            .err()
            .unwrap_or_else(|| panic!("{} should be rejected", f.display()));
        out.push((expect_path(&f), json!({"error": true, "message": e.to_string()})));
    }

    // NOVA Lines: read in recovering mode, with `NAME.novs` for positional streams.
    for f in files(&root.join("lines"), "novl") {
        let schema = sibling_schema(&f);
        let r = novafile_text::lines::read_str(&read(&f), schema.as_ref().map(|s| s as &dyn Positional));
        let skipped = r.errors.into_iter().map(|e| (e.line, e.message)).collect();
        out.push((expect_path(&f), stream_expect(r.records, skipped, &r.segments)));
    }

    // binary record streams: frames with META header and end frames, read recovering.
    for f in files(&root.join("streams"), "bin") {
        let schema = sibling_schema(&f);
        let r = novafile_binary::stream::read_stream(&std::fs::read(&f).unwrap(), true, schema.as_ref().map(|s| s as &dyn Positional));
        out.push((expect_path(&f), stream_expect(r.records, r.errors, &r.segments)));
    }

    // schema: resolution of valid documents, issue paths of invalid ones.
    for d in dirs(&root.join("schema")) {
        let schema = novafile_schema::parse_schema(&read(&d.join("schema.novs"))).unwrap_or_else(|e| panic!("{}: {e}", d.display()));
        resolutions(&d, "", &schema, novafile_schema::Schema::resolve, &mut out);
        // Layers of a layered configuration (proposal 0010).
        resolutions(&d, "partial-", &schema, novafile_schema::Schema::resolve_partial, &mut out);
    }

    // invalid schemas: parsing must fail.
    for f in files(&root.join("schema-invalid"), "novs") {
        let e = novafile_schema::parse_schema(&read(&f))
            .err()
            .unwrap_or_else(|| panic!("{} should be rejected", f.display()));
        out.push((expect_path(&f), json!({"error": true, "message": e.message})));
    }

    // schema names: classify each (§6.7).
    let names = root.join("schema-names/names.json");
    if names.is_file() {
        let list: Vec<String> = serde_json::from_str(&read(&names)).unwrap_or_else(|e| panic!("{}: {e}", names.display()));
        let classes: serde_json::Map<String, J> = list
            .into_iter()
            .map(|n| {
                let class = match novafile_schema::schema_name_kind(&n) {
                    Some(novafile_schema::NameKind::Qualified) => "qualified",
                    Some(novafile_schema::NameKind::Local) => "local",
                    None => "invalid",
                };
                (n, J::from(class))
            })
            .collect();
        out.push((root.join("schema-names/expect.json"), J::Object(classes)));
    }

    // schema lookup: resolve the document's @schema through its catalogs (§6.9), never fetching.
    for d in dirs(&root.join("schema-lookup")) {
        let doc = [d.join("doc.nova"), d.join("sub/doc.nova")]
            .into_iter()
            .find(|p| p.is_file())
            .unwrap_or_else(|| panic!("{}: no doc.nova", d.display()));
        let reference = novafile_text::parse_document(&read(&doc))
            .ok()
            .and_then(|x| x.directive("schema").map(str::to_owned))
            .unwrap_or_else(|| panic!("{}: no @schema", doc.display()));
        let opts = novafile_schema::catalog::Options {
            start: doc.parent().map(Path::to_path_buf),
            ..Default::default()
        };
        let result = novafile_schema::catalog::Resolver::new(&opts)
            .map_err(novafile_schema::catalog::LookupError::Invalid)
            .and_then(|r| r.resolve(&reference));
        let j = match result {
            Ok(r) => {
                let resolved = match &r.origin {
                    novafile_schema::catalog::Origin::File(p) => p.strip_prefix(&d).unwrap_or(p).display().to_string(),
                    other => format!(
                        "<{}>",
                        if matches!(other, novafile_schema::catalog::Origin::Builtin) {
                            "builtin"
                        } else {
                            "cache"
                        }
                    ),
                };
                let mut j = json!({"resolved": resolved});
                if !r.warnings.is_empty() {
                    j["warnings"] = json!(r.warnings);
                }

                j
            }
            // Paths in messages relative to the case, so expectations are portable.
            Err(e) => json!({"error": e.kind(), "message": e.to_string().replace(&format!("{}/", d.display()), "")}),
        };
        out.push((d.join("expect.json"), j));
    }

    // schema versions: change classification and bump check (§6.7).
    for d in dirs(&root.join("schema-versions")) {
        let old = novafile_schema::parse_schema(&read(&d.join("old.novs"))).unwrap();
        let new = novafile_schema::parse_schema(&read(&d.join("new.novs"))).unwrap();
        let r = novafile_schema::check_versions(&old, &new).unwrap();
        let kind = match r.kind {
            novafile_schema::ChangeKind::None => "none",
            novafile_schema::ChangeKind::Compatible => "compatible",
            novafile_schema::ChangeKind::Breaking => "breaking",
        };

        out.push((
            d.join("expect.json"),
            json!({"kind": kind, "minimum": r.minimum.to_string(), "accepted": r.problems.is_empty()}),
        ));
    }

    // queries: one result per line of queries.txt.
    for d in dirs(&root.join("query")) {
        let doc = novafile_text::parse(&read(&d.join("doc.nova"))).unwrap();
        let mut results = Vec::new();
        for q in read(&d.join("queries.txt")).lines().filter(|l| !l.trim().is_empty()) {
            let r = match novafile_text::Path::parse(q) {
                Err(e) => json!({"query": q, "error": "invalid-path", "message": e}),
                Ok(p) => match novafile_patch::select(&doc, &p) {
                    Ok(v) => {
                        let mut e = value_expect(&v);
                        e["query"] = json!(q);
                        e
                    }
                    Err(e) => {
                        let kind = match &e {
                            novafile_text::PathError::NotFound(_) => "not-found",
                            novafile_text::PathError::Ambiguous(..) => "ambiguous",
                            novafile_text::PathError::Type { .. } => "type",
                            novafile_text::PathError::NotSingular(_) => "not-singular",
                            novafile_text::PathError::Classified(_) => "classified",
                        };

                        json!({"query": q, "error": kind, "message": e.to_string()})
                    }
                },
            };

            results.push(r);
        }

        out.push((d.join("expect.json"), J::Array(results)));
    }

    // patches.
    for d in dirs(&root.join("patch")) {
        let doc = novafile_text::parse(&read(&d.join("doc.nova"))).unwrap();
        let e = match novafile_patch::parse_patch(&read(&d.join("op.patch"))) {
            Err(e) => json!({"error": "syntax", "message": e.to_string()}),
            Ok(p) => match novafile_patch::apply_unchecked(&doc, &p) {
                Ok(a) => value_expect(&a.value),
                Err(e) => {
                    let kind = match &e {
                        novafile_patch::PatchError::Syntax { .. } => "syntax",
                        novafile_patch::PatchError::StaleBase { .. } => "stale-base",
                        novafile_patch::PatchError::Scope(_) => "scope",
                        novafile_patch::PatchError::Op { .. } => "op-failed",
                        novafile_patch::PatchError::Invalid(_) => "invalid",
                    };

                    let mut j = json!({"error": kind, "message": e.to_string()});
                    if let novafile_patch::PatchError::Op { index, .. } = e {
                        j["op"] = json!(index + 1);
                    }

                    j
                }
            },
        };

        out.push((d.join("expect.json"), e));
    }

    // templates.
    for d in dirs(&root.join("template")) {
        let args = match novafile_text::parse(&read(&d.join("args.nova"))).unwrap() {
            Value::Record(r) => r,
            _ => Record::new(),
        };

        let e = match novafile_template::render(&read(&d.join("template.nova")), &args, &novafile_template::Options::default()) {
            Ok(v) => value_expect(&v),
            Err(e) => json!({"error": true, "message": e.to_string()}),
        };

        out.push((d.join("expect.json"), e));
    }

    assert!(!out.is_empty(), "{}: no conformance cases found", root.display());
    out
}

/// Format expectations exactly as written to disk.
///
/// # Panics
///
/// Never in practice: serializing a `serde_json::Value` cannot fail.
#[must_use]
pub fn render(j: &J) -> String {
    serde_json::to_string_pretty(j).unwrap() + "\n"
}

/// Locate the conformance directory: `$NOVA_CONFORMANCE`, else a sibling checkout of the spec repo.
#[must_use]
pub fn locate() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("NOVA_CONFORMANCE") {
        return Some(PathBuf::from(p));
    }

    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    [here.join("../../spec/conformance"), here.join("../../../spec/conformance")]
        .into_iter()
        .find(|p| p.is_dir())
}

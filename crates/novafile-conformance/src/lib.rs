//! The NOVA conformance suite: language-neutral inputs with JSON expectations
//! (see `conformance/README.md` in <https://github.com/novafile/spec>).
//!
//! [`expectations`] computes, with this reference implementation, the
//! expected result of every case under a conformance directory. The `nova
//! -conformance` binary writes them (`generate`); the test checks that the
//! files on disk still match (`cargo test -p novafile-conformance`).

use novafile_text::{Profile, to_text};
use novafile_value::{Record, Value, canonical_bytes, revision};
use serde_json::{Value as J, json};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

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

fn files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == ext)).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn dirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn expect_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().unwrap().to_string_lossy();
    input.with_file_name(format!("{stem}.expect.json"))
}

/// Every `(expectation file, expected JSON)` pair under `root`.
///
/// # Panics
///
/// Panics if a case's input files cannot be read, if a case contradicts
/// its directory (a `valid` input fails, an `invalid` one succeeds), or if
/// auxiliary inputs (schemas, base documents, template args) do not parse.
#[must_use]
#[expect(clippy::too_many_lines, reason = "one flat section per conformance category")]
pub fn expectations(root: &Path) -> Vec<(PathBuf, J)> {
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

    // schema: resolution of valid documents, issue paths of invalid ones.
    for d in dirs(&root.join("schema")) {
        let schema = novafile_schema::parse_schema(&read(&d.join("schema.novs"))).unwrap_or_else(|e| panic!("{}: {e}", d.display()));
        for f in files(&d.join("valid"), "nova") {
            let v = novafile_text::parse(&read(&f)).unwrap();
            let r = schema.resolve(&v).unwrap_or_else(|e| panic!("{}: {e:?}", f.display()));
            out.push((expect_path(&f), value_expect(&r)));
        }

        for f in files(&d.join("invalid"), "nova") {
            let v = novafile_text::parse(&read(&f)).unwrap();
            let issues = schema.resolve(&v).err().unwrap_or_else(|| panic!("{} should not resolve", f.display()));
            let paths: Vec<String> = issues.iter().map(|i| i.path.to_string()).collect();
            let msgs: Vec<String> = issues.iter().map(|i| i.message.clone()).collect();
            out.push((expect_path(&f), json!({"error": true, "issue_paths": paths, "messages": msgs})));
        }
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

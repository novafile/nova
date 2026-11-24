//! Checks the expectation files of the conformance suite against this implementation.

/// The reference implementation must reproduce every expectation file.
#[test]
fn conformance_suite() {
    let Some(dir) = novafile_conformance::locate() else {
        eprintln!("skipping: no spec checkout found (set NOVA_CONFORMANCE or clone github.com/novafile/spec next to this repo)");
        return;
    };

    let mut failures = Vec::new();
    let exp = novafile_conformance::expectations(&dir);
    for (path, j) in &exp {
        let want = std::fs::read_to_string(path).unwrap_or_default();
        if want != novafile_conformance::render(j) {
            failures.push(path.display().to_string());
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n  {}",
        failures.len(),
        exp.len(),
        failures.join("\n  ")
    );
    eprintln!("{} conformance cases pass", exp.len());
}

/// The meta-schemas and the newest standard library embedded in
/// `novafile-schema` are verbatim copies of the normative ones in the
/// specification (Apache-2.0), and must stay identical.
#[test]
fn embedded_meta_schema_matches_spec() {
    let Some(dir) = novafile_conformance::locate() else {
        eprintln!("skipping: no spec checkout found");
        return;
    };

    for (file, embedded) in [
        ("schema.novs", novafile_schema::META_SCHEMA_SRC),
        ("catalog.novs", novafile_schema::META_CATALOG_SRC),
        ("std.novs", novafile_schema::STD_SRCS[novafile_schema::STD_SRCS.len() - 1]),
    ] {
        let spec = std::fs::read_to_string(dir.join("../meta").join(file)).unwrap_or_else(|e| panic!("spec meta/{file}: {e}"));
        assert_eq!(embedded, spec, "crates/novafile-schema/meta/{file} drifted from the spec's meta/{file}");
    }
}

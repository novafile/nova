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

/// The meta-schema embedded in `novafile-schema` is a verbatim copy of the
/// normative one in the specification (Apache-2.0), and must stay identical.
#[test]
fn embedded_meta_schema_matches_spec() {
    let Some(dir) = novafile_conformance::locate() else {
        eprintln!("skipping: no spec checkout found");
        return;
    };

    let spec = std::fs::read_to_string(dir.join("../meta/nova-schema.novs")).expect("spec meta/nova-schema.novs");
    assert_eq!(
        novafile_schema::META_SCHEMA_SRC,
        spec,
        "crates/novafile-schema/meta/nova-schema.novs drifted from the spec's meta/nova-schema.novs"
    );
}

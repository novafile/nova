//! Integration tests for queries, patch parsing and application, diffing and
//! source-preserving edits.

use novafile_patch::{PatchError, apply_unchecked, diff, get, parse_patch};
use novafile_text::{Profile, parse};
use novafile_value::{Value, revision};

const DOC: &str = include_str!("../../../examples/infrastructure.nova");

#[test]
fn get_then_scoped_patch() {
    let doc = parse(DOC).unwrap();
    let resp = get(&doc, "@get services[id=api].{replicas,port}").unwrap();
    let text = resp.to_text(Profile::Compact);
    let rev = revision(&doc).to_string();
    assert_eq!(text, format!("@revision {rev}\n@scope services[id=api]\nreplicas: 5\nport: 3000\n"));
    // The response is itself a valid NOVA document.
    novafile_text::parse_document(&text).unwrap();

    let patch = parse_patch(&format!("@patch {}\n@scope services[id=api]\nreplicas: 5 -> 8\n", &rev[..19])).unwrap();
    let out = apply_unchecked(&doc, &patch).unwrap();
    let r = novafile_text::Path::parse("services[id=api].replicas").unwrap();
    assert_eq!(r.get(&out.value).unwrap(), &Value::Int(8));
    assert_ne!(out.revision, revision(&doc));

    // Re-applying against the new document fails: the base is stale.
    assert!(matches!(apply_unchecked(&out.value, &patch), Err(PatchError::StaleBase { .. })));
}

#[test]
fn table_projection() {
    let doc = parse(DOC).unwrap();
    let resp = get(&doc, "services[*].{id,replicas}").unwrap();
    assert!(resp.scope.is_none());
    assert!(resp.to_text(Profile::Compact).ends_with("[2]{id,replicas}:\n  auth,3\n  api,5\n"));
}

#[test]
fn all_operations_and_atomicity() {
    let doc = parse(DOC).unwrap();
    let p = parse_patch(
        "@patch infrastructure@1.0.0\n\
         ? environment: production\n\
         + services: {id: metrics, image: \"ghcr.io/example/metrics:v1\", port: 9090, replicas: 2}\n\
         - services[id=auth]\n\
         = services[id=api].replicas: 8\n\
         + services[id=api].tags: canary\n\
         + region_backup: eu-west-1\n\
         - services[id=api].tags[edge]\n\
         services[0].port: 3000 -> 3001\n",
    )
    .unwrap();
    assert_eq!(p.schema.as_deref(), Some("infrastructure@1.0.0"));
    let out = apply_unchecked(&doc, &p).unwrap_or_else(|e| panic!("{e}"));
    let v = &out.value;
    let at = |p: &str| novafile_text::Path::parse(p).unwrap().get(v).cloned();
    assert_eq!(at("services[0].id").unwrap(), "api".into());
    assert_eq!(at("services[0].port").unwrap(), Value::Int(3001));
    assert_eq!(at("services[id=metrics].replicas").unwrap(), Value::Int(2));
    assert_eq!(at("services[id=api].tags").unwrap(), Value::List(vec!["public".into(), "canary".into()]));
    assert_eq!(at("region_backup").unwrap(), "eu-west-1".into());

    // A failing op anywhere aborts the whole patch.
    let bad = parse_patch("@patch\n= services[id=api].replicas: 9\nservices[id=auth].replicas: 4 -> 5\n").unwrap();
    let err = apply_unchecked(&doc, &bad).unwrap_err().to_string();
    assert!(err.contains("op 2 (line 3)") && err.contains("conflict: expected 4, found 3"), "{err}");
}

#[test]
fn syntax_errors() {
    for (src, needle) in [
        ("replicas: 1 -> 2\n", "must start with `@patch`"),
        ("@patch\nreplicas: 1\n", "expected `->"),
        ("@patch\n+ services\n", "requires `: <value>`"),
        ("@patch\n= services[*].x: 1\n", "may not use"),
        ("@patch\n@frobnicate\n", "unknown patch directive"),
    ] {
        let e = parse_patch(src).unwrap_err().to_string();
        assert!(e.contains(needle), "{src:?}: {e}");
    }
}

#[test]
fn diff_round_trips() {
    let a = parse(DOC).unwrap();
    let b = parse(
        &DOC.replace("replicas: 5", "replicas: 7")
            .replace("region: eu-central-1", "region: eu-west-1")
            .replace("  - id: auth\n", "  - id: authn\n"),
    )
    .unwrap();
    let p = diff(&a, &b);
    let text = p.to_string();
    assert!(text.contains("region: eu-central-1 -> eu-west-1"), "{text}");
    // Renaming the identity field defeats keyed diffing; lists of equal length diff by index.
    assert!(
        text.contains("services[0].id: auth -> authn") && text.contains("services[1].replicas: 5 -> 7"),
        "{text}"
    );
    let reparsed = parse_patch(&text).unwrap();
    assert_eq!(apply_unchecked(&a, &reparsed).unwrap().value, b);
}

#[test]
fn keyed_diff_when_order_allows() {
    let a = parse(DOC).unwrap();
    let b = parse(&format!("{DOC}  - id: metrics\n    image: m\n    port: 9090\n    replicas: 1\n    enabled: true\n").replace("replicas: 3", "replicas: 4"))
        .unwrap();
    let text = diff(&a, &b).to_string();
    assert!(text.contains("services[id=auth].replicas: 3 -> 4"), "{text}");
    assert!(text.contains("+ services: {id: metrics"), "{text}");
    assert_eq!(apply_unchecked(&a, &parse_patch(&text).unwrap()).unwrap().value, b);
}

#[test]
fn filters() {
    let doc = parse(DOC).unwrap();
    let ids = |p: &str| match novafile_patch::select(&doc, &novafile_text::Path::parse(p).unwrap()).unwrap() {
        Value::List(l) => l.into_iter().map(|v| novafile_text::flow_string(&v)).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };

    assert_eq!(ids("services[?replicas>3].id"), ["api"]);
    assert_eq!(ids("services[?replicas>=3].id"), ["auth", "api"]);
    assert_eq!(ids("services[?id!=auth].id"), ["api"]);
    assert_eq!(ids("services[?port<5000][?enabled=true].id"), ["api"]);
    assert_eq!(ids("services[?replicas=3.0].id"), ["auth"]); // numbers compare by value
    assert_eq!(ids("services[?tags[0]=public].id"), ["api"]); // nested paths; missing fields never match
    assert_eq!(ids("services[?replicas>100]"), Vec::<String>::new());
    let ts = parse("e: [{at: @timestamp(\"2026-01-01T00:00:00Z\")}, {at: @timestamp(\"2024-01-01T00:00:00Z\")}]").unwrap();
    let r = novafile_patch::select(&ts, &novafile_text::Path::parse("e[?at>=\"2025-01-01T00:00:00Z\"]").unwrap()).unwrap();
    assert_eq!(r.as_list().unwrap().len(), 1);
    // Filters are queries only: patches reject them.
    assert!(
        parse_patch("@patch\n= services[?replicas>3].replicas: 1\n")
            .unwrap_err()
            .to_string()
            .contains("may not use")
    );
    // Round-trip through display.
    let p = novafile_text::Path::parse("services[?status.code>=500].id").unwrap();
    assert_eq!(p.to_string(), "services[?status.code>=500].id");
}

const COMMENTED: &str = include_str!("../../novafile-text/tests/commented.nova");

fn on_source(src: &str, ops: &str) -> novafile_patch::SourceEdit {
    let p = parse_patch(&format!("@patch\n{ops}")).unwrap();
    let r = novafile_patch::apply_to_source(src, &p, &|_| Ok(())).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(parse(&r.text).unwrap(), r.applied.value, "{}", r.text);
    r
}

/// Lines of `b` that are not in `a` and vice versa.
fn changed(a: &str, b: &str) -> (Vec<String>, Vec<String>) {
    let (la, lb): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    (
        la.iter().filter(|l| !lb.contains(l)).map(std::string::ToString::to_string).collect(),
        lb.iter().filter(|l| !la.contains(l)).map(std::string::ToString::to_string).collect(),
    )
}

#[test]
fn source_patches_are_surgical() {
    let cases: Vec<(&str, Vec<&str>, Vec<&str>)> = vec![
        ("services[id=auth].replicas: 3 -> 4", vec!["    replicas: 3"], vec!["    replicas: 4"]),
        (
            "= services[id=auth].port: 9090",
            vec!["    port: 8080   # legacy port"],
            vec!["    port: 9090   # legacy port"],
        ),
        (
            "environment: production -> staging",
            vec!["environment: production   # do not change without approval"],
            vec!["environment: staging   # do not change without approval"],
        ),
        ("+ services[id=auth].enabled: true", vec![], vec!["    enabled: true"]),
        ("+ owner: platform", vec![], vec!["owner: platform"]),
        ("- price", vec!["price: @decimal(\"12.50\")"], vec![]),
        ("+ services: {id: metrics, replicas: 1}", vec![], vec!["  - id: metrics", "    replicas: 1"]),
        (
            "- services[id=auth]",
            vec!["  # auth is critical", "  - id: auth", "    port: 8080   # legacy port", "    replicas: 3"],
            vec![],
        ),
        (
            "services[id=api].tags[0]: public -> internal",
            vec!["    tags: [public,  # exposed"],
            vec!["    tags: [internal,  # exposed"],
        ),
        ("table[1].b: y -> z", vec!["  2, y"], vec!["  2, z"]),
    ];
    for (op, removed, added) in cases {
        let r = on_source(COMMENTED, op);
        assert!(r.surgical, "{op}: fell back to re-rendering:\n{}", r.text);
        let (gone, new) = changed(COMMENTED, &r.text);
        assert_eq!(
            (
                gone.iter().map(String::as_str).collect::<Vec<_>>(),
                new.iter().map(String::as_str).collect::<Vec<_>>()
            ),
            (removed, added),
            "{op}:\n{}",
            r.text
        );
    }
}

#[test]
fn source_patches_with_structure_changes_keep_comments() {
    // Replacing a scalar by a block value, emptying a list, set/map edits: still valid, comments kept.
    for op in [
        "= services[id=auth].port: {http: 8080, https: 8443}",
        "= services[id=api].tags: []",
        "+ table: {a: 3, b: w}",
        "- table[0]",
        "= region: [a, b]",
        "- services[id=api].tags[edge]",
    ] {
        let r = on_source(COMMENTED, op);
        for c in [
            "# Production infrastructure",
            "# auth is critical",
            "# The services we run",
            "# trailing comment at end",
        ] {
            assert!(r.text.contains(c), "{op}: lost {c:?}:\n{}", r.text);
        }
    }
}

#[test]
fn patches_declare_language_version() {
    assert!(parse_patch("@nova 0.1\n@patch\n? environment\n").is_ok());
    assert!(parse_patch("@nova 3.0\n@patch\n").unwrap_err().to_string().contains("requires NOVA 3.0"));
}

#[test]
fn existence_absence_and_in_filters() {
    let doc = parse(DOC).unwrap();
    let ids = |p: &str| match novafile_patch::select(&doc, &novafile_text::Path::parse(p).unwrap()).unwrap() {
        Value::List(l) => l.into_iter().map(|v| novafile_text::flow_string(&v)).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };

    assert_eq!(ids("services[?tags].id"), ["api"]);
    assert_eq!(ids("services[?!tags].id"), ["auth"]);
    assert_eq!(ids("services[?id in [auth, nope]].id"), ["auth"]);
    assert_eq!(ids("services[?replicas in [5.0, 7]].id"), ["api"]);
    for p in ["services[?tags]", "services[?!tags]", "services[?id in [auth, api]]"] {
        assert_eq!(novafile_text::Path::parse(p).unwrap().to_string(), p);
    }

    assert!(novafile_text::Path::parse("services[?!tags=1]").is_err());
    assert!(novafile_text::Path::parse("services[?id in auth]").is_err());
}

/// Removing the scope target must not let later operations reach the sibling
/// that slides into its position (§7.5).
#[test]
fn scope_target_cannot_be_removed() {
    let doc = parse("items:\n  - {name: mine, owner: agent}\n  - {name: admin, owner: root}\n").unwrap();
    let p = parse_patch("@patch\n@scope items[0]\n- \nowner: \"root\" -> \"agent\"\n").unwrap();
    let e = apply_unchecked(&doc, &p).unwrap_err();
    assert!(matches!(e, PatchError::Op { index: 0, .. }), "{e:?}");
    // Removing inside the scope still works.
    let p = parse_patch("@patch\n@scope items[0]\n- owner\n").unwrap();
    apply_unchecked(&doc, &p).unwrap();
}

/// Long patches on text fall back to one re-render and keep the result.
#[test]
fn long_patch_on_source_rerenders() {
    let src = "# keep\na: 0\n";
    let mut p = String::from("@patch\n");
    for i in 0..100 {
        p.push_str(&format!("= f{i}: {i}\n"));
    }

    let e = novafile_patch::apply_to_source(src, &parse_patch(&p).unwrap(), &|_| Ok(())).unwrap();
    assert!(!e.surgical);
    assert!(e.text.contains("# keep") && e.text.contains("f99: 99"), "{}", e.text);
}

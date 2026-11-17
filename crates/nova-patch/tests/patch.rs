use nova_patch::{apply_unchecked, diff, get, parse_patch, PatchError};
use nova_text::{parse, Profile};
use nova_value::{revision, Value};

const DOC: &str = include_str!("../../../examples/infrastructure.nova");

#[test]
fn get_then_scoped_patch() {
    let doc = parse(DOC).unwrap();
    let resp = get(&doc, "@get services[id=api].{replicas,port}").unwrap();
    let text = resp.to_text(Profile::Compact);
    let rev = revision(&doc).to_string();
    assert_eq!(text, format!("@revision {rev}\n@scope services[id=api]\nreplicas: 5\nport: 3000\n"));
    // The response is itself a valid NOVA document.
    nova_text::parse_document(&text).unwrap();

    let patch = parse_patch(&format!("@patch {}\n@scope services[id=api]\nreplicas: 5 -> 8\n", &rev[..19])).unwrap();
    let out = apply_unchecked(&doc, &patch).unwrap();
    let r = nova_text::Path::parse("services[id=api].replicas").unwrap();
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
    let at = |p: &str| nova_text::Path::parse(p).unwrap().get(v).cloned();
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
    let ids = |p: &str| match nova_patch::select(&doc, &nova_text::Path::parse(p).unwrap()).unwrap() {
        Value::List(l) => l.into_iter().map(|v| nova_text::flow_string(&v)).collect::<Vec<_>>(),
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
    let r = nova_patch::select(&ts, &nova_text::Path::parse("e[?at>=\"2025-01-01T00:00:00Z\"]").unwrap()).unwrap();
    assert_eq!(r.as_list().unwrap().len(), 1);
    // Filters are queries only: patches reject them.
    assert!(parse_patch("@patch\n= services[?replicas>3].replicas: 1\n")
        .unwrap_err()
        .to_string()
        .contains("may not use"));
    // Round-trip through display.
    let p = nova_text::Path::parse("services[?status.code>=500].id").unwrap();
    assert_eq!(p.to_string(), "services[?status.code>=500].id");
}

use nova_schema::{check_compatibility, parse_schema};
use nova_value::Value;

const SCHEMA: &str = include_str!("../../../examples/infrastructure.novs");
const DOC: &str = include_str!("../../../examples/infrastructure.nova");

#[test]
fn resolves_example() {
    let s = parse_schema(SCHEMA).unwrap();
    let v = nova_text::parse(DOC).unwrap();
    let r = s.resolve(&v).unwrap_or_else(|e| panic!("{e:?}"));
    let rec = r.as_record().unwrap();
    assert!(matches!(rec.get("deployed_at"), Some(Value::Timestamp(_))));
    assert_eq!(rec.get("status"), Some(&Value::Variant("active".into(), None)));
    let api = &r.as_record().unwrap().get("services").unwrap().as_list().unwrap()[1];
    assert!(matches!(api.as_record().unwrap().get("tags"), Some(Value::Set(_))));
    // validate() checks without materializing defaults.
    s.validate(&v).unwrap();
}

#[test]
fn reports_all_issues_with_paths() {
    let s = parse_schema(SCHEMA).unwrap();
    let bad = nova_text::parse(
        "environment: prod\nregion: x\ndeployed_at: yesterday\nstatus: exploded\nservices:\n  - id: a\n    image: i\n    port: 0\n    replicas: 500\n  - id: a\n    image: \"\"\n    port: 80\n    replicas: 1\n    extra: 1\n",
    )
    .unwrap();
    let issues: Vec<String> = s.resolve(&bad).unwrap_err().iter().map(|i| i.to_string()).collect();
    let joined = issues.join("\n");
    for needle in [
        "deployed_at: string is not a valid timestamp",
        "status: unknown variant \"exploded\"",
        "services[0].port: 0 is outside @range(1..65535)",
        "services[0].replicas: 500 is outside @range(0..100)",
        "services[1].image: length 0 is outside @len(1..)",
        "services[1].extra: unknown field",
        "services[1]: duplicate @key (id)",
    ] {
        assert!(joined.contains(needle), "missing {needle:?} in\n{joined}");
    }
}

#[test]
fn fingerprint_ignores_formatting() {
    let a = parse_schema(SCHEMA).unwrap();
    let reformatted = SCHEMA.replace("# Schema", "# Another comment\n# Schema");
    let b = parse_schema(&reformatted).unwrap();
    assert_eq!(a.fingerprint(), b.fingerprint());
    assert!(a.fingerprint().starts_with("infrastructure/v1#"));
    // Canonical text re-parses to the same schema.
    assert_eq!(parse_schema(&a.to_canonical_text()).unwrap().fingerprint(), a.fingerprint());
}

#[test]
fn compatibility() {
    let old = parse_schema(SCHEMA).unwrap();
    let new_src = SCHEMA
        .replace("replicas: u16 @range(0..100)", "replicas: u32 @range(0..1000)")
        .replace("  failed(string)\n", "  failed(string)\n  paused\n")
        .replace("  tags?: set<string> @id(6)\n", "  tags?: set<string> @id(6)\n  owner?: string @id(7)\n");
    let new = parse_schema(&new_src).unwrap();
    let changes = check_compatibility(&old, &new);
    assert!(changes.iter().all(|c| !c.breaking), "{changes:#?}");

    let broken = parse_schema(&SCHEMA.replace("port: Port @id(3)", "port: u8 @id(3)").replace("  draining\n", "")).unwrap();
    let changes = check_compatibility(&old, &broken);
    let b: Vec<_> = changes.iter().filter(|c| c.breaking).map(|c| c.at.as_str()).collect();
    assert_eq!(b, ["Service.port", "Status.draining"], "{changes:#?}");
}

#[test]
fn schema_errors() {
    for (src, needle) in [
        ("record A:\n  x: Missing\n", "unknown type Missing"),
        ("record A:\n  x: int\n  x: int\n", "duplicate field"),
        ("record a:\n  x: int\n", "uppercase"),
        ("record A:\n  x?: int @key\n", "@key fields"),
        ("type A = B\ntype B = A\n", "cycle"),
        ("record A:\n  x: int @range(a..b)\n", "invalid bound"),
    ] {
        let e = parse_schema(src).unwrap_err().to_string();
        assert!(e.contains(needle), "{src:?}: {e}");
    }
}

#[test]
fn rational_resolution() {
    let s = parse_schema("@root R\nrecord R:\n  a: rational\n  b: rational\n  c: decimal\n  d: rational\n").unwrap();
    let v = nova_text::parse("a: 22/7\nb: 3\nc: 3/8\nd: @decimal(\"0.25\")\n").unwrap();
    let r = s.resolve(&v).unwrap();
    assert_eq!(
        nova_text::to_text(&r, nova_text::Profile::Human),
        "a: 22/7\nb: 3/1\nc: @decimal(\"0.375\")\nd: 1/4\n"
    );
    let bad = nova_text::parse("a: 1/3\nb: 1.5\nc: 1/3\nd: 1\n").unwrap();
    let e: Vec<String> = s.resolve(&bad).unwrap_err().iter().map(|i| i.to_string()).collect();
    assert_eq!(e, ["b: expected rational, found float", "c: expected decimal, found rational"]);
}

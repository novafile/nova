use nova_schema::{check_compatibility, check_versions, parse_schema, ChangeKind};
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
    assert!(a.fingerprint().starts_with("infrastructure@1.0.0#"));
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

#[test]
fn semver_bumps_are_enforced() {
    let old = parse_schema(SCHEMA).unwrap();
    let with = |src: &str, v: &str| parse_schema(&src.replace("@schema infrastructure@1.0.0", &format!("@schema infrastructure@{v}"))).unwrap();
    let added = SCHEMA.replace("  tags?: set<string> @id(6)\n", "  tags?: set<string> @id(6)\n  owner?: string @id(7)\n");
    let broken = SCHEMA.replace("port: Port @id(3)", "port: u8 @id(3)");
    let reformatted = SCHEMA.replace("# Schema", "# Reformatted\n# Schema");

    let r = check_versions(&old, &with(&added, "1.1.0")).unwrap();
    assert_eq!((r.kind, r.problems.len()), (ChangeKind::Compatible, 0));
    let r = check_versions(&old, &with(&added, "1.0.1")).unwrap();
    assert_eq!(r.problems, ["these changes require at least 1.1.0, got 1.0.1"]);
    let r = check_versions(&old, &with(&broken, "1.1.0")).unwrap();
    assert_eq!(
        (r.kind, r.problems.clone()),
        (ChangeKind::Breaking, vec!["breaking changes require at least 2.0.0, got 1.1.0".to_string()])
    );
    assert!(check_versions(&old, &with(&broken, "2.0.0")).unwrap().problems.is_empty());
    let r = check_versions(&old, &with(&reformatted, "1.0.1")).unwrap();
    assert_eq!((r.kind, r.problems.len()), (ChangeKind::None, 0));
    assert!(!check_versions(&old, &with(&reformatted, "1.0.0")).unwrap().problems.is_empty());
    assert!(check_versions(&old, &with(&broken, "2.0.0-rc.1")).unwrap().problems.is_empty());
}

#[test]
fn document_references() {
    let s = parse_schema(&SCHEMA.replace("@1.0.0", "@1.3.0")).unwrap();
    s.accepts_reference("infrastructure@1.2.0").unwrap();
    s.accepts_reference(&format!("infrastructure@1.3.0#{}", s.fingerprint_hex())).unwrap();
    assert!(s.accepts_reference("infrastructure@1.4.0").unwrap_err().contains("cannot read"));
    assert!(s.accepts_reference("infrastructure@2.0.0").is_err());
    assert!(s.accepts_reference("billing@1.0.0").unwrap_err().contains("not"));
    assert!(s
        .accepts_reference("infrastructure@1.3.0#0000000000000000")
        .unwrap_err()
        .contains("fingerprint"));
    assert!(parse_schema("@schema infrastructure\n")
        .unwrap_err()
        .to_string()
        .contains("name@MAJOR.MINOR.PATCH"));
    assert!(parse_schema("@schema infrastructure@1.0\n").is_err());
}

#[test]
fn type_lookup_by_path() {
    let s = parse_schema(SCHEMA).unwrap();
    let t = |p: &str| s.type_at(&nova_text::Path::parse(p).unwrap()).map(|(t, f)| (t.to_string(), f.map(|f| f.name)));
    assert_eq!(t("services[3].port"), Some(("Port".into(), Some("port".into()))));
    assert_eq!(t("services"), Some(("list<Service>".into(), Some("services".into()))));
    assert_eq!(t("services[0]"), Some(("Service".into(), None)));
    assert_eq!(t("nope"), None);
}

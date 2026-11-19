//! Integration tests: parsing, resolution, evolution and version checks,
//! positional round trips and schema descriptors.

use novafile_schema::{ChangeKind, check_compatibility, check_versions, parse_schema};
use novafile_value::Value;

const SCHEMA: &str = include_str!("../../../examples/infrastructure.novs");
const DOC: &str = include_str!("../../../examples/infrastructure.nova");

#[test]
fn resolves_example() {
    let s = parse_schema(SCHEMA).unwrap();
    let v = novafile_text::parse(DOC).unwrap();
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
    let bad = novafile_text::parse(
        "environment: prod\nregion: x\ndeployed_at: yesterday\nstatus: exploded\nservices:\n  - id: a\n    image: i\n    port: 0\n    replicas: 500\n  - id: a\n    image: \"\"\n    port: 80\n    replicas: 1\n    extra: 1\n",
    )
    .unwrap();
    let issues: Vec<String> = s.resolve(&bad).unwrap_err().iter().map(std::string::ToString::to_string).collect();
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
    // Removing `draining` also moves `failed` up; the removal reports that shift once.
    assert_eq!(b, ["Service.port", "Status.draining"], "{changes:#?}");
}

/// The places `old` -> `new` breaks, for two schemas given as bodies.
fn breaks(old: &str, new: &str) -> Vec<String> {
    let (old, new) = (parse_schema(old).unwrap(), parse_schema(new).unwrap());
    check_compatibility(&old, &new).into_iter().filter(|c| c.breaking).map(|c| c.at).collect()
}

#[test]
fn compatibility_rules() {
    let none: [&str; 0] = [];
    let rec = |body: &str| format!("record R:\n{body}");
    // Removing a default makes absent fields fail to resolve.
    assert_eq!(breaks(&rec("  x: int = 5\n"), &rec("  x: int\n")), ["R.x"]);
    assert_eq!(breaks(&rec("  x?: int\n"), &rec("  x: int\n")), ["R.x"]);
    assert_eq!(breaks(&rec("  x?: int\n"), &rec("  x: int = 5\n")), none);
    // A rational rejects floats and decimal strings that a decimal accepts.
    assert_eq!(breaks("type T = decimal\n", "type T = rational\n"), ["T"]);
    assert_eq!(breaks("type T = int\n", "type T = rational\n"), none);
    // Ints widen to float only where every value is exact.
    assert_eq!(breaks("type T = u16\n", "type T = float\n"), none);
    assert_eq!(breaks("type T = i64\n", "type T = float\n"), ["T"]);
    assert_eq!(breaks("type T = int?\n", "type T = int\n"), ["T"]);
    assert_eq!(breaks("type T = int\n", "type T = int?\n"), none);
    assert_eq!(breaks("type T = list<u8>\n", "type T = list<int>\n"), none);
    assert_eq!(breaks("type T = list<int>\n", "type T = set<int>\n"), ["T"]);
    assert_eq!(breaks("record R @open:\n  x: int\n", &rec("  x: int\n")), ["R"]);
    // Protobuf uses @id as the field number.
    assert_eq!(breaks(&rec("  port: u16 @id(1)\n"), &rec("  port: u16 @id(2)\n")), ["R.port"]);
    assert_eq!(breaks(&rec("  port: u16 @id(1)\n"), &rec("  port: u16\n")), ["R.port"]);
    assert_eq!(breaks(&rec("  port: u16\n"), &rec("  port: u16 @id(1)\n")), none);
    // Without @id, Protobuf numbers variants by position: only appending is safe.
    let e = |vs: &str| format!("enum E:\n{vs}");
    assert_eq!(breaks(&e("  a\n  b\n"), &e("  a\n  b\n  c(int)\n")), none);
    // An insertion shifts later variants; that is reported once, at the inserted variant.
    assert_eq!(breaks(&e("  a\n  b\n"), &e("  z\n  a\n  b\n")), ["E.z"]);
    assert_eq!(breaks(&e("  a\n  b(int)\n"), &e("  b(int)\n  a\n")), ["E.a", "E.b"]);
    // With @id, the variant number is the id: reordering and inserting are safe.
    assert_eq!(breaks(&e("  a @id(1)\n  b @id(2)\n"), &e("  z @id(3)\n  b @id(2)\n  a @id(1)\n")), none);
    assert_eq!(breaks(&e("  a @id(1)\n  b(int) @id(2)\n"), &e("  a @id(1)\n  b(int) @id(3)\n")), ["E.b"]);
    // An added variant must not take a number that old data uses.
    assert_eq!(breaks(&e("  a @id(1)\n  b @id(2)\n"), &e("  a @id(1)\n  c @id(2)\n")), ["E.b", "E.c"]);
    // Adding ids that equal the old positions is safe; other ids renumber.
    assert_eq!(breaks(&e("  a\n  b\n"), &e("  b @id(2)\n  a @id(1)\n  c @id(3)\n")), none);
    assert_eq!(breaks(&e("  a\n  b\n"), &e("  a @id(2)\n  b @id(1)\n")), ["E.a", "E.b"]);
    assert_eq!(breaks(&e("  a @id(2)\n  b @id(1)\n"), &e("  b\n  a\n")), none);
}

#[test]
fn enum_removal_is_reported_once() {
    let (old, new) = (
        parse_schema("enum E:\n  a\n  b\n  c\n  d\n").unwrap(),
        parse_schema("enum E:\n  a\n  c\n  d\n").unwrap(),
    );
    let changes: Vec<String> = check_compatibility(&old, &new).iter().map(ToString::to_string).collect();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert!(
        changes[0].contains("E.b: variant removed; later variants shift position") && changes[0].contains("@id"),
        "{changes:?}"
    );
    // A real move among the kept variants is still reported (`c` keeps number 3).
    let new = parse_schema("enum E:\n  a\n  d\n  c\n").unwrap();
    let at: Vec<String> = check_compatibility(&old, &new).into_iter().map(|c| c.at).collect();
    assert_eq!(at, ["E.b", "E.d"]);
    // Removing the last variant shifts nothing.
    let new = parse_schema("enum E:\n  a\n  b\n  c\n").unwrap();
    let changes = check_compatibility(&old, &new);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].message, "variant removed");
}

#[test]
fn variant_ids() {
    for src in [
        "enum E:\n  a @id(2)\n  b(int @range(1..)) @id(1)\n",
        "enum E = a @id(2) | b(int @range(1..)) @id(1)\n",
    ] {
        let s = parse_schema(src).unwrap();
        let Some(novafile_schema::Def::Enum(vs)) = s.def("E") else { panic!() };
        assert_eq!(vs.iter().map(|v| v.id).collect::<Vec<_>>(), [Some(2), Some(1)]);
        assert_eq!(vs[1].payload.as_ref().unwrap().to_string(), "int @range(1..)");
        // Canonical text and the descriptor keep the ids.
        let text = s.to_canonical_text();
        assert!(text.contains("  a @id(2)\n  b(int @range(1..)) @id(1)\n"), "{text}");
        assert_eq!(parse_schema(&text).unwrap().to_canonical_text(), text);
        let d = s.to_value();
        novafile_schema::meta_schema().validate(&d).unwrap();
        assert_eq!(novafile_schema::Schema::from_value(&d).unwrap(), s);
    }

    for (src, needle) in [
        ("enum E:\n  a @id(1)\n  b @id(1)\n", "line 3: duplicate variant id 1"),
        (
            "enum E:\n  a @id(1)\n  b\n",
            "line 3: variant \"b\": either every variant of an enum has @id or none does",
        ),
        ("enum E = a | b @id(2)\n", "either every variant"),
        ("enum E:\n  a @id(0)\n", "@id(0) is not allowed"),
        ("enum E:\n  a @id(x)\n", "invalid @id(x)"),
        // An attribute inside the parentheses belongs to the payload type.
        ("enum E:\n  a(int @id(1))\n", "only valid on record fields"),
    ] {
        let e = parse_schema(src).unwrap_err().to_string();
        assert!(e.contains(needle), "{src:?}: {e}");
    }

    // A descriptor with partial ids is rejected like schema text.
    let mut d = parse_schema("enum E:\n  a @id(1)\n  b @id(2)\n").unwrap();
    let Some((_, novafile_schema::Def::Enum(vs))) = d.defs.first_mut() else {
        panic!()
    };
    vs[1].id = None;
    assert!(novafile_schema::Schema::from_value(&d.to_value()).unwrap_err().contains("either every variant"));
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
    let v = novafile_text::parse("a: 22/7\nb: 3\nc: 3/8\nd: @decimal(\"0.25\")\n").unwrap();
    let r = s.resolve(&v).unwrap();
    assert_eq!(
        novafile_text::to_text(&r, novafile_text::Profile::Human),
        "a: 22/7\nb: 3/1\nc: @decimal(\"0.375\")\nd: 1/4\n"
    );
    let bad = novafile_text::parse("a: 1/3\nb: 1.5\nc: 1/3\nd: 1\n").unwrap();
    let e: Vec<String> = s.resolve(&bad).unwrap_err().iter().map(std::string::ToString::to_string).collect();
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
    assert_eq!(check_versions(&old, &with(&broken, "2.0.0")).unwrap().problems, [] as [std::string::String; 0]);
    let r = check_versions(&old, &with(&reformatted, "1.0.1")).unwrap();
    assert_eq!((r.kind, r.problems.len()), (ChangeKind::None, 0));
    assert_ne!(
        check_versions(&old, &with(&reformatted, "1.0.0")).unwrap().problems,
        [] as [std::string::String; 0]
    );
    assert_eq!(
        check_versions(&old, &with(&broken, "2.0.0-rc.1")).unwrap().problems,
        [] as [std::string::String; 0]
    );
    // In 0.y a minor bump is the major one; over-bumping additions is fine.
    let r = check_versions(&with(SCHEMA, "0.2.3"), &with(&added, "0.3.0")).unwrap();
    assert_eq!((r.kind, r.problems.len()), (ChangeKind::Compatible, 0), "{:?}", r.problems);
    let r = check_versions(&with(SCHEMA, "0.2.3"), &with(&added, "0.2.4")).unwrap();
    assert_eq!((r.kind, r.problems.len()), (ChangeKind::Compatible, 0), "{:?}", r.problems);
}

#[test]
fn document_references() {
    let s = parse_schema(&SCHEMA.replace("@1.0.0", "@1.3.0")).unwrap();
    s.accepts_reference("infrastructure@1.2.0").unwrap();
    s.accepts_reference(&format!("infrastructure@1.3.0#{}", s.fingerprint_hex())).unwrap();
    assert!(s.accepts_reference("infrastructure@1.4.0").unwrap_err().contains("cannot read"));
    assert!(s.accepts_reference("infrastructure@2.0.0").is_err());
    assert!(s.accepts_reference("billing@1.0.0").unwrap_err().contains("not"));
    assert!(
        s.accepts_reference("infrastructure@1.3.0#0000000000000000")
            .unwrap_err()
            .contains("fingerprint")
    );
    // Build metadata does not change the version, so the fingerprint is still checked.
    let built = parse_schema(&SCHEMA.replace("@1.0.0", "@1.3.0+b")).unwrap();
    assert!(
        built
            .accepts_reference("infrastructure@1.3.0+a#0000000000000000")
            .unwrap_err()
            .contains("fingerprint")
    );
    assert!(
        parse_schema("@schema infrastructure\n")
            .unwrap_err()
            .to_string()
            .contains("name@MAJOR.MINOR.PATCH")
    );
    assert!(parse_schema("@schema infrastructure@1.0\n").is_err());
}

#[test]
fn type_lookup_by_path() {
    let s = parse_schema(SCHEMA).unwrap();
    let t = |p: &str| {
        s.type_at(&novafile_text::Path::parse(p).unwrap())
            .map(|(t, f)| (t.to_string(), f.map(|f| f.name)))
    };

    assert_eq!(t("services[3].port"), Some(("Port".into(), Some("port".into()))));
    assert_eq!(t("services"), Some(("list<Service>".into(), Some("services".into()))));
    assert_eq!(t("services[0]"), Some(("Service".into(), None)));
    assert_eq!(t("nope"), None);
}

#[test]
fn exact_ranges_and_len_units() {
    let s = parse_schema(
        "@root R\nrecord R:\n  p: float @range(0..0.1)\n  q: decimal @range(1/3..)\n  n: string @len(..4, utf8)\n  g: string @len(1..1, graphemes)\n  u: string @len(..2, utf16)\n  c: string @len(..4)\n",
    )
    .unwrap();
    let issues = |src: &str| match s.resolve(&novafile_text::parse(src).unwrap()) {
        Ok(_) => vec![],
        Err(i) => i.iter().map(std::string::ToString::to_string).collect::<Vec<_>>(),
    };

    // The binary64 nearest 0.1 is slightly above 1/10, so it is outside 0..0.1 exactly.
    let ok = "q: 0.34\nn: abcd\ng: \"e\u{301}\"\nu: \"ab\"\nc: \"äöüß\"\n";
    assert_eq!(issues(&format!("p: 0.1\n{ok}")), ["p: 0.1 is outside @range(0..0.1)"]);
    assert_eq!(issues(&format!("p: 0.09999999999999999\n{ok}")), [] as [std::string::String; 0]);
    // 0.3333 as a decimal is below 1/3.
    assert_eq!(issues(&format!("p: 0.0\nq: 0.3333\n{}", &ok[8..])), ["q: 0.3333 is outside @range(1/3..)"]);
    // UTF-8 bytes vs scalars vs graphemes vs UTF-16.
    assert_eq!(
        issues("p: 0.0\nq: 1\nn: äöü\ng: \"e\u{301}\"\nu: \"😀\"\nc: äöüß\n"),
        ["n: length 6 is outside @len(..4, utf8)"]
    );
    assert_eq!(
        issues("p: 0.0\nq: 1\nn: ab\ng: ab\nu: \"😀a\"\nc: äöüßx\n"),
        [
            "g: length 2 is outside @len(1..1, graphemes)",
            "u: length 3 is outside @len(..2, utf16)",
            "c: length 5 is outside @len(..4)"
        ]
    );
    // Canonical text keeps exact bounds and units.
    let t = s.to_canonical_text();
    assert!(
        t.contains("@range(0..0.1)") && t.contains("@range(1/3..)") && t.contains("@len(..4, utf8)") && t.contains("@len(1..1, graphemes)"),
        "{t}"
    );
    assert!(
        parse_schema("@root R\nrecord R:\n  n: string @len(..4, bits)\n")
            .unwrap_err()
            .to_string()
            .contains("unknown @len unit")
    );
}

#[test]
fn positional_round_trip() {
    let s = parse_schema(SCHEMA).unwrap();
    let doc = novafile_text::parse(DOC).unwrap();
    let pos = s.to_positional(&doc).unwrap();
    let text = novafile_text::document_to_text(&[("schema", &s.fingerprint()), ("mode", "positional")], &pos, novafile_text::Profile::Compact);
    assert!(text.contains("- (auth,ghcr.io/example/auth:v2,8080,3,true,!_)"), "{text}");
    assert!(text.contains("- (api,ghcr.io/example/api:v5,3000,5,true,@set[public,edge])"), "{text}");
    assert!(text.contains("@mode positional"), "{text}");
    // Reading back needs the schema and yields the resolved document.
    let back = s.from_positional(&novafile_text::parse(&text).unwrap()).unwrap();
    assert_eq!(back, s.resolve(&doc).unwrap());
    // Shape errors are reported.
    let bad = novafile_text::parse("- production\n- x\n").unwrap();
    assert!(s.from_positional(&bad).unwrap_err().contains("expected 5 positional fields"));
}

#[test]
fn schema_files_declare_language_version() {
    assert!(parse_schema("@nova 0.1\n@schema x@1.0.0\n").is_ok());
    assert!(
        parse_schema("@nova 9.9\n@schema x@1.0.0\n")
            .unwrap_err()
            .to_string()
            .contains("requires NOVA 9.9")
    );
    assert!(
        parse_schema("@schema x@1.0.0\n@nova 0.1\n")
            .unwrap_err()
            .to_string()
            .contains("first directive")
    );
}

#[test]
fn constraints_on_type_arguments() {
    let s = parse_schema("@root R\nrecord R:\n  ports: list<u16 @range(1..1024)>\n  m: map<string @len(1..8), (int @range(0..), string)>\n").unwrap();
    let text = s.to_canonical_text();
    assert!(
        text.contains("ports: list<u16 @range(1..1024)>") && text.contains("m: map<string @len(1..8), (int @range(0..), string)>"),
        "{text}"
    );
    assert_eq!(parse_schema(&text).unwrap().fingerprint(), s.fingerprint());
    let issues = s.resolve(&novafile_text::parse("ports: [80, 2000]\nm: {abc: (-1, x)}\n").unwrap()).unwrap_err();
    let msgs: Vec<String> = issues.iter().map(std::string::ToString::to_string).collect();
    assert_eq!(msgs, ["ports[1]: 2000 is outside @range(1..1024)", "m.abc[0]: -1 is outside @range(0..)"]);
}

#[test]
fn schemas_as_data() {
    use novafile_schema::meta_schema;
    let meta = meta_schema();
    for src in [
        SCHEMA,
        novafile_schema::META_SCHEMA_SRC,
        "@root R\nrecord R @open:\n  ports: list<u16 @range(1..1024)>?\n  m: map<string @len(1..8, utf8), (int @range(1/3..), string)>\n  r?: rational @range(-1.5..) @deprecated = 1/2\n",
    ] {
        let s = parse_schema(src).unwrap();
        let d = s.to_value();
        // The descriptor is valid data under the meta-schema …
        meta.validate(&d).unwrap_or_else(|e| panic!("{e:?}"));
        // … and converts back to the same schema.
        let back = novafile_schema::Schema::from_value(&d).unwrap();
        assert_eq!(back.fingerprint(), s.fingerprint(), "{}", back.to_canonical_text());
        // It also survives NOVA text.
        let text = novafile_text::to_text(&d, novafile_text::Profile::Human);
        let reparsed = novafile_schema::Schema::from_value(&novafile_text::parse(&text).unwrap()).unwrap();
        assert_eq!(reparsed.fingerprint(), s.fingerprint(), "{text}");
    }

    // The meta-schema describes itself.
    meta.validate(&meta.to_value()).unwrap();
    // Queries work on descriptors.
    let d = parse_schema(SCHEMA).unwrap().to_value();
    let names = nova_patch_free_select(&d, "defs[name=Service].record.fields");
    assert!(names.contains("port") && names.contains("replicas"), "{names}");
    // Invalid descriptors are rejected with useful messages.
    let bad = novafile_text::parse("defs:\n  - name: A\n    alias: {kind: !named, ref: Missing}\n").unwrap();
    assert!(novafile_schema::Schema::from_value(&bad).unwrap_err().contains("unknown type Missing"));
    let bad = novafile_text::parse("defs:\n  - name: A\n").unwrap();
    assert!(novafile_schema::Schema::from_value(&bad).unwrap_err().contains("exactly one"));
    let bad = novafile_text::parse("defs:\n  - name: A\n    alias: {kind: !u17}\n").unwrap();
    assert!(novafile_schema::Schema::from_value(&bad).unwrap_err().contains("invalid schema descriptor"));
}

fn nova_patch_free_select(v: &Value, path: &str) -> String {
    novafile_text::flow_string(novafile_text::Path::parse(path).unwrap().get(v).unwrap())
}

#[test]
fn deeply_nested_types_are_rejected() {
    let deep = format!("{}int{}", "list<".repeat(100_000), ">".repeat(100_000));
    assert!(novafile_schema::parse_type(&deep).unwrap_err().contains("nested deeper"));
    let tuples = format!("{}int{}", "(".repeat(100_000), ")".repeat(100_000));
    assert!(novafile_schema::parse_type(&tuples).is_err());
    let ok = format!("{}int{}", "list<".repeat(60), ">".repeat(60));
    assert!(novafile_schema::parse_type(&ok).is_ok());
}

#[test]
fn positional_nova_lines() {
    use novafile_text::lines::{WriteMode, read_str, write_string};
    let s = parse_schema("@schema logs@1.0.0\n@root Event[]\nrecord Event:\n  id: u32\n  msg?: string\n").unwrap();
    let records = vec![
        novafile_text::parse_flow("{id: 1, msg: hi}").unwrap(),
        novafile_text::parse_flow("{id: 2}").unwrap(),
    ];
    let text = write_string(&records, WriteMode::Positional(&s), None, None, None).unwrap();
    assert!(
        text.contains(&format!("@schema {}\n@mode positional\n(1, hi)\n(2, !_)\n", s.fingerprint())),
        "{text}"
    );
    let back = read_str(&text, Some(&s));
    assert!(back.errors.is_empty(), "{:?}", back.errors);
    assert_eq!(back.records, records);

    // A different schema, or none, cannot read it.
    let other = parse_schema("@schema logs@1.0.0\n@root Event[]\nrecord Event:\n  id: u64\n  msg?: string\n").unwrap();
    assert!(read_str(&text, Some(&other)).errors[0].message.contains("fingerprint"));

    // The root must be a list.
    let flat = parse_schema("@schema one@1.0.0\n@root Event\nrecord Event:\n  id: u32\n").unwrap();
    let err = write_string(&records, WriteMode::Positional(&flat), None, None, None).unwrap_err();
    assert!(err.to_string().contains("list @root"), "{err}");
}

#[test]
fn root_type_expressions() {
    let s = parse_schema("@schema evs@1.0.0\n@root Event[]\nrecord Event:\n  id: u32\n").unwrap();
    // Canonical text writes the root in long form; the descriptor carries it as a type.
    assert!(s.to_canonical_text().contains("@root list<Event>"), "{}", s.to_canonical_text());
    assert_eq!(parse_schema(&s.to_canonical_text()).unwrap().fingerprint(), s.fingerprint());
    let back = novafile_schema::Schema::from_value(&s.to_value()).unwrap();
    assert_eq!(back.fingerprint(), s.fingerprint());
    assert_eq!(s.root_name(), None);

    s.validate(&novafile_text::parse("- id: 1\n- id: 2\n").unwrap()).unwrap();
    assert!(s.validate(&novafile_text::parse("id: 1\n").unwrap()).is_err());

    let e = parse_schema("@root Missing[]\n").unwrap_err().to_string();
    assert!(e.contains("@root: unknown type Missing"), "{e}");
    assert!(parse_schema("@root list<\n").is_err());

    // Named roots are unchanged in text and descriptor.
    let named = parse_schema(SCHEMA).unwrap();
    assert_eq!(named.root_name(), Some("Infrastructure"));
    assert!(named.to_canonical_text().contains("@root Infrastructure\n"));
}

const UNIONS: &str = r#"@schema unions@1.0.0
@root R

type Formula = list<string> | Not | AtStart
type Ping = Pong | int
type Pong = Ping

record Not:
  not: Formula

record AtStart:
  "at start": Formula

enum Phase = pending | failed(string | int @range(1..)) @id(2)

record R:
  f: Formula
  "a \"q\"?"?: string | timestamp
"#;

#[test]
fn union_and_quoted_names_round_trip() {
    // Alias cycles through unions only are invalid; through a container they are fine.
    let err = parse_schema(UNIONS.replace("@id(2)", "").as_str()).unwrap_err();
    assert!(err.message.contains("cycle"), "{err}");
    parse_schema("type T = list<T> | string\n").unwrap();

    let src = UNIONS.replace("type Ping = Pong | int\ntype Pong = Ping\n", "").replace(" @id(2)", "");
    let s = parse_schema(&src).unwrap();
    let text = s.to_canonical_text();
    assert!(text.contains("  \"at start\": Formula\n"), "{text}");
    assert!(text.contains("  \"a \\\"q\\\"?\"?: string | timestamp\n"), "{text}");
    assert!(text.contains("failed(string | int @range(1..))"), "{text}");
    assert_eq!(parse_schema(&text).unwrap().to_canonical_text(), text);

    // The descriptor converts back without changing the fingerprint.
    let back = novafile_schema::Schema::from_value(&s.to_value()).unwrap();
    assert_eq!(back.fingerprint(), s.fingerprint());

    // @key needs an identifier name.
    assert!(parse_schema("record K:\n  \"a b\": string @key\n").is_err());
}

#[test]
fn unions_resolve_in_member_order() {
    let src = UNIONS.replace("type Ping = Pong | int\ntype Pong = Ping\n", "").replace(" @id(2)", "");
    let s = parse_schema(&src).unwrap();
    let doc = |t: &str| novafile_text::parse(t).unwrap();

    let r = s
        .resolve(&doc("f:\n  not:\n    \"at start\": [x]\n\"a \\\"q\\\"?\": 2026-01-01T00:00:00Z\n"))
        .unwrap();
    // `string` comes first, so the timestamp-shaped string stays a string.
    assert!(matches!(r.as_record().unwrap().get("a \"q\"?"), Some(Value::String(_))));

    let issues = s.resolve(&doc("f:\n  not: {\"at start\": [x], extra: 1}\n")).unwrap_err();
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].path.to_string(), "f");
    assert_eq!(
        issues[0].message,
        "matches no member of Formula: Not: not: matches no member of Formula: AtStart: extra: unknown field"
    );
    assert_eq!(s.resolve(&doc("f: 5\n")).unwrap_err()[0].message, "5 matches no member of Formula");
}

#[test]
fn union_evolution() {
    let check = |old: &str, new: &str| {
        let p = |t: &str| parse_schema(&format!("type F = A | B\nrecord A:\n  a: int\nrecord B:\n  b: int\nrecord R:\n  v: {t}\n")).unwrap();
        check_compatibility(&p(old), &p(new)).iter().any(|c| c.breaking)
    };

    assert!(!check("int", "int | string"));
    assert!(check("string", "timestamp | string"));
    assert!(check("int | string", "string | int"));
    assert!(!check("int | string", "int? | string"));
    assert!(!check("int | string", "null | int | string"));
    assert!(check("int? | string", "int | string"));
    assert!(!check("u8 | u8", "u8 | u16"));
    assert!(check("u8 | decimal", "u16 | decimal"));
    // Aliases to unions are flattened: appending after F is fine, inserting before B is not.
    assert!(!check("F", "F | int"));
    assert!(!check("F", "A | B | int"));
    assert!(check("F | int", "A | int | B"));
}

#[test]
fn recursive_union_aliases_terminate() {
    for t in ["string | list<T>", "list<T> | string", "list<T> | null", "string | map<string, T>"] {
        let s = parse_schema(&format!("@schema r@1.0.0\ntype T = {t}\nrecord R:\n  v: T\n")).unwrap();
        assert!(check_compatibility(&s, &s).is_empty(), "{t}");
        let v = novafile_text::parse("v: [[], [[]]]\n").unwrap();
        let _ = s.resolve(&v);
    }
}

#[test]
fn code_built_union_cycle_is_an_issue() {
    use novafile_schema::{Def, Schema, TypeKind, TypeRef};
    let named = |n: &str| TypeRef::new(TypeKind::Named(n.into()));
    let s = Schema {
        root: Some(named("A")),
        defs: vec![
            (
                "A".into(),
                Def::Alias(TypeRef::new(TypeKind::Union(vec![
                    named("B"),
                    TypeRef::new(TypeKind::Int(novafile_schema::IntKind::Int)),
                ]))),
            ),
            ("B".into(), Def::Alias(named("A"))),
        ],
        ..Schema::default()
    };
    let issues = s.resolve(&Value::Int(1)).unwrap_err();
    assert!(issues[0].message.contains("cycle"), "{issues:?}");
}

#[test]
fn key_duplicates_do_not_steer_union_members() {
    let s = parse_schema("@root R\nrecord K:\n  id: string @key\nrecord R:\n  v: list<K> | string\n").unwrap();
    let issues = s.resolve(&novafile_text::parse("v:\n  - id: a\n  - id: a\n").unwrap()).unwrap_err();
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].to_string(), "v[1]: duplicate @key (id)");
}

#[test]
fn overlapping_unions_stay_bounded() {
    // Two identical recursive members: each failing level would double the work.
    let s = parse_schema("@root T\ntype T = list<T> | list<T> | string\n").unwrap();
    let deep = format!("{}5{}", "[".repeat(100), "]".repeat(100));
    let v = novafile_text::parse(&format!("x: {deep}\n"))
        .unwrap()
        .as_record()
        .unwrap()
        .get("x")
        .unwrap()
        .clone();
    let issues = s.resolve(&v).unwrap_err();
    assert!(issues.iter().any(|i| i.message.contains("too much work")), "{issues:?}");

    // Aliases repeated across members: 2^60 paths if each were expanded again.
    let mut src = String::from("@schema b@1.0.0\n@root R\nrecord R:\n  v: list<A0>\n");
    for i in 0..60 {
        let n = i + 1;
        src.push_str(&["type A", &i.to_string(), " = A", &n.to_string(), " | A", &n.to_string(), "\n"].concat());
    }
    src += "type A60 = K | string\nrecord K:\n  id: string @key\n";
    let s = parse_schema(&src).unwrap();
    let changes = check_compatibility(&s, &s);
    assert!(changes.is_empty(), "{changes:?}");
    assert!(s.resolve(&novafile_text::parse("v: [5]\n").unwrap()).is_err());
    assert!(s.type_at(&novafile_text::Path::parse("v[0].nope").unwrap()).is_none());
}

//! Integration tests: parsing, the writer profiles, paths, spans and document-preserving
//! formatting.

use std::fmt::Write as _;

use novafile_text::{Path, Profile, parse, parse_document, parse_flow, split_stream, to_text};
use novafile_value::{Decimal, Duration, Record, Timestamp, Value};

const HUMAN: &str = r"# NOVA human syntax
@schema infrastructure@1.0.0

environment: production
region: eu-central-1
deployed_at: 2026-11-17T16:00:00Z

services:
  - id: auth
    image: ghcr.io/example/auth:v2
    port: 8080
    replicas: 3
    enabled: true   # comment

  - id: api
    image: ghcr.io/example/api:v5
    port: 3000
    replicas: 5
    enabled: true
";

const COMPACT: &str = r#"@schema infrastructure@1.0.0
environment: production
region: eu-central-1
deployed_at: "2026-11-17T16:00:00Z"
services[2]{id,image,port,replicas,enabled}:
  auth,ghcr.io/example/auth:v2,8080,3,true
  api,ghcr.io/example/api:v5,3000,5,true
"#;

#[test]
fn human_and_compact_agree() {
    let h = parse(HUMAN).unwrap();
    let c = parse(COMPACT).unwrap();
    assert_eq!(h, c);
    let doc = parse_document(HUMAN).unwrap();
    assert_eq!(doc.directive("schema"), Some("infrastructure@1.0.0"));
    let r = h.as_record().unwrap();
    // Without a schema, an unmarked timestamp stays a string (spec §4.4).
    assert_eq!(r.get("deployed_at"), Some(&Value::String("2026-11-17T16:00:00Z".into())));
    assert_eq!(Path::parse("services[id=api].port").unwrap().get(&h).unwrap(), &Value::Int(3000));
}

#[test]
fn compact_writer_emits_table() {
    let v = parse(HUMAN).unwrap();
    let out = to_text(&v, Profile::Compact);
    assert!(
        out.contains("services[2]{id,image,port,replicas,enabled}:\n  auth,ghcr.io/example/auth:v2,8080,3,true\n"),
        "{out}"
    );
    assert_eq!(parse(&out).unwrap(), v);
}

fn rec(entries: Vec<(&str, Value)>) -> Value {
    Value::Record(Record::from_entries(entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()).unwrap())
}

fn zoo() -> Value {
    rec(vec![
        ("null", Value::Null),
        ("t", Value::Bool(true)),
        ("big", Value::Int(170_141_183_460_469_231_731_687_303_715_884_105_727)),
        ("neg", Value::Int(-42)),
        ("f", Value::Float(1.5)),
        ("f2", Value::Float(1e300)),
        ("f3", Value::Float(-0.0)),
        ("nan", Value::Float(f64::NAN)),
        ("inf", Value::Float(f64::NEG_INFINITY)),
        ("dec", Value::Decimal(Decimal::parse("12.50").unwrap())),
        ("ts", Value::Timestamp(Timestamp::parse("2026-11-17T16:00:00.5Z").unwrap())),
        ("dur", Value::Duration(Duration::parse("1h30m").unwrap())),
        ("bytes", Value::Bytes(vec![0, 255, 7])),
        ("s_num", "123".into()),
        ("ratio", Value::Rational(novafile_value::Rational::parse("-22/7").unwrap())),
        ("s_ratio", "1/2".into()),
        ("s_date", "11/17/2026".into()),
        ("s_true", "true".into()),
        ("s_space", " padded ".into()),
        ("s_colon", "a: b".into()),
        ("s_hash", "x #y".into()),
        ("s_dash", "- item".into()),
        ("s_empty", "".into()),
        ("s_dollar", "$HOME".into()),
        ("s_unicode", "grüße 🚀".into()),
        ("s_ctrl", "bell\u{7}".into()),
        ("multi", "line one\n  line two\n".into()),
        ("multi_strip", "a\nb".into()),
        ("multi_blank", "a\n\nb\n".into()),
        ("multi_trailing", "a\n\n".into()),
        ("url", "https://example.com/x?y=1".into()),
        ("list", Value::List(vec![Value::Int(1), "two".into(), Value::Null])),
        ("nested", Value::List(vec![Value::List(vec![Value::Int(1)]), Value::List(vec![])])),
        ("tuple1", Value::Tuple(vec![Value::Int(1)])),
        ("tuple0", Value::Tuple(vec![])),
        ("set", Value::set(vec!["b".into(), "a".into()]).unwrap()),
        (
            "map",
            Value::map(vec![
                (Value::Int(1), "one".into()),
                (Value::Tuple(vec![Value::Int(1), Value::Int(2)]), "pair".into()),
            ])
            .unwrap(),
        ),
        ("unit", Value::Variant("active".into(), None)),
        ("var", Value::Variant("failed".into(), Some(Box::new(rec(vec![("code", Value::Int(3))]))))),
        ("var_s", Value::Variant("named".into(), Some(Box::new("x".into())))),
        ("empty_rec", rec(vec![])),
        ("empty_list", Value::List(vec![])),
        ("odd key!", Value::Int(1)),
        (
            "deep",
            rec(vec![(
                "a",
                rec(vec![(
                    "b",
                    Value::List(vec![rec(vec![("c", Value::Int(1)), ("d", Value::List(vec![rec(vec![("e", "f".into())])]))])]),
                )]),
            )]),
        ),
        (
            "rows",
            Value::List(vec![rec(vec![("a", Value::Int(1)), ("b", "x, y".into())]), rec(vec![("a", Value::Int(2))])]),
        ),
    ])
}

#[test]
fn every_profile_round_trips() {
    let v = zoo();
    for p in [Profile::Human, Profile::Compact, Profile::Canonical] {
        let text = to_text(&v, p);
        let back = parse(&text).unwrap_or_else(|e| panic!("{p:?}: {e}\n{text}"));
        assert_eq!(back, v, "{p:?}\n{text}");
    }
}

#[test]
fn top_level_shapes() {
    for v in [
        Value::Int(5),
        "hello world".into(),
        "-".into(),
        Value::List(vec![Value::Int(1), rec(vec![("a", Value::Int(1)), ("b", Value::Int(2))])]),
        Value::List(vec![rec(vec![("a", Value::Int(1))]), rec(vec![("a", Value::Int(2))])]),
        rec(vec![]),
    ] {
        for p in [Profile::Human, Profile::Compact] {
            let t = to_text(&v, p);
            assert_eq!(parse(&t).unwrap_or_else(|e| panic!("{e}\n{t}")), v, "{t}");
        }
    }
}

#[test]
fn canonical_is_order_independent() {
    let a = parse("b: 1\na: @set[2, 1]\n").unwrap();
    let b = parse("a: @set[1, 2]\nb: 1\n").unwrap();
    assert_eq!(to_text(&a, Profile::Canonical), to_text(&b, Profile::Canonical));
}

#[test]
fn errors() {
    let bad = [
        ("a: 1\na: 2\n", "duplicate key"),
        ("a:\n   b: 1\n", "exactly 2"),
        ("a:\n\tb: 1\n", "tabs"),
        ("a: b: c\n", "ambiguous"),
        ("a:\nb: 1\n", "explicitly"),
        ("a: (1)\n", "1-tuple"),
        ("a: $x\n", "template"),
        ("@unknown x\na: 1\n", "unknown directive"),
        ("t[2]{a,b}:\n  1,2\n", "but 1 follows"),
        ("t[18446744073709551615]{a}:\n  1\n", "but 1 follows"),
        ("t[1]{a,b}:\n  1,2,3\n", "3 cells"),
        ("a: [1, 2\n", "expected"),
        ("a: \"unterminated\n", "control character"),
        ("a: @set[1, 1]\n", "duplicate set"),
        ("a: @timestamp(\"2026-13-01T00:00:00Z\")\n", "timestamp"),
    ];
    for (src, needle) in bad {
        let e = parse(src).expect_err(src).to_string();
        assert!(e.contains(needle), "{src:?}: {e}");
    }
}

#[test]
fn plain_scalar_classification() {
    let v = parse("a: 1.0\nb: 1.2.3\nc: 08080\nd: -0\ne: NO\nf: 1e3\ng: hello world\nh: 2/4\ni: 1/0\nj: [22/7, @rational(\"6/3\")]\n").unwrap();
    let r = v.as_record().unwrap();
    assert_eq!(r.get("a"), Some(&Value::Float(1.0)));
    assert_eq!(r.get("b"), Some(&"1.2.3".into()));
    assert_eq!(r.get("c"), Some(&"08080".into()));
    assert_eq!(r.get("d"), Some(&Value::Int(0)));
    assert_eq!(r.get("e"), Some(&"NO".into()));
    assert_eq!(r.get("f"), Some(&Value::Float(1000.0)));
    assert_eq!(r.get("g"), Some(&"hello world".into()));
    let half = Value::Rational(novafile_value::Rational::new(1, 2).unwrap());
    assert_eq!(r.get("h"), Some(&half));
    // `1/0` does not match the rational grammar (denominator must be positive), so it is a string.
    assert_eq!(r.get("i"), Some(&"1/0".into()));
    assert_eq!(novafile_text::flow_string(r.get("j").unwrap()), "[22/7, 2/1]");
}

#[test]
fn sparse_table_cells_are_absent() {
    let v = parse("t[2]{a,b}:\n  1,\n  ,null\n").unwrap();
    assert_eq!(v, parse("t: [{a: 1}, {b: null}]").unwrap());
}

#[test]
fn flow_and_streams() {
    assert_eq!(
        parse_flow(" {a: [1, 2,], \"b c\": !ok(1)} ").unwrap(),
        parse("a: [1, 2]\n\"b c\": !ok(1)\n").unwrap()
    );
    let docs = split_stream("a: 1\n---\na: 2\n---\n");
    assert_eq!(docs.len(), 2);
}

#[test]
fn paths() {
    for p in [
        "services[id=api].replicas",
        "a[0][-1]",
        "m[\"k\"].x",
        "services[*].{id,replicas}",
        "\"odd key\".x",
        "[0].a",
    ] {
        let parsed = Path::parse(p).unwrap();
        assert_eq!(Path::parse(&parsed.to_string()).unwrap(), parsed, "{p}");
    }

    assert!(Path::parse("a.{b}.c").is_err());
}

#[test]
#[ignore = "prints a sample document for manual inspection"]
fn show() {
    println!("{}", to_text(&zoo(), Profile::Human));
}

#[test]
fn spans_locate_values() {
    let src = HUMAN;
    let (_, spans) = novafile_text::parse_with_spans(src).unwrap();
    let text = |p: &str| {
        let s = spans.get(&Path::parse(p).unwrap()).unwrap_or_else(|| panic!("no span for {p}"));
        &src[s.start..s.end]
    };

    assert_eq!(text("region"), "region: eu-central-1");
    assert_eq!(text("services[1].port"), "port: 3000");
    assert!(text("services[1]").starts_with("- id: api") && text("services[1]").ends_with("enabled: true"));
    assert_eq!(text("services[0].enabled"), "enabled: true");
    let off = src.find("3000").unwrap() + 1;
    assert_eq!(spans.at(off).unwrap().path.to_string(), "services[1].port");
    // Tables and flow values too.
    let (_, spans) = novafile_text::parse_with_spans(COMPACT).unwrap();
    let s = spans.get(&Path::parse("services[1].port").unwrap()).unwrap();
    assert_eq!(&COMPACT[s.start..s.end], "3000");
    let (_, spans) = novafile_text::parse_with_spans("a: {b: [1, {c: x}]}\n").unwrap();
    let s = spans.get(&Path::parse("a.b[1].c").unwrap()).unwrap();
    assert_eq!(s.end - s.start, "c: x".len());
}

const COMMENTED: &str = include_str!("commented.nova");

#[test]
fn preserving_format_keeps_comments_and_is_idempotent() {
    for profile in [Profile::Human, Profile::Canonical] {
        let once = novafile_text::format_preserving(COMMENTED, profile).unwrap();
        assert_eq!(parse(&once).unwrap(), parse(COMMENTED).unwrap(), "{once}");
        assert_eq!(novafile_text::format_preserving(&once, profile).unwrap(), once, "not idempotent:\n{once}");
        for c in COMMENTED.lines().filter_map(|l| l.find('#').map(|i| l[i..].trim())) {
            assert!(once.contains(c), "lost comment {c:?} in {profile:?}:\n{once}");
        }
    }

    let human = novafile_text::format_preserving(COMMENTED, Profile::Human).unwrap();
    assert!(human.contains("price: @decimal(\"12.50\")") && human.contains("ratio: 2/4"), "{human}");
    assert!(human.contains("    replicas: 3\n\n  - id: api  # public API\n"), "{human}");
    let canonical = novafile_text::format_preserving(COMMENTED, Profile::Canonical).unwrap();
    assert!(
        canonical.contains("price: @decimal(\"12.5\")") && canonical.contains("ratio: 1/2"),
        "{canonical}"
    );
    // Formatting a comment-free document matches the plain writer.
    let plain = to_text(&parse(HUMAN).unwrap(), Profile::Human);
    assert_eq!(novafile_text::format_preserving(&plain, Profile::Human).unwrap(), plain);
}

#[test]
fn render_preserving_follows_paths() {
    let mut v = parse(COMMENTED).unwrap();
    if let novafile_value::Value::Record(r) = &mut v {
        r.set("region", "eu-west-1".into());
        r.remove("price");
    }

    let out = novafile_text::render_preserving(COMMENTED, &v, Profile::Human).unwrap();
    assert!(
        out.contains("region: eu-west-1") && !out.contains("price") && out.contains("port: 8080  # legacy port"),
        "{out}"
    );
}

#[test]
fn language_versions() {
    assert!(parse("@nova 0.1\na: 1\n").is_ok());
    assert!(parse("@nova 0.2\na: 1\n").unwrap_err().to_string().contains("requires NOVA 0.2"));
    assert!(parse("@schema x@1.0.0\n@nova 0.1\na: 1\n").unwrap_err().to_string().contains("first directive"));
    assert!(parse("@nova 1\na: 1\n").unwrap_err().to_string().contains("MAJOR.MINOR"));
    assert!(parse_document("@nova 0.1\n@template 0.1\na: 1\n").unwrap().is_template());
    assert!(
        parse_document("@template 7.0\na: 1\n")
            .unwrap_err()
            .to_string()
            .contains("template language 7.0")
    );
    assert!(parse_document("@template\na: 1\n").is_ok());
}

#[test]
fn large_inputs_parse_in_linear_time() {
    // Regression test for quadratic list-item counting and duplicate-key checks:
    // with those bugs these took minutes; now they take well under a second.
    let start = std::time::Instant::now();
    let wide = (0..30_000).fold(String::new(), |mut s, i| {
        let _ = writeln!(s, "key_{i}: {i}");
        s
    });
    let long: String = std::iter::once("items:\n".to_string())
        .chain((0..30_000).map(|i| format!("  - id: x{i}\n    n: {i}\n")))
        .collect();
    let flow = format!("{{{}}}\n", (0..30_000).map(|i| format!("k{i}: {i}")).collect::<Vec<_>>().join(", "));
    for src in [&wide, &long, &flow] {
        let v = parse(src).unwrap();
        let _ = novafile_value::revision(&v);
        novafile_text::format_preserving(src, Profile::Human).unwrap();
    }

    assert!(start.elapsed() < std::time::Duration::from_secs(20), "took {:?}", start.elapsed());
}

#[test]
fn flow_continuation_lines_and_stream_splitting() {
    // Continuation lines must be indented; closing brackets may sit at the start column.
    assert!(parse("a: [\n  1,\n  2,\n]\nb: {\n  x: 1\n}\n").is_ok());
    assert!(parse("- [1,\n   2]\n").is_ok());
    let e = parse("a: [x,\n---\n]\n").unwrap_err().to_string();
    assert!(e.contains("indented deeper"), "{e}");
    // So `---` at column 1 always separates documents.
    let docs = split_stream("a: [x,\n  y]\n---\nb: 1\n");
    assert_eq!(docs.len(), 2);
    assert_eq!(parse(docs[0]).unwrap(), parse("a: [x, y]\n").unwrap());
}

#[test]
fn tab_after_dash_is_a_list_marker() {
    // Tabs are inline whitespace (spec §4.1), so `-\t` opens a list item like `- `.
    assert_eq!(parse("-\tx\n").unwrap(), Value::List(vec!["x".into()]));
    assert!(parse("-\t").is_err());
    let v = rec(vec![("a", "-\tx".into()), ("b", "-".into())]);
    for p in [Profile::Human, Profile::Compact] {
        let t = to_text(&v, p);
        assert_eq!(parse(&t).unwrap_or_else(|e| panic!("{e}\n{t}")), v, "{t}");
    }
}

#[test]
fn deep_template_conditions_hit_the_depth_limit() {
    // Plain `parse` reads the template before rejecting it, so these must fail cleanly rather than
    // overflow the stack.
    let nots = format!("@template\n@if {}true:\n  a: 1\n", "not ".repeat(20_000));
    let parens = format!("@template\n@if {}true{}:\n  a: 1\n", "(".repeat(5_000), ")".repeat(5_000));
    for src in [&nots, &parens] {
        let e = parse_document(src).unwrap_err().to_string();
        assert!(e.contains("depth"), "{e}");
        assert!(parse(src).is_err());
    }

    assert!(parse_document("@template\n@if not (not true):\n  a: 1\n@else if false:\n  b: 2\n").is_ok());

    // `@else if` arms are flat, so a long chain costs no depth.
    let elses = format!("@template\n@if true:\n  a: 1\n{}", "@else if true:\n  a: 1\n".repeat(20_000));
    assert!(parse_document(&elses).is_ok());
}

#[test]
fn key_like_strings_round_trip() {
    let header_key = rec(vec![(
        "k",
        Value::List(vec![rec(vec![("a}", Value::Int(1))]), rec(vec![("a}", Value::Int(2))])]),
    )]);
    for v in [
        rec(vec![("s", "a:#b".into()), ("l", Value::List(vec!["x:#".into()]))]),
        Value::List(vec!["a:#b".into()]),
        Value::List(vec![header_key.clone()]),
        header_key,
    ] {
        for p in [Profile::Human, Profile::Compact] {
            let t = to_text(&v, p);
            assert_eq!(parse(&t).unwrap_or_else(|e| panic!("{e}\n{t}")), v, "{t}");
        }
    }
}

#[test]
fn many_directives_columns_and_comments_stay_linear() {
    // Regression test for per-directive line counting, per-column duplicate checks and per-container
    // comment scans, each quadratic before.
    let start = std::time::Instant::now();
    let directives = format!("{}a: 1\n", "@x-a 1\n".repeat(80_000));
    assert_eq!(parse_document(&directives).unwrap().directives[79_999].line, 80_000);
    let cols: Vec<String> = (0..20_000).map(|i| format!("c{i}")).collect();
    let table = format!("t[1]{{{}}}:\n  {}\n", cols.join(","), vec!["1"; 20_000].join(","));
    parse(&table).unwrap();
    let wide = Value::List(vec![rec(cols.iter().map(|c| (c.as_str(), Value::Int(1))).collect()); 2]);
    let _ = to_text(&wide, Profile::Compact);
    let commented = (0..20_000).fold(String::new(), |mut s, i| {
        let _ = writeln!(s, "k{i}: [1]  # c{i}");
        s
    });
    novafile_text::format_preserving(&commented, Profile::Human).unwrap();
    assert!(start.elapsed() < std::time::Duration::from_secs(20), "took {:?}", start.elapsed());
}

/// One table row on its own, as NOVA Lines reads it: empty cells, tuples, `!_`, a
/// record cell, a leading `@timestamp(…)`, and a trailing empty cell.
#[test]
fn parse_row_cells() {
    use novafile_text::parse_row;
    let row = parse_row(r#"@timestamp("2026-11-17T16:00:02Z"), !warn, api, (GET, /health, !_), , {slow_ms: 950}"#).unwrap();
    assert_eq!(row.len(), 6);
    assert!(row[0].is_some() && row[4].is_none());
    assert_eq!(novafile_text::flow_string(row[3].as_ref().unwrap()), "(GET, /health, !_)");
    assert_eq!(parse_row("a, (7, []),").unwrap().len(), 3);
    assert_eq!(parse_row("a, b  # comment").unwrap().len(), 2);
    assert!(parse_row("a b").is_err());
    assert!(parse_row("a,\nb").is_err());
}

/// Column specs: nesting, `..`, quoting, and the validation rules.
#[test]
fn column_specs() {
    use novafile_text::{Column, Columns, parse_columns};
    let cols = parse_columns("{ts, level, req{method, path, status}, user{id, roles}, ..}").unwrap();
    assert_eq!(cols.len(), 5);
    assert_eq!(
        cols[2],
        Column::Nested(
            "req".into(),
            vec![Column::Field("method".into()), Column::Field("path".into()), Column::Field("status".into())]
        )
    );
    assert_eq!(cols[4], Column::Rest);
    assert_eq!(Columns(&cols).to_string(), "{ts, level, req{method, path, status}, user{id, roles}, ..}");
    assert_eq!(Columns(&parse_columns(r#"{"a b", c{..}}"#).unwrap()).to_string(), r#"{"a b", c{..}}"#);
    for bad in ["{a, a}", "{.., a}", "{a, .., ..}", "{a{}}", "{a, b{c, c}}", "{a} x", "{a"] {
        assert!(parse_columns(bad).is_err(), "{bad}");
    }
}

#[test]
fn table_row_count_errors_carry_a_fix() {
    let open = "rules[]{a, b}:\n  1, 2\n  3, 4\n  5, 6\nnext: 1\n";
    let e = parse(open).unwrap_err();
    assert_eq!(e.message, "table header needs a row count, found 3 rows; write [3]");
    assert_eq!(e.fix, Some((6..6, "3".into())));

    let few = "rules[5]{a}:\n  1\n  # a comment\n\n  2\n";
    let e = parse(few).unwrap_err();
    assert_eq!(e.message, "table header declares 5 rows, but 2 follow; write [2]");
    let many = "[1]{a}:\n  1\n  2\n";
    assert!(parse(many).unwrap_err().message.contains("but 2 follow"));

    let (fixed, n) = novafile_text::fix_syntax(&format!("{open}more[9]{{x}}:\n  1\n")).unwrap();
    assert_eq!(n, 2);
    assert!(fixed.starts_with("rules[3]{a, b}:") && fixed.contains("more[1]{x}:"), "{fixed}");
    assert!(parse(&fixed).is_ok());
    assert_eq!(novafile_text::fix_syntax("a: 1\n"), None);
    assert_eq!(novafile_text::fix_syntax("a: [\n"), None);
}

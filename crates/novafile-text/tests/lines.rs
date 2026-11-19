//! NOVA Lines (proposal 0002 §4.13): table rows, directives, segments and statuses.

use novafile_text::lines::{Mode, Reader, Status, WriteMode, read_str, write_string};
use novafile_text::{Column, decode_row, infer_columns, parse, parse_columns, parse_row};
use novafile_value::Value;

/// The proposal's nested-columns example.
const LOG: &str = r#"@nova 0.2
@columns {ts, level, service, req{method, path, status}, user{id, roles}, ..}
@timestamp("2026-11-17T16:00:00Z"), !error, api, (POST, /orders, 502), (42, [admin]), {error: {kind: upstream, msg: "timeout after 30s"}}
@timestamp("2026-11-17T16:00:01Z"), !info, auth, (GET, /login, 200), (7, []),
@timestamp("2026-11-17T16:00:02Z"), !warn, api, (GET, /health, !_), , {slow_ms: 950}
@end 3
"#;

/// Parse a flow value.
fn v(s: &str) -> Value {
    novafile_text::parse_flow(s).unwrap()
}

#[test]
fn proposal_rows_decode_to_the_stated_records() {
    let r = read_str(LOG, None);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(
        r.records,
        [
            v(
                r#"{ts: @timestamp("2026-11-17T16:00:00Z"), level: !error, service: api, req: {method: POST, path: /orders, status: 502}, user: {id: 42, roles: [admin]}, error: {kind: upstream, msg: "timeout after 30s"}}"#
            ),
            v(
                r#"{ts: @timestamp("2026-11-17T16:00:01Z"), level: !info, service: auth, req: {method: GET, path: /login, status: 200}, user: {id: 7, roles: []}}"#
            ),
            v(r#"{ts: @timestamp("2026-11-17T16:00:02Z"), level: !warn, service: api, req: {method: GET, path: /health}, slow_ms: 950}"#),
        ]
    );
    assert_eq!(r.segments.len(), 1);
    assert_eq!(r.segments[0].status, Status::Complete);
    assert_eq!(r.segments[0].count, 3);
}

#[test]
fn literal_cell_under_nested_column() {
    let cols = parse_columns("{id, req{method, path}}").unwrap();
    let rec = decode_row(&cols, parse_row(r#"1, "n/a""#).unwrap()).unwrap();
    assert_eq!(Value::Record(rec), v(r#"{id: 1, req: "n/a"}"#));
}

#[test]
fn row_errors() {
    let cols = parse_columns("{a, b{c, d}, ..}").unwrap();
    let err = |row: &str| decode_row(&cols, parse_row(row).unwrap()).unwrap_err();
    assert!(err("1, (2, 3)").contains("2 cells"), "cell count");
    assert!(err("1, (2, 3, 4), ").contains("3 elements"), "tuple arity");
    assert!(err("1, , {a: 2}").contains("declared column"), "rest repeats a column");
    assert!(err("1, , 5").contains("rest cell"), "rest is not a record");
    assert!(parse_columns("{.., a}").unwrap_err().message.contains("last"));
    assert!(parse_columns("{a, a}").unwrap_err().message.contains("duplicate"));
}

#[test]
fn statuses_and_segments() {
    let open = read_str("@nova 0.2\n{a: 1}\n", None);
    assert_eq!(open.segments[0].status, Status::Open);

    let corrupt = read_str("@nova 0.2\n{a: 1}\n@end 2\n", None);
    assert!(matches!(corrupt.segments[0].status, Status::Corrupt(_)));

    let cat = format!("{LOG}{LOG}");
    let r = read_str(&cat, None);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.records.len(), 6);
    assert_eq!(r.segments.len(), 2);
    assert!(r.segments.iter().all(|s| s.status == Status::Complete));

    let after_end = read_str("@nova 0.2\n{a: 1}\n@end 1\n{a: 2}\n", None);
    assert_eq!(after_end.errors.len(), 1);
    assert!(after_end.errors[0].message.contains("after @end"));
}

#[test]
fn version_is_checked() {
    assert_eq!(read_str("@nova 0.1\n1\n", None).errors.len(), 0);
    assert!(read_str("@nova 9.9\n1\n", None).errors[0].message.contains("requires NOVA 9.9"));
}

#[test]
fn key_is_unique_per_segment() {
    let r = read_str("@key id\n{id: 1}\n{id: 2}\n{id: 1}\n@nova 0.2\n@key id\n{id: 1}\n", None);
    assert_eq!(r.records.len(), 3);
    assert_eq!(r.errors.len(), 1);
    assert_eq!(r.errors[0].line, 4);
    assert!(r.errors[0].message.contains("duplicate"));
    assert!(read_str("@key a.b\n", None).errors[0].message.contains("top-level"));
}

#[test]
fn recovering_and_strict() {
    let src = "@nova 0.2\n{a: 1}\n{a: \n{a: 3}\n";
    let r = read_str(src, None);
    assert_eq!(r.records.len(), 2);
    assert_eq!(r.errors.len(), 1);
    assert_eq!(r.errors[0].line, 3);

    let strict: Vec<_> = Reader::new(src.as_bytes(), None).strict().collect();
    assert_eq!(strict.len(), 2, "one record, then the error, then nothing");
}

#[test]
fn timestamp_row_is_not_a_directive() {
    let r = read_str("@columns {ts, n}\n@timestamp(\"2026-11-17T16:00:00Z\"), 1\n", None);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.records.len(), 1);
}

#[test]
fn positional_needs_a_schema() {
    let r = read_str("@schema x@1.0.0#0000000000000000\n@mode positional\n(1)\n", None);
    assert!(r.errors[0].message.contains("needs the schema"));
}

#[test]
fn writers_round_trip() {
    let records = parse(
        "- {id: 1, req: {m: GET, p: /a}, tags: [x]}\n- {id: 2, req: {m: POST, p: /b}}\n- {id: 3, req: {m: GET, p: /c}, rare: (1, 2)}\n- {id: 4, req: \"n/a\"}\n",
    )
    .unwrap()
    .as_list()
    .unwrap()
    .clone();

    let cols = infer_columns(&records).unwrap();
    assert_eq!(
        cols,
        [Column::Field("id".into()), Column::Field("req".into()), Column::Rest],
        "req is not all records, so it stays plain"
    );

    for mode in [WriteMode::Values, WriteMode::Table] {
        let text = write_string(&records, mode, None, None, Some("id")).unwrap();
        let back = read_str(&text, None);
        assert!(back.errors.is_empty(), "{text}\n{:?}", back.errors);
        assert_eq!(back.records, records, "{text}");
        assert_eq!(back.segments[0].status, Status::Complete);
    }

    let nested = write_string(&records[..3], WriteMode::Table, None, None, None).unwrap();
    assert!(nested.contains("@columns {id, req{m, p}, ..}"), "{nested}");
    assert_eq!(read_str(&nested, None).records, records[..3]);
}

#[test]
fn inference_falls_back_to_value_mode() {
    assert_eq!(infer_columns(&[v("1"), v("{a: 1}")]), None);
    // A one-column row with an empty cell would be a blank line.
    assert_eq!(infer_columns(&[v("{a: 1}"), v("{}")]), None);
    let text = write_string(&[v("{a: 1}"), v("{}")], WriteMode::Table, None, None, None).unwrap();
    assert!(!text.contains("@columns"), "{text}");
    let back = read_str(&text, None);
    assert_eq!(back.records.len(), 2);
    assert_eq!(back.segments[0].header.mode, Mode::Values);
}

/// An invalid header skips only its own segment's records.
#[test]
fn invalid_header_skips_only_its_segment() {
    let r = read_str("@nova 0.2\n@columns {a, a}\n1\n@nova 0.2\n@columns {a}\n2\n", None);
    assert_eq!(r.records, [v("{a: 2}")], "{:?}", r.errors);
    assert_eq!(r.segments.len(), 2);
}

//! Property tests: `from_str_with_schema` accepts a document's `@schema` reference exactly when
//! spec §6.7 says the schema can read it.

use proptest::prelude::*;
use serde::Deserialize;

/// The document type every case loads.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Settings {
    name: Option<String>,
}

/// A release version; small parts so equal and neighbouring versions come up often.
fn version() -> impl Strategy<Value = (u64, u64, u64)> {
    (0..3u64, 0..3u64, 0..3u64)
}

/// Spec §6.7, written out independently of `Version::can_read`: 0.0.z reads only itself,
/// 0.y.z reads the same minor up to itself, x.y.z (x ≥ 1) the same major up to itself.
fn readable(schema: (u64, u64, u64), doc: (u64, u64, u64)) -> bool {
    if (schema.0, schema.1) == (0, 0) || (doc.0, doc.1) == (0, 0) {
        return schema == doc;
    }

    schema.0 == doc.0 && (schema.0 > 0 || schema.1 == doc.1) && doc <= schema
}

proptest! {
    #[test]
    fn schema_reference_is_checked(
        mine in version(),
        doc in version(),
        same_name: bool,
        other_lines in prop::sample::select(vec!["", "@nova 0.1\n"]),
    ) {
        let schema = novafile_schema::parse_schema(&format!(
            "@schema example.org/app@{}.{}.{}\n@root Settings\nrecord Settings:\n  name?: string\n",
            mine.0, mine.1, mine.2
        ))
        .unwrap();
        let name = if same_name { "example.org/app" } else { "example.org/other" };
        let text = format!("{other_lines}@schema {name}@{}.{}.{}\nname: x\n", doc.0, doc.1, doc.2);
        let got = novafile_serde::from_str_with_schema::<Settings>(&text, &schema);
        prop_assert_eq!(got.is_ok(), same_name && readable(mine, doc), "{}: {:?}", text, got);
    }
}

//! `#[derive(NovaSchema)]`: derived schemas match hand-written ones and accept serde output.

use novafile_schema::{NovaSchema, parse_schema};
use novafile_value::Timestamp;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// The Rust twin of examples/infrastructure.novs.
#[derive(NovaSchema, Serialize)]
#[nova(name = "infrastructure", version = "1.0.0")]
struct Infrastructure {
    #[nova(id = 1)]
    environment: String,
    #[nova(id = 2)]
    region: String,
    #[nova(id = 3)]
    deployed_at: Timestamp,
    #[nova(id = 4, default = "!active")]
    status: Status,
    #[nova(id = 5)]
    services: Vec<Service>,
}

#[derive(NovaSchema, Serialize)]
struct Service {
    #[nova(key, id = 1)]
    id: String,
    #[nova(len = "1..", id = 2)]
    image: String,
    #[nova(id = 3)]
    port: Port,
    #[nova(range = "0..100", id = 4)]
    replicas: u16,
    #[nova(id = 5, default = "true")]
    enabled: bool,
    #[nova(id = 6)]
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<BTreeSet<String>>,
}

#[derive(NovaSchema, Serialize)]
struct Port(#[nova(range = "1..65535")] u16);

#[derive(NovaSchema, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
enum Status {
    Active,
    Draining,
    Failed(String),
}

#[test]
fn derived_schema_matches_the_handwritten_one() {
    let derived = Infrastructure::novafile_schema();
    let written = parse_schema(include_str!("../../../examples/infrastructure.novs")).unwrap();
    // `tags: Option<BTreeSet<String>>` is `tags?: set<string>?` (Option may be null); the
    // hand-written schema says `tags?: set<string>`. Everything else is identical.
    let text = derived.to_canonical_text();
    assert!(text.contains("  tags?: set<string>? @id(6)\n"), "{text}");
    let expected = written
        .to_canonical_text()
        .replace("  tags?: set<string> @id(6)\n", "  tags?: set<string>? @id(6)\n");
    assert_eq!(text, expected);
}

#[test]
fn serde_values_resolve_against_the_derived_schema() {
    let schema = Infrastructure::novafile_schema();
    let infra = Infrastructure {
        environment: "production".into(),
        region: "eu-central-1".into(),
        deployed_at: Timestamp::parse("2026-11-17T16:00:00Z").unwrap(),
        status: Status::Failed("oom".into()),
        services: vec![
            Service {
                id: "auth".into(),
                image: "a".into(),
                port: Port(8080),
                replicas: 3,
                enabled: true,
                tags: None,
            },
            Service {
                id: "api".into(),
                image: "b".into(),
                port: Port(3000),
                replicas: 5,
                enabled: false,
                tags: Some(["edge".into()].into()),
            },
        ],
    };

    let v = novafile_serde::to_value(&infra).unwrap();
    schema.resolve(&v).unwrap_or_else(|e| panic!("{e:?}"));
    // Constraints from attributes are enforced.
    let bad = novafile_serde::to_value(&Infrastructure {
        services: vec![Service {
            id: "x".into(),
            image: String::new(),
            port: Port(0),
            replicas: 101,
            enabled: true,
            tags: None,
        }],
        ..infra
    })
    .unwrap();
    let issues: Vec<String> = schema.resolve(&bad).unwrap_err().iter().map(std::string::ToString::to_string).collect();
    assert_eq!(issues.len(), 3, "{issues:?}");
}

#[derive(NovaSchema, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct Shapes {
    first_name: String,
    #[serde(rename = "renamed")]
    other: i64,
    #[serde(default)]
    maybe: u8,
    #[serde(skip)]
    hidden: String,
    #[deprecated]
    old: bool,
    point: (f64, f64),
    grid: [u8; 3],
    labels: BTreeMap<String, String>,
    by_id: BTreeMap<u32, String>,
    nested: Vec<Vec<char>>,
    shape: Shape,
    anything: novafile_value::Value,
}

#[derive(NovaSchema, Serialize)]
#[allow(dead_code)]
enum Shape {
    Circle { r: f64 },
    Pair(u8, u8),
    Empty,
}

#[test]
fn mapping_of_rust_types() {
    let text = Shapes::novafile_schema().to_canonical_text();
    for line in [
        "  firstName: string\n",
        "  renamed: i64\n",
        "  maybe?: u8\n",
        "  old: bool @deprecated\n",
        "  point: (float, float)\n",
        "  grid: (u8, u8, u8)\n",
        "  labels: map<string, string>\n",
        "  byId: map<u32, string>\n",
        "  nested: list<list<string @len(1..1)>>\n",
        "  shape: Shape\n",
        "  anything: any\n",
        "enum Shape:\n  Circle(ShapeCircle)\n  Pair((u8, u8))\n  Empty\n",
        "record ShapeCircle:\n  r: float\n",
    ] {
        assert!(text.contains(line), "missing {line:?} in\n{text}");
    }

    assert!(!text.contains("hidden"));
    // The rendered schema is valid schema text.
    parse_schema(&text).unwrap();
}

#[derive(NovaSchema)]
#[allow(dead_code)]
struct Tree {
    label: String,
    children: Vec<Tree>,
}

#[test]
fn recursive_types() {
    let text = Tree::novafile_schema().to_canonical_text();
    assert!(text.contains("record Tree:\n  label: string\n  children: list<Tree>\n"), "{text}");
}

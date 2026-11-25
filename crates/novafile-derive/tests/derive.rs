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

#[derive(NovaSchema)]
#[allow(dead_code)]
struct Chain(Option<Box<Chain>>);

#[test]
#[should_panic(expected = "cycle")]
fn recursive_aliases_fail_instead_of_hanging() {
    // `type Chain = Chain?` never reaches a type, like the same alias in a `.novs`.
    let _ = Chain::novafile_schema();
}

#[derive(NovaSchema)]
#[allow(dead_code)]
struct Big {
    n: u128,
}

#[test]
fn u128_is_non_negative() {
    let s = Big::novafile_schema();
    assert!(s.to_canonical_text().contains("  n: int @range(0..)\n"), "{}", s.to_canonical_text());
    assert!(s.resolve(&novafile_text::parse("n: -1\n").unwrap()).is_err());
    assert!(s.resolve(&novafile_text::parse("n: 1\n").unwrap()).is_ok());
}

/// A user type named `Root` keeps its name when the root needs an alias.
#[test]
fn root_alias_does_not_collide_with_a_type_named_root() {
    #[derive(NovaSchema)]
    #[allow(dead_code)]
    struct Root {
        a: u8,
    }

    let s = novafile_schema::schema_for::<Vec<Root>>();
    assert_eq!(s.defs.iter().filter(|(n, _)| n == "Root").count(), 1, "{s}");
    let doc = novafile_text::parse("- a: 1\n").unwrap();
    assert!(s.resolve(&doc).is_ok(), "{s}");
}

/// A foreign type with serde support but no `NovaSchema` impl (like `url::Url`).
#[derive(serde::Serialize)]
struct Foreign(String);

/// `secret`/`personal` (#29), std types and `type` overrides (#30).
#[derive(NovaSchema, Serialize)]
#[nova(name = "example.org/app", version = "1.0.0")]
struct Database {
    #[nova(secret)]
    url: String,
    #[nova(personal, len = "1..")]
    contact_email: String,
    #[nova(type = "string @len(1..)", secret)]
    dsn: Foreign,
    #[nova(type = "string")]
    #[serde(skip_serializing_if = "Option::is_none")]
    backup: Option<Foreign>,
    data_dir: std::path::PathBuf,
    listen: std::net::SocketAddr,
    peers: Vec<std::net::IpAddr>,
}

#[test]
fn classes_std_types_and_type_overrides() {
    let s = Database::novafile_schema();
    let text = s.to_canonical_text();
    for line in [
        "  url: string @secret\n",
        "  contact_email: string @len(1..) @personal\n",
        "  dsn: string @len(1..) @secret\n",
        "  backup?: string?\n",
        "  data_dir: string\n",
        "  listen: string\n",
        "  peers: list<string>\n",
    ] {
        assert!(text.contains(line), "{line:?} missing in\n{text}");
    }

    // The derived schema passes the checks a hand-written one does.
    parse_schema(&text).unwrap();

    let db = Database {
        url: "postgres://u:hunter2@db".into(),
        contact_email: "ada@example.org".into(),
        dsn: Foreign("host=db".into()),
        backup: None,
        data_dir: "/var/lib/app".into(),
        listen: "127.0.0.1:8080".parse().unwrap(),
        peers: vec!["::1".parse().unwrap()],
    };
    let v = novafile_serde::to_value(&db).unwrap();
    let (_, marks) = s.classify(&v).unwrap();
    let classified: Vec<String> = marks.iter().map(|(p, c)| format!("{p} {c}")).collect();
    assert_eq!(classified.len(), 3, "{classified:?}");
}

#[derive(NovaSchema)]
#[allow(dead_code)]
struct SecretKey {
    #[nova(key, type = "string @secret")]
    id: Foreign,
}

/// A classified key, which `#[nova(key, secret)]` rejects at compile time, is also caught
/// when the class comes from a `type` override.
#[test]
#[should_panic(expected = "@key field cannot have a @secret type")]
fn classified_keys_are_rejected() {
    let _ = SecretKey::novafile_schema();
}

#[derive(NovaSchema)]
#[allow(dead_code)]
struct Dangling {
    #[nova(type = "Endpoint")]
    endpoint: Foreign,
}

#[test]
#[should_panic(expected = "unknown type Endpoint")]
fn type_overrides_must_name_known_types() {
    let _ = Dangling::novafile_schema();
}

/// A derived `@key` field must be required, as in a parsed schema.
#[test]
#[should_panic(expected = "@key fields must be required and non-null")]
fn optional_key_is_rejected() {
    #[derive(NovaSchema, Serialize)]
    struct K {
        #[nova(key)]
        id: Option<String>,
    }

    let _ = K::novafile_schema();
}

/// Two derived fields cannot share an id, as in a parsed schema.
#[test]
#[should_panic(expected = "duplicate field id 1")]
fn duplicate_field_id_is_rejected() {
    #[derive(NovaSchema, Serialize)]
    struct D {
        #[nova(id = 1)]
        a: String,
        #[nova(id = 1)]
        b: String,
    }

    let _ = D::novafile_schema();
}

/// Two derived fields cannot share a name, as in a parsed schema.
#[test]
#[should_panic(expected = "duplicate field \"p\"")]
fn duplicate_field_name_is_rejected() {
    #[derive(NovaSchema, Serialize)]
    struct D {
        #[nova(rename = "p")]
        a: String,
        #[nova(rename = "p")]
        b: String,
    }

    let _ = D::novafile_schema();
}

/// An enum without variants has no schema, as in a parsed schema.
#[test]
#[should_panic(expected = "has no variants")]
fn empty_enum_is_rejected() {
    #[derive(NovaSchema)]
    enum Never {}

    let _ = Never::novafile_schema();
}

/// Two Rust types with one name do not merge into one definition.
#[test]
#[should_panic(expected = "both map to the schema name Item")]
fn same_named_types_are_rejected() {
    mod a {
        #[derive(novafile_schema::NovaSchema, serde::Serialize)]
        pub struct Item {
            pub x: String,
        }
    }

    mod b {
        #[derive(novafile_schema::NovaSchema, serde::Serialize)]
        pub struct Item {
            pub y: i64,
        }
    }

    #[derive(NovaSchema, Serialize)]
    struct Top {
        first: a::Item,
        second: b::Item,
    }

    let _ = Top::novafile_schema();
}

/// Check that serde's output for `value` resolves against `T`'s derived schema.
fn assert_serde_resolves<T: NovaSchema + Serialize>(value: &T) {
    let schema = T::novafile_schema();
    let v = novafile_serde::to_value(value).unwrap();
    schema.resolve(&v).unwrap_or_else(|e| panic!("{schema}\n{v:?}\n{e:?}"));
}

/// Every `rename_all` rule names fields exactly as serde does, odd underscores included.
#[test]
fn rename_all_matches_serde_for_fields() {
    macro_rules! check_rule {
        ($rule:literal) => {{
            #[derive(NovaSchema, Serialize)]
            #[serde(rename_all = $rule)]
            struct R {
                user_name: String,
                _x: String,
                #[expect(non_snake_case, reason = "the test needs a double underscore, which serde keeps as is")]
                a__b: String,
            }

            assert_serde_resolves(&R {
                user_name: "u".into(),
                _x: "x".into(),
                a__b: "b".into(),
            });
        }};
    }

    check_rule!("lowercase");
    check_rule!("UPPERCASE");
    check_rule!("PascalCase");
    check_rule!("camelCase");
    check_rule!("snake_case");
    check_rule!("SCREAMING_SNAKE_CASE");
    check_rule!("kebab-case");
    check_rule!("SCREAMING-KEBAB-CASE");
}

/// Variant names, a variant's own `rename_all` and `rename_all_fields` follow serde.
#[test]
fn rename_rules_match_serde_for_variants() {
    #[derive(NovaSchema, Serialize)]
    #[serde(rename_all = "snake_case", rename_all_fields = "camelCase")]
    enum E {
        HttpServer {
            inner_field: String,
        },
        #[serde(rename_all = "kebab-case")]
        GrpcServer {
            inner_field: String,
        },
    }

    #[derive(NovaSchema, Serialize)]
    struct Top {
        a: E,
        b: E,
    }

    assert_serde_resolves(&Top {
        a: E::HttpServer { inner_field: "a".into() },
        b: E::GrpcServer { inner_field: "b".into() },
    });
}

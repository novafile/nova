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

/// `not_null` keeps an `Option` field optional but rejects `null` (#33).
#[test]
fn not_null_options_are_optional_but_not_nullable() {
    #[derive(NovaSchema, Serialize)]
    #[allow(dead_code)]
    struct Db {
        #[nova(not_null)]
        url: Option<String>,
        #[nova(not_null, type = "string @len(1..)")]
        dsn: Option<Foreign>,
        backup: Option<String>,
    }

    let s = Db::novafile_schema();
    let text = s.to_canonical_text();
    for line in ["  url?: string\n", "  dsn?: string @len(1..)\n", "  backup?: string?\n"] {
        assert!(text.contains(line), "{line:?} missing in\n{text}");
    }

    let check = |src: &str| s.validate(&novafile_text::parse(src).unwrap());
    check("backup: null\n").unwrap();
    check("url: \"postgres://db\"\n").unwrap();
    assert!(check("url: null\n").is_err());
}

/// Doc comments become documentation; `#[nova(doc)]` replaces or removes it (#34).
#[test]
fn doc_comments_document_the_schema() {
    /// Which database to use.
    ///
    /// See [`Db::url`].
    #[derive(NovaSchema, Serialize)]
    #[nova(doc = "Which database to use.")]
    #[allow(dead_code)]
    struct Db {
        /// The database to connect to,
        /// as a PostgreSQL URL.
        url: Option<String>,
        /// Rust-only detail.
        #[nova(doc = "")]
        pool: u8,
        mode: Mode,
    }

    /// How to open the database.
    #[derive(NovaSchema, Serialize)]
    #[allow(dead_code)]
    enum Mode {
        /// Read only.
        Ro,
        Rw,
    }

    let s = Db::novafile_schema();
    let text = s.to_string();
    for part in [
        "# Which database to use.\nrecord Db:\n  # The database to connect to,\n  # as a PostgreSQL URL.\n  url?: string?\n  pool: u8\n",
        "# How to open the database.\nenum Mode:\n  # Read only.\n  Ro\n  Rw\n",
    ] {
        assert!(text.contains(part), "{part:?} missing in\n{text}");
    }

    assert_eq!(parse_schema(&text).unwrap().to_string(), text);
}

/// Layered configuration (#35): each layer is checked partially, the merged value in
/// full, with the record rules of `together`, `one_of` and `any_of`.
#[test]
fn layered_configuration_with_record_rules() {
    // `#[serde(default)]` on the struct lets a layer leave `name` out, while the
    // schema still requires it in the merged value.
    #[derive(NovaSchema, Serialize, serde::Deserialize, Default)]
    #[serde(default)]
    #[nova(one_of = "url, url_file", together = "user, password")]
    struct Db {
        name: String,
        #[nova(not_null)]
        #[serde(skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[nova(not_null, rename = "url-file")]
        #[serde(skip_serializing_if = "Option::is_none", rename = "url-file")]
        url_file: Option<String>,
        user: Option<String>,
        password: Option<String>,
    }

    let s = Db::novafile_schema();
    assert!(s.to_canonical_text().contains("record Db @one_of(url, url-file) @together(user, password):\n"));

    // A file layer without `name` (it comes from the environment) passes the partial check only.
    let file = "url: \"postgres://db\"\nuser: ada\npassword: pw\n";
    assert!(novafile_serde::from_str_with_schema::<Db>(file, &s).is_err());
    let layer: Db = novafile_serde::from_str_with_schema_partial(file, &s).unwrap();
    assert!(novafile_serde::from_str_with_schema_partial::<Db>("url: 1\n", &s).is_err());

    // The merged value is checked in full, through `to_value`. `None` written as
    // `null` counts as absent, so `user`/`password` need no `skip_serializing_if`.
    let merged = Db { name: "app".into(), ..layer };
    s.resolve(&novafile_serde::to_value(&merged).unwrap()).unwrap();
    let both = Db {
        url_file: Some("/run/db".into()),
        ..merged
    };
    let issues = s.resolve(&novafile_serde::to_value(&both).unwrap()).unwrap_err();
    assert_eq!(issues[0].message, "@one_of(url, url-file): 2 present (url, url-file); expected exactly 1");
    let half = Db {
        name: "app".into(),
        url: Some("x".into()),
        user: Some("ada".into()),
        ..Db::default()
    };
    assert!(s.resolve(&novafile_serde::to_value(&half).unwrap()).is_err());
}

/// A rule that names a required field fails like it does in schema text.
#[test]
#[should_panic(expected = "must be optional")]
fn record_rules_need_optional_fields() {
    #[derive(NovaSchema)]
    #[nova(any_of = "a, b")]
    #[allow(dead_code)]
    struct R {
        a: String,
        b: Option<String>,
    }

    let _ = R::novafile_schema();
}

/// A field refining a standard library type, as `#[nova(type = "std.…")]`.
#[derive(NovaSchema, Serialize)]
struct Upstream {
    /// Base URL of the upstream service, origin only.
    #[nova(type = r#"std.Uri @pattern("https?://[^/?#@]+/?")"#)]
    address: String,
}

/// The same, with the standard library version pinned.
#[derive(NovaSchema, Serialize)]
#[nova(import = "novafile.dev/std@0.1.0 as std")]
struct PinnedUpstream {
    #[nova(type = "std.Uri")]
    address: String,
}

/// An import of a schema that is not bundled.
#[derive(NovaSchema, Serialize)]
#[nova(import = "acme.test/common@1.0.0 as c")]
struct Unbundled {
    id: String,
}

/// `std.*` in a derived schema imports the bundled standard library (issue #37): the rendered
/// schema carries the `@import`, parses back to the same fingerprint, and checks both the std
/// type and the local refinement.
#[test]
fn derived_schemas_import_the_standard_library() {
    let s = Upstream::novafile_schema();
    let text = s.to_string();
    assert!(text.contains("@import novafile.dev/std@0.1.0#"), "{text}");
    assert!(text.contains(" as std\n"), "{text}");
    assert_eq!(parse_schema(&text).unwrap().fingerprint(), s.fingerprint());

    let check = |doc: &str| s.resolve(&novafile_text::parse(doc).unwrap()).is_ok();
    assert!(check("address: \"https://api.example.com\"\n"));
    assert!(!check("address: \"https://api.example.com/path\"\n"), "the local @pattern narrows");
    assert!(!check("address: \"https://exa mple.com\"\n"), "std.Uri rejects a space");

    // Without a `std.*` reference there is no import.
    assert!(!Infrastructure::novafile_schema().to_string().contains("@import"));

    // An explicit import pins the version and is not added twice.
    let pinned = PinnedUpstream::novafile_schema();
    assert_eq!(pinned.imports.len(), 1);
    assert_eq!(pinned.imports[0].reference.version.to_string(), "0.1.0");
}

#[test]
#[should_panic(expected = "can only import a bundled schema")]
fn derived_schemas_import_only_bundled_schemas() {
    let _ = Unbundled::novafile_schema();
}

/// Foreign types mapped to standard library types (issue #38).
#[derive(NovaSchema)]
#[expect(dead_code, reason = "only the schema is used")]
struct Client {
    endpoint: url::Url,
    id: uuid::Uuid,
    version: Option<semver::Version>,
    /// `type` still overrides the mapping, and `len` refines it.
    #[nova(type = "string")]
    plain: url::Url,
    #[nova(len = "..40")]
    short: url::Url,
}

/// `url::Url`, `uuid::Uuid` and `semver::Version` derive as `std.Uri`, `std.Uuid` and
/// `std.SemVer`, and what those types write passes the schema.
#[test]
fn foreign_types_map_to_the_standard_library() {
    let s = Client::novafile_schema();
    let text = s.to_string();
    for field in [
        "endpoint: std.Uri",
        "id: std.Uuid",
        "version?: std.SemVer?",
        "plain: string",
        "short: std.Uri @len(..40)",
    ] {
        assert!(text.contains(field), "{field} in\n{text}");
    }
    assert!(text.contains("@import novafile.dev/std@0.1.0#"), "{text}");

    let url = |s: &str| url::Url::parse(s).unwrap().to_string();
    let doc = |endpoint: &str, id: &str| {
        let r = novafile_value::Record::from_entries(vec![
            ("endpoint".into(), endpoint.into()),
            ("id".into(), id.into()),
            ("version".into(), semver::Version::parse("1.2.3-rc.1+build.5").unwrap().to_string().into()),
            ("plain".into(), "not a url".into()),
            ("short".into(), url("https://a.example").into()),
        ])
        .unwrap();
        s.resolve(&novafile_value::Value::Record(r))
    };

    let id = uuid::Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef).to_string();
    // `Url` writes these as URIs: it percent-encodes spaces and writes Unicode hosts as punycode.
    for written in [url(" https://bücher.example/a b?q=1#f "), url("http://[::1]:8080/"), url("file:///tmp/x")] {
        doc(&written, &id).unwrap_or_else(|e| panic!("{written}: {e:?}"));
    }

    // Input `Url` would read is stricter here: unencoded spaces, Unicode hosts, upper-case UUIDs.
    assert!(doc("https://exa mple.com", &id).is_err());
    assert!(doc("https://bücher.example", &id).is_err());
    assert!(doc(&url("https://a.example"), &id.to_uppercase()).is_err());

    // The boundary: from text that is not RFC 3986, `Url` keeps characters a URI may not hold
    // (`[`, `]`, `|`, `{` in a query), so its output fails `std.Uri`. Such fields take
    // `#[nova(type = "string")]`.
    let whatwg = url("https://a.example/?filter[a]=b|c{d}");
    assert_eq!(whatwg, "https://a.example/?filter[a]=b|c{d}");
    assert!(doc(&whatwg, &id).is_err());
}

/// A PDDL-like formula: operators as variants renamed to tags that are not identifiers.
#[derive(NovaSchema, Serialize, serde::Deserialize, Debug, PartialEq)]
enum Formula {
    #[serde(rename = "and")]
    And(Vec<Formula>),
    #[serde(rename = "=")]
    Eq(String, String),
    #[serde(rename = "at start")]
    AtStart(Box<Formula>),
    #[serde(rename = "true")]
    True,
}

/// Renamed variants derive quoted variant names (spec §6.1, issue novafile/spec#1), and what
/// `novafile-serde` writes resolves against the derived schema and reads back.
#[test]
fn renamed_variants_derive_quoted_names() {
    let s = Formula::novafile_schema();
    let text = s.to_string();
    for line in ["  and(list<Formula>)\n", "  \"=\"((string, string))\n", "  \"at start\"(Formula)\n", "  true\n"] {
        assert!(text.contains(line), "{line:?} in\n{text}");
    }

    let f = Formula::And(vec![Formula::AtStart(Box::new(Formula::Eq("?x".into(), "a".into()))), Formula::True]);
    let doc = novafile_serde::to_string(&f).unwrap();
    assert!(doc.contains("!\"at start\""), "{doc}");
    let back: Formula = novafile_serde::from_str_with_schema(&doc, &s).unwrap();
    assert_eq!(back, f);
}

/// An enum whose rename would make a variant named like a redaction placeholder.
#[derive(NovaSchema, Serialize)]
#[expect(dead_code, reason = "only the schema is derived")]
enum Forged {
    #[serde(rename = "@redacted")]
    Marker,
}

/// A derived enum cannot declare an `@` variant: resolution would turn the string
/// `@redacted` into a placeholder (spec §2.11).
#[test]
#[should_panic(expected = "reserved")]
fn derived_variants_cannot_use_reserved_tags() {
    let _ = Forged::novafile_schema();
}

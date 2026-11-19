//! Lint rules, `@x-lint` configuration and fixes.

use novafile_lint::{Config, Level, apply_fixes, lint_document, lint_schema};

const DOC: &str = "# config\nserviceName: api   # the name\nhttpPort: 8080\nuser_id: 1\nuserId: 2\n\"display name\": x\nstartedAt: 2026-11-17T16:00:00Z\nmixed: [1, two, 3]\nnested:\n  maxRetries: 3\n";

#[test]
fn document_rules() {
    let f = lint_document(DOC, &Config::default()).unwrap();
    let rules: Vec<(&str, Level)> = f.iter().map(|f| (f.rule, f.level)).collect();
    assert_eq!(
        rules,
        [
            ("key-case", Level::Warning),
            ("key-case", Level::Warning),
            ("key-case", Level::Warning),
            ("key-collision", Level::Warning),
            ("quoted-key", Level::Hint),
            ("key-case", Level::Warning),
            ("untyped-timestamp", Level::Hint),
            ("mixed-list", Level::Hint),
            ("key-case", Level::Warning),
        ],
        "{f:#?}"
    );
    assert!(f.iter().any(|x| x.message.contains("`userId`") && x.message.contains("not fixed")));
}

#[test]
fn fixes_keep_comments_and_value() {
    let f = lint_document(DOC, &Config::default()).unwrap();
    let fixed = apply_fixes(DOC, &f);
    assert!(fixed.contains("service_name: api   # the name\nhttp_port: 8080\n"), "{fixed}");
    assert!(fixed.contains("started_at: @timestamp(\"2026-11-17T16:00:00Z\")"), "{fixed}");
    assert!(fixed.contains("  max_retries: 3"), "{fixed}");
    assert!(fixed.contains("userId: 2"), "colliding key must not be renamed:\n{fixed}");
    // Fixing is idempotent: only unfixable findings remain.
    let again = lint_document(&fixed, &Config::default()).unwrap();
    assert!(again.iter().all(|f| f.fix.is_none()), "{again:#?}");
}

#[test]
fn fixes_never_rename_two_keys_to_one() {
    let src = "fooBar: 1\nfoo-bar: 2\n";
    let f = lint_document(src, &Config::default()).unwrap();
    assert!(f.iter().all(|f| f.fix.is_none()), "{f:#?}");
    assert_eq!(apply_fixes(src, &f), src);
}

#[test]
fn configuration() {
    let camel = Config::default().with_directive("case=camel allow=quoted-key,mixed-list").unwrap();
    let f = lint_document(DOC, &camel).unwrap();
    assert!(f.iter().all(|f| f.rule != "quoted-key" && f.rule != "mixed-list"));
    assert!(f.iter().any(|f| f.rule == "key-case" && f.message.contains("`user_id` is not camelCase")));
    // In-document configuration and schema-governed documents.
    let f = lint_document("@x-lint allow=key-case\nfooBar: 1\n", &Config::default()).unwrap();
    assert_eq!(f, [] as [novafile_lint::Finding; 0]);
    let f = lint_document("@schema x@1.0.0\nfooBar: 2026-11-17T16:00:00Z\n", &Config::default()).unwrap();
    assert!(f.is_empty(), "{f:#?}");
}

#[test]
fn schema_rules() {
    let src = "@schema x@1.0.0\n@root R\nrecord R:\n  ok_field: int @id(1)\n  badField: int\nenum S:\n  Active\n  paused\n";
    let f = lint_schema(src, &Config::default()).unwrap();
    let got: Vec<(&str, &str)> = f.iter().map(|f| (f.rule, &src[f.start..f.end])).collect();
    assert_eq!(
        got,
        [("partial-ids", "record R"), ("field-case", "badField"), ("variant-case", "Active")],
        "{f:#?}"
    );
    assert_eq!(
        lint_schema(include_str!("../../../examples/infrastructure.novs"), &Config::default()).unwrap(),
        [] as [novafile_lint::Finding; 0]
    );
}

#[test]
fn union_order() {
    let src = "@root R\ntype T = string | timestamp\nrecord R:\n  \"at\": timestamp | string\n  ok: int | string\n";
    let f = lint_schema(src, &Config::default()).unwrap();
    let got: Vec<(&str, &str)> = f.iter().map(|f| (f.rule, &src[f.start..f.end])).collect();
    assert_eq!(got, [("union-order", "type T"), ("union-order", "  \"at\"")], "{f:#?}");
    assert!(f[1].message.starts_with("R.at: `timestamp` captures"), "{}", f[1].message);
    let quiet = Config {
        allow: vec!["union-order".into()],
        ..Config::default()
    };
    assert_eq!(lint_schema(src, &quiet).unwrap(), [] as [novafile_lint::Finding; 0]);
}

#[test]
fn compat_mode() {
    let src = "@compat json\nuserId: 1\nprice: @decimal(\"1.50\")\ntags: @set[a, b]\nok: [1, 2]\n";
    let f = lint_document(src, &Config::default()).unwrap();
    let got: Vec<(&str, &str)> = f.iter().map(|f| (f.rule, &src[f.start..f.end])).collect();
    assert_eq!(got, [("compat-loss", "@decimal(\"1.50\")"), ("compat-loss", "@set[a, b]")], "{f:#?}");
    assert!(
        lint_document("@compat toml\na: null\n", &Config::default()).unwrap()[0]
            .message
            .contains("null")
    );
    assert!(lint_document("@compat xml\na: 1\n", &Config::default()).is_err());
}

#[test]
fn schema_patterns_literals_and_x_lint() {
    let src = "@schema pddl@0.1.0\n@x-lint case=kebab\n@root R\ntype Name = string @pattern(\"[A-Za-z][A-Za-z0-9_-]*\")\nrecord R:\n  scale-up: int\n  m: Name | \"total-time\"\n  d: \"a\" | \"a\"\n  c: string @pattern(\"^[a-z]+$\")\n";
    let f = lint_schema(src, &Config::default()).unwrap();
    let got: Vec<(&str, &str)> = f.iter().map(|f| (f.rule, &src[f.start..f.end])).collect();
    assert_eq!(
        got,
        [("union-order", "  m"), ("union-order", "  d"), ("pattern-anchor", "\"^[a-z]+$\"")],
        "{f:#?}"
    );
    assert!(f[0].message.contains("`\"total-time\"` is unreachable"), "{}", f[0].message);
    // Without the directive, `scale-up` is not snake case.
    let plain = src.replace("@x-lint case=kebab\n", "");
    assert!(lint_schema(&plain, &Config::default()).unwrap().iter().any(|f| f.rule == "field-case"));
    assert!(lint_schema("@x-lint case=nope\nrecord R:\n  a: int\n", &Config::default()).is_err());
}

#[test]
fn schema_name_lints() {
    let rules = |name: &str| -> Vec<&'static str> {
        let src = format!("@schema {name}@1.0.0\n@root R\nrecord R:\n  a: int\n");
        lint_schema(&src, &Config::default()).unwrap().into_iter().map(|f| f.rule).collect()
    };
    assert_eq!(rules("acme.example/invoice/v2"), ["schema-name-version"]);
    assert_eq!(rules("acme/2.1"), ["schema-name-version"]);
    assert_eq!(rules("acme.example/invoice"), [] as [&str; 0]);
    assert_eq!(rules("acme.example/v2x"), [] as [&str; 0]);
    assert_eq!(rules("novafile.dev/community/pddl"), ["schema-name-reserved"]);
}

#[test]
fn template_rules() {
    let src = "@template\n@param name: string\n@param p: string\n@def lower(s: string) = $s\n@def unused(x: int) = $x\na: \"${upper(name)}\"\nb: $lower(x)\nc: $matches($name, $p)\nd: $matches($name, \"[a-z]+\")\n";
    let f = lint_document(src, &Config::default()).unwrap();
    let rules: Vec<(&str, Level)> = f.iter().map(|f| (f.rule, f.level)).collect();
    assert_eq!(
        rules,
        [
            ("shadowed-builtin", Level::Warning),
            ("unused-def", Level::Hint),
            ("bare-argument", Level::Warning),
            ("dynamic-pattern", Level::Hint),
        ],
        "{f:#?}"
    );
    assert_eq!(&src[f[0].start..f[0].end], "@def lower");
    assert_eq!(&src[f[2].start..f[2].end], "upper");
    assert_eq!(&src[f[3].start..f[3].end], "$matches");
    let f = lint_document(&src.replacen("\n", "\n@x-lint allow=unused-def,bare-argument\n", 1), &Config::default()).unwrap();
    assert_eq!(f.len(), 2, "{f:#?}");
}

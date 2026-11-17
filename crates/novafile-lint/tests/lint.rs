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
fn configuration() {
    let camel = Config::default().with_directive("case=camel allow=quoted-key,mixed-list").unwrap();
    let f = lint_document(DOC, &camel).unwrap();
    assert!(f.iter().all(|f| f.rule != "quoted-key" && f.rule != "mixed-list"));
    assert!(f.iter().any(|f| f.rule == "key-case" && f.message.contains("`user_id` is not camelCase")));
    // In-document configuration and schema-governed documents.
    let f = lint_document("@x-lint allow=key-case\nfooBar: 1\n", &Config::default()).unwrap();
    assert!(f.is_empty());
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
    assert!(
        lint_schema(include_str!("../../../examples/infrastructure.novs"), &Config::default())
            .unwrap()
            .is_empty()
    );
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

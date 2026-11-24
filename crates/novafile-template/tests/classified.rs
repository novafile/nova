//! Classified values in templates (proposal 0009.7): taint propagation and redaction.

use novafile_schema::{RedactKey, Reveal, Schema, parse_schema};
use novafile_template::{Loader, Options, Redaction, TemplateError, render, render_redacted};
use novafile_text::Profile;
use novafile_value::Record;

/// The schema that the imported data document declares.
const SCHEMA: &str = "@root Db

record Creds @secret:
  user: string
  token: string

record Db:
  host: string
  user: string
  password: string @secret
  owner: string @personal
  admin?: Creds
  tokens?: list<string> @secret
  env?: map<string, string> @secret
";

/// A loader that keeps its documents in memory and knows the schema `ops`.
struct Mem;

impl Loader for Mem {
    fn load(&self, path: &str, _: Option<&str>) -> Result<(String, String), String> {
        let src = match path {
            "db.nova" => {
                "@schema ops\nhost: db.internal\nuser: app\npassword: hunter2\nowner: ada\nadmin: {user: root, token: t0p}\ntokens: [t0p]\nenv: {KEY: t0p}\n"
            }
            "mid.nova" => "@template\n@import \"db.nova\" as db\nsecret: $db.password\n",
            "unknown.nova" => "@schema elsewhere\npassword: hunter2\n",
            _ => return Err(format!("no {path}")),
        };

        Ok((path.to_owned(), src.to_owned()))
    }

    fn schema(&self, reference: &str, _: Option<&str>) -> Result<Option<Schema>, String> {
        Ok((reference == "ops").then(|| parse_schema(SCHEMA).unwrap()))
    }
}

/// Render `body` below an import of `db.nova` as `$db`, in the given mode.
fn run(body: &str, reveal: Reveal, key: Option<&RedactKey>) -> Result<String, TemplateError> {
    let src = format!("@template\n@import \"db.nova\" as db\n{body}");
    let opts = Options {
        loader: &Mem,
        ..Options::default()
    };

    let r = render_redacted(&src, &Record::new(), &opts, &Redaction { reveal, key })?;
    Ok(novafile_text::to_text(r.for_output(), Profile::Compact))
}

/// Render `body` in redact mode and check that the secrets do not show.
fn redacted(body: &str) -> String {
    let out = run(body, Reveal::Redact, None).unwrap();
    assert!(!out.contains("hunter2") && !out.contains("t0p"), "{out}");
    out
}

/// Render `body` in redact mode, expect an error, and check that it does not show the secret.
fn refused(body: &str) -> String {
    let e = run(body, Reveal::Redact, None).unwrap_err().to_string();
    assert!(e.contains("classified value") && !e.contains("hunter2"), "{e}");
    e
}

/// A secret field of an imported, schema-backed document renders as `@redacted`.
#[test]
fn secret_field_from_imported_document_is_redacted() {
    let out = redacted("pw: $db.password\nhost: $db.host\n");
    assert!(out.contains("pw: @redacted") && out.contains("host: db.internal"), "{out}");
}

/// A string that interpolates a secret is classified as a whole, with the strongest class.
#[test]
fn interpolation_takes_the_strongest_class() {
    let out = redacted("url: \"postgres://${db.user}:${db.password}@${db.host}/x\"\nwho: \"${db.owner}/${db.password}\"\n");
    assert!(out.contains("url: @redacted") && out.contains("who: @redacted"), "{out}");
    assert!(!out.contains("p:"), "a secret part removes the pseudonym: {out}");
}

/// Personal values get pseudonyms of the rendered value; equal values get equal pseudonyms.
#[test]
fn personal_values_get_pseudonyms_of_the_derived_value() {
    let key = RedactKey::new([3; 32]);
    let out = run("a: $db.owner\nb: \"${db.owner}\"\nc: \"by ${db.owner}\"\n", Reveal::Redact, Some(&key)).unwrap();
    assert!(!out.contains("ada"), "{out}");
    let pseudonym = |field: &str| out.lines().find_map(|l| l.strip_prefix(&format!("{field}: "))).unwrap().to_owned();
    assert!(pseudonym("a").starts_with("@redacted(\"p:"), "{out}");
    assert_eq!(pseudonym("a"), pseudonym("b"), "equal values, equal pseudonyms");
    assert_ne!(pseudonym("a"), pseudonym("c"));
}

/// A classified container and every value read from it are redacted.
#[test]
fn classified_containers_taint_their_parts() {
    let out = redacted("a: $db.admin\nt: $db.admin.token\nl: [$db.password, $db.host]\n");
    assert!(out.contains("a: @redacted") && out.contains("t: @redacted"), "{out}");
    assert!(out.contains("[@redacted,db.internal]"), "{out}");
}

/// A loop over an unclassified record keeps the class of each element.
#[test]
fn loops_over_unclassified_records_keep_the_class_of_each_element() {
    let out = redacted("copy:\n  @for k, v in $db:\n    \"${k}\": $v\n");
    assert!(out.contains("password: @redacted") && out.contains("host: db.internal"), "{out}");
}

/// The class travels through an imported template.
#[test]
fn taint_survives_nested_imports() {
    let out = redacted("@import \"mid.nova\" as m\nx: $m.secret\ny: $m\n");
    assert!(out.contains("x: @redacted") && out.contains("y:\n  secret: @redacted"), "{out}");
}

/// Reveal modes show what they name, and reveal-all equals the render without classification.
#[test]
fn reveal_renders_in_clear() {
    let body = "url: \"postgres://${db.password}@host\"\n@if $db.password != \"\":\n  set: true\n";
    let out = run(body, Reveal::All, None).unwrap();
    assert!(out.contains("url: postgres://hunter2@host") && out.contains("set: true"), "{out}");

    // The same as the render without classification.
    let opts = Options {
        loader: &Mem,
        ..Options::default()
    };
    let clear = render(&format!("@template\n@import \"db.nova\" as db\n{body}"), &Record::new(), &opts).unwrap();
    assert_eq!(out, novafile_text::to_text(&clear, Profile::Compact));

    // Personal data in clear, secrets still redacted.
    let out = run("o: $db.owner\npw: $db.password\n", Reveal::Personal, None).unwrap();
    assert!(out.contains("o: ada") && out.contains("pw: @redacted"), "{out}");

    // Paths name positions in the output.
    let out = run("o: $db.owner\npw: $db.password\n", Reveal::parse("pw").unwrap(), None).unwrap();
    assert!(out.contains("o: @redacted") && out.contains("pw: hunter2"), "{out}");
}

/// Conditions, loops, spreads, keys and selectors that depend on a classified value are errors.
#[test]
fn branching_on_classified_values_is_an_error() {
    refused("x:\n  @if $db.password == \"\":\n    empty: true\n");
    refused("x:\n  @for k, v in $db.admin:\n    \"${k}\": 1\n");
    refused("x: {...$db.admin}\n");
    refused("x:\n  \"${db.password}\": 1\n");
    refused("x: $db.tokens[t0p]\n");
    refused("x: $db.tokens[nope]\n");
    refused("x: $db.env.KEY\n");
    refused("x: $db.env[\"NOPE\"]\n");
    refused("@let l = [{id: $db.password}]\nx: $l[id=guess]\n");
}

/// Whether a classified value is present is not classified (proposal 0009.6.1).
#[test]
fn presence_of_a_classified_value_may_be_tested() {
    let out = run(
        "x:\n  @if $db.admin != null:\n    y: 1\n  @if null == $db.password:\n    z: 1\n",
        Reveal::Redact,
        None,
    )
    .unwrap();
    assert!(out.contains("y: 1") && !out.contains("z: 1"), "{out}");
}

/// An import whose `@schema` the loader cannot give is refused unless everything is revealed.
#[test]
fn unresolvable_schema_fails_closed() {
    let e = run("@import \"unknown.nova\" as u\nx: $u.password\n", Reveal::Redact, None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("cannot be resolved") && !e.contains("hunter2"), "{e}");
    let out = run("@import \"unknown.nova\" as u\nx: $u.password\n", Reveal::All, None).unwrap();
    assert!(out.contains("hunter2"), "{out}");
}

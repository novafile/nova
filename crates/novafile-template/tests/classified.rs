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
  mfa?: string

record Db:
  host: string
  user: string
  password: string @secret
  owner: string @personal
  admin?: Creds
  tokens?: list<string> @secret
  env?: map<string, string> @secret
  note?: string @secret
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

    // A missing optional field is absent, not an error.
    let out = run("x:\n  @if $db.note != null:\n    n: 1\n  @else:\n    n: 0\n", Reveal::Redact, None).unwrap();
    assert!(out.contains("n: 0"), "{out}");
}

/// Which fields a classified container holds is classified: a missing field looks like a set one.
#[test]
fn fields_inside_a_classified_container_do_not_show_presence() {
    let out = run("t: $db.admin.mfa\nu: $db.admin.user\n", Reveal::Redact, None).unwrap();
    assert!(out.contains("t: @redacted") && out.contains("u: @redacted"), "{out}");
    refused("x:\n  @if $db.admin.mfa != null:\n    y: 1\n");
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

/// Built-in functions and operators give their result the class of a classified operand.
#[test]
fn functions_and_operators_propagate_the_class() {
    let out = redacted("u: $upper($db.password)\nh: $sha256($db.password)\nn: $len($db.tokens)\ne: $($db.password == \"x\")\nj: $to_json($db.admin)\n");
    for field in ["u", "h", "n", "e", "j"] {
        assert!(out.contains(&format!("{field}: @redacted")), "{field}: {out}");
    }
}

/// Built-ins that only move values keep the class of each value they move.
#[test]
fn pass_through_functions_keep_classes_per_value() {
    let out = redacted("h: $get($db, host, \"\")\np: $get($db, password, \"\")\nks: $keys($db)\nv: $values($db)\n");
    assert!(out.contains("h: db.internal") && out.contains("p: @redacted"), "{out}");
    assert!(out.contains("ks: [admin,env,host,owner,password,tokens,user]"), "{out}");
    assert!(out.contains("v: [@redacted,@redacted,db.internal,@redacted,@redacted,@redacted,app]"), "{out}");
}

/// Comprehensions, `@def` calls and branches see classified values like the other constructs.
#[test]
fn comprehensions_and_defs_handle_classified_values() {
    let out = redacted("@def same(s: string) = $s\nx: $same($db.password)\nks: $([for k, v in $db: $k])\nvs: $([for k, v in $db if $k == host: $v])\n");
    assert!(out.contains("x: @redacted") && out.contains("vs: [db.internal]"), "{out}");
    refused("l: $([for t in $db.tokens: $t])\n");
    refused("l: $([for k, v in $db if $v == \"hunter2\": $k])\n");
    refused("r: $({for k, v in $db: $v: 1})\n");
    refused("x:\n  @if $($db.password == \"\" or true):\n    y: 1\n");
    refused("x:\n  @if $(not ($db.password == \"\")):\n    y: 1\n");
    refused("s: @set[$db.password, a]\n");
}

/// Failing on a classified value looks like succeeding: both render `@redacted`, since an error
/// would tell something about the value (whether the password is a number, say).
#[test]
fn failures_on_classified_values_look_like_success() {
    let out = redacted("@def n(x: int) = $x\na: $int($db.password)\nb: $n($db.password)\nc: $($db.password + 1)\n");
    for field in ["a", "b", "c"] {
        assert!(out.contains(&format!("{field}: @redacted")), "{field}: {out}");
    }

    // Without a classified operand, a failure is still an error.
    let e = run("@def n(x: int) = $x\nb: $n($db.host)\n", Reveal::Redact, None).unwrap_err().to_string();
    assert!(e.contains("does not match"), "{e}");
}

/// A presence test through a classified list would show its length.
#[test]
fn presence_inside_a_classified_list_is_refused() {
    refused("x:\n  @if $db.tokens[3] != null:\n    y: 1\n");
    refused("x:\n  @if $db.tokens[0] == null:\n    y: 1\n");
    let out = redacted("t: $db.tokens[3]\n");
    assert!(out.contains("t: @redacted"), "{out}");
}

/// Plain rendering is unchanged: classification needs `render_redacted`.
#[test]
fn plain_render_shows_derived_values() {
    let opts = Options {
        loader: &Mem,
        ..Options::default()
    };
    let src = "@template\n@import \"db.nova\" as db\nu: $upper($db.password)\n";
    let v = render(src, &Record::new(), &opts).unwrap();
    assert!(novafile_text::to_text(&v, Profile::Compact).contains("HUNTER2"));
}

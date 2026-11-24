//! Redaction of classified values (proposal 0009).

use novafile_schema::{RedactKey, Reveal, Schema, parse_schema};
use novafile_text::Profile;

/// The schema of the proposal's example.
const OPS: &str = "@import novafile.dev/std@0.1.0 as std
@root Doc

type Email = std.Email @personal

record Credentials @secret:
  user: string
  password: string

record Service:
  id: string @key
  db_url: std.Uri @secret
  owner?: Email
  env?: map<string, string @secret>
  admin?: Credentials
  note?: string

type Choice = string @pattern(\"[0-9]+\") @secret | string

record Doc:
  services: list<Service>
  choice?: Choice
";

/// The example document.
const DOC: &str = "services:
  - id: api
    db_url: \"postgres://u:pw@db/x\"
    owner: \"ada@example.org\"
    env: {DATABASE_PASSWORD: hunter2}
    admin: {user: root, password: toor}
    note: hello
  - id: web
    db_url: \"https://example.org/\"
    owner: \"ada@example.org\"
choice: \"1234\"
";

/// The schema and the parsed document.
fn setup() -> (Schema, novafile_value::Value) {
    (parse_schema(OPS).unwrap(), novafile_text::parse(DOC).unwrap())
}

/// The redacted document as compact text.
fn text(reveal: &Reveal, key: Option<&RedactKey>) -> String {
    let (s, v) = setup();
    let r = s.redact(&v, reveal, key).unwrap();
    novafile_text::to_text(r.for_output(), Profile::Compact)
}

#[test]
fn secrets_and_personal_data_become_placeholders() {
    let key = RedactKey::new([1; 32]);
    let out = text(&Reveal::Redact, Some(&key));
    for secret in ["postgres", "hunter2", "toor", "root", "ada@example.org", "1234"] {
        assert!(!out.contains(secret), "{secret} leaked:\n{out}");
    }

    assert!(out.contains("db_url: @redacted"), "{out}");
    assert!(out.contains("admin: @redacted"), "a classified container becomes one placeholder:\n{out}");
    assert!(out.contains("note: hello"), "{out}");
    assert!(out.contains("id: api"), "{out}");
    // Equal personal values get equal pseudonyms under one key.
    let p = RedactKey::new([1; 32]).pseudonym(&novafile_value::Value::String("ada@example.org".into()));
    assert_eq!(out.matches(&format!("@redacted(\"{p}\")")).count(), 2, "{out}");
}

#[test]
fn the_union_member_decides_the_class() {
    let (s, _) = setup();
    let v = novafile_text::parse("services: []\nchoice: abc\n").unwrap();
    let out = novafile_text::to_text(s.redact(&v, &Reveal::Redact, None).unwrap().for_output(), Profile::Compact);
    assert!(out.contains("choice: abc"), "a failed member leaves no class behind:\n{out}");
}

#[test]
fn reveal_modes() {
    let personal = text(&Reveal::Personal, None);
    assert!(personal.contains("ada@example.org") && !personal.contains("hunter2"), "{personal}");
    let all = text(&Reveal::All, None);
    assert!(all.contains("hunter2") && all.contains("toor"), "{all}");
    let path = text(&Reveal::parse("services[0].admin").unwrap(), None);
    assert!(path.contains("toor") && !path.contains("hunter2"), "{path}");
    let keyed = text(&Reveal::parse("services[id=api].admin").unwrap(), None);
    assert!(keyed.contains("toor") && !keyed.contains("hunter2"), "{keyed}");
    assert!(Reveal::parse("services[*]").is_err());
}

#[test]
fn issues_do_not_show_classified_values() {
    let (s, _) = setup();
    let v = novafile_text::parse("services:\n  - id: api\n    db_url: \"not a uri\"\n").unwrap();
    let issues = s.redact(&v, &Reveal::Redact, None).unwrap_err();
    let all: String = issues.iter().map(ToString::to_string).collect();
    assert!(!all.contains("not a uri"), "{all}");
    assert!(all.contains("value does not match"), "{all}");
}

#[test]
fn revision_tokens_are_stable_and_keyed() {
    let rev = novafile_value::revision(&novafile_value::Value::Int(1));
    let (a, b) = (RedactKey::new([1; 32]), RedactKey::new([2; 32]));
    assert_eq!(a.token(&rev), a.token(&rev));
    assert_ne!(a.token(&rev), b.token(&rev));
    assert!(a.token(&rev).starts_with("k:"));
}

#[test]
fn reordered_set_elements_stay_redacted() {
    let s = parse_schema("@root R\nrecord E:\n  name: string\n  pin?: string @secret\nrecord R:\n  items: set<E>\n").unwrap();
    for doc in [
        "items: [{name: z, pin: \"9876\"}, {name: a}]\n",
        "items: [{name: a}, {name: z, pin: \"9876\"}]\n",
    ] {
        let v = novafile_text::parse(doc).unwrap();
        let out = novafile_text::to_text(s.redact(&v, &Reveal::Redact, None).unwrap().for_output(), Profile::Compact);
        assert!(!out.contains("9876"), "{out}");
        assert!(out.contains("z,@redacted") && out.contains("a,"), "{out}");
    }
}

#[test]
fn no_issue_shows_a_classified_value() {
    let s = parse_schema(
        "enum E @secret = a | b(int)
record C @personal:
  n: int @range(..5)
  t: timestamp
type U = string @pattern(\"[0-9]+\") @secret | int
record R:
  a: string @secret @pattern(\"[0-9]+\")
  b: int @secret @range(..5)
  c: i8 @secret
  d: timestamp @secret
  e: E
  f: C
  g: decimal @secret
  h: bytes @secret
  i: list<int @secret> @len(..1)
  j: U
  k: map<string, int @secret>
  l: E
  m: E
",
    )
    .unwrap();
    let v = novafile_text::parse(
        "a: SENTINEL\nb: 7777\nc: 7777\nd: SENTINEL\ne: SENTINEL\nf: {n: 7777, t: SENTINEL}\ng: SENTINEL\nh: \"SENTINEL!\"\ni: [7777, 7777]\nj: SENTINEL\nk: {x: SENTINEL}\nl: !SENTINEL\nm: !a(7777)\n",
    )
    .unwrap();
    let t = novafile_schema::parse_type("R").unwrap();
    let issues = s.resolve_as(&v, &t).unwrap_err();
    assert!(issues.len() >= 12, "{issues:?}");
    for i in &issues {
        let m = i.to_string();
        assert!(!m.contains("SENTINEL") && !m.contains("7777"), "{m}");
    }
}

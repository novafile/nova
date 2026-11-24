//! Integration tests for schema imports and the bundled standard library
//! (proposal 0008), and for classified types (proposal 0009).

use novafile_schema::{Class, STD_NAME, Schema, SchemaRef, check_compatibility, parse_schema, parse_schema_unlinked, std_schemas};

/// A schema that imports the bundled standard library.
const CRM: &str = "@schema acme.example/crm@1.0.0
@import novafile.dev/std@0.1.0 as std

type Email = std.Email @personal

record Contact:
  id: std.Uuid @key @id(1)
  name: std.Name @len(..20) @id(2)
  email?: Email @id(3)
  phone?: std.E164 @id(4)
  files?: list<std.PortablePath> @id(5)
";

/// Parse `src` and panic with the error if it is invalid.
fn schema(src: &str) -> Schema {
    parse_schema(src).unwrap_or_else(|e| panic!("{e}"))
}

/// Resolve the flow value `src` against the type `ty` of `s`.
fn check(s: &Schema, ty: &str, src: &str) -> bool {
    let v = novafile_text::parse_flow(src).unwrap();
    s.resolve_as(&v, &novafile_schema::parse_type(ty).unwrap()).is_ok()
}

#[test]
fn std_types_resolve_through_an_import() {
    let s = schema(CRM);
    let ok = "{id: 0f8fad5b-d9cb-469f-a165-70867728950e, name: Ada, email: \"ada@example.org\", phone: \"+4930123456\"}";
    assert!(check(&s, "Contact", ok));
    assert!(!check(&s, "Contact", "{id: 0F8FAD5B-D9CB-469F-A165-70867728950E, name: Ada}"), "uppercase UUID");
    assert!(
        !check(&s, "Contact", "{id: 0f8fad5b-d9cb-469f-a165-70867728950e, name: \" Ada\"}"),
        "untrimmed name"
    );
    assert!(
        !check(&s, "Contact", "{id: 0f8fad5b-d9cb-469f-a165-70867728950e, name: Ada, phone: \"030 123\"}"),
        "E.164"
    );
    assert!(
        !check(&s, "Contact", "{id: 0f8fad5b-d9cb-469f-a165-70867728950e, name: Ada, files: [\"../etc\"]}"),
        "path traversal"
    );
}

#[test]
fn refinement_intersects_with_the_alias() {
    let s = schema(CRM);
    assert!(check(&s, "Contact", "{id: 0f8fad5b-d9cb-469f-a165-70867728950e, name: \"Ada Lovelace\"}"));
    assert!(
        !check(
            &s,
            "Contact",
            "{id: 0f8fad5b-d9cb-469f-a165-70867728950e, name: \"Ada Lovelace, Countess of Lovelace\"}"
        ),
        "@len(..20)"
    );
    let p = schema("@import novafile.dev/std@0.1.0 as std\ntype Lower = std.Name @pattern(\"[a-z ]*\")\n");
    assert!(check(&p, "Lower", "\"ada\""));
    assert!(!check(&p, "Lower", "\"Ada\""), "the refinement pattern");
    assert!(!check(&p, "Lower", "\"ada \""), "the alias pattern");
}

#[test]
fn canonical_text_keeps_imports_with_fingerprints() {
    let s = schema(CRM);
    let std_fp = std_schemas()[0].fingerprint_hex();
    let text = s.to_canonical_text();
    assert!(text.contains(&format!("@import novafile.dev/std@0.1.0#{std_fp} as std\n")), "{text}");
    assert!(!text.contains("type std."), "imported definitions stay out: {text}");
    assert_eq!(schema(&text).to_canonical_text(), text, "canonical text is a fixed point");
}

#[test]
fn import_errors() {
    let err = |src: &str| parse_schema(src).unwrap_err().message;
    assert!(err("@import novafile.dev/std@0.1.0#0000000000000000 as std\n").contains("fingerprint mismatch"));
    assert!(err("@import novafile.dev/std@9.0.0 as std\n").contains("only the bundled"));
    assert!(err("@import acme.example/x@1.0.0 as x\n").contains("cannot import"));
    assert!(err("@import novafile.dev/std@0.1.0 as std\ntype A = std.Nope\n").contains("unknown type std.Nope"));
    assert!(err("@import novafile.dev/std@0.1.0 as s\n@import novafile.dev/std@0.1.0 as s\n").contains("duplicate import alias"));
    assert!(err("type A = int\n@import novafile.dev/std@0.1.0 as std\n").contains("before @root"));
    assert!(err("@import novafile.dev/std@0.1.0 as std\ntype A = std.Port @pattern(\"1\")\n").contains("@pattern applies to string"));
}

#[test]
fn link_uses_the_loader_for_other_schemas() {
    let common = schema("@schema acme.example/common@1.2.0\ntype Code = string @pattern(\"[A-Z]+\")\nrecord Pair:\n  a: Code\n  b: Code\n");
    let mut s = parse_schema_unlinked("@import acme.example/common@1.2.0 as c\n@root c.Pair\n").unwrap();
    let load = |r: &SchemaRef| {
        if r.name == "acme.example/common" {
            Ok(common.clone())
        } else {
            Err("unknown".to_owned())
        }
    };
    s.link(&load).unwrap();
    assert!(s.def("c.Code").is_some(), "references inside imported definitions are qualified too");
    assert!(s.validate(&novafile_text::parse("a: X\nb: Y\n").unwrap()).is_ok());
    assert!(s.validate(&novafile_text::parse("a: x\nb: Y\n").unwrap()).is_err());
}

#[test]
fn std_is_bundled_under_its_reserved_name() {
    assert!(std_schemas().iter().all(|s| s.name.as_deref() == Some(STD_NAME)));
}

#[test]
fn classes_parse_and_print() {
    let src = "record Creds @secret:\n  user: string\n\nrecord Service:\n  id: string @key\n  db: string @secret\n  owner?: string @personal\n  env?: map<string, string @secret>\n";
    let s = schema(src);
    assert_eq!(s.def_class("Creds"), Some(Class::Secret));
    let text = s.to_canonical_text();
    assert!(text.contains("record Creds @secret:"), "{text}");
    assert!(text.contains("db: string @secret"), "{text}");
    assert!(text.contains("map<string, string @secret>"), "{text}");
    assert_eq!(schema(&text), s);
    // A class does not change which values are valid.
    assert!(check(&s, "Service", "{id: a, db: x, owner: y, env: {K: v}}"));
}

#[test]
fn classes_are_forbidden_where_values_appear_in_paths() {
    let err = |src: &str| parse_schema(src).unwrap_err().message;
    assert!(err("record R:\n  id: string @secret @key\n").contains("@key field"));
    assert!(err("type T = string @personal\nrecord R:\n  id: T @key\n").contains("@key field"));
    assert!(err("type M = map<string @secret, int>\n").contains("map key"));
    assert!(err("type S = set<string @personal>\n").contains("set element"));
    assert!(err("type S = string @secret @personal\n").contains("at most one class"));
    assert!(err("enum E @open = a\n").contains("@open applies to records"));
}

#[test]
fn declassification_is_breaking_and_classification_is_not() {
    let plain = schema("record R:\n  a: string\n");
    let secret = schema("record R:\n  a: string @secret\n");
    let personal = schema("record R:\n  a: string @personal\n");
    let breaking = |o: &Schema, n: &Schema| check_compatibility(o, n).iter().any(|c| c.breaking);
    let changed = |o: &Schema, n: &Schema| !check_compatibility(o, n).is_empty();
    assert!(changed(&plain, &secret) && !breaking(&plain, &secret));
    assert!(changed(&personal, &secret) && !breaking(&personal, &secret));
    assert!(breaking(&secret, &plain));
    assert!(breaking(&secret, &personal));
    let rec = schema("record R @secret:\n  a: string\n");
    assert!(breaking(&rec, &schema("record R:\n  a: string\n")));
    let nested = schema("type L = list<string @secret>\n");
    assert!(breaking(&nested, &schema("type L = list<string>\n")));
}

#[test]
fn a_local_alias_may_become_the_std_type() {
    let pattern = std_schemas()[0].def("Uuid").map(|d| match d {
        novafile_schema::Def::Alias(t) => t.to_string(),
        _ => unreachable!("Uuid is an alias"),
    });
    let old = schema(&format!("type Id = {}\nrecord R:\n  id: Id\n", pattern.unwrap()));
    let new = schema("@import novafile.dev/std@0.1.0 as std\ntype Id = std.Uuid\nrecord R:\n  id: std.Uuid\n");
    assert!(
        check_compatibility(&old, &new).iter().all(|c| !c.breaking),
        "{:?}",
        check_compatibility(&old, &new)
    );
}

#[test]
fn descriptors_keep_imports_and_classes() {
    let s = schema(&format!("{CRM}\nrecord Creds @secret:\n  key: string\n"));
    let v = s.to_value();
    novafile_schema::meta_schema().validate(&v).unwrap_or_else(|e| panic!("{e:?}"));
    let back = Schema::from_value(&v).unwrap();
    assert_eq!(back.to_canonical_text(), s.to_canonical_text());
    assert_eq!(back.fingerprint(), s.fingerprint());
}

#[test]
fn renaming_an_import_alias_is_compatible() {
    let old = schema("@import novafile.dev/std@0.1.0 as std\nrecord R:\n  id: std.Uuid\n");
    let new = schema("@import novafile.dev/std@0.1.0 as s\nrecord R:\n  id: s.Uuid\n");
    assert!(
        check_compatibility(&old, &new).iter().all(|c| !c.breaking),
        "{:?}",
        check_compatibility(&old, &new)
    );
}

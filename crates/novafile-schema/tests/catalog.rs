//! Catalogs and schema lookup (spec §6.9, proposal 0005 §0005.9).

use novafile_schema::catalog::{LookupError, Options, Origin, Resolver};
use novafile_schema::{SchemaRef, parse_schema};
use std::path::{Path, PathBuf};

/// A fresh directory for one test.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nova-catalog-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Write `text` to `dir/rel`, creating directories.
fn put(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A schema `name@version` with one field `a` of type `ty`.
fn schema(name_version: &str, ty: &str) -> String {
    format!("@schema {name_version}\n@root R\nrecord R:\n  a: {ty}\n")
}

/// A resolver for documents in `dir`, using only its project catalog.
fn resolver(dir: &Path) -> Resolver {
    Resolver::new(&Options {
        start: Some(dir.to_path_buf()),
        ..Options::default()
    })
    .unwrap()
}

/// The file `reference` resolves to, relative to `dir`.
fn resolved(dir: &Path, reference: &str) -> String {
    match resolver(dir).resolve(reference).unwrap().origin {
        Origin::File(p) => p.strip_prefix(dir).unwrap().display().to_string(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn catalog_directory_and_newest_readable() {
    let d = scratch("dir");
    put(&d, "nova-catalog.nova", "schemas:\n  - path: schemas/\n");
    put(&d, "schemas/a.novs", &schema("acme.test/a@1.2.0", "int"));
    put(&d, "schemas/old.novs", &schema("acme.test/a@1.1.0", "int"));
    put(&d, "schemas/new.novs", &schema("acme.test/a@1.3.0", "int"));
    put(&d, "schemas/two.novs", &schema("acme.test/a@2.0.0", "int"));
    put(&d, "schemas/anon.novs", "@root R\nrecord R:\n  a: int\n");
    assert_eq!(resolved(&d, "acme.test/a@1.0.0"), "schemas/new.novs");
    assert_eq!(resolved(&d, "acme.test/a@1.1.0"), "schemas/new.novs");
    // A catalog source is not consulted for unrelated names.
    let e = resolver(&d).resolve("other.test/a@1.0.0").unwrap_err();
    assert_eq!(e.kind(), "not-found", "{e}");
}

#[test]
fn fingerprint_picks_the_exact_version() {
    let d = scratch("fp");
    put(&d, "nova-catalog.nova", "schemas:\n  - path: s/\n");
    put(&d, "s/a11.novs", &schema("acme.test/a@1.1.0", "int"));
    put(&d, "s/a12.novs", &schema("acme.test/a@1.2.0", "int?"));
    let fp = parse_schema(&schema("acme.test/a@1.1.0", "int")).unwrap().fingerprint_hex();
    assert_eq!(resolved(&d, &format!("acme.test/a@1.1.0#{fp}")), "s/a11.novs");
    assert_eq!(resolved(&d, "acme.test/a@1.1.0"), "s/a12.novs");
}

#[test]
fn conflicts_and_pins_are_errors() {
    let d = scratch("conflict");
    put(&d, "nova-catalog.nova", "schemas:\n  - path: s/\n");
    put(&d, "s/x.novs", &schema("acme.test/a@1.0.0", "int"));
    put(&d, "s/y.novs", &schema("acme.test/a@1.0.0", "string"));
    let e = resolver(&d).resolve("acme.test/a@1.0.0").unwrap_err();
    assert_eq!(e.kind(), "conflict");
    assert!(e.to_string().contains("x.novs") && e.to_string().contains("y.novs"), "{e}");

    let d = scratch("pin");
    put(
        &d,
        "nova-catalog.nova",
        &format!("schemas:\n  - path: s/\npins:\n  - schema: acme.test/a@1.0.0\n    revision: \"sha256:{}\"\n", "0".repeat(64)),
    );
    put(&d, "s/x.novs", &schema("acme.test/a@1.0.0", "int"));
    let e = resolver(&d).resolve("acme.test/a@1.0.0").unwrap_err();
    assert!(matches!(e, LookupError::Pin(_)), "{e:?}");

    // The right revision passes.
    let rev = parse_schema(&schema("acme.test/a@1.0.0", "int")).unwrap().revision();
    put(&d, "nova-catalog.nova", &format!("schemas:\n  - path: s/\npins:\n  - schema: acme.test/a@1.0.0\n    revision: \"{rev}\"\n"));
    assert_eq!(resolved(&d, "acme.test/a@1.0.0"), "s/x.novs");
}

#[test]
fn nearest_catalog_only() {
    let d = scratch("near");
    put(&d, "nova-catalog.nova", "schemas:\n  - path: far/\n");
    put(&d, "far/a.novs", &schema("acme.test/a@1.5.0", "int"));
    put(&d, "sub/nova-catalog.nova", "schemas:\n  - path: near/\n");
    put(&d, "sub/near/a.novs", &schema("acme.test/a@1.0.0", "int"));
    assert_eq!(resolved(&d.join("sub"), "acme.test/a@1.0.0"), "near/a.novs");
}

#[test]
fn builtin_meta_schemas_and_the_legacy_name() {
    let d = scratch("meta");
    let r = resolver(&d).resolve("nova-schema@0.3.2").unwrap();
    assert_eq!(r.origin, Origin::Builtin);
    assert_eq!(r.schema.name.as_deref(), Some("novafile.dev/meta/schema"));
    assert!(r.warnings[0].contains("novafile.dev/meta/schema"), "{:?}", r.warnings);
    assert!(resolver(&d).resolve("novafile.dev/meta/catalog@0.1.0").is_ok());

    // A file may not shadow a bundled version with other content.
    put(&d, "nova-catalog.nova", "schemas:\n  - path: s/\n");
    put(&d, "s/m.novs", &schema("novafile.dev/meta/catalog@0.1.0", "int"));
    assert_eq!(resolver(&d).resolve("novafile.dev/meta/catalog@0.1.0").unwrap_err().kind(), "conflict");
}

#[test]
fn fetch_locations_and_checks() {
    let d = scratch("fetch");
    put(
        &d,
        "nova-catalog.nova",
        "remote:\n  - prefix: acme.dev/\n    url: \"https://schemas.acme.dev/{rest}/{version}.novs\"\n  - prefix: acme.dev/billing/\n    url: \"https://billing.acme.dev/{name}@{version}\"\n",
    );
    // Named explicitly (as with --catalog), the catalog's remote entries are followed.
    let r = Resolver::new(&Options {
        catalogs: vec![d.join("nova-catalog.nova")],
        ..Options::default()
    })
    .unwrap();
    let at = |s: &str| r.fetch_url(&SchemaRef::parse(s).unwrap()).ok();
    assert_eq!(at("acme.dev/x/y@1.0.0").as_deref(), Some("https://schemas.acme.dev/x/y/1.0.0.novs"));
    assert_eq!(
        at("acme.dev/billing/invoice@2.1.0").as_deref(),
        Some("https://billing.acme.dev/acme.dev/billing/invoice@2.1.0")
    );
    assert_eq!(at("other.org/a@1.0.0").as_deref(), Some("https://other.org/a/1.0.0.novs"));
    for never in ["local@1.0.0", "acme.test/a@1.0.0", "x.example/a@1.0.0", "novafile.dev/meta/schema@0.3.3"] {
        assert_eq!(at(never), None, "{never}");
    }

    // As the project catalog, it redirects only fingerprinted or pinned versions:
    // it arrives with the repository, and the cache it would fill is shared.
    let project = resolver(&d);
    let e = project.fetch_url(&SchemaRef::parse("acme.dev/x/y@1.0.0").unwrap()).unwrap_err();
    assert!(e.contains("pin"), "{e}");
    assert!(project.fetch_url(&SchemaRef::parse("acme.dev/x/y@1.0.0#0123456789abcdef").unwrap()).is_ok());
    assert!(project.fetch_url(&SchemaRef::parse("other.org/a@1.0.0").unwrap()).is_ok());

    // Fetched text must declare exactly the requested version and match its fingerprint.
    let want = SchemaRef::parse("acme.dev/a@1.0.0").unwrap();
    assert!(r.accept_fetched(&want, &schema("acme.dev/a@1.0.0", "int")).is_ok());
    assert!(r.accept_fetched(&want, &schema("acme.dev/a@1.0.1", "int")).is_err());
    assert!(r.accept_fetched(&want, &schema("acme.dev/b@1.0.0", "int")).is_err());
    let wrong_fp = SchemaRef::parse("acme.dev/a@1.0.0#0000000000000000").unwrap();
    assert!(r.accept_fetched(&wrong_fp, &schema("acme.dev/a@1.0.0", "int")).is_err());

    // Unknown placeholders and http: URLs make a catalog invalid.
    put(&d, "nova-catalog.nova", "remote:\n  - prefix: acme.dev/\n    url: \"https://x/{host}\"\n");
    assert!(Resolver::new(&Options { start: Some(d.clone()), ..Options::default() }).is_err());
    put(&d, "nova-catalog.nova", "remote:\n  - prefix: acme.dev/\n    url: \"http://x/{name}\"\n");
    assert!(Resolver::new(&Options { start: Some(d.clone()), ..Options::default() }).is_err());
}

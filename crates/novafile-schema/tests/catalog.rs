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
        &format!(
            "schemas:\n  - path: s/\npins:\n  - schema: acme.test/a@1.0.0\n    revision: \"sha256:{}\"\n",
            "0".repeat(64)
        ),
    );
    put(&d, "s/x.novs", &schema("acme.test/a@1.0.0", "int"));
    let e = resolver(&d).resolve("acme.test/a@1.0.0").unwrap_err();
    assert!(matches!(e, LookupError::Pin(_)), "{e:?}");

    // The right revision passes.
    let rev = parse_schema(&schema("acme.test/a@1.0.0", "int")).unwrap().revision();
    put(
        &d,
        "nova-catalog.nova",
        &format!("schemas:\n  - path: s/\npins:\n  - schema: acme.test/a@1.0.0\n    revision: \"{rev}\"\n"),
    );
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
    let at = |s: &str| r.fetch_url(&SchemaRef::parse(s).unwrap()).ok().map(|f| f.url);
    assert_eq!(at("acme.dev/x/y@1.0.0").as_deref(), Some("https://schemas.acme.dev/x/y/1.0.0.novs"));
    assert_eq!(
        at("acme.dev/billing/invoice@2.1.0").as_deref(),
        Some("https://billing.acme.dev/acme.dev/billing/invoice@2.1.0")
    );
    assert_eq!(at("other.org/a@1.0.0").as_deref(), Some("https://other.org/a/1.0.0.novs"));
    for never in ["local@1.0.0", "acme.test/a@1.0.0", "x.example/a@1.0.0", "novafile.dev/meta/schema@0.3.5"] {
        assert_eq!(at(never), None, "{never}");
    }

    assert!(r.fetch_url(&SchemaRef::parse("acme.dev/x/y@1.0.0").unwrap()).unwrap().cacheable);

    // As the project catalog, its redirects are followed but never cached: it
    // arrives with the repository, and the cache is shared by every project.
    let project = resolver(&d);
    let redirected = project.fetch_url(&SchemaRef::parse("acme.dev/x/y@1.0.0").unwrap()).unwrap();
    assert_eq!(redirected.url, "https://schemas.acme.dev/x/y/1.0.0.novs");
    assert!(!redirected.cacheable);
    assert!(project.fetch_url(&SchemaRef::parse("other.org/a@1.0.0").unwrap()).unwrap().cacheable);

    // Fetched text must declare exactly the requested version and match its fingerprint.
    let want = SchemaRef::parse("acme.dev/a@1.0.0").unwrap();
    assert!(r.accept_fetched(&want, &schema("acme.dev/a@1.0.0", "int")).is_ok());
    assert!(r.accept_fetched(&want, &schema("acme.dev/a@1.0.1", "int")).is_err());
    assert!(r.accept_fetched(&want, &schema("acme.dev/b@1.0.0", "int")).is_err());
    let wrong_fp = SchemaRef::parse("acme.dev/a@1.0.0#0000000000000000").unwrap();
    assert!(r.accept_fetched(&wrong_fp, &schema("acme.dev/a@1.0.0", "int")).is_err());

    // Unknown placeholders and http: URLs make a catalog invalid.
    put(&d, "nova-catalog.nova", "remote:\n  - prefix: acme.dev/\n    url: \"https://x/{host}\"\n");
    assert!(
        Resolver::new(&Options {
            start: Some(d.clone()),
            ..Options::default()
        })
        .is_err()
    );
    put(&d, "nova-catalog.nova", "remote:\n  - prefix: acme.dev/\n    url: \"http://x/{name}\"\n");
    assert!(
        Resolver::new(&Options {
            start: Some(d.clone()),
            ..Options::default()
        })
        .is_err()
    );
}

#[test]
fn cached_versions_are_listed_by_name_and_version() {
    let d = scratch("cache");
    put(&d, "acme.dev/billing/invoice/2.1.0.novs", "");
    put(&d, "acme.dev/billing/invoice/2.2.0.novs", "");
    put(&d, "acme.dev/a/not-a-version.novs", "");
    let mut got: Vec<String> = novafile_schema::catalog::cached_versions(&d).iter().map(ToString::to_string).collect();
    got.sort();
    assert_eq!(got, ["acme.dev/billing/invoice@2.1.0", "acme.dev/billing/invoice@2.2.0"]);
}

/// A fetched schema is stored in the cache, found there by later lookups, and a
/// different file for the same version is refused (versions are immutable).
#[test]
fn cache_round_trip_and_immutable_versions() {
    let d = scratch("store");
    let cache = d.join("cache");
    let r = Resolver::new(&Options {
        start: Some(d.clone()),
        cache: Some(cache.clone()),
        ..Options::default()
    })
    .unwrap();
    let want = SchemaRef::parse("acme.dev/a@1.0.0").unwrap();
    let text = schema("acme.dev/a@1.0.0", "int");
    let s = r.accept_fetched(&want, &text).unwrap();
    let path = r.store(&s, &text).unwrap();
    assert_eq!(path, cache.join("acme.dev/a/1.0.0.novs"));

    let found = r.resolve("acme.dev/a@1.0.0").unwrap();
    assert_eq!(found.origin, Origin::Cache(path.clone()));
    // Storing the same content again is fine; other content for the version is not.
    r.store(&s, &text).unwrap();
    let other = parse_schema(&schema("acme.dev/a@1.0.0", "string")).unwrap();
    assert_eq!(r.store(&other, &schema("acme.dev/a@1.0.0", "string")).unwrap_err().kind(), "conflict");
    assert!(std::fs::read_to_string(&path).unwrap().contains("a: int"));
    // Build metadata names the same version, so it cannot bring other content either.
    let meta = schema("acme.dev/a@1.0.0+b", "string");
    let other = parse_schema(&meta).unwrap();
    assert_eq!(r.store(&other, &meta).unwrap_err().kind(), "conflict");
    assert!(!cache.join("acme.dev/a/1.0.0+b.novs").exists());
}

#[test]
fn imports_resolve_through_the_catalog() {
    let d = scratch("imports");
    put(&d, "nova-catalog.nova", "schemas:\n  - path: schemas/\n");
    put(
        &d,
        "schemas/common.novs",
        "@schema acme.test/common@1.2.0\ntype Code = string @pattern(\"[A-Z]+\")\n",
    );
    put(&d, "schemas/common13.novs", "@schema acme.test/common@1.3.0\ntype Code = string\n");
    put(
        &d,
        "schemas/app.novs",
        "@schema acme.test/app@1.0.0\n@import acme.test/common@1.2.0 as c\n@import novafile.dev/std@0.1.0 as std\n@root R\nrecord R:\n  code: c.Code\n  id: std.Uuid\n",
    );
    let r = resolver(&d);
    assert!(r.problems.is_empty(), "{:?}", r.problems);
    let app = r.resolve("acme.test/app@1.0.0").unwrap().schema;
    // The exact imported version is linked, not the newest readable one.
    assert!(
        app.validate(&novafile_text::parse("code: x\nid: 0f8fad5b-d9cb-469f-a165-70867728950e\n").unwrap())
            .is_err()
    );
    assert!(app.to_canonical_text().contains("@import acme.test/common@1.2.0#"));
    // std is built in and never fetched.
    assert_eq!(r.resolve("novafile.dev/std@0.1.0").unwrap().origin, Origin::Builtin);
    assert!(r.fetch_url(&SchemaRef::parse("novafile.dev/std@0.1.0").unwrap()).is_err());
    // Text loaded directly links through the same places.
    assert!(r.load_text("@import acme.test/common@1.2.0 as c\ntype T = c.Code\n").is_ok());
}

#[test]
fn import_cycles_are_problems() {
    let d = scratch("cycle");
    put(&d, "nova-catalog.nova", "schemas:\n  - path: schemas/\n");
    put(
        &d,
        "schemas/a.novs",
        "@schema acme.test/a@1.0.0\n@import acme.test/b@1.0.0 as b\ntype A = b.B\n",
    );
    put(
        &d,
        "schemas/b.novs",
        "@schema acme.test/b@1.0.0\n@import acme.test/a@1.0.0 as a\ntype B = string\n",
    );
    let r = resolver(&d);
    assert!(r.problems.iter().any(|p| p.contains("import cycle")), "{:?}", r.problems);
    assert_eq!(r.resolve("acme.test/a@1.0.0").unwrap_err().kind(), "not-found");
}

//! `nvf` driven as a process: patching through stdin, in place, and concurrently; redaction of
//! classified values (proposal 0009).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A fresh, empty scratch directory for one test.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nvf-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A fixed redaction key, so pseudonyms and revision tokens are the same in every run.
const KEY: &str = "0101010101010101010101010101010101010101010101010101010101010101";

/// `nvf` in `dir`, isolated from the user's catalogs, cache, key and agent detection.
fn command(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_nvf"));
    c.current_dir(dir)
        .env("NOVA_AGENT", "off")
        .env("NOVA_REDACT_KEY", KEY)
        .env("XDG_CACHE_HOME", dir.join(".cache"))
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .env("LOCALAPPDATA", dir.join(".cache"))
        .env("APPDATA", dir.join(".config"))
        .env_remove("NOVA_CATALOG")
        .env_remove("NOVA_FETCH");
    c
}

/// Runs `nvf` with `args` in `dir`, feeding `stdin`.
fn nvf(dir: &Path, args: &[&str], stdin: &str) -> Output {
    run(command(dir).args(args), stdin)
}

/// Runs `cmd`, feeding `stdin`.
fn run(cmd: &mut Command, stdin: &str) -> Output {
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

/// Stdout as text.
fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

#[test]
fn patch_from_stdin_keeps_the_document() {
    let d = scratch("stdin");
    std::fs::write(d.join("add.patch"), "@patch\n+ c: 1\n").unwrap();
    let o = nvf(&d, &["patch", "-", "add.patch"], "a: 1\nb: 5\n");
    assert!(o.status.success(), "{o:?}");
    assert_eq!(stdout(&o), "a: 1\nb: 5\nc: 1\n");

    let o = nvf(&d, &["patch", "-", "add.patch", "-w"], "a: 1\n");
    assert!(!o.status.success(), "{o:?}");
    assert!(!d.join("-").exists());
}

#[test]
fn patch_in_place_keeps_json() {
    let d = scratch("json");
    std::fs::write(d.join("x.json"), "{\"a\": 1}\n").unwrap();
    std::fs::write(d.join("set.patch"), "@patch\na: 1 -> 2\n").unwrap();
    let o = nvf(&d, &["patch", "x.json", "set.patch", "-w"], "");
    assert!(o.status.success(), "{o:?}");
    let json = std::fs::read_to_string(d.join("x.json")).unwrap();
    assert_eq!(json.split_whitespace().collect::<String>(), "{\"a\":2}");
}

#[test]
fn stale_patch_fails_and_leaves_the_file() {
    let d = scratch("stale");
    std::fs::write(d.join("doc.nova"), "a: 1\n").unwrap();
    let rev = stdout(&nvf(&d, &["hash", "doc.nova"], ""));
    std::fs::write(d.join("bump.patch"), format!("@patch {}\n+ b: 2\n", rev.trim())).unwrap();
    assert!(nvf(&d, &["patch", "doc.nova", "bump.patch", "-w"], "").status.success());

    // The document has moved on from the revision the patch was written against.
    let o = nvf(&d, &["patch", "doc.nova", "bump.patch", "-w"], "");
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert_eq!(std::fs::read_to_string(d.join("doc.nova")).unwrap(), "a: 1\nb: 2\n");
}

#[test]
fn concurrent_patches_are_not_lost() {
    let d = scratch("parallel");
    std::fs::write(d.join("doc.nova"), "items: []\n").unwrap();
    let children: Vec<_> = (0..8)
        .map(|i| {
            let p = format!("{i}.patch");
            std::fs::write(d.join(&p), format!("@patch\n+ items: {i}\n")).unwrap();
            command(&d).args(["patch", "doc.nova", &p, "-w"]).stderr(Stdio::null()).spawn().unwrap()
        })
        .collect();
    for mut c in children {
        assert!(c.wait().unwrap().success());
    }

    let o = nvf(&d, &["get", "doc.nova", "items", "--to", "json"], "");
    let mut got: Vec<i32> = serde_json::from_str(&stdout(&o)).unwrap();
    got.sort_unstable();
    assert_eq!(got, (0..8).collect::<Vec<_>>());
}

#[cfg(unix)]
#[test]
fn locking_leaves_no_lock_file() {
    let d = scratch("nolock");
    std::fs::write(d.join("doc.nova"), "a: 1\n").unwrap();
    std::fs::write(d.join("add.patch"), "@patch\n+ b: 2\n").unwrap();
    assert!(nvf(&d, &["patch", "doc.nova", "add.patch", "-w"], "").status.success());
    let mut names: Vec<_> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
    names.sort();
    assert_eq!(names, ["add.patch", "doc.nova"]);
}

#[test]
fn fmt_refuses_other_formats() {
    let d = scratch("fmt-yaml");
    // Also valid NOVA text: the extension decides, not the content.
    std::fs::write(d.join("config.yaml"), "a: 1\n").unwrap();
    let o = nvf(&d, &["fmt", "config.yaml", "-w"], "");
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("nvf convert config.yaml --to nova -o config.nova"),
        "{o:?}"
    );
    assert_eq!(std::fs::read_to_string(d.join("config.yaml")).unwrap(), "a: 1\n");
}

#[test]
fn patch_prints_in_the_input_format() {
    let d = scratch("patch-format");
    std::fs::write(d.join("x.json"), "{\"a\": 1}\n").unwrap();
    std::fs::write(d.join("x.yaml"), "a: 1\n").unwrap();
    std::fs::write(d.join("x.jsonl"), "{\"a\": 1}\n").unwrap();
    std::fs::write(d.join("set.patch"), "@patch\na: 1 -> 2\n").unwrap();
    std::fs::write(d.join("add.patch"), "@patch\n+ [0].b: 2\n").unwrap();

    let o = nvf(&d, &["patch", "x.json", "set.patch"], "");
    assert!(o.status.success(), "{o:?}");
    assert_eq!(stdout(&o).split_whitespace().collect::<String>(), "{\"a\":2}");

    let o = nvf(&d, &["patch", "x.yaml", "set.patch"], "");
    assert!(o.status.success(), "{o:?}");
    assert_eq!(stdout(&o).trim(), "a: 2");

    // JSON Lines has no writer: printed as NOVA, refused in place.
    let o = nvf(&d, &["patch", "x.jsonl", "add.patch"], "");
    assert!(o.status.success(), "{o:?}");
    assert!(stdout(&o).contains("@compat json"), "{o:?}");
    let o = nvf(&d, &["patch", "x.jsonl", "add.patch", "-w"], "");
    assert_eq!(o.status.code(), Some(2), "{o:?}");
}

#[test]
fn imprecise_json_number_warns_or_fails_with_strict() {
    let d = scratch("pi");
    std::fs::write(d.join("pi.json"), "{\"pi\": 3.1415926535897932384626433832795028841971693993751}\n").unwrap();
    let o = nvf(&d, &["convert", "pi.json", "--to", "nova"], "");
    assert!(o.status.success(), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("warning: conversion to NOVA"), "{o:?}");
    assert!(stdout(&o).contains("pi: 3.14159"), "{o:?}");

    let o = nvf(&d, &["convert", "pi.json", "--to", "nova", "--strict"], "");
    assert_eq!(o.status.code(), Some(2), "{o:?}");
}

#[test]
fn record_streams_round_trip() {
    let d = scratch("record-streams");
    std::fs::write(
        d.join("a.jsonl"),
        "{\"id\": 1, \"req\": {\"m\": \"GET\"}}\n{\"id\": 2, \"req\": {\"m\": \"PUT\"}}\n",
    )
    .unwrap();
    let ok = |args: &[&str]| {
        let o = nvf(&d, args, "");
        assert!(o.status.success(), "{args:?}: {o:?}");
        stdout(&o)
    };
    ok(&["convert", "a.jsonl", "--to", "novl", "-o", "a.novl"]);
    assert!(std::fs::read_to_string(d.join("a.novl")).unwrap().contains("@columns {id, req{m}}"));
    ok(&["convert", "a.novl", "--to", "nova", "-o", "a.nova"]);
    ok(&["convert", "a.nova", "--to", "binary-stream", "-o", "a.novb"]);
    ok(&["convert", "a.novb", "--to", "novl", "-o", "b.novl"]);
    assert_eq!(
        std::fs::read_to_string(d.join("a.novl")).unwrap(),
        std::fs::read_to_string(d.join("b.novl")).unwrap()
    );
    assert_eq!(
        ok(&["convert", "b.novl", "--to", "json-compact"]).trim(),
        r#"[{"id":1,"req":{"m":"GET"}},{"id":2,"req":{"m":"PUT"}}]"#
    );

    // A corrupt end marker is a warning, and an error with --strict.
    std::fs::write(d.join("bad.novl"), "@nova 0.2\n{a: 1}\n@end 2\n").unwrap();
    ok(&["convert", "bad.novl", "--to", "json"]);
    assert_eq!(nvf(&d, &["convert", "bad.novl", "--strict", "--to", "json"], "").status.code(), Some(2));
}

#[test]
fn rewrites_never_change_the_format_or_target_stdin() {
    let d = scratch("rewrite");
    let src = "# keep me\nat: @timestamp(\"2024-01-01T00:00:00Z\")\npayload: !failed(\"disk full\")\n";
    std::fs::write(d.join("d.nova"), src).unwrap();

    // Human formatting keeps a variant payload's closing parenthesis.
    let o = nvf(&d, &["fmt", "d.nova"], "");
    assert!(o.status.success(), "{o:?}");
    assert!(stdout(&o).contains("payload: !failed(\"disk full\")\n"), "{}", stdout(&o));

    let o = nvf(&d, &["fmt", "d.nova", "--to", "json", "-w"], "");
    assert!(!o.status.success(), "{o:?}");
    assert_eq!(std::fs::read_to_string(d.join("d.nova")).unwrap(), src);

    for args in [&["fmt", "-", "-w"][..], &["lint", "--fix", "-"][..]] {
        let o = nvf(&d, args, "fooBar: 1\n");
        assert!(!o.status.success(), "{args:?}: {o:?}");
        assert!(!d.join("-").exists(), "{args:?}");
    }

    // --fix still rewrites a real file.
    std::fs::write(d.join("k.nova"), "fooBar: 1\n").unwrap();
    let o = nvf(&d, &["lint", "--fix", "k.nova"], "");
    assert_eq!(std::fs::read_to_string(d.join("k.nova")).unwrap(), "foo_bar: 1\n", "{o:?}");
}

#[test]
fn validate_looks_the_schema_up_in_the_project_catalog() {
    let d = scratch("catalog");
    std::fs::create_dir_all(d.join("schemas")).unwrap();
    std::fs::write(d.join("nova-catalog.nova"), "schemas:\n  - path: schemas/\n").unwrap();
    std::fs::write(d.join("schemas/a.novs"), "@schema acme.test/a@1.2.0\n@root R\nrecord R:\n  n: int\n").unwrap();
    std::fs::write(d.join("ok.nova"), "@schema acme.test/a@1.0.0\nn: 1\n").unwrap();
    std::fs::write(d.join("bad.nova"), "@schema acme.test/a@1.0.0\nn: x\n").unwrap();
    std::fs::write(d.join("other.nova"), "@schema other.test/a@1.0.0\nn: 1\n").unwrap();

    let o = nvf(&d, &["validate", "ok.nova"], "");
    assert!(o.status.success(), "{o:?}");
    assert!(!nvf(&d, &["validate", "bad.nova"], "").status.success());

    // Nothing to find: an error that names the ways out, and no network access.
    let o = nvf(&d, &["validate", "other.nova", "--fetch"], "");
    assert!(!o.status.success(), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("never fetched"), "{o:?}");

    let o = nvf(&d, &["schema", "resolve", "acme.test/a@1.1.0"], "");
    assert!(o.status.success(), "{o:?}");
    assert!(stdout(&o).starts_with("acme.test/a@1.2.0  "), "{}", stdout(&o));
    assert!(stdout(&o).contains("schemas/a.novs") && stdout(&o).contains("sha256:"), "{}", stdout(&o));

    // The meta-schema is built in, also under its old name.
    let o = nvf(&d, &["schema", "resolve", "nova-schema@0.3.2"], "");
    assert!(o.status.success() && stdout(&o).contains("built-in"), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("novafile.dev/meta/schema"), "{o:?}");
}

/// Stderr as text.
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A schema that imports the standard library and classifies values.
const OPS: &str = "@schema acme.test/ops@1.0.0
@import novafile.dev/std@0.1.0 as std
@root Doc

type Email = std.Email @personal

record Credentials @secret:
  user: string
  password: string

record Service:
  id: string @key
  db_url: std.Uri @secret
  owner?: Email
  pin?: int @secret
  admin?: Credentials
  replicas?: int

record Doc:
  services: list<Service>
";

/// A document with secrets and personal data, against [`OPS`].
const DOC: &str = "@schema acme.test/ops@1.0.0
services:
  - id: api
    db_url: \"postgres://u:pw@db/x\"
    owner: \"ada@example.org\"
    pin: 1234
    admin: {user: root, password: toor}
    replicas: 3
  - id: web
    db_url: \"https://example.org/\"
    owner: \"ada@example.org\"
    replicas: 2
";

/// A scratch directory with a project catalog that finds [`OPS`], and [`DOC`] as `doc.nova`.
fn classified(name: &str) -> PathBuf {
    let d = scratch(name);
    std::fs::create_dir_all(d.join("schemas")).unwrap();
    std::fs::write(d.join("nova-catalog.nova"), "schemas:\n  - path: schemas/\n").unwrap();
    std::fs::write(d.join("schemas/ops.novs"), OPS).unwrap();
    std::fs::write(d.join("doc.nova"), DOC).unwrap();
    d
}

/// Runs `nvf` and returns stdout; fails the test if it fails.
fn ok(d: &Path, args: &[&str]) -> String {
    let o = nvf(d, args, "");
    assert!(o.status.success(), "{args:?}: {o:?}");
    stdout(&o)
}

#[test]
fn get_redacts_secrets_and_pseudonymizes_personal_data() {
    let d = classified("get-redact");
    let out = ok(&d, &["get", "doc.nova", "services"]);
    for secret in ["postgres", "1234", "toor", "root", "ada@example.org"] {
        assert!(!out.contains(secret), "{secret} leaked:\n{out}");
    }

    // The revision would let a reader test guesses of a hidden value: a token replaces it.
    assert!(out.starts_with("@revision k:"), "{out}");
    assert_eq!(out.matches("@redacted(\"p:").count(), 2, "{out}");

    // Raw output: one placeholder per line, equal values with equal pseudonyms.
    let raw = ok(&d, &["get", "doc.nova", "services[*].owner", "-r"]);
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2, "{raw}");
    assert!(lines[0].starts_with("@redacted(p:") && lines[0] == lines[1], "{raw}");
    assert_eq!(ok(&d, &["get", "doc.nova", "services[0].pin", "-r"]), "@redacted\n");

    let json = ok(&d, &["get", "doc.nova", "services[id=api].admin", "--to", "json-compact"]);
    assert_eq!(json.trim(), r#"{"$redacted":null}"#);

    // Filters that compare a hidden value fail instead of answering.
    let o = nvf(&d, &["get", "doc.nova", "services[?pin<5000].id"], "");
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert!(stderr(&o).contains("filter on classified value"), "{o:?}");
    assert_eq!(ok(&d, &["get", "doc.nova", "services[?replicas>2].id", "-r"]), "api\n");
}

#[test]
fn reveal_shows_what_it_names() {
    let d = classified("reveal");
    let all = ok(&d, &["get", "--reveal", "doc.nova", "services[id=api]"]);
    assert!(
        all.contains("postgres://u:pw@db/x") && all.contains("toor") && !all.contains("@redacted"),
        "{all}"
    );

    let personal = ok(&d, &["get", "doc.nova", "services[id=api]", "--reveal=personal"]);
    assert!(personal.contains("ada@example.org") && !personal.contains("postgres"), "{personal}");
    assert!(personal.contains("db_url: @redacted"), "{personal}");

    let one = ok(&d, &["get", "doc.nova", "services", "--reveal=services[id=api].db_url"]);
    assert!(one.contains("postgres://u:pw@db/x"), "{one}");
    assert!(!one.contains("https://example.org/") && !one.contains("ada@example.org"), "{one}");

    let resolved = ok(&d, &["validate", "doc.nova", "--resolve"]);
    assert!(resolved.contains("db_url: @redacted") && !resolved.contains("toor"), "{resolved}");
    assert!(ok(&d, &["validate", "doc.nova", "--resolve", "--reveal"]).contains("toor"));
}

#[test]
fn convert_redacts_and_refuses_binary_placeholders() {
    let d = classified("convert");
    let json = ok(&d, &["convert", "doc.nova", "--to", "json-compact"]);
    assert!(json.contains(r#""db_url":{"$redacted":null}"#) && !json.contains("toor"), "{json}");

    let o = nvf(&d, &["convert", "doc.nova", "--to", "json", "--strict"], "");
    assert_eq!(o.status.code(), Some(2), "redaction is a lossy conversion: {o:?}");

    for to in ["binary", "cbor", "binary-stream", "yaml", "toml"] {
        let o = nvf(&d, &["convert", "doc.nova", "--to", to], "");
        assert_eq!(o.status.code(), Some(2), "{to}: {o:?}");
        assert!(stderr(&o).contains("use --reveal or a text format"), "{to}: {o:?}");
    }

    let o = nvf(&d, &["convert", "doc.nova", "--to", "cbor", "--reveal"], "");
    assert!(o.status.success(), "{o:?}");
}

#[test]
fn patch_accepts_the_token_from_get() {
    let d = classified("token");
    let out = ok(&d, &["--agent", "on", "get", "doc.nova", "services[id=api].replicas"]);
    let token = out.lines().next().and_then(|l| l.strip_prefix("@revision ")).unwrap();
    assert!(token.starts_with("k:"), "{out}");
    // The token stands for the stored document, not for the view.
    assert_eq!(
        ok(&d, &["--agent", "on", "get", "doc.nova", "services"]).lines().next(),
        Some(out.lines().next().unwrap())
    );

    std::fs::write(
        d.join("p.patch"),
        format!("@patch {token}\n@scope services[id=api]\nreplicas: 3 -> 4\n= db_url: \"postgres://new\"\n"),
    )
    .unwrap();
    ok(&d, &["patch", "doc.nova", "p.patch", "-w"]);
    let doc = std::fs::read_to_string(d.join("doc.nova")).unwrap();
    assert!(doc.contains("replicas: 4") && doc.contains("postgres://new"), "{doc}");

    // The document has moved on, so the same token is stale.
    let o = nvf(&d, &["patch", "doc.nova", "p.patch", "-w"], "");
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert!(stderr(&o).contains("conflict") && !stderr(&o).contains("sha256:"), "{o:?}");

    // A placeholder from a redacted view cannot be written back.
    std::fs::write(d.join("back.patch"), "@patch\n= services[id=api].db_url: @redacted\n").unwrap();
    assert_eq!(nvf(&d, &["patch", "doc.nova", "back.patch"], "").status.code(), Some(2));
}

#[test]
fn diff_reports_changed_classified_values_without_them() {
    let d = classified("diff");
    std::fs::write(
        d.join("new.nova"),
        DOC.replace("postgres://u:pw@db/x", "postgres://u:pw2@db/x")
            .replace("replicas: 3", "replicas: 4"),
    )
    .unwrap();
    let out = ok(&d, &["diff", "doc.nova", "new.nova"]);
    assert!(out.contains("= services[id=api].db_url: @redacted\n"), "{out}");
    assert!(out.contains("services[id=api].replicas: 3 -> 4\n"), "{out}");
    assert!(!out.contains("postgres"), "{out}");

    assert!(ok(&d, &["--agent", "on", "diff", "doc.nova", "new.nova"]).contains("@base k:"));
    assert!(ok(&d, &["diff", "--reveal", "doc.nova", "new.nova"]).contains("pw2"));
}

#[test]
fn unresolvable_schema_is_refused_in_agent_mode() {
    let d = classified("lost");
    std::fs::write(d.join("lost.nova"), DOC.replace("acme.test/ops", "acme.test/missing")).unwrap();
    for args in [
        &["--agent", "on", "get", "lost.nova", "services"][..],
        &["--agent", "on", "get", "lost.nova", "services", "--reveal=personal"],
        &["--agent", "on", "convert", "lost.nova", "--to", "json"],
        &["--agent", "on", "diff", "lost.nova", "doc.nova"],
        &["--agent", "on", "hash", "lost.nova"],
    ] {
        let o = nvf(&d, args, "");
        assert_eq!(o.status.code(), Some(2), "{args:?}: {o:?}");
        assert!(stdout(&o).is_empty() && stderr(&o).contains("--no-schema"), "{args:?}: {o:?}");
    }

    let o = nvf(&d, &["--agent", "on", "get", "lost.nova", "services[0].id", "-r", "--no-schema"], "");
    assert!(o.status.success() && stderr(&o).contains("without classification"), "{o:?}");
    assert_eq!(stdout(&o), "api\n");

    // Outside agent mode, a person gets the values and a warning.
    let o = nvf(&d, &["get", "lost.nova", "services[0].db_url", "-r"], "");
    assert!(o.status.success() && stderr(&o).contains("cannot find the schema"), "{o:?}");
    assert!(stdout(&o).contains("postgres"), "{o:?}");
    assert_eq!(ok(&d, &["get", "lost.nova", "services[0].id", "-r", "--reveal"]), "api\n");

    // A document that is invalid against its schema cannot be classified either.
    std::fs::write(d.join("bad.nova"), "@schema acme.test/ops@1.0.0\nservices: [{id: x, db_url: 3, pin: 1234}]\n").unwrap();
    let o = nvf(&d, &["--agent", "on", "get", "bad.nova", "services"], "");
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert!(stdout(&o).is_empty() && stderr(&o).contains("cannot be classified"), "{o:?}");

    // Without @schema nothing is classified, and nothing changes.
    std::fs::write(d.join("plain.nova"), "password: hunter2\n").unwrap();
    assert_eq!(ok(&d, &["get", "plain.nova", "password", "-r"]), "hunter2\n");
}

#[test]
fn schema_files_import_std_and_catalog_schemas() {
    let d = classified("import");
    let doc = DOC.lines().skip(1).collect::<Vec<_>>().join("\n");
    std::fs::write(d.join("bare.nova"), doc).unwrap();
    let o = nvf(&d, &["validate", "bare.nova", "--schema", "schemas/ops.novs"], "");
    assert!(o.status.success(), "{o:?}");
    assert!(ok(&d, &["schema", "fmt", "schemas/ops.novs"]).contains("@import novafile.dev/std@0.1.0"));
    std::fs::write(d.join("bad.nova"), "services: [{id: x, db_url: \"not a uri\"}]\n").unwrap();
    assert!(!nvf(&d, &["validate", "bad.nova", "--schema", "schemas/ops.novs"], "").status.success());

    // Imports of other schemas resolve through the catalogs.
    std::fs::write(d.join("schemas/base.novs"), "@schema acme.test/base@1.0.0\ntype Name = string @len(1..8)\n").unwrap();
    std::fs::write(
        d.join("app.novs"),
        "@schema acme.test/app@1.0.0\n@import acme.test/base@1.0.0 as base\n@root R\nrecord R:\n  name: base.Name\n",
    )
    .unwrap();
    std::fs::write(d.join("app.nova"), "name: short\n").unwrap();
    std::fs::write(d.join("long.nova"), "name: much-too-long\n").unwrap();
    assert!(nvf(&d, &["validate", "app.nova", "--schema", "app.novs"], "").status.success());
    assert!(!nvf(&d, &["validate", "long.nova", "--schema", "app.novs"], "").status.success());
}

#[test]
fn key_file_is_made_once_and_private() {
    let d = classified("keyfile");
    let get = || run(command(&d).env_remove("NOVA_REDACT_KEY").args(["get", "doc.nova", "services[0].owner"]), "");
    let first = stdout(&get());
    let path = d.join(".cache/nova/redact.key");
    assert!(path.is_file(), "{first}");
    assert_eq!(std::fs::read_to_string(&path).unwrap().len(), 64);
    assert_eq!(stdout(&get()), first, "the key is reused");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[test]
fn lint_errors_fail_the_run() {
    let d = scratch("lint-error");
    std::fs::write(
        d.join("s.novs"),
        "@schema acme.test/l@1.0.0\n@root R\nrecord R:\n  token: string @secret = \"x\"\n",
    )
    .unwrap();
    let o = nvf(&d, &["lint", "s.novs", "--format", "json"], "");
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    assert!(stdout(&o).contains(r#""level":"error""#), "{o:?}");
}

#[test]
fn conflicts_do_not_show_classified_values() {
    let d = classified("conflict");
    for (name, body) in [
        ("leaf", "services[id=api].db_url: \"guess\" -> \"y\"\n"),
        ("test", "? services[id=api].db_url: \"guess\"\n"),
        ("container", "services[id=api]: {id: api} -> {id: api}\n"),
    ] {
        std::fs::write(d.join("c.patch"), format!("@patch\n{body}")).unwrap();
        let o = nvf(&d, &["patch", "doc.nova", "c.patch"], "");
        assert_eq!(o.status.code(), Some(2), "{name}: {o:?}");
        let err = stderr(&o);
        assert!(err.contains("did not match") && !err.contains("postgres"), "{name}: {err}");
    }

    // Conflicts on unclassified values still show both sides.
    std::fs::write(d.join("c.patch"), "@patch\nservices[id=api].replicas: 9 -> 4\n").unwrap();
    assert!(stderr(&nvf(&d, &["patch", "doc.nova", "c.patch"], "")).contains("found 3"));
}

#[test]
fn render_redacts_imported_classified_values() {
    let d = classified("render");
    std::fs::write(
        d.join("t.nova"),
        "@template\n@import \"doc.nova\" as d\nurl: $d.services[0].db_url\nn: $d.services[0].replicas\n",
    )
    .unwrap();
    let out = ok(&d, &["render", "t.nova"]);
    assert!(out.contains("url: @redacted") && !out.contains("postgres"), "{out}");
    assert!(ok(&d, &["render", "--reveal", "t.nova"]).contains("postgres"));
    let o = nvf(&d, &["render", "t.nova", "--to", "yaml"], "");
    assert!(!o.status.success() && !stdout(&o).contains("postgres"), "{o:?}");

    // A file is a deployment artefact: it gets the values in clear.
    ok(&d, &["render", "t.nova", "-o", "out.nova"]);
    assert!(std::fs::read_to_string(d.join("out.nova")).unwrap().contains("postgres"));
}

#[test]
fn reordered_sets_hide_the_right_element_everywhere() {
    let d = scratch("set-order");
    std::fs::create_dir_all(d.join("schemas")).unwrap();
    std::fs::write(d.join("nova-catalog.nova"), "schemas:\n  - path: schemas/\n").unwrap();
    std::fs::write(
        d.join("schemas/s.novs"),
        "@schema acme.test/s@1.0.0\n@root R\nrecord E:\n  name: string\n  pin?: string @secret\nrecord R:\n  items: set<E>\n  n: int = 1\n",
    )
    .unwrap();
    // Out of canonical order: the element with the secret comes first.
    let doc = "@schema acme.test/s@1.0.0\nitems:\n  - {name: z, pin: \"9876\"}\n  - {name: a}\n";
    std::fs::write(d.join("doc.nova"), doc).unwrap();
    let out = ok(&d, &["get", "doc.nova", "items"]);
    assert!(!out.contains("9876") && out.contains("z,@redacted"), "{out}");
    // The view keeps the stored form: no default `n` appears.
    assert!(!ok(&d, &["get", "doc.nova", "."]).contains("n: 1"));

    std::fs::write(d.join("c.patch"), "@patch\n? items[0].pin: \"0000\"\n").unwrap();
    let err = stderr(&nvf(&d, &["patch", "doc.nova", "c.patch"], ""));
    assert!(err.contains("did not match") && !err.contains("9876"), "{err}");

    std::fs::write(d.join("new.nova"), doc.replace("9876", "1111")).unwrap();
    let diff = ok(&d, &["diff", "doc.nova", "new.nova"]);
    assert!(!diff.contains("9876") && !diff.contains("1111"), "{diff}");
}

#[test]
fn validate_and_patch_output_hide_classified_values() {
    let d = classified("echo");
    std::fs::write(d.join("bad.nova"), DOC.replace("replicas: 3", "replicas: lots")).unwrap();
    let err = stderr(&nvf(&d, &["validate", "bad.nova"], ""));
    assert!(err.contains("replicas") && !err.contains("postgres"), "{err}");

    std::fs::write(d.join("p.patch"), "@patch\nservices[id=api].replicas: 3 -> 4\n").unwrap();
    let out = ok(&d, &["patch", "doc.nova", "p.patch"]);
    assert!(out.contains("replicas: 4") && !out.contains("postgres"), "{out}");
}

#[test]
fn hash_stats_and_fmt_do_not_show_classified_values() {
    let d = classified("other-sinks");
    let hash = ok(&d, &["hash", "doc.nova"]);
    assert!(hash.starts_with("k:"), "{hash}");
    assert!(ok(&d, &["hash", "--reveal", "doc.nova"]).starts_with("sha256:"));

    let json = ok(&d, &["fmt", "doc.nova", "--to", "json"]);
    assert!(json.contains("$redacted") && !json.contains("postgres"), "{json}");
    assert!(ok(&d, &["fmt", "doc.nova", "--to", "json", "--reveal"]).contains("postgres"));

    let plain = ok(&d, &["stats", "--reveal", "doc.nova"]);
    assert_ne!(ok(&d, &["stats", "doc.nova"]), plain, "sizes are those of the redacted view");
}

#[test]
fn the_key_file_is_made_once_and_hash_needs_a_key() {
    let d = classified("key-file");
    let keyless = || {
        let mut c = command(&d);
        c.env_remove("NOVA_REDACT_KEY");
        c
    };
    let hash = || run(keyless().args(["hash", "doc.nova"]), "");
    let (a, b) = (hash(), hash());
    assert!(a.status.success() && stdout(&a).starts_with("k:"), "{a:?}");
    assert_eq!(stdout(&a), stdout(&b), "the key file keeps the token stable");

    let key = d.join(".cache/nova/redact.key");
    let dir = key.parent().unwrap();
    assert_eq!(std::fs::read_to_string(&key).unwrap().len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&key).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let names: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert!(names.iter().all(|n| !n.to_string_lossy().ends_with(".tmp")), "{names:?}");

    // A broken key file: no stable token, so hash fails instead of printing a random one.
    std::fs::write(&key, "").unwrap();
    let o = hash();
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert!(stderr(&o).contains("NOVA_REDACT_KEY") && stderr(&o).contains("delete the file"), "{o:?}");
    assert!(stdout(&run(keyless().args(["hash", "--reveal", "doc.nova"]), "")).starts_with("sha256:"));
}

/// `fmt -w` takes the same lock as `patch -w`, so it never writes back a stale copy.
#[cfg(unix)]
#[test]
fn fmt_in_place_waits_for_the_lock() {
    let d = scratch("fmt-lock");
    std::fs::write(d.join("doc.nova"), "a:   1\n").unwrap();
    let held = std::fs::File::open(d.join("doc.nova")).unwrap();
    held.lock().unwrap();
    let mut fmt = command(&d).args(["fmt", "doc.nova", "-w"]).stderr(Stdio::null()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(fmt.try_wait().unwrap().is_none(), "fmt -w wrote while another process held the lock");

    drop(held);
    assert!(fmt.wait().unwrap().success());
    assert_eq!(std::fs::read_to_string(d.join("doc.nova")).unwrap(), "a: 1\n");
}

/// `fmt -w` refuses schemas, whose writer would drop their comments.
#[test]
fn fmt_refuses_to_rewrite_schemas() {
    let d = scratch("fmt-novs");
    let src = "# SPDX-License-Identifier: MIT\n@schema acme.dev/a@1.0.0\n@root A\n\nrecord A:\n  a: int\n";
    std::fs::write(d.join("a.novs"), src).unwrap();
    let o = nvf(&d, &["fmt", "a.novs", "-w"], "");
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert_eq!(std::fs::read_to_string(d.join("a.novs")).unwrap(), src);
    assert!(nvf(&d, &["fmt", "a.novs"], "").status.success());
}

/// `patch -w` on YAML says that comments are not kept.
#[test]
fn patch_in_place_notes_lost_comments() {
    let d = scratch("yaml-comments");
    std::fs::write(d.join("x.yaml"), "# keep\na: 1\n").unwrap();
    std::fs::write(d.join("set.patch"), "@patch\na: 1 -> 2\n").unwrap();
    let o = nvf(&d, &["patch", "x.yaml", "set.patch", "-w"], "");
    assert!(o.status.success(), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("comments are not kept"), "{o:?}");
}

/// A patch and its document cannot both be read from stdin.
#[test]
fn patch_rejects_two_stdins() {
    let d = scratch("two-stdins");
    let o = nvf(&d, &["patch", "-", "-"], "@patch\n+ a: 1\n");
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("both come from stdin"), "{o:?}");
}

/// Output goes through a symlink to its target, also when the target does not exist yet.
#[cfg(unix)]
#[test]
fn output_writes_through_a_dangling_symlink() {
    let d = scratch("dangling");
    std::fs::create_dir(d.join("sub")).unwrap();
    std::fs::write(d.join("a.nova"), "a: 1\n").unwrap();
    std::os::unix::fs::symlink("sub/target.json", d.join("link.json")).unwrap();
    let o = nvf(&d, &["convert", "a.nova", "--to", "json", "-o", "link.json"], "");
    assert!(o.status.success(), "{o:?}");
    assert!(std::fs::symlink_metadata(d.join("link.json")).unwrap().file_type().is_symlink());
    assert!(std::fs::read_to_string(d.join("sub/target.json")).unwrap().contains("\"a\""));
}

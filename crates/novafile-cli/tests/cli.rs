//! `nvf` driven as a process: patching through stdin, in place, and concurrently.

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

/// Runs `nvf` with `args` in `dir`, feeding `stdin`.
fn nvf(dir: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_nvf"))
        .args(args)
        .current_dir(dir)
        .env("NOVA_AGENT", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
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
            Command::new(env!("CARGO_BIN_EXE_nvf"))
                .args(["patch", "doc.nova", &p, "-w"])
                .current_dir(&d)
                .env("NOVA_AGENT", "off")
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
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

use novafile_template::{FsLoader, Limits, Options, render};
use novafile_value::{Record, Value};

fn args(src: &str) -> Record {
    match novafile_text::parse(src).unwrap() {
        Value::Record(r) => r,
        _ => panic!(),
    }
}

const TPL: &str = r#"@template
@param env: string
@param replicas: u16 @range(1..50) = 2
@param services: list<string>
@param metrics: bool = false
@param owner?: string
@let defaults:
  enabled: true
  replicas: $replicas
@let registry = "ghcr.io/example"

environment: $env
region: "${env}-eu-central-1"
services:
  @for i, name in $services:
    - ...$defaults
      id: $name
      image: "${registry}/${name}:v2"
      port: 8080
      index: $i
  @if $metrics and $env == production:
    - id: metrics
      image: "${registry}/metrics:v1"
      port: 9090
  @else:
    - id: noop
      port: 1
labels:
  @for name in $services:
    "app-${name}": $name
  @if $owner != null:
    owner: $owner
"#;

#[test]
fn renders() {
    let v = render(TPL, &args("env: production\nservices: [auth, api]\nmetrics: true\n"), &Options::default()).unwrap();
    let expected = novafile_text::parse(
        r#"
environment: production
region: production-eu-central-1
services:
  - enabled: true
    replicas: 2
    id: auth
    image: ghcr.io/example/auth:v2
    port: 8080
    index: 0
  - enabled: true
    replicas: 2
    id: api
    image: ghcr.io/example/api:v2
    port: 8080
    index: 1
  - id: metrics
    image: ghcr.io/example/metrics:v1
    port: 9090
labels:
  app-auth: auth
  app-api: api
"#,
    )
    .unwrap();
    assert_eq!(v, expected, "{}", novafile_text::to_text(&v, novafile_text::Profile::Human));

    let v = render(TPL, &args("env: staging\nservices: [x]\nowner: ops\nreplicas: 7\n"), &Options::default()).unwrap();
    let t = novafile_text::to_text(&v, novafile_text::Profile::Human);
    assert!(t.contains("id: noop") && t.contains("owner: ops") && t.contains("replicas: 7"), "{t}");
}

#[test]
fn spread_override_order() {
    let v = render(
        "@template\n@let base = {a: 1, b: 2}\nx: {...$base, b: 3}\ny: {b: 3, ...$base}\n",
        &Record::new(),
        &Options::default(),
    )
    .unwrap();
    assert_eq!(v, novafile_text::parse("x: {a: 1, b: 3}\ny: {b: 2, a: 1}\n").unwrap());
}

#[test]
fn param_errors() {
    for (a, needle) in [
        ("services: [a]\n", "missing required parameter \"env\""),
        ("env: x\nservices: [a]\nreplicas: 99\n", "outside @range(1..50)"),
        ("env: x\nservices: a\n", "expected list<string>"),
        ("env: x\nservices: [a]\nbogus: 1\n", "unknown parameter"),
        ("env: x\nservices: [a]\nmetrics: yes\n", "expected bool"),
    ] {
        let e = render(TPL, &args(a), &Options::default()).unwrap_err().to_string();
        assert!(e.contains(needle), "{a:?}: {e}");
    }
}

#[test]
fn eval_errors() {
    for (src, needle) in [
        ("@template\nx: $nope\n", "undefined reference $nope"),
        ("@template\n@let a = [1]\nx: \"${a}\"\n", "cannot interpolate a list"),
        ("@template\n@let a = 1\nx:\n  @if $a:\n    y: 1\n", "conditions must be bool"),
        ("@template\n@let a = [1]\nx: {...$a}\n", "cannot spread a list into a record"),
        ("@template\n@let k = [a, a]\nx:\n  @for n in $k:\n    \"${n}\": 1\n", "duplicate key"),
        ("@template\n@import \"/etc/passwd\" as p\nx: 1\n", "imports are disabled"),
    ] {
        let e = render(src, &Record::new(), &Options::default()).unwrap_err().to_string();
        assert!(e.contains(needle), "{src:?}: {e}");
    }
}

#[test]
fn output_budget_stops_exponential_reuse() {
    let mut src = String::from("@template\n@let a0 = [1, 1, 1, 1, 1, 1, 1, 1, 1, 1]\n");
    for i in 1..8 {
        src.push_str(&format!(
            "@let a{i} = [$a{p}, $a{p}, $a{p}, $a{p}, $a{p}, $a{p}, $a{p}, $a{p}, $a{p}, $a{p}]\n",
            p = i - 1
        ));
    }
    src.push_str("x: $a7\n");
    let opts = Options {
        limits: Limits {
            max_nodes: 100_000,
            ..Limits::default()
        },
        ..Options::default()
    };
    let e = render(&src, &Record::new(), &opts).unwrap_err().to_string();
    assert!(e.contains("exceeds the limit"), "{e}");
}

#[test]
fn data_documents_render_to_themselves_and_templates_need_rendering() {
    let v = render("a: 1\n", &Record::new(), &Options::default()).unwrap();
    assert_eq!(v, novafile_text::parse("a: 1\n").unwrap());
    assert!(novafile_text::parse("@template\na: 1\n").is_err());
}

#[test]
fn sandboxed_imports() {
    let dir = std::env::temp_dir().join(format!("nova-tpl-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("base")).unwrap();
    std::fs::write(
        dir.join("base/common.nova"),
        "@template\n@let r = \"ghcr.io/x\"\nregistry: $r\nlimits: {cpu: 1}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("app.nova"),
        "@template\n@import \"base/common.nova\" as common\n...$common\nname: app\nimage: \"${common.registry}/app\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("evil.nova"), "@template\n@import \"../outside.nova\" as x\na: 1\n").unwrap();
    std::fs::write(dir.join("cycle.nova"), "@template\n@import \"cycle.nova\" as x\na: 1\n").unwrap();
    let loader = FsLoader { root: dir.clone() };
    let opts = Options {
        loader: &loader,
        ..Options::default()
    };
    let src = std::fs::read_to_string(dir.join("app.nova")).unwrap();
    let v = render(&src, &Record::new(), &opts).unwrap();
    assert_eq!(
        v,
        novafile_text::parse("registry: ghcr.io/x\nlimits: {cpu: 1}\nname: app\nimage: ghcr.io/x/app\n").unwrap()
    );
    let e = render(&std::fs::read_to_string(dir.join("evil.nova")).unwrap(), &Record::new(), &opts)
        .unwrap_err()
        .to_string();
    assert!(e.contains("escapes"), "{e}");
    let e = render(&std::fs::read_to_string(dir.join("cycle.nova")).unwrap(), &Record::new(), &opts)
        .unwrap_err()
        .to_string();
    assert!(e.contains("cycle"), "{e}");
    std::fs::remove_dir_all(dir).ok();
}

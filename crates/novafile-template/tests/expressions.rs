//! Template language 0.2: expressions, `@def` and the standard library (proposal 0006). The
//! `case_*` tests mirror the conformance cases of §0006.12, one test per case.

use std::fmt::Write as _;

use novafile_template::{BUILTINS, Limits, Options, render};
use novafile_value::{Record, Value};

/// Parse `src` (a NOVA record) as template arguments.
fn args(src: &str) -> Record {
    match novafile_text::parse(src).unwrap() {
        Value::Record(r) => r,
        v => panic!("{v:?}"),
    }
}

/// Render `tpl` with `a` and compare with the NOVA text `want`.
#[track_caller]
fn ok(tpl: &str, a: &str, want: &str) {
    let v = render(tpl, &args(a), &Options::default()).unwrap_or_else(|e| panic!("{tpl}\n{e}"));
    assert_eq!(
        v,
        novafile_text::parse(want).unwrap(),
        "{}",
        novafile_text::to_text(&v, novafile_text::Profile::Human)
    );
}

/// Render `tpl` with `a`, expecting an error that contains `needle`.
#[track_caller]
fn fails(tpl: &str, a: &str, needle: &str) {
    match render(tpl, &args(a), &Options::default()) {
        Ok(v) => panic!("{tpl}\nrendered {v:?}, expected an error with {needle:?}"),
        Err(e) => assert!(e.to_string().contains(needle), "{tpl}\n{e}"),
    }
}

/// A list of `n` ints as an argument value.
fn ints(n: i128) -> Value {
    Value::List((0..n).map(Value::Int).collect())
}

#[test]
fn case_case_mapping() {
    ok(
        "@template 0.2\na: $upper(\"straße\")\nb: $lower(\"ΣΑΣ\")\nc: $lower(\"İ\")\nd: $upper(\"ǆ\")\n",
        "",
        "a: STRASSE\nb: σας\nc: \"i\\u{307}\"\nd: Ǆ\n",
    );
}

#[test]
fn case_trim_unicode() {
    ok(
        "@template\na: $trim(\"\\u{A0}\\u{2003}x\\u{3000}\")\nb: $trim(\"\\u{200B}x\\u{200B}\")\n",
        "",
        "a: x\nb: \"\\u{200B}x\\u{200B}\"\n",
    );
}

#[test]
fn case_split_join() {
    ok(
        "@template\na: $split(\"a,,b\", \",\")\nb: $split(\"\", \",\")\nc: $join([1, -2, @timestamp(\"2026-01-01T00:00:00Z\"), @duration(\"90s\")], \",\")\n",
        "",
        "a: [a, \"\", b]\nb: [\"\"]\nc: \"1,-2,2026-01-01T00:00:00Z,1m30s\"\n",
    );
}

#[test]
fn case_replace_empty() {
    fails(
        "@template\n@param s: string\nx: $replace($s, \"\", x)\n",
        "s: abc\n",
        "replace: the string to replace must not be empty",
    );
}

#[test]
fn case_slice_truncate() {
    let name = format!("{}-{}", "a".repeat(62), "b".repeat(7));
    ok(
        "@template\n@param name: string\nx: $trim_suffix($slice($name, 0, 63), \"-\")\nn: $len($x0)\n"
            .replace("$x0", "$trim_suffix($slice($name, 0, 63), \"-\")")
            .as_str(),
        &format!("name: {name}\n"),
        &format!("x: {}\nn: 62\n", "a".repeat(62)),
    );
}

#[test]
fn case_record_iteration_order() {
    ok(
        "@template\n@param r: any\nitems:\n  @for k, v in $r:\n    - $k\n",
        "r: {b: 1, a: 2, \"B\": 3}\n",
        "items: [B, a, b]\n",
    );
}

#[test]
fn case_set_iteration_order() {
    ok(
        "@template\nx: $([for x in @set[\"b\", \"a\", \"c\"]: $x])\ny: $([for i, x in @set[64, 63, -64]: [$i, $x]])\n",
        "",
        "x: [a, b, c]\ny: [[0, 63], [1, -64], [2, 64]]\n",
    );
}

#[test]
fn case_comprehension_filter() {
    ok(
        "@template\n@param xs: list<int>\na: $([for x in $xs if $x > 2: $x * 10])\nb: $({for i, x in $xs if $i != 1: \"k${i}\": $x})\n",
        "xs: [1, 2, 3, 4]\n",
        "a: [30, 40]\nb: {k0: 1, k2: 3, k3: 4}\n",
    );
}

#[test]
fn case_comprehension_duplicate_key() {
    fails("@template\nx: $({for v in [a, b, a]: $v: 1})\n", "", "duplicate key \"a\"");
}

#[test]
fn case_operator_whitespace() {
    ok(
        "@template\n@param x: int\n@param x-1: int\na: $x-1\nb: $x - 1\n",
        "x: 5\nx-1: 10\n",
        "a: 10\nb: 4\n",
    );
}

#[test]
fn case_mixed_kind_arithmetic() {
    fails("@template\n@param i: int\nx: $i + 1.0\n", "i: 1\n", "cannot apply `+` to a int and a float");
}

#[test]
fn case_int_portable_range() {
    fails("@template\nx: $(18446744073709551615 + 1)\n", "", "outside the portable range");
    ok(
        "@template\nx: $(18446744073709551614 + 1)\ny: $(-9223372036854775807 - 1)\n",
        "",
        "x: 18446744073709551615\ny: -9223372036854775808\n",
    );
}

#[test]
fn case_floor_mod() {
    ok(
        "@template\na: $div(-7, 2)\nb: $mod(-7, 2)\nc: $round(-2.5)\nd: $mod(7, -2)\n",
        "",
        "a: -4\nb: 1\nc: -3\nd: -1\n",
    );
}

#[test]
fn case_compare_kinds() {
    fails("@template\nx: $(1 < \"2\")\n", "", "cannot compare a int with a string");
}

#[test]
fn case_def_block() {
    ok(
        "@template\n@def labels(component: string):\n  name: $component\n  @if $component == web:\n    tier: frontend\na: $labels(web)\nb: $labels(db)\n",
        "",
        "a: {name: web, tier: frontend}\nb: {name: db}\n",
    );
}

#[test]
fn case_def_forward_call() {
    fails(
        "@template\n@def a(x: int) = $b($x)\n@def b(x: int) = $x\ny: $a(1)\n",
        "",
        "calls b, which is defined below it",
    );
}

#[test]
fn case_def_self_call() {
    fails("@template\n@def a(x: int) = $a($x)\ny: 1\n", "", "@def a calls itself");
}

#[test]
fn case_def_argument_no_conversion() {
    fails(
        "@template\n@def f(p: u16) = $p\nx: $f(\"80\")\n",
        "",
        "argument \"p\" of f does not match `u16`",
    );
}

#[test]
fn case_def_shadows_builtin() {
    ok("@template\n@def lower(s: string) = \"custom-${s}\"\nx: $lower(A)\n", "", "x: custom-A\n");
}

#[test]
fn case_step_budget() {
    let mut src = String::from("@template\n@def f0(x: int): int = $x\n");
    for i in 1..30 {
        let _ = writeln!(src, "@def f{i}(x: int): int = $f{p}($x) + $f{p}($x)", p = i - 1);
    }

    src.push_str("x: $f29(1)\n");
    fails(&src, "", "exceeds the limit of 10000000 steps");
}

#[test]
fn case_nested_comprehension_budget() {
    let mut a = Record::new();
    a.set("xs", ints(300));
    let src = "@template\n@param xs: list<int>\nx: $len([for a in $xs: [for b in $xs: [for c in $xs if $c != $c: $a]]])\n";
    let e = render(src, &a, &Options::default()).unwrap_err();
    assert!(e.to_string().contains("exceeds the limit of 10000000 steps"), "{e}");
}

#[test]
fn case_intermediate_output_budget() {
    let mut a = Record::new();
    a.set("xs", ints(2000));
    a.set("ys", ints(1000));
    let src = "@template\n@param xs: list<int>\n@param ys: list<int>\nx: $len([for a in $xs: [for b in $ys: 1]])\n";
    let e = render(src, &a, &Options::default()).unwrap_err();
    assert!(e.to_string().contains("exceeds the limit of 1000000 nodes"), "{e}");
}

#[test]
fn case_interpolation_call() {
    ok(
        "@template\n@param name: string\na: \"${lower($name)}-svc\"\nb: \"${name}\"\nc: |\n  ${upper($name)} ${ name }\n",
        "name: Web\n",
        "a: web-svc\nb: Web\nc: \"WEB Web\\n\"\n",
    );
}

#[test]
fn case_revision_checksum() {
    let v = render(
        "@template\na: $revision({x: 1, y: [2, 3]})\nb: $revision({y: [2, 3], x: 1})\n",
        &Record::new(),
        &Options::default(),
    )
    .unwrap();
    let r = v.as_record().unwrap();
    assert_eq!(r.get("a"), r.get("b"));
    assert!(r.get("a").and_then(Value::as_str).unwrap().starts_with("sha256:"));
}

#[test]
fn case_to_json_canonical() {
    ok(
        "@template\nx: $to_json({b: 1, a: [true, null]})\n",
        "",
        "x: \"{\\\"a\\\":[true,null],\\\"b\\\":1}\"\n",
    );
}

#[test]
fn case_to_json_big_int() {
    fails("@template\nx: $to_json(9007199254740993)\n", "", "to_json:");
}

#[test]
fn case_base64_strict() {
    fails("@template\nx: $base64_decode(\"aGk\")\n", "", "base64_decode: invalid base64");
    fails("@template\nx: $base64_decode(\"aGl=\")\n", "", "base64_decode: invalid base64");
    fails("@template\nx: $base64_decode(\"aG k=\")\n", "", "base64_decode: invalid base64");
    ok(
        "@template\nx: $base64_decode(\"aGk=\")\ny: $base64_encode(hi)\n",
        "",
        "x: @bytes(\"aGk=\")\ny: aGk=\n",
    );
}

#[test]
fn case_matches_whole() {
    ok("@template\na: $matches(abc, b)\nb: $matches(abc, \"a.c\")\n", "", "a: false\nb: true\n");
}

#[test]
fn case_no_truthiness_functions() {
    fails(
        "@template\n@param xs: list<int>\nx:\n  @if $len($xs):\n    y: 1\n",
        "xs: [1]\n",
        "conditions must be bool, found int",
    );
}

#[test]
fn builtin_list_has_forty_functions() {
    let mut names = BUILTINS.to_vec();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 40);
}

#[test]
fn steps_are_counted_exactly() {
    // Reading `$xs` weighs its 3 elements; 3 iterations + 3 filters + 3 `>` + 2 `*` = 11;
    // `upper` of a 1-byte string: 1 + 1 + 1 = 3.
    let src = "@template\n@let xs = [1, 2, 3]\nx: $([for v in $xs if $v > 1: $v * 2])\ny: $upper(a)\n";
    let with = |max_steps| {
        render(
            src,
            &Record::new(),
            &Options {
                limits: Limits {
                    max_steps,
                    ..Limits::default()
                },
                ..Options::default()
            },
        )
    };
    assert!(with(17).is_ok());
    assert!(with(16).unwrap_err().to_string().contains("limit of 16 steps"));
}

#[test]
fn version_declarations() {
    ok("@template 0.1\nx: 1\n", "", "x: 1\n");
    ok("@template 0.2\nx: $(1 + 1)\n", "", "x: 2\n");
    fails("@template 0.3\nx: 1\n", "", "supports the NOVA template language 0.1 and 0.2");
    // The one 0.1 spelling that 0.2 no longer reads fails to load.
    fails(
        "@template 0.1\n@param svc: any\nx: \"${svc.\\\"odd key\\\"}\"\n",
        "svc: {\"odd key\": 1}\n",
        "interpolation",
    );
    ok(
        "@template\n@param svc: any\nx: \"${svc.\"odd key\"}\"\n",
        "svc: {\"odd key\": 1}\n",
        "x: \"1\"\n",
    );
}

#[test]
fn operators_and_precedence() {
    ok(
        "@template\n@param n: int\na: $n + 2 * 3\nb: $(($n + 2) * 3)\nc: $(not $n > 1 or $n == 1 and true)\nd: $(@duration(\"1m\") * 3)\ne: $(@timestamp(\"2026-01-01T00:00:00Z\") - @timestamp(\"2025-12-31T00:00:00Z\"))\nf: $(0.5 + 0.25)\ng: $(@decimal(\"0.1\") + @decimal(\"0.2\"))\nh: $(1/2 + 1/3)\n",
        "n: 1\n",
        "a: 7\nb: 9\nc: true\nd: @duration(\"3m\")\ne: @duration(\"1d\")\nf: 0.75\ng: @decimal(\"0.3\")\nh: 5/6\n",
    );
    fails("@template\nx: $(1 < 2 < 3)\n", "", "do not chain");
    fails("@template\nx: $(1 and true)\n", "", "the operands of `and` must be bool");
    // Short-circuit: the right operand is never evaluated.
    ok("@template\nx: $(false and $nope)\n", "", "x: false\n");
    fails("@template\nx: $upper (a)\n", "", "unexpected content");
    ok("@template\nx: 1 + $a\n", "", "x: \"1 + $a\"\n");
}

#[test]
fn flow_collections_contain_expressions() {
    ok(
        "@template\n@param r: int\nx: {maxUnavailable: $div($r, 2), list: [$r + 1, $(2 * $r)], t: ($r, $r - 1)}\n",
        "r: 5\n",
        "x: {maxUnavailable: 2, list: [6, 10], t: (5, 4)}\n",
    );
    ok(
        "@template\n@param tags: list<string>\nx: \"--tags=${join($tags, \",\")}\"\n",
        "tags: [a, b]\n",
        "x: \"--tags=a,b\"\n",
    );
}

#[test]
fn defs_have_lexical_scope_and_typed_results() {
    ok(
        "@template\n@let registry = \"ghcr.io/x\"\n@def image(name: string, tag: string = latest): string = \"${registry}/${name}:${tag}\"\n@def twice(s: string) = [$image($s), $image($s, v2)]\nx: $twice(api)\n",
        "",
        "x: [\"ghcr.io/x/api:latest\", \"ghcr.io/x/api:v2\"]\n",
    );
    // A def does not see the lets below it, nor the caller's loop variables.
    fails(
        "@template\n@def f(x: int) = $later\n@let later = 1\ny: $f(1)\n",
        "",
        "undefined reference $later",
    );
    fails("@template\n@def f(x: int) = $v\ny: $([for v in [1]: $f(1)])\n", "", "undefined reference $v");
    fails(
        "@template\n@def f(x: int): string = $x\ny: $f(1)\n",
        "",
        "the result of f does not match `string`",
    );
    fails("@template\n@def f(x: int) = $x\ny: $f(1, 2)\n", "", "f takes 1 argument(s), but 2 were given");
    fails("@template\n@def f(x: int) = $x\n@def f(y: int) = $y\nz: 1\n", "", "@def f is already defined");
    fails("@template\n@def f() = 1\nz: 1\n", "", "at least one parameter");
    // Function and value namespaces are separate.
    ok("@template\n@param len: int\nx: $len([1, 2]) + $len\n", "len: 10\n", "x: 12\n");
}

#[test]
fn collection_builtins() {
    ok(
        "@template\n@let r = {b: 2, a: 1}\nk: $keys($r)\nv: $values($r)\ns: $sort([3, 1, 2])\nu: $unique([1, 2, 1, 3, 2])\nc: $concat([1], [2, 3])\nm: $merge({a: 1, b: 1}, {b: 2})\nmin: $min(3, 1, 2)\nmax: $max([@duration(\"1s\"), @duration(\"1m\")])\ng: [$get($r, a, 0), $get($r, z, 0), $get([1], 5, x), $get({n: null}, n, 1)]\nd: $default(null, fb)\nn: [$contains(abc, b), $contains([1, 2], 2), $contains({a: null}, a), $contains(@map{1 => 2}, 1)]\nkind: $kind(@set[1])\nsl: $slice([1, 2, 3], 1)\nlen: [$len(\"ß\"), $len(@bytes(\"aGk=\")), $len({a: 1})]\n",
        "",
        "k: [a, b]\nv: [1, 2]\ns: [1, 2, 3]\nu: [1, 2, 3]\nc: [1, 2, 3]\nm: {a: 1, b: 2}\nmin: 1\nmax: @duration(\"1m\")\ng: [1, 0, x, null]\nd: fb\nn: [true, true, true, true]\nkind: set\nsl: [2, 3]\nlen: [1, 2, 1]\n",
    );
    fails("@template\nx: $sort([1, a])\n", "", "sort: cannot compare");
    fails("@template\nx: $min([])\n", "", "min: needs a non-empty");
    fails("@template\nx: $get([1], -1, 0)\n", "", "negative index");
}

#[test]
fn conversion_and_encoding_builtins() {
    ok(
        "@template\na: $string(1.5)\nb: $int(\"42\")\nc: $float(1/3)\nd: $decimal(\"1.50\")\ne: $sha256(abc)\nf: $to_nova({b: 1, a: [x, \"y z\"]})\ng: $to_nova(1)\nh: [$floor(1.5), $ceil(@decimal(\"-1.5\")), $round(5/2)]\n",
        "",
        "a: \"1.5\"\nb: 42\nc: 0.3333333333333333\nd: @decimal(\"1.5\")\ne: ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\nf: \"a: [x, \\\"y z\\\"]\\nb: 1\\n\"\ng: \"1\"\nh: [1, -1, 3]\n",
    );
    fails("@template\nx: $int(\"08\")\n", "", "not an integer token");
    fails("@template\nx: $int(\"1.0\")\n", "", "not an integer token");
    fails("@template\nx: $decimal(1.5)\n", "", "floats are not converted");
    fails("@template\nx: $matches(a, \"[\")\n", "", "matches:");
}

#[test]
fn proposal_example_renders() {
    let tpl = r#"@template 0.2
@param release: string
@param chart: string @pattern("[a-z0-9-]+")
@param replicas: u16 @range(1..50) = 2
@param ports: list<u16>
@param config: any
@param log_level?: string

@def dns_label(s: string): string = $trim_suffix($slice($lower($replace($s, "_", "-")), 0, 63), "-")

@let name = $dns_label("${release}-${chart}")

@def labels(component: string):
  "app.kubernetes.io/name": $chart
  "app.kubernetes.io/instance": $release
  "app.kubernetes.io/component": $component

metadata:
  name: $name
  labels: $labels(web)
  annotations:
    "checksum/config": $revision($config)
spec:
  replicas: $replicas
  template:
    spec:
      containers:
        - name: web
          image: "ghcr.io/example/${chart}:${config.version}"
          args:
            - "--log-level=${default($log_level, info)}"
            - "--features=${join($sort($config.features), ",")}"
          ports: $([for p in $sort($unique($ports)): {containerPort: $p, name: "http-${p}"}])
          env:
            @for k, v in $config.env:
              - name: $upper($replace($k, "-", "_"))
                value: $string($v)
  @if $replicas > 1:
    strategy:
      rollingUpdate: {maxUnavailable: $div($replicas, 2)}
"#;
    let a = "release: My_Release\nchart: web\nreplicas: 3\nports: [8443, 8080, 8443]\nconfig: {version: v1, features: [b, a], env: {log-format: json, cache-size: 64}}\n";
    let v = render(tpl, &args(a), &Options::default()).unwrap();
    let spec = novafile_text::Path::parse("spec.template.spec.containers[0]").unwrap();
    let c = spec.get(&v).unwrap();
    let want = novafile_text::parse(
        r#"name: web
image: "ghcr.io/example/web:v1"
args: ["--log-level=info", "--features=a,b"]
ports: [{containerPort: 8080, name: http-8080}, {containerPort: 8443, name: http-8443}]
env:
  - name: CACHE_SIZE
    value: "64"
  - name: LOG_FORMAT
    value: json
"#,
    )
    .unwrap();
    assert_eq!(c, &want);
    let name = novafile_text::Path::parse("metadata.name").unwrap();
    assert_eq!(name.get(&v).unwrap(), &Value::String("my-release-web".into()));
    let ru = novafile_text::Path::parse("spec.strategy.rollingUpdate.maxUnavailable").unwrap();
    assert_eq!(ru.get(&v).unwrap(), &Value::Int(1));
}

/// Arguments `big` (a string of `n` bytes) and `xs` (a list of `m` copies of it).
fn big_args(n: usize, m: usize) -> Record {
    let mut a = Record::new();
    a.set("big", Value::String("x".repeat(n)));
    a.set("xs", Value::List(vec![Value::String("a".into()); m]));
    a
}

#[test]
fn amplifying_builtins_check_the_budget_before_building() {
    let opts = Options::default();
    // 1000 occurrences replaced by 100 KB each would be 100 MB.
    let src = "@template\n@param big: string\n@param xs: list<string>\nx: $len($replace($join($xs, \"\"), a, $big))\n";
    let e = render(src, &big_args(100_000, 1000), &opts).unwrap_err().to_string();
    assert!(e.contains("replace: template output exceeds the limit of 67108864 bytes"), "{e}");
    let src = "@template\n@param big: string\n@param xs: list<string>\nx: $len($join($xs, $big))\n";
    let e = render(src, &big_args(100_000, 1000), &opts).unwrap_err().to_string();
    assert!(e.contains("join: template output exceeds the limit"), "{e}");
    let src = "@template\n@param big: string\n@param xs: list<string>\nx: $len($split($big, x))\n";
    let e = render(src, &big_args(2_000_000, 0), &opts).unwrap_err().to_string();
    assert!(e.contains("split: template output exceeds the limit of 1000000 nodes"), "{e}");
}

#[test]
fn merging_large_maps_is_linear() {
    let map = |off: i128| Value::Map((0..100_000).map(|i| (Value::Int(i + off), Value::Int(off))).collect());
    let mut a = Record::new();
    a.set("a", map(0));
    a.set("b", map(50_000));
    let start = std::time::Instant::now();
    let v = render(
        "@template\n@param a: any\n@param b: any\nn: $len($merge($a, $b))\nv: $get($merge($a, $b), 60000, null)\n",
        &a,
        &Options::default(),
    )
    .unwrap();
    assert_eq!(v, novafile_text::parse("n: 150000\nv: 50000\n").unwrap());
    assert!(start.elapsed() < std::time::Duration::from_secs(30), "took {:?}", start.elapsed());
}

#[test]
fn reading_large_values_costs_steps() {
    // 1000 lists of 100 ints: `len` charges only the 1000 top-level elements, but every
    // read copies all 101,000 nodes, so 200 reads exceed the step budget.
    let mut a = Record::new();
    a.set("xs", Value::List(vec![ints(100); 1000]));
    a.set("n", ints(200));
    let src = "@template\n@param xs: any\n@param n: list<int>\nx: $([for i in $n: $len($xs)])\n";
    let e = render(src, &a, &Options::default()).unwrap_err().to_string();
    assert!(e.contains("steps"), "{e}");
    let src = "@template\n@param xs: any\n@param n: list<int>\nx: $([for i in $n: $(1 == 2)])\ny: $([for i in $n if $xs == [1]: 1])\n";
    let e = render(src, &a, &Options::default()).unwrap_err().to_string();
    assert!(e.contains("steps"), "{e}");
}

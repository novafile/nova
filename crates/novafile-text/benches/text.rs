//! Benchmarks: text parsing and writing per profile, and the binary codec, on a
//! generated document of uniform records.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use novafile_text::{Profile, parse, to_text};
use novafile_value::{EncodeOptions, Record, Value, canonical_bytes, decode, encode};

/// A record holding `n` service records, the shape tables and string dedupe target.
fn document(n: usize) -> Value {
    let services = (0..n)
        .map(|i| {
            let mut r = Record::new();
            r.set("id", Value::String(format!("svc-{i}")));
            r.set("image", Value::String(format!("ghcr.io/example/svc-{i}:v2")));
            r.set("port", Value::Int(8000 + i128::try_from(i).unwrap()));
            r.set("replicas", Value::Int(3));
            r.set("weight", Value::Float(0.5));
            r.set("enabled", Value::Bool(i % 2 == 0));
            r.set("tags", Value::List(vec!["web".into(), "eu".into()]));
            Value::Record(r)
        })
        .collect();

    let mut root = Record::new();
    root.set("environment", "production".into());
    root.set("services", Value::List(services));
    Value::Record(root)
}

/// Text: write each profile, then parse what it wrote.
fn text(c: &mut Criterion) {
    let v = document(1000);
    let mut g = c.benchmark_group("text");
    for p in [Profile::Human, Profile::Compact, Profile::Canonical] {
        let src = to_text(&v, p);
        g.throughput(Throughput::Bytes(src.len() as u64));
        g.bench_function(format!("write/{p:?}"), |b| b.iter(|| to_text(black_box(&v), p)));
        g.bench_function(format!("parse/{p:?}"), |b| b.iter(|| parse(black_box(&src)).unwrap()));
    }

    g.finish();
}

/// Binary: encode with each option set, decode, and canonical bytes.
fn binary(c: &mut Criterion) {
    let v = document(1000);
    let mut g = c.benchmark_group("binary");
    for (name, opts) in [
        (
            "plain",
            EncodeOptions {
                canonical: false,
                dedupe_strings: false,
                tables: false,
            },
        ),
        (
            "packed",
            EncodeOptions {
                canonical: false,
                dedupe_strings: true,
                tables: true,
            },
        ),
        (
            "canonical",
            EncodeOptions {
                canonical: true,
                dedupe_strings: false,
                tables: false,
            },
        ),
    ] {
        let bytes = encode(&v, opts);
        g.throughput(Throughput::Bytes(bytes.len() as u64));
        g.bench_function(format!("encode/{name}"), |b| b.iter(|| encode(black_box(&v), opts)));
        g.bench_function(format!("decode/{name}"), |b| b.iter(|| decode(black_box(&bytes)).unwrap()));
    }

    g.bench_function("canonical_bytes", |b| b.iter(|| canonical_bytes(black_box(&v))));
    g.finish();
}

criterion_group!(benches, text, binary);
criterion_main!(benches);

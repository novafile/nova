//! Benchmarks: encoding and decoding a checksummed frame around a generated document.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use novafile_binary::{Frame, FrameOptions, crc32, decode_frame, encode_frame};
use novafile_value::{Record, Value};

/// A record holding `n` service records.
fn document(n: usize) -> Value {
    let services = (0..n)
        .map(|i| {
            let mut r = Record::new();
            r.set("id", Value::String(format!("svc-{i}")));
            r.set("image", Value::String(format!("ghcr.io/example/svc-{i}:v2")));
            r.set("port", Value::Int(8000 + i128::try_from(i).unwrap()));
            r.set("enabled", Value::Bool(i % 2 == 0));
            Value::Record(r)
        })
        .collect();

    let mut root = Record::new();
    root.set("services", Value::List(services));
    Value::Record(root)
}

/// Frame encode/decode with the default options (dedupe, tables, CRC-32), and the CRC alone.
fn frame(c: &mut Criterion) {
    let f = Frame {
        schema: Some("services@1.0.0".into()),
        value: document(1000),
        meta: false,
    };

    let bytes = encode_frame(&f, FrameOptions::default());
    let mut g = c.benchmark_group("frame");
    g.throughput(Throughput::Bytes(bytes.len() as u64));
    g.bench_function("encode", |b| b.iter(|| encode_frame(black_box(&f), FrameOptions::default())));
    g.bench_function("decode", |b| b.iter(|| decode_frame(black_box(&bytes)).unwrap()));
    g.bench_function("crc32", |b| b.iter(|| crc32(black_box(&bytes))));
    g.finish();
}

criterion_group!(benches, frame);
criterion_main!(benches);

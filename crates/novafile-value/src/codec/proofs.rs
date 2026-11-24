//! Kani proofs: the zigzag and varint helpers are exact for every integer.

use super::{put_varint, read_varint, unzigzag, varint_len, zigzag};

/// Zigzag is a bijection: every `i128` comes back from its image.
#[kani::proof]
fn zigzag_round_trips() {
    let v: i128 = kani::any();
    assert_eq!(unzigzag(zigzag(v)), v);
}

/// Zigzag puts small magnitudes first: `|v| <= 63` fits one varint byte.
#[kani::proof]
fn zigzag_keeps_small_values_small() {
    let v: i128 = kani::any();
    kani::assume((-64..=63).contains(&v));
    assert!(zigzag(v) < 0x80);
}

/// [`varint_len`] predicts the length [`put_varint`] writes, and only the last byte
/// has its continuation bit clear.
#[kani::proof]
#[kani::unwind(20)]
fn varint_len_matches_put_varint() {
    let v: u128 = kani::any();
    let mut out = Vec::new();
    put_varint(&mut out, v);

    assert_eq!(out.len(), varint_len(v));
    assert!(out.len() <= 19);
    let (last, rest) = out.split_last().expect("put_varint writes at least one byte");
    assert_eq!(last & 0x80, 0);
    assert!(rest.iter().all(|b| b & 0x80 != 0));
}

/// The decoder reads back every varint the encoder writes, and consumes exactly its bytes.
#[kani::proof]
#[kani::unwind(20)]
fn varint_round_trips() {
    let v: u128 = kani::any();
    let mut out = Vec::new();
    put_varint(&mut out, v);

    let mut pos = 0;

    assert_eq!(read_varint(&out, &mut pos), Ok(v));
    assert_eq!(pos, out.len());
}

/// The varint reader never panics and never reads past its input, for any 20 bytes.
///
/// 20 bytes reach the overflow check on the 19th byte and one byte past it.
#[kani::proof]
#[kani::unwind(21)]
fn varint_reader_is_total() {
    let buf: [u8; 20] = kani::any();
    let mut pos = 0;
    let read = read_varint(&buf, &mut pos);

    assert!(pos <= buf.len());
    // Every value it accepts is minimal: re-encoding gives back the bytes read.
    if let Ok(v) = read {
        let mut out = Vec::new();
        put_varint(&mut out, v);
        assert_eq!(out.as_slice(), &buf[..pos]);
    }
}

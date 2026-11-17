//! Binary decoders must not panic, and whatever decodes must survive canonical re-encoding.
#![no_main]

use libfuzzer_sys::fuzz_target;
use novafile_binary::decode_frame;
use novafile_value::{canonical_bytes, decode};

fuzz_target!(|data: &[u8]| {
    let _ = decode_frame(data);
    let Ok(v) = decode(data) else { return };
    let bytes = canonical_bytes(&v);
    assert_eq!(decode(&bytes).unwrap(), v);
});

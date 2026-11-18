//! Binary decoders must not panic, the owned and borrowed decoders must agree,
//! and whatever decodes must survive re-encoding in every mode.
#![no_main]

use libfuzzer_sys::fuzz_target;
use novafile_binary::decode_frame;
use novafile_value::{EncodeOptions, canonical_bytes, decode, decode_ref, encode};

fuzz_target!(|data: &[u8]| {
    let _ = decode_frame(data);
    let borrowed = decode_ref(data);
    let Ok(v) = decode(data) else {
        assert!(borrowed.is_err());
        return;
    };

    assert_eq!(borrowed.unwrap().to_value(), v);
    let bytes = canonical_bytes(&v);
    assert_eq!(decode(&bytes).unwrap(), v);
    for (dedupe_strings, tables) in [(false, false), (true, false), (false, true), (true, true)] {
        let bytes = encode(
            &v,
            EncodeOptions {
                canonical: false,
                dedupe_strings,
                tables,
            },
        );
        assert_eq!(decode(&bytes).unwrap(), v);
    }
});

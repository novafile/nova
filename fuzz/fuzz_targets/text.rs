//! Text parsers must not panic, and whatever parses must survive every writer profile.
#![no_main]

use libfuzzer_sys::fuzz_target;
use novafile_text::{Profile, parse, parse_document, parse_flow, to_text};

fuzz_target!(|src: &str| {
    let _ = parse_document(src);
    let _ = parse_flow(src);
    let Ok(v) = parse(src) else { return };
    for p in [Profile::Human, Profile::Compact, Profile::Canonical] {
        let text = to_text(&v, p);
        let back = parse(&text).unwrap_or_else(|e| panic!("{p:?} output does not parse: {e}\n{text}"));
        assert_eq!(back, v, "{p:?} round trip changed the value\n{text}");
    }
});

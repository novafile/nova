//! Property tests: arbitrary values survive every text profile, and the parsers
//! never panic on arbitrary input. The binary codec properties live in `novafile-value`.

#[path = "../../novafile-value/tests/support/strategy.rs"]
mod strategy;

use novafile_text::{Profile, parse, parse_document, parse_flow, to_text};
use proptest::prelude::*;
use strategy::value;

proptest! {
    #[test]
    fn every_profile_round_trips(v in value()) {
        for p in [Profile::Human, Profile::Compact, Profile::Canonical] {
            let text = to_text(&v, p);
            let back = parse(&text).map_err(|e| TestCaseError::fail(format!("{p:?}: {e}\n{text}")))?;
            prop_assert_eq!(&back, &v, "{:?}\n{}", p, text);
        }
    }

    #[test]
    fn parsers_never_panic(src in "\\PC{0,64}") {
        let _ = parse(&src);
        let _ = parse_document(&src);
        let _ = parse_flow(&src);
    }
}

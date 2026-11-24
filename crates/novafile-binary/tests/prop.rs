//! Property tests: frames round-trip in every encoder mode, and the frame and
//! stream readers never panic on arbitrary or corrupted input.

#[path = "../../novafile-value/tests/support/strategy.rs"]
mod strategy;

use novafile_binary::stream::read_stream;
use novafile_binary::{Frame, FrameOptions, FrameReader, decode_frame, encode_frame};
use novafile_value::{EncodeOptions, encode};
use proptest::prelude::*;
use strategy::value;

/// Any frame: an optional schema reference, a value and the meta flag.
fn frame() -> impl Strategy<Value = Frame> {
    (prop::option::of("[a-z]{1,8}@[0-9]\\.[0-9]\\.[0-9]"), value(), any::<bool>()).prop_map(|(schema, value, meta)| Frame { schema, value, meta })
}

/// Any encoder switches.
fn options() -> impl Strategy<Value = FrameOptions> {
    (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>()).prop_map(|(canonical, dedupe_strings, tables, checksum)| FrameOptions {
        canonical,
        dedupe_strings,
        tables,
        checksum,
    })
}

proptest! {
    /// A frame decodes to itself and consumes exactly its own bytes.
    #[test]
    fn frames_round_trip(f in frame(), opts in options()) {
        let bytes = encode_frame(&f, opts);
        prop_assert_eq!(decode_frame(&bytes), Ok((f, bytes.len())));
    }

    /// Concatenated frames read back in order through the frame reader.
    #[test]
    fn frame_reader_reads_concatenated_frames(fs in prop::collection::vec(frame(), 0..4), opts in options()) {
        let bytes: Vec<u8> = fs.iter().flat_map(|f| encode_frame(f, opts)).collect();
        let back: Result<Vec<Frame>, _> = FrameReader::new(&bytes).collect();
        prop_assert_eq!(back, Ok(fs));
    }

    /// The readers never panic on arbitrary bytes, in strict or recovering mode.
    #[test]
    fn readers_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..96)) {
        let _ = decode_frame(&bytes);
        let _ = FrameReader::new(&bytes).recovering().count();
        let _ = read_stream(&bytes, false, None);
        let _ = read_stream(&bytes, true, None);
    }

    /// Flipping bits in one byte of a valid frame never panics the readers.
    #[test]
    fn readers_survive_corrupted_frames(f in frame(), opts in options(), at: prop::sample::Index, bits in 1u8..) {
        let mut bytes = encode_frame(&f, opts);
        let i = at.index(bytes.len());
        bytes[i] ^= bits;
        let _ = decode_frame(&bytes);
        let _ = read_stream(&bytes, true, None);
    }

    /// With a checksum, a corrupted payload or CRC never decodes as the original frame.
    ///
    /// The CRC covers only the payload (spec §5.4), so header bytes are out of scope.
    #[test]
    fn checksum_rejects_payload_corruption(f in frame(), at: prop::sample::Index, bits in 1u8..) {
        let opts = FrameOptions { checksum: true, ..FrameOptions::default() };
        let mut bytes = encode_frame(&f, opts);
        let payload = encode(&f.value, EncodeOptions { canonical: false, dedupe_strings: true, tables: true }).len();
        let i = bytes.len() - 4 - payload + at.index(payload + 4);
        bytes[i] ^= bits;
        prop_assert!(decode_frame(&bytes).is_err());
    }
}

//! The Protobuf wire format: varints, tags, and a bounds-checked field reader.

use crate::shape::{MAX_FIELD_NUMBER, WireType};

/// Append `v` as a base-128 varint.
#[expect(clippy::cast_possible_truncation, reason = "each byte carries the low 7 bits of `v`")]
pub(crate) fn put_varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }

    buf.push(v as u8);
}

/// Append a field key.
pub(crate) fn put_tag(buf: &mut Vec<u8>, field: u32, wt: WireType) {
    put_varint(buf, (u64::from(field) << 3) | wt as u64);
}

/// Append a length-delimited payload (length prefix and bytes).
pub(crate) fn put_len(buf: &mut Vec<u8>, bytes: &[u8]) {
    put_varint(buf, bytes.len() as u64);
    buf.extend_from_slice(bytes);
}

/// Zigzag-encode a `sint32` so small magnitudes of either sign get short varints.
pub(crate) fn zigzag32(n: i32) -> u64 {
    u64::from(((n << 1) ^ (n >> 31)).cast_unsigned())
}

/// Zigzag-encode a `sint64`.
pub(crate) fn zigzag64(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)).cast_unsigned()
}

/// Decode a zigzag `sint32`; only the low 32 bits of the varint are used.
pub(crate) fn unzigzag32(v: u64) -> i32 {
    #[expect(clippy::cast_possible_truncation, reason = "sint32 fields use the low 32 bits of the varint")]
    let v = v as u32;
    (v >> 1).cast_signed() ^ -(v & 1).cast_signed()
}

/// Decode a zigzag `sint64`.
pub(crate) fn unzigzag64(v: u64) -> i64 {
    (v >> 1).cast_signed() ^ -(v & 1).cast_signed()
}

/// One field value as it appears on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wire<'a> {
    /// Wire type 0: a varint.
    Varint(u64),
    /// Wire type 1: eight little-endian bytes.
    I64(u64),
    /// Wire type 2: a length-delimited payload, borrowed from the input.
    Len(&'a [u8]),
    /// Wire type 5: four little-endian bytes.
    I32(u32),
}

impl Wire<'_> {
    /// The wire type this value was read with.
    pub(crate) fn wire_type(&self) -> WireType {
        match self {
            Wire::Varint(_) => WireType::Varint,
            Wire::I64(_) => WireType::I64,
            Wire::Len(_) => WireType::Len,
            Wire::I32(_) => WireType::I32,
        }
    }
}

/// A cursor over a byte slice. Every read is bounds-checked.
pub(crate) struct Reader<'a> {
    /// The unread rest of the input.
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    /// A reader positioned at the start of `bytes`.
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes }
    }

    /// Whether all input has been consumed.
    pub(crate) fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Read a base-128 varint of at most 10 bytes. Truncated input and values
    /// beyond 64 bits are errors.
    pub(crate) fn varint(&mut self) -> Result<u64, String> {
        let mut v: u64 = 0;
        for (i, &b) in self.bytes.iter().enumerate().take(10) {
            // The tenth byte may only contribute the top bit.
            if i == 9 && b > 1 {
                return Err("varint overflows 64 bits".into());
            }

            v |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                self.bytes = &self.bytes[i + 1..];
                return Ok(v);
            }
        }

        Err(if self.bytes.len() < 10 {
            "truncated varint"
        } else {
            "varint is longer than 10 bytes"
        }
        .into())
    }

    /// Consume the next `n` bytes, or fail without consuming if fewer remain.
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if n > self.bytes.len() {
            return Err(format!("truncated input: need {n} bytes, {} remain", self.bytes.len()));
        }

        let (head, tail) = self.bytes.split_at(n);
        self.bytes = tail;
        Ok(head)
    }

    /// Read an 8-byte little-endian value (wire type I64).
    pub(crate) fn fixed64(&mut self) -> Result<u64, String> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes(b.try_into().expect("8 bytes")))
    }

    /// Read a 4-byte little-endian value (wire type I32).
    fn fixed32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes(b.try_into().expect("4 bytes")))
    }

    /// Read one field: its number and value. Groups and unknown wire types are errors.
    pub(crate) fn field(&mut self) -> Result<(u32, Wire<'a>), String> {
        let key = self.varint()?;
        let number = key >> 3;
        if number == 0 || number > u64::from(MAX_FIELD_NUMBER) {
            return Err(format!("invalid field number {number}"));
        }

        #[expect(clippy::cast_possible_truncation, reason = "checked against MAX_FIELD_NUMBER above")]
        let number = number as u32;
        let value = match key & 7 {
            0 => Wire::Varint(self.varint()?),
            1 => Wire::I64(self.fixed64()?),
            2 => {
                let len = self.varint()?;
                let len = usize::try_from(len).map_err(|_| format!("field {number}: length {len} is too large"))?;
                Wire::Len(self.take(len).map_err(|e| format!("field {number}: {e}"))?)
            }
            3 | 4 => return Err(format!("field {number}: groups (wire types 3 and 4) are not supported")),
            5 => Wire::I32(self.fixed32()?),
            wt => return Err(format!("field {number}: invalid wire type {wt}")),
        };

        Ok((number, value))
    }
}

/// Parse a whole message into its fields, in wire order.
pub(crate) fn fields(bytes: &[u8]) -> Result<Vec<(u32, Wire<'_>)>, String> {
    let mut r = Reader::new(bytes);
    let mut out = Vec::new();
    while !r.is_empty() {
        out.push(r.field()?);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip() {
        for v in [0, 1, 127, 128, 300, u64::from(u32::MAX), u64::MAX] {
            let mut buf = Vec::new();
            put_varint(&mut buf, v);
            let mut r = Reader::new(&buf);
            assert_eq!(r.varint().unwrap(), v);
            assert!(r.is_empty());
        }

        let mut buf = Vec::new();
        put_varint(&mut buf, 300);
        assert_eq!(buf, [0xac, 0x02]);
    }

    #[test]
    fn bad_varints() {
        assert!(Reader::new(&[0x80]).varint().is_err());
        assert!(Reader::new(&[0xff; 10]).varint().is_err());
        assert!(Reader::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02]).varint().is_err());
        assert!(Reader::new(&[0xff; 11]).varint().is_err());
    }

    #[test]
    fn zigzag() {
        for n in [0, -1, 1, i32::MIN, i32::MAX] {
            assert_eq!(unzigzag32(zigzag32(n)), n);
            assert_eq!(unzigzag64(zigzag64(i64::from(n))), i64::from(n));
        }

        assert_eq!(zigzag32(-1), 1);
        assert_eq!(zigzag64(i64::MIN), u64::MAX);
        assert_eq!(unzigzag64(u64::MAX), i64::MIN);
    }

    #[test]
    fn field_errors() {
        assert!(fields(&[0x0b]).is_err()); // group start
        assert!(fields(&[0x0c]).is_err()); // group end
        assert!(fields(&[0x0e]).is_err()); // wire type 6
        assert!(fields(&[0x00, 0x00]).is_err()); // field 0
        assert!(fields(&[0x0a, 0x05, 1, 2]).is_err()); // short LEN
        assert!(fields(&[0x09, 1, 2, 3]).is_err()); // short I64
        assert!(fields(&[0x0d, 1]).is_err()); // short I32
        assert_eq!(fields(&[0x0d, 1, 0, 0, 0]).unwrap(), vec![(1, Wire::I32(1))]);
    }
}

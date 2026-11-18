//! Content revisions (spec §2.13): SHA-256 of the canonical encoding. Behind the `revision`
//! feature (on by default), which brings in `sha2`.

use crate::{Value, canonical_bytes};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fmt::Write as _;

/// A content revision: SHA-256 of the canonical encoding (spec §2.13).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Revision(pub [u8; 32]);

impl Revision {
    /// The digest as 64 lowercase hex digits, without the `sha256:` prefix.
    #[must_use]
    pub fn hex(&self) -> String {
        self.0.iter().fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    /// Does `text` (`sha256:<hex>`, possibly abbreviated to ≥ 8 hex digits)
    /// identify this revision?
    #[must_use]
    pub fn matches(&self, text: &str) -> bool {
        let h = text.strip_prefix("sha256:").unwrap_or(text).to_ascii_lowercase();
        h.len() >= 8 && h.len() <= 64 && h.bytes().all(|c| c.is_ascii_hexdigit()) && self.hex().starts_with(&h)
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.hex())
    }
}

impl fmt::Debug for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// The [`Revision`] of `v`: SHA-256 of its canonical encoding.
#[must_use]
pub fn revision(v: &Value) -> Revision {
    Revision(Sha256::digest(canonical_bytes(v)).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_prefix() {
        let r = revision(&Value::List(vec![Value::Int(1), Value::String("x".into())]));
        let s = r.to_string();
        assert!(r.matches(&s));
        assert!(r.matches(&s[..15]));
        assert!(!r.matches("sha256:abc"));
    }
}

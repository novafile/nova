//! Redaction (proposal 0009): replace classified values by placeholders
//! before a reader emits them.
//!
//! Redaction is a property of reading. It starts from the classified paths
//! that [`Schema::classify`] reports, and builds a [`Redacted`] value in which
//! every classified value the [`Reveal`] mode does not show is replaced by a
//! placeholder: `@redacted` for secrets, and `@redacted("p:…")`, a keyed
//! pseudonym, for personal data. A [`RedactKey`] also turns revisions into
//! opaque tokens (`k:…`), so a reader cannot test guesses of a low-entropy
//! secret against the revision of the stored document.

use crate::model::{Class, Schema};
use crate::resolve::ClassMark;
use novafile_text::{Path, Segment};
use novafile_value::{REDACTED_TAG, Revision, Value};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// Hex digits of a pseudonym after `p:` (48 bits).
const PSEUDONYM_HEX: usize = 12;

/// Hex digits of an opaque revision token after `k:` (128 bits).
const TOKEN_HEX: usize = 32;

/// Which classified values a reader shows in clear.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Reveal {
    /// Every classified value becomes a placeholder. The default.
    #[default]
    Redact,
    /// Personal values in clear; secrets become placeholders.
    Personal,
    /// Every value in clear.
    All,
    /// Values at or below these singular paths in clear; the classes
    /// still apply everywhere else.
    Paths(Vec<Path>),
}

impl Reveal {
    /// Parse a `--reveal` argument: empty or `all` reveals everything,
    /// `personal` reveals personal data, anything else is a comma-separated
    /// list of singular paths.
    ///
    /// # Errors
    ///
    /// Fails if a path does not parse or is not singular (it has `[*]`, a
    /// filter or a projection).
    pub fn parse(arg: &str) -> Result<Reveal, String> {
        match arg.trim() {
            "" | "all" => Ok(Reveal::All),
            "personal" => Ok(Reveal::Personal),
            list => list
                .split(',')
                .map(|p| {
                    let path = Path::parse(p.trim()).map_err(|e| format!("--reveal {p}: {e}"))?;
                    if path.is_singular() {
                        Ok(path)
                    } else {
                        Err(format!("--reveal {p}: only singular paths can be revealed"))
                    }
                })
                .collect::<Result<_, _>>()
                .map(Reveal::Paths),
        }
    }

    /// This mode with keyed selectors (`[id=api]`), map keys and negative
    /// indices replaced by the concrete segments they select in `v`, so
    /// they compare with the paths [`Schema::classify`] reports. A path that
    /// selects nothing reveals nothing.
    fn concrete(&self, v: &Value) -> Reveal {
        match self {
            Reveal::Paths(ps) => Reveal::Paths(ps.iter().filter_map(|p| concrete_path(v, p).map(Path)).collect()),
            other => other.clone(),
        }
    }

    /// Whether a value of class `class` at `path` is shown in clear.
    fn shows(&self, class: Class, path: &[Segment]) -> bool {
        match self {
            Reveal::Redact => false,
            Reveal::Personal => class == Class::Personal,
            Reveal::All => true,
            Reveal::Paths(ps) => ps.iter().any(|p| path.starts_with(&p.0)),
        }
    }
}

/// The concrete segments that the singular path `p` selects in `v`: fields
/// and non-negative indices only, or `None` if `p` selects nothing. They
/// compare with the paths [`Schema::classify`] reports.
#[must_use]
pub fn concrete_path(v: &Value, p: &Path) -> Option<Vec<Segment>> {
    let mut out = Vec::with_capacity(p.0.len());
    let mut cur = v;
    for seg in &p.0 {
        let (next, step) = match (cur, seg) {
            (Value::Record(r), Segment::Field(f)) => (r.get(f)?, seg.clone()),
            (Value::Variant(t, Some(pl)), Segment::Field(f)) if t == f => (&**pl, seg.clone()),
            (Value::List(l) | Value::Tuple(l) | Value::Set(l), Segment::Index(i)) => {
                let len = i64::try_from(l.len()).ok()?;
                let at = if *i < 0 { len.checked_add(*i)? } else { *i };
                (l.get(usize::try_from(at).ok()?)?, Segment::Index(at))
            }
            (Value::List(l) | Value::Set(l), Segment::Match(field, want)) => {
                let i = l.iter().position(|e| e.as_record().and_then(|r| r.get(field)) == Some(want))?;
                (&l[i], Segment::Index(i64::try_from(i).ok()?))
            }
            (Value::List(l) | Value::Set(l), Segment::Key(want)) => {
                let i = l.iter().position(|e| e == want)?;
                (&l[i], Segment::Index(i64::try_from(i).ok()?))
            }
            (Value::Map(m), Segment::Key(k)) => (m.iter().find(|(mk, _)| mk == k).map(|(_, x)| x)?, seg.clone()),
            (Value::Map(m), Segment::Field(f)) => (m.iter().find(|(mk, _)| mk.as_str() == Some(f)).map(|(_, x)| x)?, seg.clone()),
            _ => return None,
        };
        out.push(step);
        cur = next;
    }

    Some(out)
}

/// A key that makes pseudonyms and revision tokens: 32 bytes, held by the
/// reader and never shown. Pseudonyms and tokens are stable for one key, so
/// rotating the key unlinks them from earlier ones.
#[derive(Clone)]
pub struct RedactKey([u8; 32]);

impl std::fmt::Debug for RedactKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RedactKey(…)")
    }
}

impl RedactKey {
    /// A key from 32 bytes, which must come from a secure random source.
    #[must_use]
    pub fn new(bytes: [u8; 32]) -> RedactKey {
        RedactKey(bytes)
    }

    /// Parse a key from 64 hex digits.
    ///
    /// # Errors
    ///
    /// Fails if `hex` is not exactly 64 hex digits.
    pub fn from_hex(hex: &str) -> Result<RedactKey, String> {
        let hex = hex.trim();
        let bad = || "a redaction key is 64 hex digits".to_owned();
        if hex.len() != 64 || !hex.is_ascii() {
            return Err(bad());
        }

        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| bad())?;
        }

        Ok(RedactKey(out))
    }

    /// The key as 64 lowercase hex digits, for storing it.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    /// HMAC-SHA-256 of `message` under this key (RFC 2104).
    fn mac(&self, message: &[u8]) -> [u8; 32] {
        let pad = |byte: u8| {
            let mut p = [byte; 64];
            p.iter_mut().zip(self.0).for_each(|(p, k)| *p ^= k);
            p
        };

        let inner = Sha256::new().chain_update(pad(0x36)).chain_update(message).finalize();
        Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize().into()
    }

    /// The pseudonym of `v`: `p:` and the first 12 hex digits of the HMAC of
    /// its canonical encoding. Equal values get equal pseudonyms.
    #[must_use]
    pub fn pseudonym(&self, v: &Value) -> String {
        format!("p:{}", &hex(&self.mac(&novafile_value::canonical_bytes(v)))[..PSEUDONYM_HEX])
    }

    /// The opaque token for a revision: `k:` and 32 hex digits of the HMAC of
    /// its text. It is stable for one revision and key, so compare-and-swap
    /// works on tokens as on revisions.
    #[must_use]
    pub fn token(&self, revision: &Revision) -> String {
        format!("k:{}", &hex(&self.mac(revision.to_string().as_bytes()))[..TOKEN_HEX])
    }
}

/// Lowercase hex digits of `bytes`.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(2 * bytes.len()), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// A value with its classified parts replaced by placeholders. It is for
/// output only: patch application, the binary encoding and revision hashes
/// must never see it, so it does not convert back into a [`Value`] except
/// through [`Redacted::for_output`].
#[derive(Clone, Debug, PartialEq)]
pub struct Redacted {
    /// The value, with placeholders (variants tagged [`REDACTED_TAG`]).
    value: Value,
    /// How many values became placeholders.
    count: usize,
}

impl Redacted {
    /// The value to hand to a text or JSON writer. Writers print the
    /// placeholders as `@redacted` and `{"$redacted": …}`.
    #[must_use]
    pub fn for_output(&self) -> &Value {
        &self.value
    }

    /// How many values became placeholders.
    #[must_use]
    pub fn count(&self) -> usize {
        self.count
    }
}

/// Replace the classified values of `v` that `reveal` does not show.
/// `classes` are the paths [`Schema::classify`] reports for `v`; `key` makes
/// pseudonyms for personal values, which stay bare `@redacted` without one.
/// A classified container becomes one placeholder, shape included.
#[must_use]
pub fn redact(v: &Value, classes: &[ClassMark], reveal: &Reveal, key: Option<&RedactKey>) -> Redacted {
    let reveal = &reveal.concrete(v);
    let mut out = Redacted { value: v.clone(), count: 0 };
    let mut marks: Vec<(&[Segment], Class)> = classes.iter().map(|(p, c)| (p.0.as_slice(), *c)).collect();
    // Outer paths first, so a whole container goes before its parts.
    marks.sort_by_key(|(p, _)| p.len());
    let mut hidden: Vec<&[Segment]> = Vec::new();
    for &(path, own) in &marks {
        if hidden.iter().any(|h| path.starts_with(h)) {
            continue;
        }

        let class = marks.iter().filter(|(p, _)| path.starts_with(p)).map(|(_, c)| *c).fold(own, Class::max);
        if reveal.shows(class, path) {
            continue;
        }

        let slot = slot_mut(&mut out.value, path);
        if slot.is_redacted() {
            continue;
        }

        let pseudonym = (class == Class::Personal).then_some(key).flatten().map(|k| k.pseudonym(slot));
        *slot = Value::Variant(REDACTED_TAG.to_owned(), pseudonym.map(|p| Box::new(Value::String(p))));
        out.count += 1;
        hidden.push(path);
    }

    out
}

/// The value at the concrete `path` below `v`, or the deepest value on the
/// way to it that exists. Paths from [`Schema::classify`] use fields,
/// indices, variant payloads (as fields) and map keys, as written in the
/// source before resolution. A map key that resolution converted (`"1/2"` to
/// a rational), or a set that resolution reordered, no longer matches its
/// source path: the enclosing value is then redacted instead, so the
/// failure hides more, never less.
fn slot_mut<'a>(v: &'a mut Value, path: &[Segment]) -> &'a mut Value {
    let mut cur = v;
    for seg in path {
        if child_index(cur, seg).is_none() {
            break;
        }

        cur = child_mut(cur, seg).expect("child_index found the child");
    }

    cur
}

/// Whether `v` has the child that `seg` names, as `Some(())`.
fn child_index(v: &Value, seg: &Segment) -> Option<()> {
    match (v, seg) {
        (Value::Record(r), Segment::Field(f)) => r.get(f).map(|_| ()),
        (Value::Variant(t, Some(_)), Segment::Field(f)) => (t == f).then_some(()),
        (Value::List(l) | Value::Tuple(l) | Value::Set(l), Segment::Index(i)) => l.get(usize::try_from(*i).ok()?).map(|_| ()),
        (Value::Map(m), Segment::Key(k)) => m.iter().any(|(mk, _)| mk == k).then_some(()),
        // A record resolved as a map keeps the field segments of its source.
        (Value::Map(m), Segment::Field(f)) => m.iter().any(|(mk, _)| mk.as_str() == Some(f)).then_some(()),
        _ => None,
    }
}

/// The child of `v` that `seg` names, if it exists.
fn child_mut<'a>(v: &'a mut Value, seg: &Segment) -> Option<&'a mut Value> {
    match (v, seg) {
        (Value::Record(r), Segment::Field(f)) => r.get_mut(f),
        (Value::Variant(_, Some(p)), Segment::Field(_)) => Some(p),
        (Value::List(l) | Value::Tuple(l) | Value::Set(l), Segment::Index(i)) => l.get_mut(usize::try_from(*i).ok()?),
        (Value::Map(m), Segment::Key(k)) => m.iter_mut().find(|(mk, _)| mk == k).map(|(_, v)| v),
        (Value::Map(m), Segment::Field(f)) => m.iter_mut().find(|(mk, _)| mk.as_str() == Some(f)).map(|(_, v)| v),
        _ => None,
    }
}

impl Schema {
    /// Redact the stored value `v` for output (proposal 0009): a shortcut for
    /// [`Schema::classify_source`] followed by [`redact`]. The output keeps
    /// the stored form: no defaults and no conversions.
    ///
    /// # Errors
    ///
    /// As [`Schema::resolve`]. Issues about classified values do not show
    /// them.
    pub fn redact(&self, v: &Value, reveal: &Reveal, key: Option<&RedactKey>) -> Result<Redacted, Vec<crate::Issue>> {
        let classes = self.classify_source(v)?;
        Ok(redact(v, &classes, reveal, key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HMAC-SHA-256 test case 2 of RFC 4231, with the key padded to 32 bytes
    /// by the test: the construction matches the standard one.
    #[test]
    fn mac_matches_rfc_4231_construction() {
        let mut key = [0u8; 32];
        key[..4].copy_from_slice(b"Jefe");
        let mac = RedactKey::new(key).mac(b"what do ya want for nothing?");
        assert_eq!(hex(&mac), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }

    /// Keys survive a round trip through hex, and bad hex is rejected.
    #[test]
    fn key_hex_round_trip() {
        let k = RedactKey::new([7; 32]);
        assert_eq!(RedactKey::from_hex(&k.to_hex()).unwrap().0, k.0);
        assert!(RedactKey::from_hex("zz").is_err());
    }
}

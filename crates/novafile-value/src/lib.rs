//! The NOVA semantic value model (spec §2), canonical binary value encoding
//! (spec §5) and revisions (spec §2.13).
//!
//! Every NOVA representation — human text, compact text, binary — maps to
//! exactly this model. Equality, ordering and hashing are all defined in
//! terms of the canonical encoding, which makes them consistent by
//! construction.

mod codec;
mod decimal;
#[cfg(feature = "facet")]
mod facet_support;
mod numeric;
mod rational;
mod record;
#[cfg(feature = "schemars")]
pub mod schemars_support;
#[cfg(feature = "serde")]
pub mod serde_support;
mod time;
mod value_ref;

pub use codec::{DecodeError, EncodeOptions, Limits, Revision, canonical_bytes, decode, decode_prefix, decode_ref, decode_ref_prefix, encode, revision};
pub use decimal::Decimal;
pub use numeric::numeric_cmp;
pub use rational::Rational;
pub use record::{DuplicateKey, Record};
pub use time::{Duration, Timestamp};
pub use value_ref::ValueRef;

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

/// A NOVA value.
///
/// `Set` and `Map` should not contain duplicates (by value equality); use
/// [`Value::set`] / [`Value::map`] to construct them with that check. The
/// canonical encoder sorts records, sets and maps itself, so stored order
/// never influences equality, hashing or canonical bytes.
#[derive(Clone, Debug)]
pub enum Value {
    /// The absence of a value.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A signed integer in the `i128` range.
    Int(i128),
    /// An IEEE 754 binary64 number; all NaNs are equal to each other.
    Float(f64),
    /// An exact decimal number (spec §2.3).
    Decimal(Decimal),
    /// An exact fraction (spec §2.4).
    Rational(Rational),
    /// A UTF-8 string.
    String(String),
    /// An opaque byte string.
    Bytes(Vec<u8>),
    /// A UTC instant (spec §2.6).
    Timestamp(Timestamp),
    /// A signed span of time (spec §2.7).
    Duration(Duration),
    /// An ordered sequence.
    List(Vec<Value>),
    /// A fixed-length sequence whose positions may have different kinds.
    Tuple(Vec<Value>),
    /// An unordered collection of distinct values.
    Set(Vec<Value>),
    /// String-keyed fields (spec §2.10).
    Record(Record),
    /// An unordered association with arbitrary keys, unique by value equality.
    Map(Vec<(Value, Value)>),
    /// A named alternative with an optional payload.
    Variant(String, Option<Box<Value>>),
}

impl Value {
    /// The lowercase kind name (`"int"`, `"record"`, ...), as used in messages.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Decimal(_) => "decimal",
            Value::Rational(_) => "rational",
            Value::String(_) => "string",
            Value::Bytes(_) => "bytes",
            Value::Timestamp(_) => "timestamp",
            Value::Duration(_) => "duration",
            Value::List(_) => "list",
            Value::Tuple(_) => "tuple",
            Value::Set(_) => "set",
            Value::Record(_) => "record",
            Value::Map(_) => "map",
            Value::Variant(..) => "variant",
        }
    }

    /// Whether this is a list, tuple, set, record or map, or a variant whose
    /// payload is one.
    #[must_use]
    pub fn is_container(&self) -> bool {
        matches!(self, Value::List(_) | Value::Tuple(_) | Value::Set(_) | Value::Record(_) | Value::Map(_))
            || matches!(self, Value::Variant(_, Some(p)) if p.is_container())
    }

    /// Build a set, rejecting duplicate elements.
    ///
    /// # Errors
    ///
    /// Returns an error if two elements are equal.
    pub fn set(items: Vec<Value>) -> Result<Value, String> {
        let mut seen = std::collections::HashSet::new();
        for it in &items {
            if !seen.insert(canonical_bytes(it)) {
                return Err("duplicate set element".into());
            }
        }

        Ok(Value::Set(items))
    }

    /// Build a map, rejecting duplicate keys.
    ///
    /// # Errors
    ///
    /// Returns an error if two keys are equal.
    pub fn map(entries: Vec<(Value, Value)>) -> Result<Value, String> {
        let mut seen = std::collections::HashSet::new();
        for (k, _) in &entries {
            if !seen.insert(canonical_bytes(k)) {
                return Err("duplicate map key".into());
            }
        }

        Ok(Value::Map(entries))
    }

    /// The record, if this is a record.
    #[must_use]
    pub fn as_record(&self) -> Option<&Record> {
        match self {
            Value::Record(r) => Some(r),
            _ => None,
        }
    }

    /// The record, mutably, if this is a record.
    pub fn as_record_mut(&mut self) -> Option<&mut Record> {
        match self {
            Value::Record(r) => Some(r),
            _ => None,
        }
    }

    /// The string, if this is a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The elements, if this is a list (not a tuple or set).
    #[must_use]
    pub fn as_list(&self) -> Option<&Vec<Value>> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    /// Recursively sort record keys (presentation order) by UTF-8 bytes.
    pub fn sort_keys(&mut self) {
        match self {
            Value::Record(r) => {
                r.sort_keys();
                r.iter_mut().for_each(|(_, v)| v.sort_keys());
            }
            Value::List(l) | Value::Tuple(l) | Value::Set(l) => l.iter_mut().for_each(Value::sort_keys),
            Value::Map(m) => m.iter_mut().for_each(|(k, v)| {
                k.sort_keys();
                v.sort_keys();
            }),
            Value::Variant(_, Some(p)) => p.sort_keys(),
            _ => {}
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_owned())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i128::from(i))
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<Record> for Value {
    fn from(r: Record) -> Self {
        Value::Record(r)
    }
}

/// Semantic equality (spec §2.12): equal iff canonical encodings are equal.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        // Fast paths for common scalars, consistent with canonical encoding.
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            _ if self.kind() != other.kind() => false,
            _ => canonical_bytes(self) == canonical_bytes(other),
        }
    }
}

impl Eq for Value {}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        canonical_bytes(self).hash(state);
    }
}

/// The canonical total order (spec §2.12): bytewise order of canonical encodings.
impl Ord for Value {
    fn cmp(&self, other: &Self) -> Ordering {
        canonical_bytes(self).cmp(&canonical_bytes(other))
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

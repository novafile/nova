//! Serde integration for the value types (feature `serde`).
//!
//! With any serde format, NOVA's richer types serialize *naturally*: a
//! timestamp, duration, decimal or rational becomes its canonical text, a set
//! a sequence, a variant a string or single-entry map. With the NOVA
//! serializer and deserializer (`novafile-serde`), the same impls round-trip
//! losslessly by way of reserved newtype-struct names (`$nova::…`).

use crate::{Decimal, Duration, Rational, Record, Timestamp, Value};
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, SerializeTuple, Serializer};
use std::cell::RefCell;
use std::fmt;

/// Reserved newtype-struct names understood by the NOVA (de)serializer.
pub mod magic {
    pub const VALUE: &str = "$nova::value";
    pub const TIMESTAMP: &str = "$nova::timestamp";
    pub const DURATION: &str = "$nova::duration";
    pub const DECIMAL: &str = "$nova::decimal";
    pub const RATIONAL: &str = "$nova::rational";
    pub const SET: &str = "$nova::set";
}

// Hand-off slots that let a whole `Value` pass through serde's data model
// unchanged when both ends are NOVA. They are only ever filled and drained
// within a single (de)serialize call on the current thread.
thread_local! {
    // Outer `None`: not capturing; `Some(None)`: capturing, nothing captured yet.
    #[expect(clippy::option_option, reason = "outer level marks an active capture, inner level its result")]
    static CAPTURE: RefCell<Option<Option<Value>>> = const { RefCell::new(None) };
    static INJECT: RefCell<Option<Value>> = const { RefCell::new(None) };
}

#[doc(hidden)]
pub fn __capture_begin() {
    CAPTURE.with(|c| *c.borrow_mut() = Some(None));
}

#[doc(hidden)]
#[must_use]
pub fn __capture_take() -> Option<Value> {
    CAPTURE.with(|c| c.borrow_mut().take().flatten())
}

#[doc(hidden)]
pub fn __inject(v: Value) {
    INJECT.with(|c| *c.borrow_mut() = Some(v));
}

// ---------------------------------------------------------------------------
// Text-backed scalar types
// ---------------------------------------------------------------------------

macro_rules! text_type {
    ($ty:ty, $name:expr, $expect:expr, $parse:expr) => {
        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_newtype_struct($name, &self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct V;
                impl<'de> Visitor<'de> for V {
                    type Value = $ty;
                    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                        f.write_str($expect)
                    }
                    fn visit_str<E: de::Error>(self, s: &str) -> Result<$ty, E> {
                        let parse: fn(&str) -> Result<$ty, String> = $parse;
                        parse(s).map_err(E::custom)
                    }
                    fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<$ty, D::Error> {
                        let s = String::deserialize(d)?;
                        self.visit_str(&s)
                    }
                }
                d.deserialize_newtype_struct($name, V)
            }
        }
    };
}

text_type!(Timestamp, magic::TIMESTAMP, "an RFC 3339 timestamp", Timestamp::parse);
text_type!(Duration, magic::DURATION, "a duration such as 1h30m", Duration::parse);
text_type!(Decimal, magic::DECIMAL, "a decimal number", Decimal::parse);
text_type!(Rational, magic::RATIONAL, "a rational such as 22/7", |s| if s.contains('/') {
    Rational::parse(s)
} else {
    s.parse::<i128>()
        .map_err(|_| format!("invalid rational {s:?}"))
        .and_then(|n| Rational::new(n, 1))
});

// ---------------------------------------------------------------------------
// Value
// ---------------------------------------------------------------------------

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_newtype_struct(magic::VALUE, &Natural(self))
    }
}

/// Serializes a value in the plain serde data model, unless a NOVA
/// serializer has asked to capture it whole.
struct Natural<'a>(&'a Value);

impl Serialize for Natural<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let captured = CAPTURE.with(|c| {
            let mut c = c.borrow_mut();
            if matches!(*c, Some(None)) {
                *c = Some(Some(self.0.clone()));
                true
            } else {
                false
            }
        });
        if captured {
            return s.serialize_unit();
        }
        match self.0 {
            Value::Null => s.serialize_unit(),
            Value::Bool(b) => s.serialize_bool(*b),
            Value::Int(i) => {
                if let Ok(x) = i64::try_from(*i) {
                    s.serialize_i64(x)
                } else if let Ok(x) = u64::try_from(*i) {
                    s.serialize_u64(x)
                } else {
                    s.serialize_i128(*i)
                }
            }
            Value::Float(f) => s.serialize_f64(*f),
            Value::Decimal(d) => d.serialize(s),
            Value::Rational(r) => r.serialize(s),
            Value::Timestamp(t) => t.serialize(s),
            Value::Duration(d) => d.serialize(s),
            Value::String(x) => s.serialize_str(x),
            Value::Bytes(b) => s.serialize_bytes(b),
            Value::List(l) => {
                let mut q = s.serialize_seq(Some(l.len()))?;
                for e in l {
                    q.serialize_element(e)?;
                }
                q.end()
            }
            Value::Tuple(l) => {
                let mut q = s.serialize_tuple(l.len())?;
                for e in l {
                    q.serialize_element(e)?;
                }
                q.end()
            }
            Value::Set(l) => s.serialize_newtype_struct(magic::SET, l),
            Value::Record(r) => {
                let mut m = s.serialize_map(Some(r.len()))?;
                for (k, v) in r.iter() {
                    m.serialize_entry(k, v)?;
                }
                m.end()
            }
            Value::Map(entries) => {
                let mut m = s.serialize_map(Some(entries.len()))?;
                for (k, v) in entries {
                    m.serialize_entry(k, v)?;
                }
                m.end()
            }
            Value::Variant(tag, None) => s.serialize_str(tag),
            Value::Variant(tag, Some(p)) => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry(tag, p)?;
                m.end()
            }
        }
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(ValueVisitor)
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(INJECT.with(|c| c.borrow_mut().take()).unwrap_or(Value::Null))
    }
    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        Value::deserialize(d)
    }
    fn visit_bool<E>(self, b: bool) -> Result<Value, E> {
        Ok(Value::Bool(b))
    }
    fn visit_i64<E>(self, i: i64) -> Result<Value, E> {
        Ok(Value::Int(i128::from(i)))
    }
    fn visit_u64<E>(self, i: u64) -> Result<Value, E> {
        Ok(Value::Int(i128::from(i)))
    }
    fn visit_i128<E>(self, i: i128) -> Result<Value, E> {
        Ok(Value::Int(i))
    }
    fn visit_u128<E: de::Error>(self, i: u128) -> Result<Value, E> {
        i128::try_from(i).map(Value::Int).map_err(|_| E::custom("integer exceeds the i128 range"))
    }
    fn visit_f64<E>(self, f: f64) -> Result<Value, E> {
        Ok(Value::Float(f))
    }
    fn visit_str<E>(self, s: &str) -> Result<Value, E> {
        Ok(Value::String(s.to_owned()))
    }
    fn visit_string<E>(self, s: String) -> Result<Value, E> {
        Ok(Value::String(s))
    }
    fn visit_bytes<E>(self, b: &[u8]) -> Result<Value, E> {
        Ok(Value::Bytes(b.to_vec()))
    }
    fn visit_byte_buf<E>(self, b: Vec<u8>) -> Result<Value, E> {
        Ok(Value::Bytes(b))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Value, A::Error> {
        let mut out = Vec::with_capacity(a.size_hint().unwrap_or(0).min(4096));
        while let Some(v) = a.next_element::<Value>()? {
            out.push(v);
        }
        Ok(Value::List(out))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Value, A::Error> {
        let mut entries: Vec<(Value, Value)> = Vec::new();
        while let Some((k, v)) = a.next_entry::<Value, Value>()? {
            entries.push((k, v));
        }
        if entries.iter().all(|(k, _)| matches!(k, Value::String(_))) {
            let mut r = Record::new();
            for (k, v) in entries {
                let Value::String(k) = k else { unreachable!() };
                r.insert(k, v).map_err(de::Error::custom)?;
            }
            Ok(Value::Record(r))
        } else {
            Value::map(entries).map_err(de::Error::custom)
        }
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
        d.deserialize_newtype_struct(magic::VALUE, ValueVisitor)
    }
}

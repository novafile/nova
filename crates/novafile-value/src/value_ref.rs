//! Borrowed values: the result of decoding binary input without copying its strings and bytes.

use crate::{Decimal, Duration, Rational, Record, Timestamp, Value};

/// A NOVA value whose strings, byte strings and keys borrow from the decoded input.
///
/// [`decode_ref`](crate::decode_ref) produces it with the same validation as
/// [`decode`](crate::decode), so sets, maps and records hold no duplicates.
/// Containers still allocate their element vectors; only the leaf data is
/// borrowed. Records keep their wire order. Equality, ordering and hashing are
/// defined on [`Value`]; convert with [`ValueRef::to_value`] to use them.
#[derive(Clone, Debug)]
pub enum ValueRef<'a> {
    /// The absence of a value.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A signed integer in the `i128` range.
    Int(i128),
    /// An IEEE 754 binary64 number.
    Float(f64),
    /// An exact decimal number.
    Decimal(Decimal),
    /// An exact fraction.
    Rational(Rational),
    /// A UTF-8 string.
    String(&'a str),
    /// An opaque byte string.
    Bytes(&'a [u8]),
    /// A UTC instant.
    Timestamp(Timestamp),
    /// A signed span of time.
    Duration(Duration),
    /// An ordered sequence.
    List(Vec<ValueRef<'a>>),
    /// A fixed-length sequence whose positions may have different kinds.
    Tuple(Vec<ValueRef<'a>>),
    /// An unordered collection of distinct values.
    Set(Vec<ValueRef<'a>>),
    /// String-keyed fields with unique keys, in wire order.
    Record(Vec<(&'a str, ValueRef<'a>)>),
    /// An unordered association with arbitrary keys, unique by value equality.
    Map(Vec<(ValueRef<'a>, ValueRef<'a>)>),
    /// A named alternative with an optional payload.
    Variant(&'a str, Option<Box<ValueRef<'a>>>),
}

impl ValueRef<'_> {
    /// The owned [`Value`] with the same content, copying every string and byte string.
    #[must_use]
    pub fn to_value(&self) -> Value {
        /// Convert each element.
        fn all(items: &[ValueRef<'_>]) -> Vec<Value> {
            items.iter().map(ValueRef::to_value).collect()
        }

        match self {
            ValueRef::Null => Value::Null,
            ValueRef::Bool(b) => Value::Bool(*b),
            ValueRef::Int(i) => Value::Int(*i),
            ValueRef::Float(f) => Value::Float(*f),
            ValueRef::Decimal(d) => Value::Decimal(*d),
            ValueRef::Rational(r) => Value::Rational(*r),
            ValueRef::String(s) => Value::String((*s).to_owned()),
            ValueRef::Bytes(b) => Value::Bytes(b.to_vec()),
            ValueRef::Timestamp(t) => Value::Timestamp(*t),
            ValueRef::Duration(d) => Value::Duration(*d),
            ValueRef::List(l) => Value::List(all(l)),
            ValueRef::Tuple(l) => Value::Tuple(all(l)),
            ValueRef::Set(l) => Value::Set(all(l)),
            ValueRef::Record(entries) => {
                let mut r = Record::with_capacity(entries.len());
                for (k, v) in entries {
                    // Keys were checked for duplicates while decoding.
                    r.push((*k).to_owned(), v.to_value());
                }

                Value::Record(r)
            }
            ValueRef::Map(entries) => Value::Map(entries.iter().map(|(k, v)| (k.to_value(), v.to_value())).collect()),
            ValueRef::Variant(name, payload) => Value::Variant((*name).to_owned(), payload.as_ref().map(|p| Box::new(p.to_value()))),
        }
    }
}

//! `serde::Deserializer` over an owned [`Value`].

use crate::error::{Error, field_seg};
use novafile_value::Value;
use novafile_value::serde_support::{__inject, magic};
use serde::de::{self, DeserializeSeed, IntoDeserializer, Visitor};

/// Deserializes from a value. `key` is set for map keys, where numeric
/// target types also accept their decimal string form (as in JSON objects).
pub struct ValueDeserializer {
    value: Value,
    key: bool,
}

impl ValueDeserializer {
    #[must_use]
    pub fn new(value: Value) -> Self {
        ValueDeserializer { value, key: false }
    }

    fn key(value: Value) -> Self {
        ValueDeserializer { value, key: true }
    }

    fn invalid<T>(&self, exp: &dyn de::Expected) -> Result<T, Error> {
        Err(de::Error::invalid_type(unexpected(&self.value), exp))
    }
}

fn unexpected(v: &Value) -> de::Unexpected<'_> {
    use de::Unexpected as U;
    match v {
        Value::Null => U::Unit,
        Value::Bool(b) => U::Bool(*b),
        Value::Int(i) => match i64::try_from(*i) {
            Ok(x) => U::Signed(x),
            Err(_) => U::Other("a large integer"),
        },
        Value::Float(f) => U::Float(*f),
        Value::String(s) => U::Str(s),
        Value::Bytes(b) => U::Bytes(b),
        Value::List(_) => U::Other("a list"),
        Value::Tuple(_) => U::Other("a tuple"),
        Value::Set(_) => U::Other("a set"),
        Value::Record(_) => U::Other("a record"),
        Value::Map(_) => U::Other("a map"),
        Value::Variant(..) => U::Other("a variant"),
        Value::Decimal(_) => U::Other("a decimal"),
        Value::Rational(_) => U::Other("a rational"),
        Value::Timestamp(_) => U::Other("a timestamp"),
        Value::Duration(_) => U::Other("a duration"),
    }
}

fn visit_int<'de, V: Visitor<'de>>(i: i128, v: V) -> Result<V::Value, Error> {
    if let Ok(x) = i64::try_from(i) {
        v.visit_i64(x)
    } else if let Ok(x) = u64::try_from(i) {
        v.visit_u64(x)
    } else {
        v.visit_i128(i)
    }
}

macro_rules! key_number {
    ($method:ident, $t:ty) => {
        fn $method<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
            if self.key {
                if let Value::String(s) = &self.value {
                    let n: $t = s.parse().map_err(|_| de::Error::invalid_value(de::Unexpected::Str(s), &v))?;
                    return ValueDeserializer::new(n.into_value()?).deserialize_any(v);
                }
            }

            self.deserialize_any(v)
        }
    };
}

trait IntoValue {
    fn into_value(self) -> Result<Value, Error>;
}

macro_rules! into_value_int {
    ($($t:ty),*) => {$(impl IntoValue for $t { fn into_value(self) -> Result<Value, Error> { Ok(Value::Int(self.into())) } })*};
}

into_value_int!(i8, i16, i32, i64, i128, u8, u16, u32, u64);
impl IntoValue for u128 {
    fn into_value(self) -> Result<Value, Error> {
        i128::try_from(self)
            .map(Value::Int)
            .map_err(|_| Error::new("u128 value exceeds the NOVA integer range of this implementation"))
    }
}

impl IntoValue for f32 {
    fn into_value(self) -> Result<Value, Error> {
        Ok(Value::Float(self.into()))
    }
}

impl IntoValue for f64 {
    fn into_value(self) -> Result<Value, Error> {
        Ok(Value::Float(self))
    }
}

impl IntoValue for bool {
    fn into_value(self) -> Result<Value, Error> {
        Ok(Value::Bool(self))
    }
}

impl<'de> de::Deserializer<'de> for ValueDeserializer {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        match self.value {
            Value::Null => v.visit_unit(),
            Value::Bool(b) => v.visit_bool(b),
            Value::Int(i) => visit_int(i, v),
            Value::Float(f) => v.visit_f64(f),
            Value::Decimal(d) => v.visit_string(d.to_string()),
            Value::Rational(r) => v.visit_string(r.to_string()),
            Value::Timestamp(t) => v.visit_string(t.to_string()),
            Value::Duration(d) => v.visit_string(d.to_string()),
            Value::String(s) => v.visit_string(s),
            Value::Bytes(b) => v.visit_byte_buf(b),
            Value::List(l) | Value::Tuple(l) | Value::Set(l) => visit_seq(l, v),
            Value::Record(r) => v.visit_map(RecordAccess::new(r.into_entries())),
            Value::Map(m) => v.visit_map(MapAccess {
                iter: m.into_iter(),
                value: None,
            }),
            Value::Variant(tag, None) => v.visit_string(tag),
            Value::Variant(tag, Some(p)) => v.visit_map(RecordAccess::new(vec![(tag, *p)])),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        match self.value {
            Value::Null => v.visit_none(),
            _ => v.visit_some(self),
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        match self.value {
            Value::Null => v.visit_unit(),
            _ => self.invalid(&v),
        }
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(self, _: &'static str, v: V) -> Result<V::Value, Error> {
        self.deserialize_unit(v)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(self, name: &'static str, v: V) -> Result<V::Value, Error> {
        match name {
            magic::VALUE => {
                __inject(self.value);
                v.visit_unit()
            }
            magic::TIMESTAMP | magic::DURATION | magic::DECIMAL | magic::RATIONAL => {
                let text = match self.value {
                    Value::Timestamp(t) => t.to_string(),
                    Value::Duration(d) => d.to_string(),
                    Value::Decimal(d) => d.to_string(),
                    Value::Rational(r) => r.to_string(),
                    Value::Int(i) => i.to_string(),
                    Value::Float(f) if name == magic::DECIMAL => format!("{f:?}"),
                    Value::String(s) => s,
                    _ => return self.invalid(&v),
                };

                v.visit_string(text)
            }
            _ => v.visit_newtype_struct(self),
        }
    }

    fn deserialize_seq<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        match self.value {
            Value::List(l) | Value::Tuple(l) | Value::Set(l) => visit_seq(l, v),
            _ => self.invalid(&v),
        }
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, _: usize, v: V) -> Result<V::Value, Error> {
        self.deserialize_seq(v)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(self, _: &'static str, _: usize, v: V) -> Result<V::Value, Error> {
        self.deserialize_seq(v)
    }

    fn deserialize_map<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        match self.value {
            Value::Record(r) => v.visit_map(RecordAccess::new(r.into_entries())),
            Value::Map(m) => v.visit_map(MapAccess {
                iter: m.into_iter(),
                value: None,
            }),
            _ => self.invalid(&v),
        }
    }

    fn deserialize_struct<V: Visitor<'de>>(self, _: &'static str, _: &'static [&'static str], v: V) -> Result<V::Value, Error> {
        self.deserialize_map(v)
    }

    fn deserialize_enum<V: Visitor<'de>>(self, _: &'static str, _: &'static [&'static str], v: V) -> Result<V::Value, Error> {
        match self.value {
            Value::Variant(tag, payload) => v.visit_enum(EnumAccess {
                tag,
                payload: payload.map(|p| *p),
            }),
            // Unit variants written as plain strings, and JSON-style `{tag: payload}`.
            Value::String(tag) => v.visit_enum(EnumAccess { tag, payload: None }),
            Value::Record(r) if r.len() == 1 => {
                let (tag, p) = r.into_entries().pop().unwrap();
                v.visit_enum(EnumAccess { tag, payload: Some(p) })
            }
            _ => self.invalid(&v),
        }
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_any(v)
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_any(v)
    }

    key_number!(deserialize_i8, i8);
    key_number!(deserialize_i16, i16);
    key_number!(deserialize_i32, i32);
    key_number!(deserialize_i64, i64);
    key_number!(deserialize_i128, i128);
    key_number!(deserialize_u8, u8);
    key_number!(deserialize_u16, u16);
    key_number!(deserialize_u32, u32);
    key_number!(deserialize_u64, u64);
    key_number!(deserialize_u128, u128);
    key_number!(deserialize_f32, f32);
    key_number!(deserialize_f64, f64);
    key_number!(deserialize_bool, bool);

    serde::forward_to_deserialize_any! { char str string identifier ignored_any }

    fn is_human_readable(&self) -> bool {
        true
    }
}

fn visit_seq<'de, V: Visitor<'de>>(items: Vec<Value>, v: V) -> Result<V::Value, Error> {
    let len = items.len();
    let mut access = SeqAccess {
        iter: items.into_iter().enumerate(),
    };

    let out = v.visit_seq(&mut access)?;
    if access.iter.len() != 0 {
        return Err(de::Error::invalid_length(len, &"fewer elements"));
    }

    Ok(out)
}

struct SeqAccess {
    iter: std::iter::Enumerate<std::vec::IntoIter<Value>>,
}

impl<'de> de::SeqAccess<'de> for SeqAccess {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>, Error> {
        match self.iter.next() {
            Some((i, v)) => seed.deserialize(ValueDeserializer::new(v)).map(Some).map_err(|e| e.at(format!("[{i}]"))),
            None => Ok(None),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

struct RecordAccess {
    iter: std::vec::IntoIter<(String, Value)>,
    value: Option<(String, Value)>,
}

impl RecordAccess {
    fn new(entries: Vec<(String, Value)>) -> Self {
        RecordAccess {
            iter: entries.into_iter(),
            value: None,
        }
    }
}

impl<'de> de::MapAccess<'de> for RecordAccess {
    type Error = Error;
    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>, Error> {
        match self.iter.next() {
            Some((k, v)) => {
                let key = seed.deserialize(ValueDeserializer::key(Value::String(k.clone())))?;
                self.value = Some((k, v));
                Ok(Some(key))
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<T::Value, Error> {
        let (k, v) = self.value.take().ok_or_else(|| Error::new("value requested before key"))?;
        seed.deserialize(ValueDeserializer::new(v)).map_err(|e| e.at(field_seg(&k)))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

struct MapAccess {
    iter: std::vec::IntoIter<(Value, Value)>,
    value: Option<(Value, Value)>,
}

impl<'de> de::MapAccess<'de> for MapAccess {
    type Error = Error;
    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>, Error> {
        match self.iter.next() {
            Some((k, v)) => {
                let key = seed.deserialize(ValueDeserializer::key(k.clone()))?;
                self.value = Some((k, v));
                Ok(Some(key))
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<T::Value, Error> {
        let (k, v) = self.value.take().ok_or_else(|| Error::new("value requested before key"))?;
        seed.deserialize(ValueDeserializer::new(v))
            .map_err(|e| e.at(format!("[{}]", novafile_text::flow_string(&k))))
    }
}

struct EnumAccess {
    tag: String,
    payload: Option<Value>,
}

impl<'de> de::EnumAccess<'de> for EnumAccess {
    type Error = Error;
    type Variant = VariantAccess;
    fn variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<(T::Value, VariantAccess), Error> {
        let tag: de::value::StringDeserializer<Error> = self.tag.clone().into_deserializer();
        let v = seed.deserialize(tag)?;
        Ok((
            v,
            VariantAccess {
                tag: self.tag,
                payload: self.payload,
            },
        ))
    }
}

struct VariantAccess {
    tag: String,
    payload: Option<Value>,
}

impl<'de> de::VariantAccess<'de> for VariantAccess {
    type Error = Error;
    fn unit_variant(self) -> Result<(), Error> {
        match self.payload {
            None | Some(Value::Null) => Ok(()),
            Some(_) => Err(Error::new(format!("variant {} takes no payload", self.tag))),
        }
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        seed.deserialize(ValueDeserializer::new(self.payload.unwrap_or(Value::Null)))
            .map_err(|e| e.at(self.tag))
    }

    fn tuple_variant<V: Visitor<'de>>(self, _: usize, v: V) -> Result<V::Value, Error> {
        let tag = self.tag;
        match self.payload {
            Some(Value::Tuple(l) | Value::List(l)) => visit_seq(l, v).map_err(|e| e.at(tag)),
            _ => Err(Error::new(format!("variant {tag} expects a tuple payload"))),
        }
    }

    fn struct_variant<V: Visitor<'de>>(self, _: &'static [&'static str], v: V) -> Result<V::Value, Error> {
        let tag = self.tag;
        match self.payload {
            Some(Value::Record(r)) => v.visit_map(RecordAccess::new(r.into_entries())).map_err(|e| e.at(tag)),
            _ => Err(Error::new(format!("variant {tag} expects a record payload"))),
        }
    }
}

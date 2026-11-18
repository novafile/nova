//! `serde::Serializer` that produces a [`Value`].

use crate::error::Error;
use novafile_value::serde_support::{__capture_begin, __capture_take, magic};
use novafile_value::{Decimal, Duration, Rational, Record, Timestamp, Value};
use serde::ser::{self, Serialize};

/// Serializes into a [`Value`]. String-keyed maps and structs become records,
/// enum variants NOVA variants; see the crate documentation for the full mapping.
pub struct ValueSerializer;

/// What every serializer method returns.
type R = Result<Value, Error>;

/// The string a NOVA kind marker serialized to; `what` names the kind in the error.
fn text_of(v: Value, what: &str) -> Result<String, Error> {
    match v {
        Value::String(s) => Ok(s),
        other => Err(Error::new(format!("{what} must serialize as a string, got {}", other.kind()))),
    }
}

impl ser::Serializer for ValueSerializer {
    type Ok = Value;
    type Error = Error;
    type SerializeSeq = SeqSer;
    type SerializeTuple = SeqSer;
    type SerializeTupleStruct = SeqSer;
    type SerializeTupleVariant = VariantSeqSer;
    type SerializeMap = MapSer;
    type SerializeStruct = StructSer;
    type SerializeStructVariant = VariantStructSer;

    fn serialize_bool(self, v: bool) -> R {
        Ok(Value::Bool(v))
    }

    fn serialize_i8(self, v: i8) -> R {
        Ok(Value::Int(v.into()))
    }

    fn serialize_i16(self, v: i16) -> R {
        Ok(Value::Int(v.into()))
    }

    fn serialize_i32(self, v: i32) -> R {
        Ok(Value::Int(v.into()))
    }

    fn serialize_i64(self, v: i64) -> R {
        Ok(Value::Int(v.into()))
    }

    fn serialize_i128(self, v: i128) -> R {
        Ok(Value::Int(v))
    }

    fn serialize_u8(self, v: u8) -> R {
        Ok(Value::Int(v.into()))
    }

    fn serialize_u16(self, v: u16) -> R {
        Ok(Value::Int(v.into()))
    }

    fn serialize_u32(self, v: u32) -> R {
        Ok(Value::Int(v.into()))
    }

    fn serialize_u64(self, v: u64) -> R {
        Ok(Value::Int(v.into()))
    }

    fn serialize_u128(self, v: u128) -> R {
        i128::try_from(v)
            .map(Value::Int)
            .map_err(|_| Error::new("u128 value exceeds the NOVA integer range of this implementation"))
    }

    fn serialize_f32(self, v: f32) -> R {
        Ok(Value::Float(novafile_value::f32_to_f64(v)))
    }

    fn serialize_f64(self, v: f64) -> R {
        Ok(Value::Float(v))
    }

    fn serialize_char(self, v: char) -> R {
        Ok(Value::String(v.to_string()))
    }

    fn serialize_str(self, v: &str) -> R {
        Ok(Value::String(v.to_owned()))
    }

    fn serialize_bytes(self, v: &[u8]) -> R {
        Ok(Value::Bytes(v.to_vec()))
    }

    fn serialize_none(self) -> R {
        Ok(Value::Null)
    }

    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> R {
        v.serialize(self)
    }

    fn serialize_unit(self) -> R {
        Ok(Value::Null)
    }

    fn serialize_unit_struct(self, _: &'static str) -> R {
        Ok(Value::Null)
    }

    fn serialize_unit_variant(self, _: &'static str, _: u32, variant: &'static str) -> R {
        Ok(Value::Variant(variant.to_owned(), None))
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, name: &'static str, v: &T) -> R {
        match name {
            magic::VALUE => {
                __capture_begin();
                let natural = v.serialize(ValueSerializer);
                match __capture_take() {
                    Some(captured) => Ok(captured),
                    None => natural,
                }
            }
            magic::TIMESTAMP => Ok(Value::Timestamp(
                Timestamp::parse(&text_of(v.serialize(self)?, "timestamp")?).map_err(Error::new)?,
            )),
            magic::DURATION => Ok(Value::Duration(Duration::parse(&text_of(v.serialize(self)?, "duration")?).map_err(Error::new)?)),
            magic::DECIMAL => Ok(Value::Decimal(Decimal::parse(&text_of(v.serialize(self)?, "decimal")?).map_err(Error::new)?)),
            magic::RATIONAL => Ok(Value::Rational(Rational::parse(&text_of(v.serialize(self)?, "rational")?).map_err(Error::new)?)),
            magic::SET => match v.serialize(self)? {
                Value::List(items) | Value::Set(items) => Value::set(items).map_err(Error::new),
                other => Err(Error::new(format!("set must serialize as a sequence, got {}", other.kind()))),
            },
            _ => v.serialize(self),
        }
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(self, _: &'static str, _: u32, variant: &'static str, v: &T) -> R {
        Ok(Value::Variant(
            variant.to_owned(),
            Some(Box::new(v.serialize(self).map_err(|e| e.at(variant.to_owned()))?)),
        ))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<SeqSer, Error> {
        Ok(SeqSer {
            items: Vec::with_capacity(len.unwrap_or(0).min(4096)),
            tuple: false,
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<SeqSer, Error> {
        Ok(SeqSer {
            items: Vec::with_capacity(len),
            tuple: true,
        })
    }

    fn serialize_tuple_struct(self, _: &'static str, len: usize) -> Result<SeqSer, Error> {
        self.serialize_tuple(len)
    }

    fn serialize_tuple_variant(self, _: &'static str, _: u32, variant: &'static str, len: usize) -> Result<VariantSeqSer, Error> {
        Ok(VariantSeqSer {
            tag: variant,
            items: Vec::with_capacity(len),
        })
    }

    fn serialize_map(self, _: Option<usize>) -> Result<MapSer, Error> {
        Ok(MapSer {
            entries: Vec::new(),
            key: None,
        })
    }

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<StructSer, Error> {
        Ok(StructSer { record: Record::new() })
    }

    fn serialize_struct_variant(self, _: &'static str, _: u32, variant: &'static str, _: usize) -> Result<VariantStructSer, Error> {
        Ok(VariantStructSer {
            tag: variant,
            record: Record::new(),
        })
    }

    fn is_human_readable(&self) -> bool {
        true
    }
}

/// Collects the elements of a sequence, tuple or tuple struct.
pub struct SeqSer {
    /// Elements serialized so far.
    items: Vec<Value>,
    /// Produce a tuple instead of a list.
    tuple: bool,
}

impl SeqSer {
    /// Serialize and append an element, adding its index to error paths.
    fn push<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        let i = self.items.len();
        self.items.push(v.serialize(ValueSerializer).map_err(|e| e.at(format!("[{i}]")))?);
        Ok(())
    }
}

impl ser::SerializeSeq for SeqSer {
    type Ok = Value;
    type Error = Error;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        self.push(v)
    }

    fn end(self) -> R {
        Ok(if self.tuple { Value::Tuple(self.items) } else { Value::List(self.items) })
    }
}

impl ser::SerializeTuple for SeqSer {
    type Ok = Value;
    type Error = Error;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        self.push(v)
    }

    fn end(self) -> R {
        ser::SerializeSeq::end(self)
    }
}

impl ser::SerializeTupleStruct for SeqSer {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        self.push(v)
    }

    fn end(self) -> R {
        ser::SerializeSeq::end(self)
    }
}

/// Collects the elements of a tuple variant.
pub struct VariantSeqSer {
    /// Variant name.
    tag: &'static str,
    /// Elements serialized so far.
    items: Vec<Value>,
}

impl ser::SerializeTupleVariant for VariantSeqSer {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        let i = self.items.len();
        self.items
            .push(v.serialize(ValueSerializer).map_err(|e| e.at(format!("[{i}]")).at(self.tag.to_owned()))?);
        Ok(())
    }

    fn end(self) -> R {
        Ok(Value::Variant(self.tag.to_owned(), Some(Box::new(Value::Tuple(self.items)))))
    }
}

/// Collects map entries; see `end` for whether the result is a record or a map.
pub struct MapSer {
    /// Entries serialized so far.
    entries: Vec<(Value, Value)>,
    /// The last key, waiting for its value.
    key: Option<Value>,
}

impl ser::SerializeMap for MapSer {
    type Ok = Value;
    type Error = Error;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, k: &T) -> Result<(), Error> {
        self.key = Some(k.serialize(ValueSerializer)?);
        Ok(())
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        let k = self.key.take().ok_or_else(|| Error::new("map value without a key"))?;
        let seg = match &k {
            Value::String(s) => crate::error::field_seg(s),
            other => format!("[{}]", novafile_text::flow_string(other)),
        };

        let v = v.serialize(ValueSerializer).map_err(|e| e.at(seg))?;
        self.entries.push((k, v));
        Ok(())
    }

    /// String-keyed maps become records; any other key type makes a map.
    fn end(self) -> R {
        if self.entries.iter().all(|(k, _)| matches!(k, Value::String(_))) {
            let mut r = Record::new();
            for (k, v) in self.entries {
                let Value::String(k) = k else { unreachable!() };
                r.insert(k, v).map_err(|e| Error::new(e.to_string()))?;
            }

            Ok(Value::Record(r))
        } else {
            Value::map(self.entries).map_err(Error::new)
        }
    }
}

/// Collects the fields of a struct into a record.
pub struct StructSer {
    /// Fields serialized so far.
    record: Record,
}

impl ser::SerializeStruct for StructSer {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, key: &'static str, v: &T) -> Result<(), Error> {
        let v = v.serialize(ValueSerializer).map_err(|e| e.at(crate::error::field_seg(key)))?;
        self.record.insert(key, v).map_err(|e| Error::new(e.to_string()))
    }

    fn end(self) -> R {
        Ok(Value::Record(self.record))
    }
}

/// Collects the fields of a struct variant into a record payload.
pub struct VariantStructSer {
    /// Variant name.
    tag: &'static str,
    /// Fields serialized so far.
    record: Record,
}

impl ser::SerializeStructVariant for VariantStructSer {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, key: &'static str, v: &T) -> Result<(), Error> {
        let v = v
            .serialize(ValueSerializer)
            .map_err(|e| e.at(crate::error::field_seg(key)).at(self.tag.to_owned()))?;
        self.record.insert(key, v).map_err(|e| Error::new(e.to_string()))
    }

    fn end(self) -> R {
        Ok(Value::Variant(self.tag.to_owned(), Some(Box::new(Value::Record(self.record)))))
    }
}

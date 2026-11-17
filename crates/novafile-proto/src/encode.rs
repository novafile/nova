//! NOVA value → Protobuf wire bytes.

use crate::shape::{Classified, Message, Plan, Scalar, Shape, Single, WireType, classify_field, classify_payload, field_number};
use crate::wire::{put_len, put_tag, put_varint, zigzag32, zigzag64};
use crate::{Fail, MAX_DEPTH, index};
use novafile_schema::Schema;
use novafile_text::Segment;
use novafile_value::Value;

const NANOS_PER_SEC: i128 = 1_000_000_000;

pub(crate) fn encode(value: &Value, schema: &Schema) -> Result<Vec<u8>, String> {
    let plan = Plan::new(schema)?;
    // Resolution applies the lexical conversions of spec §6.3 (so `"active"`
    // works for `!active`, a list for a set, …) and fills in defaults, which
    // are then written explicitly.
    let resolved = schema
        .resolve(value)
        .map_err(|issues| crate::issues("value does not match the schema", &issues))?;
    let mut buf = Vec::new();
    Encoder { schema }.message(&mut buf, &resolved, plan.root, 0).map_err(Fail::render)?;
    Ok(buf)
}

struct Encoder<'s> {
    schema: &'s Schema,
}

impl<'s> Encoder<'s> {
    fn message(&self, buf: &mut Vec<u8>, v: &Value, m: Message<'s>, depth: usize) -> Result<(), Fail> {
        if depth > MAX_DEPTH {
            return Err(Fail::new(format!("nesting is deeper than {MAX_DEPTH} messages")));
        }

        match (m, v) {
            (Message::Record(name, def), Value::Record(r)) => {
                if let Some(extra) = r.keys().find(|k| def.field(k).is_none()) {
                    return Err(Fail::new("field is not in the schema (extra fields of @open records have no field number)").at(Segment::Field(extra.clone())));
                }

                let mut fields = def.fields.iter().map(|f| Ok((field_number(name, f)?, f))).collect::<Result<Vec<_>, String>>()?;
                fields.sort_by_key(|(n, _)| *n);
                for (number, f) in fields {
                    if let Some(fv) = r.get(&f.name) {
                        let c = classify_field(self.schema, name, f)?;
                        self.field(buf, number, fv, c, depth).map_err(|e| e.at(Segment::Field(f.name.clone())))?;
                    }
                }

                Ok(())
            }
            (Message::Union(name, vs), Value::Variant(tag, payload)) => {
                let (i, vd) = vs
                    .iter()
                    .enumerate()
                    .find(|(_, d)| d.name == *tag)
                    .ok_or_else(|| Fail::new(format!("unknown variant {tag:?} of {name}")))?;
                #[expect(clippy::cast_possible_truncation, reason = "a schema cannot hold 2^32 variants")]
                let number = i as u32 + 1;
                match (classify_payload(self.schema, name, vd)?, payload) {
                    (Single::Message(Message::Empty), None) => {
                        put_tag(buf, number, WireType::Len);
                        put_varint(buf, 0);
                        Ok(())
                    }
                    (single, Some(p)) => self.single(buf, number, p, single, depth).map_err(|e| e.at(Segment::Field(tag.clone()))),
                    (_, None) => Err(Fail::new(format!("variant {tag} requires a payload"))),
                }
            }
            (Message::Timestamp, Value::Timestamp(t)) => {
                put_tag(buf, 1, WireType::Varint);
                put_varint(buf, t.unix_seconds().cast_unsigned());
                put_tag(buf, 2, WireType::Varint);
                put_varint(buf, u64::from(t.subsec_nanos()));
                Ok(())
            }
            (Message::Duration, Value::Duration(d)) => {
                // Truncating division gives seconds and nanos of the same sign,
                // as google.protobuf.Duration requires.
                let n = d.as_nanos();
                let secs = i64::try_from(n / NANOS_PER_SEC).map_err(|_| Fail::new("duration exceeds the google.protobuf.Duration range"))?;
                let nanos = (n % NANOS_PER_SEC) as i64;
                put_tag(buf, 1, WireType::Varint);
                put_varint(buf, secs.cast_unsigned());
                put_tag(buf, 2, WireType::Varint);
                put_varint(buf, nanos.cast_unsigned());
                Ok(())
            }
            (m, v) => Err(mismatch(&Single::Message(m).proto_name(), v)),
        }
    }

    fn field(&self, buf: &mut Vec<u8>, number: u32, v: &Value, c: Classified<'s>, depth: usize) -> Result<(), Fail> {
        if let Value::Null = v {
            // proto3: an unset field reads back as null.
            return if c.nullable { Ok(()) } else { Err(Fail::new("null is not allowed here")) };
        }

        match c.shape {
            Shape::Single(s) => self.single(buf, number, v, s, depth),
            Shape::Repeated { elem, .. } => {
                let (Value::List(items) | Value::Set(items)) = v else {
                    return Err(mismatch("a list or set", v));
                };

                match elem {
                    Single::Scalar(s) if s.packable() => {
                        if items.is_empty() {
                            return Ok(());
                        }

                        let mut packed = Vec::new();
                        for (i, item) in items.iter().enumerate() {
                            scalar(&mut packed, item, s).map_err(|e| e.at(index(i)))?;
                        }

                        put_tag(buf, number, WireType::Len);
                        put_len(buf, &packed);
                    }
                    _ => {
                        for (i, item) in items.iter().enumerate() {
                            self.single(buf, number, item, elem, depth).map_err(|e| e.at(index(i)))?;
                        }
                    }
                }

                Ok(())
            }
            Shape::Map { key, value } => {
                let Value::Map(entries) = v else {
                    return Err(mismatch("a map", v));
                };

                let mut entry = Vec::new();
                for (k, val) in entries {
                    entry.clear();
                    self.single(&mut entry, 1, k, Single::Scalar(key), depth)
                        .and_then(|()| self.single(&mut entry, 2, val, value, depth))
                        .map_err(|e| e.at(Segment::Key(k.clone())))?;
                    put_tag(buf, number, WireType::Len);
                    put_len(buf, &entry);
                }

                Ok(())
            }
        }
    }

    fn single(&self, buf: &mut Vec<u8>, number: u32, v: &Value, s: Single<'s>, depth: usize) -> Result<(), Fail> {
        match s {
            Single::Scalar(sc) => {
                put_tag(buf, number, sc.wire_type());
                scalar(buf, v, sc)
            }
            Single::Message(m) => {
                let mut nested = Vec::new();
                self.message(&mut nested, v, m, depth + 1)?;
                put_tag(buf, number, WireType::Len);
                put_len(buf, &nested);
                Ok(())
            }
        }
    }
}

/// Write a scalar's payload (without its tag).
fn scalar(buf: &mut Vec<u8>, v: &Value, s: Scalar<'_>) -> Result<(), Fail> {
    let range = |i: &i128, ty: &str| Fail::new(format!("integer {i} is out of the {ty} range"));
    match (s, v) {
        (Scalar::Bool, Value::Bool(b)) => put_varint(buf, u64::from(*b)),
        (Scalar::SInt32, Value::Int(i)) => put_varint(buf, zigzag32(i32::try_from(*i).map_err(|_| range(i, "sint32"))?)),
        (Scalar::SInt64, Value::Int(i)) => put_varint(buf, zigzag64(i64::try_from(*i).map_err(|_| range(i, "sint64"))?)),
        (Scalar::UInt32, Value::Int(i)) => put_varint(buf, u64::from(u32::try_from(*i).map_err(|_| range(i, "uint32"))?)),
        (Scalar::UInt64, Value::Int(i)) => put_varint(buf, u64::try_from(*i).map_err(|_| range(i, "uint64"))?),
        (Scalar::Double, Value::Float(f)) => buf.extend_from_slice(&f.to_bits().to_le_bytes()),
        (Scalar::String, Value::String(s)) => put_len(buf, s.as_bytes()),
        (Scalar::Bytes, Value::Bytes(b)) => put_len(buf, b),
        (Scalar::Decimal, Value::Decimal(d)) => put_len(buf, d.to_string().as_bytes()),
        (Scalar::Rational, Value::Rational(r)) => put_len(buf, r.to_string().as_bytes()),
        (Scalar::Enum(name, vs), Value::Variant(tag, None)) => {
            let i = vs
                .iter()
                .position(|d| d.name == *tag)
                .ok_or_else(|| Fail::new(format!("unknown variant {tag:?} of {name}")))?;
            put_varint(buf, i as u64 + 1);
        }
        (s, v) => {
            let expected = match s {
                Scalar::Decimal => "decimal".into(),
                Scalar::Rational => "rational".into(),
                Scalar::Enum(name, _) => format!("a unit variant of {name}"),
                s => Single::Scalar(s).proto_name(),
            };

            return Err(mismatch(&expected, v));
        }
    }

    Ok(())
}

fn mismatch(expected: &str, v: &Value) -> Fail {
    Fail::new(format!("expected {expected}, found {}", v.kind()))
}

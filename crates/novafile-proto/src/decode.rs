//! Protobuf wire bytes → NOVA value.

use crate::shape::{Classified, Message, Plan, Scalar, Shape, Single, WireType, classify_field, classify_payload, field_number};
use crate::wire::{Reader, Wire, fields, unzigzag32, unzigzag64};
use crate::{Fail, MAX_DEPTH, index};
use novafile_schema::{FieldDef, Schema};
use novafile_text::Segment;
use novafile_value::{Decimal, Duration, Rational, Record, Timestamp, Value};
use std::collections::HashMap;

const NANOS_PER_SEC: i64 = 1_000_000_000;

pub(crate) fn decode(bytes: &[u8], schema: &Schema) -> Result<Value, String> {
    let plan = Plan::new(schema)?;
    let value = Decoder { schema }.message(&[bytes], plan.root, 0).map_err(Fail::render)?;
    schema
        .validate(&value)
        .map_err(|issues| crate::issues("decoded value does not match the schema", &issues))?;
    Ok(value)
}

struct Decoder<'s> {
    schema: &'s Schema,
}

impl<'s> Decoder<'s> {
    /// Decode a message. Several `parts` are merged, exactly as if their
    /// bytes were concatenated (how Protobuf merges repeated occurrences of
    /// a singular message field).
    fn message(&self, parts: &[&[u8]], m: Message<'s>, depth: usize) -> Result<Value, Fail> {
        if depth > MAX_DEPTH {
            return Err(Fail::new(format!("nesting is deeper than {MAX_DEPTH} messages")));
        }

        let mut wire = Vec::new();
        for part in parts {
            wire.extend(fields(part)?);
        }

        match m {
            Message::Record(name, def) => {
                let mut by_number: HashMap<u32, Vec<Wire<'_>>> = HashMap::new();
                for (n, w) in wire {
                    by_number.entry(n).or_default().push(w);
                }

                let mut out = Record::new();
                for f in &def.fields {
                    let number = field_number(name, f)?;
                    let c = classify_field(self.schema, name, f)?;
                    let value = match by_number.get(&number) {
                        Some(entries) => Some(self.field(entries, c, depth)),
                        None => self.missing(f, c, depth).transpose(),
                    };

                    if let Some(v) = value {
                        let v = v.map_err(|e| e.at(Segment::Field(f.name.clone())))?;
                        out.insert(f.name.clone(), v).expect("schema field names are unique");
                    }
                }

                // Fields with other numbers are unknown and skipped.
                Ok(Value::Record(out))
            }
            Message::Union(name, vs) => {
                // The last member set wins; repeated occurrences of that member merge.
                let mut current = None;
                let mut entries = Vec::new();
                for (n, w) in wire {
                    if (1..=vs.len()).contains(&(n as usize)) {
                        if current != Some(n) {
                            current = Some(n);
                            entries.clear();
                        }

                        entries.push(w);
                    }
                }

                let n = current.ok_or_else(|| Fail::new(format!("no variant of {name} is set")))?;
                let vd = &vs[n as usize - 1];
                let payload = match classify_payload(self.schema, name, vd)? {
                    Single::Message(Message::Empty) => {
                        // Still check that the Empty messages are well-formed.
                        self.field(&entries, single(Single::Message(Message::Empty)), depth)?;
                        None
                    }
                    s => Some(Box::new(
                        self.field(&entries, single(s), depth).map_err(|e| e.at(Segment::Field(vd.name.clone())))?,
                    )),
                };

                Ok(Value::Variant(vd.name.clone(), payload))
            }
            Message::Timestamp => {
                let secs = last_varint(&wire, 1)?.unwrap_or(0).cast_signed();
                #[expect(clippy::cast_possible_truncation, reason = "int32 fields use the low 32 bits of the varint")]
                let nanos = (last_varint(&wire, 2)?.unwrap_or(0) as u32).cast_signed();
                let nanos = u32::try_from(nanos).map_err(|_| Fail::new(format!("google.protobuf.Timestamp has negative nanos {nanos}")))?;
                Timestamp::new(secs, nanos).map(Value::Timestamp).map_err(Fail::new)
            }
            Message::Duration => {
                let secs = last_varint(&wire, 1)?.unwrap_or(0).cast_signed();
                #[expect(clippy::cast_possible_truncation, reason = "int32 fields use the low 32 bits of the varint")]
                let nanos = i64::from((last_varint(&wire, 2)?.unwrap_or(0) as u32).cast_signed());
                if nanos.abs() >= NANOS_PER_SEC || (secs > 0 && nanos < 0) || (secs < 0 && nanos > 0) {
                    return Err(Fail::new(format!("invalid google.protobuf.Duration: seconds {secs}, nanos {nanos}")));
                }

                let total = i128::from(secs) * i128::from(NANOS_PER_SEC) + i128::from(nanos);
                Duration::from_nanos(total).map(Value::Duration).map_err(Fail::new)
            }
            Message::Empty => Ok(Value::Null),
        }
    }

    /// The value of a record field that is absent from the wire (proto3 rules).
    fn missing(&self, f: &FieldDef, c: Classified<'s>, depth: usize) -> Result<Option<Value>, Fail> {
        if f.optional {
            return Ok(None);
        }

        if c.nullable {
            return Ok(Some(Value::Null));
        }

        if let Some(d) = &f.default {
            // Defaults are written as flow values; resolve them like the schema does.
            return Ok(Some(self.schema.resolve_as(d, &f.ty).unwrap_or_else(|_| d.clone())));
        }

        Ok(Some(match c.shape {
            Shape::Single(s) => self.zero(s, depth)?,
            Shape::Repeated { set: false, .. } => Value::List(Vec::new()),
            Shape::Repeated { set: true, .. } => Value::Set(Vec::new()),
            Shape::Map { .. } => Value::Map(Vec::new()),
        }))
    }

    /// The proto3 zero value of a single value; an absent message decodes as
    /// an empty one.
    fn zero(&self, s: Single<'s>, depth: usize) -> Result<Value, Fail> {
        Ok(match s {
            Single::Scalar(Scalar::Bool) => Value::Bool(false),
            Single::Scalar(Scalar::SInt32 | Scalar::SInt64 | Scalar::UInt32 | Scalar::UInt64) => Value::Int(0),
            Single::Scalar(Scalar::Double) => Value::Float(0.0),
            Single::Scalar(Scalar::String) => Value::String(String::new()),
            Single::Scalar(Scalar::Bytes) => Value::Bytes(Vec::new()),
            Single::Scalar(Scalar::Decimal) => Value::Decimal(Decimal::from_int(0)),
            Single::Scalar(Scalar::Rational) => Value::Rational(Rational::new(0, 1).map_err(Fail::new)?),
            Single::Scalar(Scalar::Enum(name, _)) => {
                return Err(Fail::new(format!("missing value: {name} has no zero value (0 is {name} UNSPECIFIED)")));
            }
            Single::Message(m) => self.message(&[], m, depth + 1)?,
        })
    }

    /// Decode all wire occurrences of one field.
    fn field(&self, entries: &[Wire<'_>], c: Classified<'s>, depth: usize) -> Result<Value, Fail> {
        match c.shape {
            Shape::Single(Single::Scalar(s)) => scalar(*entries.last().expect("at least one entry"), s),
            Shape::Single(Single::Message(m)) => {
                let parts = entries.iter().map(|w| len(*w)).collect::<Result<Vec<_>, _>>()?;
                self.message(&parts, m, depth + 1)
            }
            Shape::Repeated { elem, set } => {
                let mut items = Vec::new();
                for w in entries {
                    let at = index(items.len());
                    match (elem, w) {
                        (Single::Scalar(s), Wire::Len(bytes)) if s.packable() => packed(bytes, s, &mut items),
                        (Single::Scalar(s), w) => scalar(*w, s).map(|v| items.push(v)),
                        (Single::Message(m), w) => len(*w).and_then(|b| self.message(&[b], m, depth + 1)).map(|v| items.push(v)),
                    }
                    .map_err(|e| e.at(at))?;
                }

                Ok(if set { Value::Set(items) } else { Value::List(items) })
            }
            Shape::Map { key, value } => {
                let mut out: Vec<(Value, Value)> = Vec::new();
                let mut index: HashMap<Value, usize> = HashMap::new();
                for w in entries {
                    let entry = fields(len(*w)?)?;
                    let of = |n: u32| entry.iter().filter(|(k, _)| *k == n).map(|(_, w)| *w).collect::<Vec<_>>();
                    let (keys, values) = (of(1), of(2));
                    let k = match keys.last() {
                        Some(w) => scalar(*w, key)?,
                        None => self.zero(Single::Scalar(key), depth)?,
                    };

                    let v = if values.is_empty() {
                        self.zero(value, depth)
                    } else {
                        self.field(&values, single(value), depth)
                    };

                    let v = v.map_err(|e| e.at(Segment::Key(k.clone())))?;
                    // Last entry wins for duplicate keys.
                    if let Some(&i) = index.get(&k) {
                        out[i].1 = v;
                    } else {
                        index.insert(k.clone(), out.len());
                        out.push((k, v));
                    }
                }

                Ok(Value::Map(out))
            }
        }
    }
}

fn single(s: Single<'_>) -> Classified<'_> {
    Classified {
        shape: Shape::Single(s),
        nullable: false,
    }
}

fn len(w: Wire<'_>) -> Result<&[u8], Fail> {
    match w {
        Wire::Len(b) => Ok(b),
        w => Err(wire_mismatch(w, WireType::Len)),
    }
}

fn wire_mismatch(w: Wire<'_>, expected: WireType) -> Fail {
    Fail::new(format!("wire type {} where {} was expected", w.wire_type() as u8, expected as u8))
}

/// The last varint value of field `n` in a well-known-type message.
fn last_varint(wire: &[(u32, Wire<'_>)], n: u32) -> Result<Option<u64>, Fail> {
    match wire.iter().rev().find(|(k, _)| *k == n) {
        None => Ok(None),
        Some((_, Wire::Varint(v))) => Ok(Some(*v)),
        Some((_, w)) => Err(wire_mismatch(*w, WireType::Varint)),
    }
}

/// Decode the elements of a packed repeated scalar field.
fn packed(bytes: &[u8], s: Scalar<'_>, items: &mut Vec<Value>) -> Result<(), Fail> {
    let mut r = Reader::new(bytes);
    while !r.is_empty() {
        let w = match s.wire_type() {
            WireType::Varint => Wire::Varint(r.varint()?),
            WireType::I64 => Wire::I64(r.fixed64()?),
            wt => unreachable!("{wt:?} is not packable"),
        };

        items.push(scalar(w, s).map_err(|e| e.at(index(items.len())))?);
    }

    Ok(())
}

#[expect(clippy::cast_possible_truncation, reason = "uint32 and enum (int32) fields use the low 32 bits of the varint")]
fn scalar(w: Wire<'_>, s: Scalar<'_>) -> Result<Value, Fail> {
    if w.wire_type() != s.wire_type() {
        return Err(wire_mismatch(w, s.wire_type()));
    }

    let text = |b: &[u8]| String::from_utf8(b.to_vec()).map_err(|_| Fail::new("string is not valid UTF-8"));
    Ok(match (s, w) {
        (Scalar::Bool, Wire::Varint(v)) => Value::Bool(v != 0),
        (Scalar::SInt32, Wire::Varint(v)) => Value::Int(unzigzag32(v).into()),
        (Scalar::SInt64, Wire::Varint(v)) => Value::Int(unzigzag64(v).into()),
        (Scalar::UInt32, Wire::Varint(v)) => Value::Int((v as u32).into()),
        (Scalar::UInt64, Wire::Varint(v)) => Value::Int(v.into()),
        (Scalar::Enum(name, vs), Wire::Varint(v)) => {
            // Enums are int32 on the wire.
            let number = (v as u32).cast_signed();
            let idx = usize::try_from(number)
                .ok()
                .and_then(|n| n.checked_sub(1))
                .filter(|&i| i < vs.len())
                .ok_or_else(|| {
                    Fail::new(match number {
                        0 => format!("{name} value 0 (UNSPECIFIED) has no NOVA variant"),
                        n => format!("unknown {name} value {n}"),
                    })
                })?;
            Value::Variant(vs[idx].name.clone(), None)
        }
        (Scalar::Double, Wire::I64(bits)) => Value::Float(f64::from_bits(bits)),
        (Scalar::String, Wire::Len(b)) => Value::String(text(b)?),
        (Scalar::Bytes, Wire::Len(b)) => Value::Bytes(b.to_vec()),
        // The empty string is the proto3 zero value; read it as zero.
        (Scalar::Decimal, Wire::Len([])) => Value::Decimal(Decimal::from_int(0)),
        (Scalar::Decimal, Wire::Len(b)) => Value::Decimal(Decimal::parse(&text(b)?).map_err(|e| Fail::new(format!("invalid decimal: {e}")))?),
        (Scalar::Rational, Wire::Len([])) => Value::Rational(Rational::new(0, 1).map_err(Fail::new)?),
        (Scalar::Rational, Wire::Len(b)) => Value::Rational(Rational::parse(&text(b)?).map_err(|e| Fail::new(format!("invalid rational: {e}")))?),
        (s, w) => unreachable!("wire type checked: {s:?} / {w:?}"),
    })
}

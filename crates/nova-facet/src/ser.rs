//! Reading a value through [`Peek`] into a NOVA [`Value`].

use crate::error::Error;
use facet::{Def, ScalarType, StructKind, Type, UserType};
use facet_reflect::{HasFields, Peek, PeekEnum, PeekStruct};
use nova_value::{Decimal, Duration, Rational, Record, Timestamp, Value};

type R = Result<Value, Error>;

/// Serialize any reflected value.
pub(crate) fn value(p: Peek<'_, '_>) -> R {
    let p = deref(p);
    if let Some(v) = nova_kind(p)? {
        return Ok(v);
    }
    let shape = p.shape();
    // A container-level `#[facet(proxy = X)]` serializes as `X`.
    if shape.proxy.is_some() {
        if let Some(owned) = p.custom_serialization_from_shape()? {
            return value(owned.as_peek());
        }
    }
    if let Some(st) = p.scalar_type() {
        return scalar(p, st);
    }
    match shape.def {
        Def::Option(_) => {
            let o = p.into_option()?;
            return match o.value() {
                Some(inner) => value(inner),
                None => Ok(Value::Null),
            };
        }
        Def::Result(_) => {
            let r = p.into_result()?;
            let (tag, inner) = if r.is_ok() { ("Ok", r.ok()) } else { ("Err", r.err()) };
            let inner = inner.ok_or_else(|| Error::new("result without a value"))?;
            return Ok(Value::Variant(tag.into(), Some(Box::new(value(inner).map_err(|e| e.at(tag.into()))?))));
        }
        Def::List(_) | Def::Array(_) | Def::Slice(_) => {
            let list = p.into_list_like()?;
            if let Some(b) = list.as_bytes() {
                return Ok(Value::Bytes(b.to_vec()));
            }
            let items = list
                .iter()
                .enumerate()
                .map(|(i, item)| value(item).map_err(|e| e.at_index(i)))
                .collect::<Result<_, _>>()?;
            return Ok(Value::List(items));
        }
        Def::Set(_) => {
            let items = p
                .into_set()?
                .iter()
                .enumerate()
                .map(|(i, item)| value(item).map_err(|e| e.at_index(i)))
                .collect::<Result<_, _>>()?;
            return Value::set(items).map_err(Error::new);
        }
        Def::Map(_) => {
            let mut entries = Vec::new();
            for (k, v) in p.into_map()?.iter() {
                let k = value(k)?;
                let seg = match &k {
                    Value::String(s) => nova_text::key_text(s),
                    other => format!("[{}]", nova_text::flow_string(other)),
                };
                entries.push((k, value(v).map_err(|e| e.at(seg))?));
            }
            return map_or_record(entries);
        }
        Def::DynamicValue(_) => return Err(Error::new(format!("dynamic value type {shape} is not supported"))),
        _ => {}
    }
    match shape.ty {
        Type::User(UserType::Struct(_)) => structure(p.into_struct()?),
        Type::User(UserType::Enum(_)) => enumeration(p.into_enum()?),
        _ => {
            // Transparent wrappers (`NonZero`, `#[facet(transparent)]` types
            // without a struct body) serialize as what they wrap.
            if shape.inner.is_some() {
                let inner = p.innermost_peek();
                if inner.shape().id != shape.id {
                    return value(inner);
                }
            }
            // Other scalars (addresses, paths, …) use their display form.
            if matches!(shape.def, Def::Scalar) && shape.vtable.has_display() {
                return Ok(Value::String(p.to_string()));
            }
            Err(Error::new(format!("cannot serialize a value of type {shape}")))
        }
    }
}

/// Follow `Box`, `Arc`, `Rc`, references, `Cow`, … to the pointee.
fn deref<'mem, 'facet>(p: Peek<'mem, 'facet>) -> Peek<'mem, 'facet> {
    if let Ok(ptr) = p.into_pointer() {
        if let Some(inner) = ptr.borrow_inner() {
            return deref(inner);
        }
    }
    p
}

/// The NOVA value types and `std::time::Duration` map to NOVA kinds directly.
fn nova_kind(p: Peek<'_, '_>) -> Result<Option<Value>, Error> {
    Ok(Some(if let Ok(v) = p.get::<Value>() {
        v.clone()
    } else if let Ok(t) = p.get::<Timestamp>() {
        Value::Timestamp(*t)
    } else if let Ok(d) = p.get::<Duration>() {
        Value::Duration(*d)
    } else if let Ok(d) = p.get::<Decimal>() {
        Value::Decimal(*d)
    } else if let Ok(r) = p.get::<Rational>() {
        Value::Rational(*r)
    } else if let Ok(d) = p.get::<std::time::Duration>() {
        Value::Duration(Duration::from_nanos(d.as_nanos() as i128).map_err(Error::new)?)
    } else {
        return Ok(None);
    }))
}

fn scalar(p: Peek<'_, '_>, st: ScalarType) -> R {
    macro_rules! int {
        ($t:ty) => {
            Value::Int((*p.get::<$t>()?).into())
        };
    }
    Ok(match st {
        ScalarType::Unit => Value::Null,
        ScalarType::Bool => Value::Bool(*p.get::<bool>()?),
        ScalarType::Char => Value::String(p.get::<char>()?.to_string()),
        ScalarType::Str | ScalarType::String | ScalarType::CowStr => Value::String(p.as_str().ok_or_else(|| Error::new("string value expected"))?.to_owned()),
        ScalarType::F32 => Value::Float((*p.get::<f32>()?).into()),
        ScalarType::F64 => Value::Float(*p.get::<f64>()?),
        ScalarType::U8 => int!(u8),
        ScalarType::U16 => int!(u16),
        ScalarType::U32 => int!(u32),
        ScalarType::U64 => int!(u64),
        ScalarType::USize => Value::Int(*p.get::<usize>()? as i128),
        ScalarType::U128 => i128::try_from(*p.get::<u128>()?)
            .map(Value::Int)
            .map_err(|_| Error::new("u128 value exceeds the NOVA integer range of this implementation"))?,
        ScalarType::I8 => int!(i8),
        ScalarType::I16 => int!(i16),
        ScalarType::I32 => int!(i32),
        ScalarType::I64 => int!(i64),
        ScalarType::ISize => Value::Int(*p.get::<isize>()? as i128),
        ScalarType::I128 => Value::Int(*p.get::<i128>()?),
        _ if p.shape().vtable.has_display() => Value::String(p.to_string()),
        _ => return Err(Error::new(format!("cannot serialize a value of type {}", p.shape()))),
    })
}

/// String-keyed maps become records; any other key type makes a map.
fn map_or_record(entries: Vec<(Value, Value)>) -> R {
    if entries.iter().all(|(k, _)| matches!(k, Value::String(_))) {
        let mut r = Record::new();
        for (k, v) in entries {
            let Value::String(k) = k else { unreachable!() };
            r.insert(k, v).map_err(|e| Error::new(e.to_string()))?;
        }
        Ok(Value::Record(r))
    } else {
        Value::map(entries).map_err(Error::new)
    }
}

/// A field's value, honoring a field-level `#[facet(proxy = X)]`.
fn field_value(field: Option<facet::Field>, v: Peek<'_, '_>) -> R {
    match field {
        Some(f) if f.proxy().is_some() => value(v.custom_serialization(f)?.as_peek()),
        _ => value(v),
    }
}

/// The fields of a struct or struct variant as a record, honoring `skip`,
/// `skip_serializing_if`, `rename`/`rename_all` and `flatten`.
fn record<'mem, 'facet>(fields: impl Iterator<Item = (facet_reflect::FieldItem, Peek<'mem, 'facet>)>) -> R {
    let mut r = Record::new();
    for (item, v) in fields {
        let key = item.effective_name().to_owned();
        // A flattened enum contributes one entry: its tag and its payload.
        let v = match v.into_enum() {
            Ok(e) if item.flattened => variant_payload(e)?.unwrap_or(Value::Null),
            _ => field_value(item.field, v)?,
        };
        r.insert(key.clone(), v).map_err(|e| Error::new(e.to_string())).map_err(|e| e.at_field(&key))?;
    }
    Ok(Value::Record(r))
}

fn structure(s: PeekStruct<'_, '_>) -> R {
    match s.ty().kind {
        StructKind::Unit => Ok(Value::Null),
        StructKind::Struct => record(s.fields_for_serialize()),
        StructKind::TupleStruct | StructKind::Tuple => {
            let fields: Vec<_> = s.fields_for_binary_serialize().collect();
            // A newtype struct is its content (as with serde).
            if s.ty().kind == StructKind::TupleStruct && fields.len() == 1 {
                let (item, v) = fields.into_iter().next().unwrap();
                return field_value(item.field, v);
            }
            let items = fields
                .into_iter()
                .enumerate()
                .map(|(i, (item, v))| field_value(item.field, v).map_err(|e| e.at_index(i)))
                .collect::<Result<_, _>>()?;
            Ok(Value::Tuple(items))
        }
    }
}

/// A variant's payload: none for unit variants, the content of a newtype
/// variant, a tuple, or a record. Errors are reported under the tag.
fn variant_payload(e: PeekEnum<'_, '_>) -> Result<Option<Value>, Error> {
    let variant = e.active_variant()?;
    let tag = variant.effective_name();
    let payload = match variant.data.kind {
        StructKind::Unit => return Ok(None),
        StructKind::Struct => record(e.fields_for_serialize()),
        StructKind::Tuple | StructKind::TupleStruct => {
            let fields: Vec<_> = e.fields_for_binary_serialize().collect();
            if fields.len() == 1 {
                let (item, v) = fields.into_iter().next().unwrap();
                field_value(item.field, v)
            } else {
                fields
                    .into_iter()
                    .enumerate()
                    .map(|(i, (item, v))| field_value(item.field, v).map_err(|e| e.at_index(i)))
                    .collect::<Result<_, _>>()
                    .map(Value::Tuple)
            }
        }
    };
    payload.map(Some).map_err(|err| err.at(tag.to_owned()))
}

/// Enums map to variants; `untagged`, internally (`tag`) and adjacently
/// (`tag` + `content`) tagged enums map like serde's representations.
fn enumeration(e: PeekEnum<'_, '_>) -> R {
    let shape = e.shape();
    let variant = e.active_variant()?;
    let tag = variant.effective_name();
    if shape.is_numeric() {
        return Ok(Value::Int(e.discriminant().into()));
    }
    let payload = variant_payload(e)?;
    if shape.is_untagged() {
        return Ok(payload.unwrap_or(Value::Null));
    }
    match (shape.get_tag_attr(), shape.get_content_attr()) {
        // As with nova-serde: serde writes the adjacent tag as a unit variant.
        (Some(t), Some(c)) => {
            let mut r = Record::new();
            r.insert(t, Value::Variant(tag.into(), None)).map_err(|e| Error::new(e.to_string()))?;
            if let Some(p) = payload {
                r.insert(c, p).map_err(|e| Error::new(e.to_string()))?;
            }
            Ok(Value::Record(r))
        }
        (Some(t), None) => {
            let mut r = Record::new();
            r.insert(t, Value::String(tag.into())).map_err(|e| Error::new(e.to_string()))?;
            match payload {
                None | Some(Value::Null) => {}
                Some(Value::Record(fields)) => {
                    for (k, v) in fields.into_entries() {
                        r.insert(k, v).map_err(|e| Error::new(e.to_string()))?;
                    }
                }
                Some(other) => {
                    return Err(Error::new(format!(
                        "internally tagged variant {tag} must have a record payload, got {}",
                        other.kind()
                    )))
                }
            }
            Ok(Value::Record(r))
        }
        _ => Ok(Value::Variant(tag.into(), payload.map(Box::new))),
    }
}

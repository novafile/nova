//! Building a value through [`Partial`] from a NOVA [`Value`].
//!
//! The target type's shape drives the walk, so the Rust type acts as a
//! schema: a string becomes a timestamp where the type asks for one, a string
//! naming a unit variant becomes that variant, and so on (spec §11.3).

use crate::error::Error;
use facet::{Def, Facet, Field, KnownPointer, ScalarType, Shape, StructKind, Type, UserType, Variant};
use facet_reflect::{DeserStrategy, Partial};
use novafile_value::{Decimal, Duration, Rational, Record, Timestamp, Value};
use std::borrow::Cow;

type P = Partial<'static, false>;
type R = Result<P, Error>;

/// Build a `T` from a value.
pub(crate) fn build<T: Facet<'static>>(v: Value) -> Result<T, Error> {
    let wip = fill(Partial::alloc_owned::<T>()?, v)?;
    wip.build()?.materialize::<T>().map_err(|e| Error::new(e.to_string()))
}

/// What to do with the current frame, copied out of facet's precomputed plan
/// so the `Partial` can move on.
enum Plan {
    FieldProxy,
    ContainerProxy,
    Pointer,
    Transparent,
    Scalar(Option<ScalarType>),
    Struct,
    Enum,
    Option,
    Result,
    List,
    Array(usize),
    Map,
    Set,
    Unsupported(&'static str),
}

fn plan(wip: &P) -> Plan {
    match wip.deser_strategy() {
        Some(DeserStrategy::FieldProxy) => Plan::FieldProxy,
        Some(DeserStrategy::ContainerProxy) => Plan::ContainerProxy,
        Some(DeserStrategy::Pointer { .. }) => Plan::Pointer,
        Some(DeserStrategy::TransparentConvert { .. }) => Plan::Transparent,
        Some(DeserStrategy::Scalar { scalar_type, .. }) => Plan::Scalar(*scalar_type),
        Some(DeserStrategy::Struct | DeserStrategy::Tuple { .. }) => Plan::Struct,
        Some(DeserStrategy::Enum) => Plan::Enum,
        Some(DeserStrategy::Option { .. }) => Plan::Option,
        Some(DeserStrategy::Result { .. }) => Plan::Result,
        Some(DeserStrategy::List { .. }) => Plan::List,
        Some(DeserStrategy::Array { len, .. }) => Plan::Array(*len),
        Some(DeserStrategy::Map { .. }) => Plan::Map,
        Some(DeserStrategy::Set { .. }) => Plan::Set,
        Some(DeserStrategy::Opaque | DeserStrategy::OpaquePointer) => Plan::Unsupported("opaque type without a proxy"),
        Some(DeserStrategy::DynamicValue) => Plan::Unsupported("dynamic value type"),
        Some(DeserStrategy::MetadataContainer) => Plan::Unsupported("metadata container"),
        _ => Plan::Unsupported("type"),
    }
}

fn fill(wip: P, v: Value) -> R {
    fill_as(wip, v, false)
}

/// Fill the current frame. `key` is set for map keys, where numeric and
/// boolean targets also accept their string form (as in JSON objects).
fn fill_as(wip: P, v: Value, key: bool) -> R {
    let shape = wip.shape();
    let plan = plan(&wip);
    if let Plan::FieldProxy = plan {
        return fill_as(wip.begin_custom_deserialization()?, v, key)?.end().map_err(Into::into);
    }
    if is_nova_kind(shape) {
        return nova_kind(wip, v);
    }
    if shape.builder_shape.is_some() {
        return fill_as(wip.begin_inner()?, v, key)?.end().map_err(Into::into);
    }
    match plan {
        Plan::FieldProxy => unreachable!(),
        Plan::ContainerProxy => {
            let (wip, _) = wip.begin_custom_deserialization_from_shape()?;
            Ok(fill_as(wip, v, key)?.end()?)
        }
        Plan::Pointer => pointer(wip, v),
        Plan::Transparent => Ok(fill_as(wip.begin_inner()?, v, key)?.end()?),
        Plan::Scalar(st) => scalar(wip, st, v, key),
        Plan::Struct => structure(wip, v),
        Plan::Enum => enumeration(wip, v),
        Plan::Option => match v {
            Value::Null => Ok(wip.set_default()?),
            v => Ok(fill_as(wip.begin_some()?, v, key)?.end()?),
        },
        Plan::Result => result(wip, v),
        Plan::List => list(wip, v),
        Plan::Array(n) => array(wip, n, v),
        Plan::Map => map(wip, v),
        Plan::Set => set(wip, v),
        Plan::Unsupported(what) => Err(Error::new(format!("cannot deserialize {shape}: unsupported {what}"))),
    }
}

/// How serde describes an unexpected value.
fn unexpected(v: &Value) -> String {
    match v {
        Value::Null => "unit value".into(),
        Value::Bool(b) => format!("boolean `{b}`"),
        Value::Int(i) => format!("integer `{i}`"),
        Value::Float(f) => format!("floating point `{f}`"),
        Value::String(s) => format!("string {s:?}"),
        Value::Bytes(_) => "byte array".into(),
        other => format!("a {}", other.kind()),
    }
}

fn invalid(v: &Value, expected: impl std::fmt::Display) -> Error {
    Error::new(format!("invalid type: {}, expected {expected}", unexpected(v)))
}

// ---------------------------------------------------------------- NOVA kinds

fn is_nova_kind(shape: &Shape) -> bool {
    [
        Value::SHAPE.id,
        Timestamp::SHAPE.id,
        Duration::SHAPE.id,
        Decimal::SHAPE.id,
        Rational::SHAPE.id,
        std::time::Duration::SHAPE.id,
    ]
    .contains(&shape.id)
}

/// Rich kinds accept themselves or, the type being the schema, the string
/// spellings that schema resolution (§6.3) would accept.
fn nova_kind(wip: P, v: Value) -> R {
    let id = wip.shape().id;
    let parsed = |what: &str, v: &Value| invalid(v, what);
    Ok(if id == Value::SHAPE.id {
        wip.set(v)?
    } else if id == Timestamp::SHAPE.id {
        wip.set(match v {
            Value::Timestamp(t) => t,
            Value::String(s) => Timestamp::parse(&s).map_err(Error::new)?,
            v => return Err(parsed("a timestamp", &v)),
        })?
    } else if id == Duration::SHAPE.id {
        wip.set(duration(v)?)?
    } else if id == Decimal::SHAPE.id {
        wip.set(match v {
            Value::Decimal(d) => d,
            Value::Int(i) => Decimal::from_int(i),
            Value::Float(f) => Decimal::parse(&format!("{f:?}")).map_err(Error::new)?,
            Value::String(s) => Decimal::parse(&s).map_err(Error::new)?,
            v => return Err(parsed("a decimal", &v)),
        })?
    } else if id == Rational::SHAPE.id {
        wip.set(match v {
            Value::Rational(r) => r,
            Value::Int(i) => Rational::new(i, 1).map_err(Error::new)?,
            Value::String(s) => Rational::parse(&s).map_err(Error::new)?,
            v => return Err(parsed("a rational", &v)),
        })?
    } else {
        let n = duration(v)?.as_nanos();
        let n = u128::try_from(n).map_err(|_| Error::new("negative duration cannot be a std::time::Duration"))?;
        wip.set(std::time::Duration::new((n / 1_000_000_000) as u64, (n % 1_000_000_000) as u32))?
    })
}

fn duration(v: Value) -> Result<Duration, Error> {
    match v {
        Value::Duration(d) => Ok(d),
        Value::String(s) => Duration::parse(&s).map_err(Error::new),
        v => Err(invalid(&v, "a duration")),
    }
}

// ------------------------------------------------------------------- scalars

fn int<T>(wip: P, i: i128, name: &str) -> R
where
    T: TryFrom<i128> + Facet<'static>,
{
    match T::try_from(i) {
        Ok(x) => Ok(wip.set(x)?),
        Err(_) => Err(Error::new(format!("invalid value: integer `{i}`, expected {name}"))),
    }
}

fn is_integer(st: ScalarType) -> bool {
    use ScalarType::*;
    matches!(st, U8 | U16 | U32 | U64 | U128 | USize | I8 | I16 | I32 | I64 | I128 | ISize)
}

fn scalar(wip: P, st: Option<ScalarType>, v: Value, key: bool) -> R {
    use ScalarType as S;
    let shape = wip.shape();
    // Map keys: numbers and booleans also accept their string form.
    let v = match (key, st, v) {
        (true, Some(st), Value::String(s)) if is_integer(st) => s.parse().map(Value::Int).unwrap_or(Value::String(s)),
        (true, Some(S::F32 | S::F64), Value::String(s)) => s.parse().map(Value::Float).unwrap_or(Value::String(s)),
        (true, Some(S::Bool), Value::String(s)) => s.parse().map(Value::Bool).unwrap_or(Value::String(s)),
        (_, _, v) => v,
    };
    Ok(match (st, v) {
        (Some(S::Unit), Value::Null) => wip.set(())?,
        (Some(S::Bool), Value::Bool(b)) => wip.set(b)?,
        (Some(S::Char), Value::String(s)) if s.chars().count() == 1 => wip.set(s.chars().next().unwrap())?,
        (Some(S::String), Value::String(s)) => wip.set(s)?,
        (Some(S::CowStr), Value::String(s)) => wip.set(Cow::<'static, str>::Owned(s))?,
        (Some(S::Str), _) => return Err(Error::new("cannot deserialize a borrowed &str from an owned value; use String")),
        (Some(S::F32), Value::Float(f)) => wip.set(f as f32)?,
        (Some(S::F32), Value::Int(i)) => wip.set(i as f32)?,
        (Some(S::F64), Value::Float(f)) => wip.set(f)?,
        (Some(S::F64), Value::Int(i)) => wip.set(i as f64)?,
        (Some(S::U8), Value::Int(i)) => int::<u8>(wip, i, "u8")?,
        (Some(S::U16), Value::Int(i)) => int::<u16>(wip, i, "u16")?,
        (Some(S::U32), Value::Int(i)) => int::<u32>(wip, i, "u32")?,
        (Some(S::U64), Value::Int(i)) => int::<u64>(wip, i, "u64")?,
        (Some(S::U128), Value::Int(i)) => int::<u128>(wip, i, "u128")?,
        (Some(S::USize), Value::Int(i)) => int::<usize>(wip, i, "usize")?,
        (Some(S::I8), Value::Int(i)) => int::<i8>(wip, i, "i8")?,
        (Some(S::I16), Value::Int(i)) => int::<i16>(wip, i, "i16")?,
        (Some(S::I32), Value::Int(i)) => int::<i32>(wip, i, "i32")?,
        (Some(S::I64), Value::Int(i)) => int::<i64>(wip, i, "i64")?,
        (Some(S::I128), Value::Int(i)) => wip.set(i)?,
        (Some(S::ISize), Value::Int(i)) => int::<isize>(wip, i, "isize")?,
        // Other scalars (addresses, …) parse from their text form.
        (_, Value::String(s)) if shape.vtable.has_parse() && !matches!(shape.ty, Type::Primitive(_)) => wip.parse_from_str(&s)?,
        (_, v) => return Err(invalid(&v, shape)),
    })
}

// ----------------------------------------------------------- smart pointers

fn pointer(wip: P, v: Value) -> R {
    let shape = wip.shape();
    let Def::Pointer(pd) = shape.def else {
        return Err(Error::new(format!("{shape} is not a pointer")));
    };
    match pd.known {
        Some(KnownPointer::Cow) => {
            if pd.pointee().is_some_and(|p| p.id == str::SHAPE.id) {
                return match v {
                    Value::String(s) => Ok(wip.set(Cow::<'static, str>::Owned(s))?),
                    v => Err(invalid(&v, "a string")),
                };
            }
            Ok(fill(wip.begin_inner()?, v)?.end()?)
        }
        Some(KnownPointer::SharedReference | KnownPointer::ExclusiveReference) => {
            Err(Error::new(format!("cannot deserialize a borrowed reference ({shape}) from an owned value")))
        }
        _ => {
            let wip = wip.begin_smart_ptr()?;
            let wip = if wip.is_building_smart_ptr_slice() {
                items(wip, v, P::begin_list_item)?
            } else {
                fill(wip, v)?
            };
            Ok(wip.end()?)
        }
    }
}

// --------------------------------------------------------------- containers

/// The items of a sequence-like value; bytes count as a sequence of integers.
fn seq(v: Value, expected: &Shape) -> Result<Vec<Value>, Error> {
    match v {
        Value::List(l) | Value::Tuple(l) | Value::Set(l) => Ok(l),
        Value::Bytes(b) => Ok(b.into_iter().map(|x| Value::Int(x.into())).collect()),
        v => Err(invalid(&v, expected)),
    }
}

fn items(mut wip: P, v: Value, begin: fn(P) -> Result<P, facet_reflect::ReflectError>) -> R {
    for (i, item) in seq(v, wip.shape())?.into_iter().enumerate() {
        wip = fill(begin(wip)?, item).map_err(|e| e.at_index(i))?.end()?;
    }
    Ok(wip)
}

fn list(wip: P, v: Value) -> R {
    if let Value::Bytes(b) = v {
        if wip.shape().id == <Vec<u8>>::SHAPE.id {
            return Ok(wip.set(b)?);
        }
        return items(wip.init_list()?, Value::Bytes(b), P::begin_list_item);
    }
    let n = match &v {
        Value::List(l) | Value::Tuple(l) | Value::Set(l) => l.len(),
        _ => 0,
    };
    items(wip.init_list_with_capacity(n)?, v, P::begin_list_item)
}

fn set(wip: P, v: Value) -> R {
    items(wip.init_set()?, v, P::begin_set_item)
}

fn array(wip: P, n: usize, v: Value) -> R {
    let l = seq(v, wip.shape())?;
    if l.len() != n {
        return Err(Error::new(format!("invalid length {}, expected an array of length {n}", l.len())));
    }
    let mut wip = wip.init_array()?;
    for (i, item) in l.into_iter().enumerate() {
        wip = fill(wip.begin_nth_field(i)?, item).map_err(|e| e.at_index(i))?.end()?;
    }
    Ok(wip)
}

fn map(wip: P, v: Value) -> R {
    let entries: Vec<(Value, Value)> = match v {
        Value::Record(r) => r.into_entries().into_iter().map(|(k, v)| (Value::String(k), v)).collect(),
        Value::Map(m) => m,
        v => return Err(invalid(&v, wip.shape())),
    };
    let mut wip = wip.init_map()?;
    for (k, v) in entries {
        let seg = match &k {
            Value::String(s) => novafile_text::key_text(s),
            other => format!("[{}]", novafile_text::flow_string(other)),
        };
        wip = fill_as(wip.begin_key()?, k, true)?.end()?;
        wip = fill(wip.begin_value()?, v).map_err(|e| e.at(seg))?.end()?;
    }
    Ok(wip)
}

fn result(wip: P, v: Value) -> R {
    let (tag, payload) = external_tag(v, wip.shape())?;
    let wip = match tag.as_str() {
        "Ok" => wip.begin_ok()?,
        "Err" => wip.begin_err()?,
        _ => return Err(Error::new(format!("unknown variant `{tag}`, expected `Ok` or `Err`"))),
    };
    Ok(fill(wip, payload.unwrap_or(Value::Null)).map_err(|e| e.at(tag))?.end()?)
}

// ------------------------------------------------------------------ structs

fn structure(wip: P, v: Value) -> R {
    let shape = wip.shape();
    let Type::User(UserType::Struct(st)) = shape.ty else {
        return Err(Error::new(format!("cannot deserialize {shape}: not a struct")));
    };
    match st.kind {
        StructKind::Unit => match v {
            Value::Null => Ok(wip),
            v => Err(invalid(&v, shape)),
        },
        StructKind::Struct => match v {
            Value::Record(r) => fields(wip, st.fields, r, shape.has_deny_unknown_fields_attr()),
            v => Err(invalid(&v, format_args!("struct {shape}"))),
        },
        // A newtype struct is its content (as with serde).
        StructKind::TupleStruct if st.fields.len() == 1 => Ok(fill(wip.begin_nth_field(0)?, v)?.end()?),
        StructKind::Tuple | StructKind::TupleStruct => {
            let l = match v {
                Value::Null if st.fields.is_empty() => Vec::new(),
                Value::Tuple(l) | Value::List(l) => l,
                v => return Err(invalid(&v, shape)),
            };
            positional(wip, st.fields.len(), l)
        }
    }
}

fn positional(mut wip: P, n: usize, l: Vec<Value>) -> R {
    if l.len() != n {
        return Err(Error::new(format!("invalid length {}, expected a tuple of size {n}", l.len())));
    }
    for (i, item) in l.into_iter().enumerate() {
        wip = fill(wip.begin_nth_field(i)?, item).map_err(|e| e.at_index(i))?.end()?;
    }
    Ok(wip)
}

fn matches(f: &Field, k: &str) -> bool {
    f.effective_name() == k || f.alias == Some(k)
}

/// Whether a key belongs to a (possibly flattened) struct shape.
fn knows(shape: &Shape, k: &str) -> bool {
    match shape.ty {
        Type::User(UserType::Struct(st)) if st.kind == StructKind::Struct => st.fields.iter().any(|f| {
            if f.is_flattened() {
                knows(f.shape(), k)
            } else {
                !f.should_skip_deserializing() && matches(f, k)
            }
        }),
        _ => match shape.def {
            Def::Option(o) => knows(o.t, k),
            _ => false,
        },
    }
}

/// Fill the fields of a struct or struct variant from a record, honoring
/// `rename`/`rename_all`, `alias`, `skip`, `default`, `flatten` and
/// `deny_unknown_fields`.
fn fields(mut wip: P, fields: &'static [Field], r: Record, deny: bool) -> R {
    let mut rest = Vec::new();
    for (k, v) in r.into_entries() {
        match fields
            .iter()
            .position(|f| !f.is_flattened() && !f.should_skip_deserializing() && matches(f, &k))
        {
            Some(i) => wip = fill(wip.begin_nth_field(i)?, v).map_err(|e| e.at_field(&k))?.end()?,
            None => rest.push((k, v)),
        }
    }
    // Flattened structs take the keys they know; a flattened map takes the rest.
    let flat: Vec<usize> = (0..fields.len()).filter(|&i| fields[i].is_flattened()).collect();
    let mut map_field = None;
    for &i in &flat {
        let shape = fields[i].shape();
        if matches!(shape.def, Def::Map(_)) {
            map_field = Some(i);
            continue;
        }
        let (mine, others): (Vec<_>, Vec<_>) = rest.into_iter().partition(|(k, _)| knows(shape, k));
        rest = others;
        if mine.is_empty() && matches!(shape.def, Def::Option(_)) {
            continue;
        }
        let rec = Record::from_entries(mine).map_err(|e| Error::new(e.to_string()))?;
        wip = fill(wip.begin_nth_field(i)?, Value::Record(rec))?.end()?;
    }
    if let Some(i) = map_field {
        let rec = Record::from_entries(std::mem::take(&mut rest)).map_err(|e| Error::new(e.to_string()))?;
        wip = fill(wip.begin_nth_field(i)?, Value::Record(rec))?.end()?;
    }
    if deny {
        if let Some((k, _)) = rest.first() {
            let expected: Vec<String> = fields
                .iter()
                .filter(|f| !f.should_skip_deserializing())
                .map(|f| format!("`{}`", f.effective_name()))
                .collect();
            return Err(Error::new(format!("unknown field `{k}`, expected one of {}", expected.join(", "))));
        }
    }
    // Missing fields without a default are an error here rather than at build time.
    let required: Vec<bool> = match wip.field_plans() {
        Some(plans) => plans.iter().map(|p| p.is_required()).collect(),
        None => vec![false; fields.len()],
    };
    for (i, f) in fields.iter().enumerate() {
        if required.get(i).copied().unwrap_or(false) && !wip.is_field_set(i)? {
            return Err(Error::new(format!("missing field `{}`", f.effective_name())));
        }
    }
    Ok(wip)
}

// -------------------------------------------------------------------- enums

/// The tag and payload of an externally tagged value: a variant, a string
/// naming a unit variant, or a JSON-style single-entry record `{tag: payload}`.
fn external_tag(v: Value, shape: &Shape) -> Result<(String, Option<Value>), Error> {
    match v {
        Value::Variant(tag, payload) => Ok((tag, payload.map(|p| *p))),
        Value::String(tag) => Ok((tag, None)),
        Value::Record(r) if r.len() == 1 => {
            let (tag, p) = r.into_entries().pop().unwrap();
            Ok((tag, Some(p)))
        }
        v => Err(invalid(&v, format_args!("enum {shape}"))),
    }
}

fn select(wip: P, variants: &'static [Variant], tag: &str) -> Result<(P, &'static Variant), Error> {
    let idx = variants
        .iter()
        .position(|v| v.effective_name() == tag)
        .or_else(|| variants.iter().position(|v| v.is_other() && v.data.kind == StructKind::Unit))
        .ok_or_else(|| {
            let names: Vec<String> = variants.iter().map(|v| format!("`{}`", v.effective_name())).collect();
            Error::new(format!("unknown variant `{tag}`, expected one of {}", names.join(", ")))
        })?;
    Ok((wip.select_nth_variant(idx)?, &variants[idx]))
}

fn enumeration(wip: P, v: Value) -> R {
    let shape = wip.shape();
    let Type::User(UserType::Enum(et)) = shape.ty else {
        return Err(Error::new(format!("cannot deserialize {shape}: not an enum")));
    };
    if shape.is_numeric() {
        return match v {
            Value::Int(i) => {
                let d = i64::try_from(i).map_err(|_| Error::new(format!("invalid value: integer `{i}`, expected enum {shape}")))?;
                Ok(wip.select_variant(d)?)
            }
            v => Err(invalid(&v, format_args!("enum {shape}"))),
        };
    }
    if shape.is_untagged() {
        return untagged(wip, et.variants, v);
    }
    match (shape.get_tag_attr(), shape.get_content_attr()) {
        (Some(t), content) => {
            let Value::Record(mut r) = v else {
                return Err(invalid(&v, format_args!("enum {shape}")));
            };
            let tag = match r.remove(t) {
                Some(Value::String(s) | Value::Variant(s, None)) => s,
                Some(other) => return Err(invalid(&other, "a variant tag").at_field(t)),
                None => return Err(Error::new(format!("missing field `{t}`"))),
            };
            let (wip, variant) = select(wip, et.variants, &tag)?;
            let payload = match content {
                Some(c) => r.remove(c),
                None if variant.data.kind == StructKind::Unit => None,
                None => Some(Value::Record(r)),
            };
            payload_into(wip, variant, payload)
        }
        (None, _) => {
            let (tag, payload) = external_tag(v, shape)?;
            let (wip, variant) = select(wip, et.variants, &tag)?;
            payload_into(wip, variant, payload)
        }
    }
}

/// Fill the selected variant from its payload. Errors are reported under the tag.
fn payload_into(wip: P, variant: &'static Variant, payload: Option<Value>) -> R {
    let tag = variant.effective_name();
    let fields = variant.data.fields;
    match variant.data.kind {
        StructKind::Unit => match payload {
            None | Some(Value::Null) => Ok(wip),
            Some(_) => Err(Error::new(format!("variant {tag} takes no payload"))),
        },
        StructKind::Tuple | StructKind::TupleStruct if fields.len() == 1 => Ok(fill(wip.begin_nth_field(0)?, payload.unwrap_or(Value::Null))
            .map_err(|e| e.at(tag.into()))?
            .end()?),
        StructKind::Tuple | StructKind::TupleStruct => match payload {
            Some(Value::Tuple(l) | Value::List(l)) => positional(wip, fields.len(), l).map_err(|e| e.at(tag.into())),
            _ => Err(Error::new(format!("variant {tag} expects a tuple payload"))),
        },
        StructKind::Struct => match payload {
            Some(Value::Record(r)) => crate::de::fields(wip, fields, r, false).map_err(|e| e.at(tag.into())),
            _ => Err(Error::new(format!("variant {tag} expects a record payload"))),
        },
    }
}

/// Untagged enums take the first variant whose payload the value fits (as
/// with serde). Each candidate is tried on a scratch value of the same type.
fn untagged(wip: P, variants: &'static [Variant], v: Value) -> R {
    let shape = wip.shape();
    for (i, variant) in variants.iter().enumerate() {
        let payload = match (&variant.data.kind, &v) {
            (StructKind::Unit, Value::Null) => None,
            (StructKind::Unit, _) => continue,
            _ => Some(v.clone()),
        };
        // SAFETY: `shape` is the shape of the enum this `Partial` is building,
        // i.e. the `SHAPE` of a real Rust type implementing `Facet`.
        let scratch = unsafe { Partial::alloc_shape_owned(shape) };
        let fits = scratch
            .map_err(Error::from)
            .and_then(|p| Ok(p.select_nth_variant(i)?))
            .and_then(|p| payload_into(p, variant, payload.clone()))
            .and_then(|p| Ok(p.build()?))
            .is_ok();
        if fits {
            return payload_into(wip.select_nth_variant(i)?, variant, payload);
        }
    }
    Err(Error::new(format!("data did not match any variant of untagged enum {shape}")))
}

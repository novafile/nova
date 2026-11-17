//! Classification of NOVA schema types into their Protobuf shapes, and the
//! validated [`Plan`] (reachable definitions, names, imports) shared by the
//! generator, the encoder and the decoder.

use novafile_schema::{Def, FieldDef, IntKind, RecordDef, Schema, TypeKind, TypeRef, VariantDef};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Largest legal Protobuf field number (2^29 - 1).
pub(crate) const MAX_FIELD_NUMBER: u32 = 536_870_911;
/// Field numbers reserved by the Protobuf implementation.
pub(crate) const RESERVED_FIELD_NUMBERS: std::ops::RangeInclusive<u32> = 19_000..=19_999;

pub(crate) const TIMESTAMP_PROTO: &str = "google/protobuf/timestamp.proto";
pub(crate) const DURATION_PROTO: &str = "google/protobuf/duration.proto";
pub(crate) const EMPTY_PROTO: &str = "google/protobuf/empty.proto";

/// A Protobuf scalar (or enum) field type. Values are encoded without a
/// nested message.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Scalar<'s> {
    Bool,
    /// `sint32`, for `i8`/`i16`/`i32`.
    SInt32,
    /// `sint64`, for `i64`/`int`.
    SInt64,
    /// `uint32`, for `u8`/`u16`/`u32`.
    UInt32,
    /// `uint64`, for `u64`.
    UInt64,
    /// `double`, for `float`.
    Double,
    String,
    Bytes,
    /// A NOVA decimal carried as its canonical text in a `string`.
    Decimal,
    /// A NOVA rational carried as `n/d` in a `string`.
    Rational,
    /// A NOVA enum whose variants carry no payload: a Protobuf `enum`.
    Enum(&'s str, &'s [VariantDef]),
}

/// The wire types of the Protobuf encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WireType {
    Varint = 0,
    I64 = 1,
    Len = 2,
    I32 = 5,
}

impl Scalar<'_> {
    pub(crate) fn wire_type(self) -> WireType {
        match self {
            Scalar::Bool | Scalar::SInt32 | Scalar::SInt64 | Scalar::UInt32 | Scalar::UInt64 | Scalar::Enum(..) => WireType::Varint,
            Scalar::Double => WireType::I64,
            Scalar::String | Scalar::Bytes | Scalar::Decimal | Scalar::Rational => WireType::Len,
        }
    }

    /// Numeric scalars are packed when repeated.
    pub(crate) fn packable(self) -> bool {
        self.wire_type() != WireType::Len
    }

    fn proto_name(self) -> String {
        match self {
            Scalar::Bool => "bool".into(),
            Scalar::SInt32 => "sint32".into(),
            Scalar::SInt64 => "sint64".into(),
            Scalar::UInt32 => "uint32".into(),
            Scalar::UInt64 => "uint64".into(),
            Scalar::Double => "double".into(),
            Scalar::String | Scalar::Decimal | Scalar::Rational => "string".into(),
            Scalar::Bytes => "bytes".into(),
            Scalar::Enum(name, _) => ident(name),
        }
    }
}

/// A Protobuf message field type.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Message<'s> {
    /// A NOVA record: a message with one field per record field.
    Record(&'s str, &'s RecordDef),
    /// A NOVA enum with at least one payload variant: a message holding a `oneof`.
    Union(&'s str, &'s [VariantDef]),
    /// `google.protobuf.Timestamp`.
    Timestamp,
    /// `google.protobuf.Duration`.
    Duration,
    /// `google.protobuf.Empty`, for unit variants inside a `oneof`.
    Empty,
}

impl Message<'_> {
    fn proto_name(self) -> String {
        match self {
            Message::Record(name, _) | Message::Union(name, _) => ident(name),
            Message::Timestamp => "google.protobuf.Timestamp".into(),
            Message::Duration => "google.protobuf.Duration".into(),
            Message::Empty => "google.protobuf.Empty".into(),
        }
    }
}

/// A single (non-repeated) value.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Single<'s> {
    Scalar(Scalar<'s>),
    Message(Message<'s>),
}

impl Single<'_> {
    pub(crate) fn proto_name(self) -> String {
        match self {
            Single::Scalar(s) => s.proto_name(),
            Single::Message(m) => m.proto_name(),
        }
    }
}

/// The Protobuf shape of a NOVA type.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Shape<'s> {
    Single(Single<'s>),
    /// `repeated T`; `set` records whether the NOVA type is a `set<T>`.
    Repeated {
        elem: Single<'s>,
        set: bool,
    },
    /// `map<K, V>`.
    Map {
        key: Scalar<'s>,
        value: Single<'s>,
    },
}

/// A classified type: its shape and whether `null` is allowed (anywhere on
/// the alias chain).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Classified<'s> {
    pub shape: Shape<'s>,
    pub nullable: bool,
}

/// Map a NOVA type to its Protobuf shape. `at` names the location for errors.
pub(crate) fn classify<'s>(schema: &'s Schema, ty: &'s TypeRef, at: &dyn Fn() -> String) -> Result<Classified<'s>, String> {
    let unsupported = |what: &str| Err(format!("{}: {what} has no Protobuf mapping", at()));
    let mut cur = ty;
    let mut nullable = false;
    for _ in 0..64 {
        nullable |= cur.nullable;
        let single = |s| {
            Ok(Classified {
                shape: Shape::Single(s),
                nullable,
            })
        };
        let scalar = |s| single(Single::Scalar(s));
        return match &cur.kind {
            TypeKind::Named(name) => match schema.def(name) {
                Some(Def::Alias(target)) => {
                    cur = target;
                    continue;
                }
                Some(Def::Record(r)) => single(Single::Message(Message::Record(name, r))),
                Some(Def::Enum(vs)) if vs.iter().all(|v| v.payload.is_none()) => scalar(Scalar::Enum(name, vs)),
                Some(Def::Enum(vs)) => single(Single::Message(Message::Union(name, vs))),
                None => Err(format!("{}: unknown type {name}", at())),
            },
            TypeKind::Bool => scalar(Scalar::Bool),
            TypeKind::Int(IntKind::I8 | IntKind::I16 | IntKind::I32) => scalar(Scalar::SInt32),
            TypeKind::Int(IntKind::I64 | IntKind::Int) => scalar(Scalar::SInt64),
            TypeKind::Int(IntKind::U8 | IntKind::U16 | IntKind::U32) => scalar(Scalar::UInt32),
            TypeKind::Int(IntKind::U64) => scalar(Scalar::UInt64),
            TypeKind::Float => scalar(Scalar::Double),
            TypeKind::Decimal => scalar(Scalar::Decimal),
            TypeKind::Rational => scalar(Scalar::Rational),
            TypeKind::String => scalar(Scalar::String),
            TypeKind::Bytes => scalar(Scalar::Bytes),
            TypeKind::Timestamp => single(Single::Message(Message::Timestamp)),
            TypeKind::Duration => single(Single::Message(Message::Duration)),
            TypeKind::List(elem) | TypeKind::Set(elem) => {
                let set = matches!(cur.kind, TypeKind::Set(_));
                let e = classify(schema, elem, at)?;
                match e.shape {
                    _ if e.nullable => unsupported("a list or set with nullable elements"),
                    Shape::Single(elem) => Ok(Classified {
                        shape: Shape::Repeated { elem, set },
                        nullable,
                    }),
                    _ => unsupported("a list or set of lists, sets or maps"),
                }
            }
            TypeKind::Map(k, v) => {
                let key = classify(schema, k, at)?;
                let key = match key.shape {
                    Shape::Single(Single::Scalar(s @ (Scalar::String | Scalar::SInt32 | Scalar::SInt64 | Scalar::UInt32 | Scalar::UInt64)))
                        if !key.nullable =>
                    {
                        s
                    }
                    _ => return unsupported(&format!("a map with {k} keys (only string and integer keys are)")),
                };
                let val = classify(schema, v, at)?;
                match val.shape {
                    _ if val.nullable => unsupported("a map with nullable values"),
                    Shape::Single(value) => Ok(Classified {
                        shape: Shape::Map { key, value },
                        nullable,
                    }),
                    _ => unsupported("a map whose values are lists, sets or maps"),
                }
            }
            TypeKind::Tuple(_) => unsupported("a tuple"),
            TypeKind::Any => unsupported("`any`"),
            TypeKind::Null => unsupported("`null`"),
        };
    }
    Err(format!("{}: alias chain is too deep", at()))
}

/// Classify a record field, reporting errors as `Record.field`.
pub(crate) fn classify_field<'s>(schema: &'s Schema, record: &str, f: &'s FieldDef) -> Result<Classified<'s>, String> {
    classify(schema, &f.ty, &|| format!("{record}.{}", f.name))
}

/// Classify an enum variant payload, which must be a single non-nullable value
/// (it becomes a `oneof` member).
pub(crate) fn classify_payload<'s>(schema: &'s Schema, name: &str, v: &'s VariantDef) -> Result<Single<'s>, String> {
    let Some(ty) = &v.payload else {
        return Ok(Single::Message(Message::Empty));
    };
    let at = || format!("{name}.{}", v.name);
    let c = classify(schema, ty, &at)?;
    match c.shape {
        _ if c.nullable => Err(format!("{}: a nullable variant payload has no Protobuf mapping", at())),
        Shape::Single(s) => Ok(s),
        _ => Err(format!(
            "{}: a list, set or map variant payload has no Protobuf mapping (oneof members cannot be repeated)",
            at()
        )),
    }
}

/// The field number of a record field, validated against the Protobuf rules.
pub(crate) fn field_number(record: &str, f: &FieldDef) -> Result<u32, String> {
    let id =
        f.id.ok_or_else(|| format!("{record}.{}: field has no @id(n); Protobuf needs it as the field number", f.name))?;
    if id == 0 || id > MAX_FIELD_NUMBER {
        return Err(format!(
            "{record}.{}: @id({id}) is not a valid Protobuf field number (1..={MAX_FIELD_NUMBER})",
            f.name
        ));
    }
    if RESERVED_FIELD_NUMBERS.contains(&id) {
        return Err(format!("{record}.{}: @id({id}) is in the range 19000..=19999 reserved by Protobuf", f.name));
    }
    Ok(id)
}

/// A NOVA name as a Protobuf identifier (`-` is not allowed in Protobuf).
pub(crate) fn ident(name: &str) -> String {
    name.replace('-', "_")
}

/// `HttpMethod` / `not-found` / `notFound` → `HTTP_METHOD` / `NOT_FOUND` / `NOT_FOUND`.
pub(crate) fn screaming_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    let mut prev_lower = false;
    for c in name.chars() {
        if c == '-' || c == '_' {
            out.push('_');
            prev_lower = false;
        } else {
            if c.is_ascii_uppercase() && prev_lower {
                out.push('_');
            }
            out.push(c.to_ascii_uppercase());
            prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        }
    }
    out
}

/// The Protobuf enum value name of a unit variant.
pub(crate) fn enum_value_name(enum_name: &str, variant: &str) -> String {
    format!("{}_{}", screaming_snake(enum_name), screaming_snake(variant))
}

/// The validated mapping of a schema: everything reachable from `@root`.
pub(crate) struct Plan<'s> {
    /// The root message.
    pub root: Message<'s>,
    /// Reachable record and enum definitions, in schema order.
    pub defs: Vec<(&'s str, &'s Def)>,
    /// Well-known-type imports needed by the `.proto` file.
    pub imports: BTreeSet<&'static str>,
}

impl<'s> Plan<'s> {
    /// Walk every definition reachable from `@root`, checking that each one
    /// has a Protobuf mapping.
    pub(crate) fn new(schema: &'s Schema) -> Result<Plan<'s>, String> {
        let root = root_message(schema)?;

        let mut reachable = HashSet::new();
        let mut imports = BTreeSet::new();
        let mut queue = vec![match root {
            Message::Record(n, _) | Message::Union(n, _) => n,
            _ => unreachable!("root is a record or union"),
        }];
        let visit = |single: Single<'s>, queue: &mut Vec<&'s str>, imports: &mut BTreeSet<&'static str>| match single {
            Single::Scalar(Scalar::Enum(n, _)) | Single::Message(Message::Record(n, _) | Message::Union(n, _)) => queue.push(n),
            Single::Message(Message::Timestamp) => {
                imports.insert(TIMESTAMP_PROTO);
            }
            Single::Message(Message::Duration) => {
                imports.insert(DURATION_PROTO);
            }
            Single::Message(Message::Empty) => {
                imports.insert(EMPTY_PROTO);
            }
            Single::Scalar(_) => {}
        };
        while let Some(name) = queue.pop() {
            if !reachable.insert(name) {
                continue;
            }
            match schema.def(name) {
                Some(Def::Record(r)) => {
                    let mut numbers = HashMap::new();
                    let mut names = HashMap::new();
                    for f in &r.fields {
                        let n = field_number(name, f)?;
                        if let Some(other) = numbers.insert(n, &f.name) {
                            return Err(format!("{name}.{}: @id({n}) is also used by field {other}", f.name));
                        }
                        if let Some(other) = names.insert(ident(&f.name), &f.name) {
                            return Err(format!("{name}.{}: Protobuf field name collides with field {other}", f.name));
                        }
                        let c = classify_field(schema, name, f)?;
                        if c.nullable && !matches!(c.shape, Shape::Single(_)) {
                            return Err(format!("{name}.{}: a nullable list, set or map has no Protobuf mapping", f.name));
                        }
                        match c.shape {
                            Shape::Single(s) | Shape::Repeated { elem: s, .. } => visit(s, &mut queue, &mut imports),
                            Shape::Map { value, .. } => visit(value, &mut queue, &mut imports),
                        }
                    }
                }
                Some(Def::Enum(vs)) => {
                    let mut names = HashMap::new();
                    for v in vs.iter() {
                        if let Some(other) = names.insert(ident(&v.name), &v.name) {
                            return Err(format!("{name}.{}: Protobuf name collides with variant {other}", v.name));
                        }
                    }
                    if vs.iter().any(|v| v.payload.is_some()) {
                        for v in vs.iter() {
                            visit(classify_payload(schema, name, v)?, &mut queue, &mut imports);
                        }
                    }
                }
                // Aliases are always followed by `classify`.
                _ => {}
            }
        }

        let defs: Vec<_> = schema
            .defs
            .iter()
            .filter(|(n, _)| reachable.contains(n.as_str()))
            .map(|(n, d)| (n.as_str(), d))
            .collect();
        check_package_names(&defs)?;
        Ok(Plan { root, defs, imports })
    }
}

/// Classify the `@root` definition, which must become a message.
fn root_message(schema: &Schema) -> Result<Message<'_>, String> {
    let root = schema.root.as_deref().ok_or("schema has no @root; Protobuf needs a root message")?;
    let at = || format!("@root {root}");
    let not_a_message = || format!("{}: must be a record (or an enum with payloads) to become a Protobuf message", at());
    let (name, def) = schema.defs.iter().find(|(n, _)| n == root).ok_or_else(|| format!("{}: not defined", at()))?;
    let single = match def {
        Def::Record(r) => Single::Message(Message::Record(name, r)),
        Def::Enum(vs) if vs.iter().any(|v| v.payload.is_some()) => Single::Message(Message::Union(name, vs)),
        Def::Enum(vs) => Single::Scalar(Scalar::Enum(name, vs)),
        Def::Alias(t) => match classify(schema, t, &at)? {
            Classified {
                shape: Shape::Single(s),
                nullable: false,
            } => s,
            _ => return Err(not_a_message()),
        },
    };
    match single {
        Single::Message(m @ (Message::Record(..) | Message::Union(..))) => Ok(m),
        _ => Err(not_a_message()),
    }
}

/// Messages, enums and enum values share the package namespace; reject
/// collisions (possible after `-` → `_` and SCREAMING_SNAKE conversion).
fn check_package_names(defs: &[(&str, &Def)]) -> Result<(), String> {
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut claim = |proto: String, what: String| match seen.get(&proto) {
        Some(other) => Err(format!("{what}: Protobuf name {proto} collides with {other}")),
        None => {
            seen.insert(proto, what);
            Ok(())
        }
    };
    for (name, def) in defs {
        claim(ident(name), (*name).to_owned())?;
        if let Def::Enum(vs) = def
            && vs.iter().all(|v| v.payload.is_none())
        {
            claim(enum_value_name(name, "unspecified"), format!("{name} (zero value)"))?;
            for v in vs {
                claim(enum_value_name(name, &v.name), format!("{name}.{}", v.name))?;
            }
        }
    }
    Ok(())
}

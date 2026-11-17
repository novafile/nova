use nova_value::Value;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntKind {
    Int,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

impl IntKind {
    pub fn bounds(self) -> (i128, i128) {
        match self {
            IntKind::Int => (i128::MIN, i128::MAX),
            IntKind::I8 => (i8::MIN as i128, i8::MAX as i128),
            IntKind::I16 => (i16::MIN as i128, i16::MAX as i128),
            IntKind::I32 => (i32::MIN as i128, i32::MAX as i128),
            IntKind::I64 => (i64::MIN as i128, i64::MAX as i128),
            IntKind::U8 => (0, u8::MAX as i128),
            IntKind::U16 => (0, u16::MAX as i128),
            IntKind::U32 => (0, u32::MAX as i128),
            IntKind::U64 => (0, u64::MAX as i128),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            IntKind::Int => "int",
            IntKind::I8 => "i8",
            IntKind::I16 => "i16",
            IntKind::I32 => "i32",
            IntKind::I64 => "i64",
            IntKind::U8 => "u8",
            IntKind::U16 => "u16",
            IntKind::U32 => "u32",
            IntKind::U64 => "u64",
        }
    }

    pub fn from_name(n: &str) -> Option<Self> {
        Some(match n {
            "int" => IntKind::Int,
            "i8" => IntKind::I8,
            "i16" => IntKind::I16,
            "i32" => IntKind::I32,
            "i64" => IntKind::I64,
            "u8" => IntKind::U8,
            "u16" => IntKind::U16,
            "u32" => IntKind::U32,
            "u64" => IntKind::U64,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeKind {
    Any,
    Null,
    Bool,
    Int(IntKind),
    Float,
    Decimal,
    Rational,
    String,
    Bytes,
    Timestamp,
    Duration,
    List(Box<TypeRef>),
    Set(Box<TypeRef>),
    Map(Box<TypeRef>, Box<TypeRef>),
    Tuple(Vec<TypeRef>),
    Named(String),
}

/// A numeric bound in `@range`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Num {
    Int(i128),
    Float(f64),
}

impl Num {
    pub fn as_f64(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::Float(f) => f,
        }
    }

    pub fn of(v: &Value) -> Option<Num> {
        match v {
            Value::Int(i) => Some(Num::Int(*i)),
            Value::Float(f) => Some(Num::Float(*f)),
            Value::Decimal(d) => Some(d.to_i128().map(Num::Int).unwrap_or(Num::Float(d.to_f64()))),
            Value::Rational(r) if r.denominator() == 1 => Some(Num::Int(r.numerator())),
            Value::Rational(r) => Some(Num::Float(r.to_f64())),
            _ => None,
        }
    }

    pub fn le(self, other: Num) -> bool {
        match (self, other) {
            (Num::Int(a), Num::Int(b)) => a <= b,
            (a, b) => a.as_f64() <= b.as_f64(),
        }
    }
}

impl fmt::Display for Num {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Num::Int(i) => write!(f, "{i}"),
            Num::Float(x) => write!(f, "{x:?}"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Constraints {
    pub range: Option<(Option<Num>, Option<Num>)>,
    pub len: Option<(Option<u64>, Option<u64>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeRef {
    pub kind: TypeKind,
    pub nullable: bool,
    pub constraints: Constraints,
}

impl TypeRef {
    pub fn new(kind: TypeKind) -> Self {
        TypeRef {
            kind,
            nullable: false,
            constraints: Constraints::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldDef {
    pub name: String,
    pub ty: TypeRef,
    pub optional: bool,
    pub default: Option<Value>,
    pub key: bool,
    pub id: Option<u32>,
    pub deprecated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordDef {
    pub fields: Vec<FieldDef>,
    pub open: bool,
}

impl RecordDef {
    pub fn field(&self, name: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|f| f.name == name)
    }

    pub fn key_fields(&self) -> Vec<&str> {
        self.fields.iter().filter(|f| f.key).map(|f| f.name.as_str()).collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariantDef {
    pub name: String,
    pub payload: Option<TypeRef>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Def {
    Record(RecordDef),
    Enum(Vec<VariantDef>),
    Alias(TypeRef),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Schema {
    pub name: Option<String>,
    pub root: Option<String>,
    pub defs: Vec<(String, Def)>,
}

impl Schema {
    pub fn def(&self, name: &str) -> Option<&Def> {
        self.defs.iter().find(|(n, _)| n == name).map(|(_, d)| d)
    }

    pub fn root_type(&self) -> Option<TypeRef> {
        self.root.as_ref().map(|r| TypeRef::new(TypeKind::Named(r.clone())))
    }
}

// ----------------------------------------------------------------------
// Canonical rendering (used for fingerprints and `nova schema fmt`)
// ----------------------------------------------------------------------

impl fmt::Display for TypeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            TypeKind::Any => f.write_str("any")?,
            TypeKind::Null => f.write_str("null")?,
            TypeKind::Bool => f.write_str("bool")?,
            TypeKind::Int(k) => f.write_str(k.name())?,
            TypeKind::Float => f.write_str("float")?,
            TypeKind::Decimal => f.write_str("decimal")?,
            TypeKind::Rational => f.write_str("rational")?,
            TypeKind::String => f.write_str("string")?,
            TypeKind::Bytes => f.write_str("bytes")?,
            TypeKind::Timestamp => f.write_str("timestamp")?,
            TypeKind::Duration => f.write_str("duration")?,
            TypeKind::List(t) => write!(f, "list<{t}>")?,
            TypeKind::Set(t) => write!(f, "set<{t}>")?,
            TypeKind::Map(k, v) => write!(f, "map<{k}, {v}>")?,
            TypeKind::Tuple(ts) => {
                f.write_str("(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{t}")?;
                }
                if ts.len() == 1 {
                    f.write_str(",")?;
                }
                f.write_str(")")?;
            }
            TypeKind::Named(n) => f.write_str(n)?,
        }
        if self.nullable {
            f.write_str("?")?;
        }
        let b = |o: Option<String>| o.unwrap_or_default();
        if let Some((lo, hi)) = &self.constraints.range {
            write!(f, " @range({}..{})", b(lo.map(|n| n.to_string())), b(hi.map(|n| n.to_string())))?;
        }
        if let Some((lo, hi)) = &self.constraints.len {
            write!(f, " @len({}..{})", b(lo.map(|n| n.to_string())), b(hi.map(|n| n.to_string())))?;
        }
        Ok(())
    }
}

impl fmt::Display for Schema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(n) = &self.name {
            writeln!(f, "@schema {n}")?;
        }
        if let Some(r) = &self.root {
            writeln!(f, "@root {r}")?;
        }
        let mut defs: Vec<_> = self.defs.iter().collect();
        defs.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, def) in defs {
            writeln!(f)?;
            match def {
                Def::Alias(t) => writeln!(f, "type {name} = {t}")?,
                Def::Enum(vs) => {
                    writeln!(f, "enum {name}:")?;
                    for v in vs {
                        match &v.payload {
                            Some(t) => writeln!(f, "  {}({t})", v.name)?,
                            None => writeln!(f, "  {}", v.name)?,
                        }
                    }
                }
                Def::Record(r) => {
                    writeln!(f, "record {name}{}:", if r.open { " @open" } else { "" })?;
                    for fd in &r.fields {
                        write!(f, "  {}{}: {}", fd.name, if fd.optional { "?" } else { "" }, fd.ty)?;
                        if fd.key {
                            f.write_str(" @key")?;
                        }
                        if let Some(id) = fd.id {
                            write!(f, " @id({id})")?;
                        }
                        if fd.deprecated {
                            f.write_str(" @deprecated")?;
                        }
                        if let Some(d) = &fd.default {
                            write!(f, " = {}", nova_text::flow_string(d))?;
                        }
                        writeln!(f)?;
                    }
                }
            }
        }
        Ok(())
    }
}

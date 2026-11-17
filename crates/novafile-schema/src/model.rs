use novafile_value::Value;
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

/// A numeric bound in `@range`, kept exactly as written: an `int`, a
/// `decimal` (e.g. `0.5`, `1e3`) or a `rational` (e.g. `1/3`). Comparisons
/// are exact across numeric kinds (spec §6.1).
#[derive(Clone, Debug, PartialEq)]
pub struct Num(pub Value);

impl Num {
    /// Parse a bound literal.
    pub fn parse(s: &str) -> Result<Num, String> {
        let s = s.trim();
        if let Ok(i) = s.parse::<i128>() {
            return Ok(Num(Value::Int(i)));
        }
        if s.contains('/') {
            return novafile_value::Rational::parse(s).map(|r| Num(Value::Rational(r)));
        }
        novafile_value::Decimal::parse(s)
            .map(|d| Num(Value::Decimal(d)))
            .map_err(|_| format!("invalid bound {s:?}"))
    }

    /// The value as a bound, if it is numeric (int, float, decimal, rational).
    pub fn of(v: &Value) -> Option<Num> {
        matches!(v, Value::Int(_) | Value::Float(_) | Value::Decimal(_) | Value::Rational(_)).then(|| Num(v.clone()))
    }

    pub fn int(i: i128) -> Num {
        Num(Value::Int(i))
    }

    /// Exact `self <= other`; false if either is NaN.
    pub fn le(&self, other: &Num) -> bool {
        matches!(
            novafile_value::numeric_cmp(&self.0, &other.0),
            Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
        )
    }
}

impl fmt::Display for Num {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x:?}"),
            Value::Decimal(d) => write!(f, "{d}"),
            Value::Rational(r) => write!(f, "{r}"),
            other => write!(f, "{}", other.kind()),
        }
    }
}

/// What `@len` counts for strings (spec §6.1). Bytes and collections always
/// count elements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LenUnit {
    /// Unicode scalar values (code points), the default.
    #[default]
    Scalars,
    /// Bytes of the UTF-8 encoding.
    Utf8,
    /// UTF-16 code units (JavaScript, Java, .NET string length).
    Utf16,
    /// Extended grapheme clusters (user-perceived characters, UAX #29).
    Graphemes,
}

impl LenUnit {
    pub fn name(self) -> &'static str {
        match self {
            LenUnit::Scalars => "scalars",
            LenUnit::Utf8 => "utf8",
            LenUnit::Utf16 => "utf16",
            LenUnit::Graphemes => "graphemes",
        }
    }

    pub fn from_name(s: &str) -> Option<LenUnit> {
        Some(match s {
            "scalars" => LenUnit::Scalars,
            "utf8" => LenUnit::Utf8,
            "utf16" => LenUnit::Utf16,
            "graphemes" => LenUnit::Graphemes,
            _ => return None,
        })
    }

    pub fn measure(self, s: &str) -> usize {
        match self {
            LenUnit::Scalars => s.chars().count(),
            LenUnit::Utf8 => s.len(),
            LenUnit::Utf16 => s.encode_utf16().count(),
            LenUnit::Graphemes => unicode_segmentation::UnicodeSegmentation::graphemes(s, true).count(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Constraints {
    pub range: Option<(Option<Num>, Option<Num>)>,
    pub len: Option<(Option<u64>, Option<u64>)>,
    pub len_unit: LenUnit,
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
#[allow(clippy::large_enum_variant)] // definitions are few; boxing would only add indirection
pub enum Def {
    Record(RecordDef),
    Enum(Vec<VariantDef>),
    Alias(TypeRef),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Schema {
    /// Schema name and SemVer version from `@schema name@X.Y.Z` (spec §6.7).
    /// Anonymous schemas (no `@schema` line) cannot be referenced or evolved.
    pub name: Option<String>,
    pub version: Option<crate::Version>,
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
// Canonical rendering (used for fingerprints and `nvf schema fmt`)
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
            write!(
                f,
                " @range({}..{})",
                b(lo.as_ref().map(|n| n.to_string())),
                b(hi.as_ref().map(|n| n.to_string()))
            )?;
        }
        if let Some((lo, hi)) = &self.constraints.len {
            write!(f, " @len({}..{}", b(lo.map(|n| n.to_string())), b(hi.map(|n| n.to_string())))?;
            if self.constraints.len_unit != LenUnit::Scalars {
                write!(f, ", {}", self.constraints.len_unit.name())?;
            }
            f.write_str(")")?;
        }
        Ok(())
    }
}

impl fmt::Display for Schema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let (Some(n), Some(v)) = (&self.name, &self.version) {
            writeln!(f, "@schema {n}@{v}")?;
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
                            write!(f, " = {}", novafile_text::flow_string(d))?;
                        }
                        writeln!(f)?;
                    }
                }
            }
        }
        Ok(())
    }
}

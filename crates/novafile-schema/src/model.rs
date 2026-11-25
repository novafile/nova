//! The schema model (spec §6.1): type expressions, constraints and named
//! definitions, plus their canonical text rendering.

mod render;

use novafile_value::Value;
use std::fmt;

/// An integer type: `int` (any integer the value model holds) or a
/// fixed-width type that adds range bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntKind {
    /// `int`: no bounds beyond the value model's `i128`.
    Int,
    /// `i8`: signed 8-bit.
    I8,
    /// `i16`: signed 16-bit.
    I16,
    /// `i32`: signed 32-bit.
    I32,
    /// `i64`: signed 64-bit.
    I64,
    /// `u8`: unsigned 8-bit.
    U8,
    /// `u16`: unsigned 16-bit.
    U16,
    /// `u32`: unsigned 32-bit.
    U32,
    /// `u64`: unsigned 64-bit.
    U64,
}

impl IntKind {
    /// Inclusive `(min, max)` of the kind; `int` spans all of `i128`.
    #[must_use]
    pub fn bounds(self) -> (i128, i128) {
        match self {
            IntKind::Int => (i128::MIN, i128::MAX),
            IntKind::I8 => (i128::from(i8::MIN), i128::from(i8::MAX)),
            IntKind::I16 => (i128::from(i16::MIN), i128::from(i16::MAX)),
            IntKind::I32 => (i128::from(i32::MIN), i128::from(i32::MAX)),
            IntKind::I64 => (i128::from(i64::MIN), i128::from(i64::MAX)),
            IntKind::U8 => (0, i128::from(u8::MAX)),
            IntKind::U16 => (0, i128::from(u16::MAX)),
            IntKind::U32 => (0, i128::from(u32::MAX)),
            IntKind::U64 => (0, i128::from(u64::MAX)),
        }
    }

    /// The schema keyword for this kind (`int`, `i8`, …).
    #[must_use]
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

    /// The kind for a schema keyword, or `None` if `n` is not an integer type name.
    #[must_use]
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

/// The shape of a type expression, without nullability or constraints.
#[derive(Clone, Debug, PartialEq)]
pub enum TypeKind {
    /// `any`: accepts every value.
    Any,
    /// `null`: only the null value.
    Null,
    /// `bool`.
    Bool,
    /// An integer type, `int` or fixed-width.
    Int(IntKind),
    /// `float`: IEEE 754 double; integers up to 2^53 in magnitude resolve to it.
    Float,
    /// `decimal`: exact base-10 number; integers, finite floats and decimal strings resolve to it.
    Decimal,
    /// `rational`: exact fraction; integers, decimals and rational strings resolve to it.
    Rational,
    /// `string`.
    String,
    /// `bytes`; a string resolves to it as standard base64.
    Bytes,
    /// `timestamp`; a string in timestamp syntax resolves to it.
    Timestamp,
    /// `duration`; a string in duration syntax resolves to it.
    Duration,
    /// `list<T>` (also written `T[]`).
    List(Box<TypeRef>),
    /// `set<T>`: distinct elements; a list without duplicates resolves to it.
    Set(Box<TypeRef>),
    /// `map<K, V>`; a record resolves to it, with its field names as keys.
    Map(Box<TypeRef>, Box<TypeRef>),
    /// `(A, B, …)`: fixed length, one type per position; `(T,)` is the 1-tuple.
    Tuple(Vec<TypeRef>),
    /// A reference to a record, enum or alias definition by name.
    Named(String),
    /// `"text"`: a string literal type, exactly the string `text` (spec
    /// §6.1). It takes no constraints.
    Literal(String),
    /// `A | B | …`: two or more members, tried in order (spec §6.1, §6.3).
    /// The union itself is never nullable and carries no constraints; its
    /// members do.
    Union(Vec<TypeRef>),
}

/// A numeric bound in `@range`, kept exactly as written: an `int`, a
/// `decimal` (e.g. `0.5`, `1e3`) or a `rational` (e.g. `1/3`). Comparisons
/// are exact across numeric kinds (spec §6.1).
#[derive(Clone, Debug, PartialEq)]
pub struct Num(pub Value);

impl Num {
    /// Parse a bound literal.
    ///
    /// # Errors
    ///
    /// Fails if `s` is not an integer, decimal or rational literal.
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
    #[must_use]
    pub fn of(v: &Value) -> Option<Num> {
        matches!(v, Value::Int(_) | Value::Float(_) | Value::Decimal(_) | Value::Rational(_)).then(|| Num(v.clone()))
    }

    /// An integer bound.
    #[must_use]
    pub fn int(i: i128) -> Num {
        Num(Value::Int(i))
    }

    /// Exact `self <= other`; false if either is NaN.
    #[must_use]
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
    /// The unit's keyword in `@len(lo..hi, unit)`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            LenUnit::Scalars => "scalars",
            LenUnit::Utf8 => "utf8",
            LenUnit::Utf16 => "utf16",
            LenUnit::Graphemes => "graphemes",
        }
    }

    /// The unit for an `@len` keyword, or `None` if `s` names none.
    #[must_use]
    pub fn from_name(s: &str) -> Option<LenUnit> {
        Some(match s {
            "scalars" => LenUnit::Scalars,
            "utf8" => LenUnit::Utf8,
            "utf16" => LenUnit::Utf16,
            "graphemes" => LenUnit::Graphemes,
            _ => return None,
        })
    }

    /// The length of `s` in this unit.
    #[must_use]
    pub fn measure(self, s: &str) -> usize {
        match self {
            LenUnit::Scalars => s.chars().count(),
            LenUnit::Utf8 => s.len(),
            LenUnit::Utf16 => s.encode_utf16().count(),
            LenUnit::Graphemes => unicode_segmentation::UnicodeSegmentation::graphemes(s, true).count(),
        }
    }
}

/// The classification of a value (proposal 0009): which readers may see it
/// in clear. `Secret` is stronger than `Personal`, so `max` picks the class
/// that wins when several apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// `@personal`: data about people. Readers see a keyed pseudonym.
    Personal,
    /// `@secret`: credentials and keys. Readers see only a placeholder.
    Secret,
}

impl Class {
    /// The attribute name without `@`: `personal` or `secret`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Class::Personal => "personal",
            Class::Secret => "secret",
        }
    }

    /// The class for an attribute name, or `None` if `s` names none.
    #[must_use]
    pub fn from_name(s: &str) -> Option<Class> {
        match s {
            "personal" => Some(Class::Personal),
            "secret" => Some(Class::Secret),
            _ => None,
        }
    }
}

impl fmt::Display for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Type-level attributes `@range`, `@len`, `@pattern` (spec §6.1) and the
/// class attributes `@personal` and `@secret` (proposal 0009).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Constraints {
    /// `@range(lo..hi)`: inclusive numeric bounds, either side optional.
    /// Non-numeric values are not checked.
    pub range: Option<(Option<Num>, Option<Num>)>,
    /// `@len(lo..hi)`: inclusive length bounds, either side optional. Applies to
    /// strings, bytes, lists, sets, maps and records.
    pub len: Option<(Option<u64>, Option<u64>)>,
    /// What `@len` counts for strings; other values always count elements.
    pub len_unit: LenUnit,
    /// `@pattern("…")`: an I-Regexp the whole string must match. Only on
    /// `string`, or on a reference to an alias of `string`.
    pub pattern: Option<crate::Pattern>,
    /// `@personal` or `@secret`: the class of every value of this type. It
    /// does not change which values are valid.
    pub class: Option<Class>,
}

impl Constraints {
    /// These constraints without the class: the part that decides validity.
    #[must_use]
    pub fn without_class(&self) -> Constraints {
        Constraints { class: None, ..self.clone() }
    }
}

/// A type expression as used for a field, element, payload or alias: kind,
/// nullability and constraints.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeRef {
    /// The underlying type.
    pub kind: TypeKind,
    /// Written `T?`: `null` is accepted as well.
    pub nullable: bool,
    /// Attributes written after the type.
    pub constraints: Constraints,
}

impl TypeRef {
    /// A non-nullable, unconstrained type of the given kind.
    #[must_use]
    pub fn new(kind: TypeKind) -> Self {
        TypeRef {
            kind,
            nullable: false,
            constraints: Constraints::default(),
        }
    }
}

/// One field of a `record` definition.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldDef {
    /// The field name as written in data.
    pub name: String,
    /// The field's type.
    pub ty: TypeRef,
    /// Written `name?:`: the field may be absent. Unlike a nullable type, this
    /// does not allow `null` when the field is present.
    pub optional: bool,
    /// `= value`: the value of an absent field. [`Schema::resolve`] fills it in;
    /// [`Schema::validate`] only accepts its absence.
    pub default: Option<Value>,
    /// `@key`: part of the identity that must be unique among the records of a
    /// list or set (spec §6.2). Key fields are required and non-null.
    pub key: bool,
    /// `@id(n)`: stable field identity, unique within the record. Evolution
    /// checks match fields by it across renames.
    pub id: Option<u32>,
    /// `@deprecated`: informational only; validation ignores it.
    pub deprecated: bool,
    /// The documentation: the `#` comment lines directly above the field,
    /// without `#`. It is not part of the canonical text or the descriptor.
    pub doc: Option<String>,
}

/// How many fields of a [`RecordRule`] must be present (proposal 0010).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleKind {
    /// `@together(…)`: all of the fields or none of them.
    Together,
    /// `@one_of(…)`: exactly one of the fields.
    OneOf,
    /// `@any_of(…)`: at least one of the fields.
    AnyOf,
}

impl RuleKind {
    /// The attribute name without `@`: `together`, `one_of` or `any_of`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            RuleKind::Together => "together",
            RuleKind::OneOf => "one_of",
            RuleKind::AnyOf => "any_of",
        }
    }

    /// The kind for an attribute name, or `None` if `s` names none.
    #[must_use]
    pub fn from_name(s: &str) -> Option<RuleKind> {
        match s {
            "together" => Some(RuleKind::Together),
            "one_of" => Some(RuleKind::OneOf),
            "any_of" => Some(RuleKind::AnyOf),
            _ => None,
        }
    }

    /// Whether `present` of `total` fields satisfy the rule.
    #[must_use]
    pub fn allows(self, present: usize, total: usize) -> bool {
        match self {
            RuleKind::Together => present == 0 || present == total,
            RuleKind::OneOf => present == 1,
            RuleKind::AnyOf => present >= 1,
        }
    }

    /// What the rule expects, for issue messages: `exactly 1`, ….
    #[must_use]
    pub fn expected(self) -> &'static str {
        match self {
            RuleKind::Together => "all or none",
            RuleKind::OneOf => "exactly 1",
            RuleKind::AnyOf => "at least 1",
        }
    }
}

/// A record rule on the record's head line, such as `@one_of(url, url_file)`
/// (proposal 0010). It counts the named fields that are present and not
/// `null`. Strict validation checks it; partial validation does not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordRule {
    /// How many of the fields must be present.
    pub kind: RuleKind,
    /// The names of two or more distinct optional fields of the record, in
    /// the order written.
    pub fields: Vec<String>,
}

impl fmt::Display for RecordRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}({})", self.kind.name(), self.fields.join(", "))
    }
}

/// A `record` definition.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordDef {
    /// Fields in declaration order, which positional encodings follow.
    pub fields: Vec<FieldDef>,
    /// `@open`: undeclared fields are accepted and kept instead of reported.
    pub open: bool,
    /// `@together`, `@one_of` and `@any_of` in the order written (proposal 0010).
    pub rules: Vec<RecordRule>,
}

impl RecordDef {
    /// The field named `name`, if declared.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// Names of the `@key` fields, in declaration order.
    #[must_use]
    pub fn key_fields(&self) -> Vec<&str> {
        self.fields.iter().filter(|f| f.key).map(|f| f.name.as_str()).collect()
    }
}

/// One variant of an `enum` definition.
#[derive(Clone, Debug, PartialEq)]
pub struct VariantDef {
    /// The variant tag.
    pub name: String,
    /// Payload type of a `Tag(T)` variant; `None` for a bare tag.
    pub payload: Option<TypeRef>,
    /// `@id(n)`: stable variant number (n >= 1), unique within the enum.
    /// Either every variant of an enum has one or none does.
    pub id: Option<u32>,
    /// The documentation: the `#` comment lines directly above the variant,
    /// without `#`. It is not part of the canonical text or the descriptor.
    pub doc: Option<String>,
}

impl VariantDef {
    /// The variant's number, given its 0-based position in the enum: its
    /// `@id`, or `position + 1` without one. Protobuf uses it on the wire.
    #[must_use]
    pub fn number(&self, position: usize) -> u32 {
        #[expect(clippy::cast_possible_truncation, reason = "a schema cannot hold 2^32 variants")]
        self.id.unwrap_or(position as u32 + 1)
    }
}

/// A named definition.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)] // definitions are few; boxing would only add indirection
pub enum Def {
    /// `record Name:` with indented fields.
    Record(RecordDef),
    /// `enum Name:` or `enum Name = a | b`: the variants in declaration order.
    Enum(Vec<VariantDef>),
    /// `type Name = T`.
    Alias(TypeRef),
}

/// An `@import name@version[#fingerprint] as alias` directive (proposal 0008).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    /// The alias that qualifies references: `std` in `std.Uuid`. An
    /// identifier without `-` or `.`, unique within the schema.
    pub alias: String,
    /// The exact version imported. [`Schema::link`] fills in the fingerprint
    /// when the source does not give one.
    pub reference: crate::SchemaRef,
}

/// A parsed schema: optional identity, optional `@root`, and named definitions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Schema {
    /// Schema name and `SemVer` version from `@schema name@X.Y.Z` (spec §6.7).
    /// Anonymous schemas (no `@schema` line) cannot be referenced or evolved.
    pub name: Option<String>,
    /// The version from the `@schema` line; set together with `name`.
    pub version: Option<crate::Version>,
    /// `@root`: the type documents resolve against, if any; usually a named definition.
    pub root: Option<TypeRef>,
    /// Definitions in declaration order; names are unique.
    pub defs: Vec<(String, Def)>,
    /// `@x-…` extension directives as written (without `@`), in order. They
    /// are not part of the canonical text or the descriptor (spec §6.6).
    pub extensions: Vec<String>,
    /// `@import` directives in source order (proposal 0008).
    pub imports: Vec<Import>,
    /// The definitions of imported schemas, named `alias.Name`, with their
    /// references rewritten the same way. [`Schema::link`] fills it in. It is
    /// not part of the canonical text, which keeps only the imports.
    pub imported: Vec<(String, Def)>,
    /// `@personal` or `@secret` on record and enum definitions, imported
    /// ones included (proposal 0009).
    pub def_classes: Vec<(String, Class)>,
    /// The documentation of definitions, imported ones included: the `#`
    /// comment lines directly above them, without `#`. It is not part of the
    /// canonical text or the descriptor.
    pub def_docs: Vec<(String, String)>,
}

impl Schema {
    /// The definition named `name`, if any.
    #[must_use]
    pub fn def(&self, name: &str) -> Option<&Def> {
        self.defs.iter().chain(&self.imported).find(|(n, _)| n == name).map(|(_, d)| d)
    }

    /// The class written on the record or enum definition `name`, if any.
    #[must_use]
    pub fn def_class(&self, name: &str) -> Option<Class> {
        self.def_classes.iter().find(|(n, _)| n == name).map(|(_, c)| *c)
    }

    /// The documentation of the definition `name`, if it has any.
    #[must_use]
    pub fn def_doc(&self, name: &str) -> Option<&str> {
        self.def_docs.iter().find(|(n, _)| n == name).map(|(_, d)| d.as_str())
    }

    /// The `@root` type, if the schema has a root.
    #[must_use]
    pub fn root_type(&self) -> Option<TypeRef> {
        self.root.clone()
    }

    /// The name of the `@root` definition, if the root is a plain named type.
    #[must_use]
    pub fn root_name(&self) -> Option<&str> {
        match &self.root.as_ref()?.kind {
            TypeKind::Named(n) => Some(n),
            _ => None,
        }
    }
}

//! Runtime support for `#[derive(NovaSchema)]` (feature `derive`).
//!
//! ```ignore
//! use novafile_schema::NovaSchema;
//!
//! #[derive(NovaSchema)]
//! #[nova(name = "infrastructure", version = "1.0.0")]
//! struct Infrastructure {
//!     #[nova(id = 1)]
//!     environment: String,
//!     #[nova(id = 5)]
//!     services: Vec<Service>,
//! }
//!
//! let schema = Infrastructure::novafile_schema();   // a novafile_schema::Schema
//! ```
//!
//! The mapping follows the Serde binding (spec §11.3), so values produced
//! by `novafile-serde` resolve against the generated schema. The exception is a
//! `url::Url` (feature `url`) built from text that is not an RFC 3986 URI.

use crate::model::{Def, IntKind, Num, Schema, TypeKind, TypeRef};
use std::collections::HashMap;

/// Collects named definitions while type expressions are generated.
#[derive(Default)]
pub struct SchemaGen {
    /// Completed definitions, in the order they were finished.
    defs: Vec<(String, Def)>,
    /// The Rust item behind each registered name, so two items never share one.
    /// A name is registered before its definition is built, so a recursive use
    /// finds it and is not built again.
    owners: HashMap<String, String>,
    /// The documentation of definitions, from the doc comments of their Rust items.
    docs: Vec<(String, String)>,
}

impl SchemaGen {
    /// Register a named definition built by `build` (once; recursion-safe)
    /// and return a reference to it.
    ///
    /// `owner` names the Rust item behind the definition, for example
    /// [`std::any::type_name`] of the type. A later call with the same `name`
    /// reuses the definition only when it has the same `owner`.
    ///
    /// # Panics
    ///
    /// Panics if `name` is already registered for a different `owner`: two
    /// Rust types would otherwise share one schema definition.
    pub fn named(&mut self, name: &str, owner: &str, build: impl FnOnce(&mut SchemaGen) -> Def) -> TypeRef {
        let r = TypeRef::new(TypeKind::Named(name.to_owned()));
        match self.owners.get(name) {
            Some(o) if o == owner => return r,
            Some(o) => panic!("#[derive(NovaSchema)]: {o} and {owner} both map to the schema name {name}; rename one with #[nova(rename)]"),
            None => {}
        }

        self.owners.insert(name.to_owned(), owner.to_owned());
        let def = build(self);
        self.defs.push((name.to_owned(), def));
        r
    }

    /// Record `doc` as the documentation of the definition `name`.
    pub fn doc(&mut self, name: &str, doc: &str) {
        self.docs.push((name.to_owned(), doc.to_owned()));
    }

    /// Build the schema from the collected definitions, with the given
    /// `@root` and optional `@schema` name and version.
    ///
    /// # Panics
    ///
    /// Panics if `version` is not a valid `SemVer` version (a bad `#[nova(version)]`).
    pub fn into_schema(self, root: Option<TypeRef>, name: Option<&str>, version: Option<&str>) -> Schema {
        Schema {
            name: name.map(str::to_owned),
            version: version.map(|v| crate::Version::parse(v).unwrap_or_else(|e| panic!("#[nova(version)]: {e}"))),
            root,
            defs: self.defs,
            def_docs: self.docs,
            ..Schema::default()
        }
    }
}

/// Types with a NOVA schema. Derive it with `#[derive(NovaSchema)]`.
pub trait NovaSchema {
    /// The type expression for `Self`, registering named definitions in `generator`.
    fn nova_type(generator: &mut SchemaGen) -> TypeRef;

    /// As a struct field, may this type be absent? (`Option<T>`: yes.)
    #[doc(hidden)]
    #[must_use]
    fn nova_field_optional() -> bool {
        false
    }

    /// `(name, version)` from `#[nova(name = …, version = …)]`.
    #[doc(hidden)]
    #[must_use]
    fn nova_schema_id() -> Option<(&'static str, &'static str)> {
        None
    }

    /// The `@import` arguments from `#[nova(import = "name@version as alias")]`.
    #[doc(hidden)]
    #[must_use]
    fn nova_imports() -> &'static [&'static str] {
        &[]
    }

    /// The complete schema with `Self` as `@root`.
    #[must_use]
    fn novafile_schema() -> Schema
    where
        Self: Sized,
    {
        schema_for::<Self>()
    }
}

/// The complete schema for `T`, with `T` as the root type.
///
/// # Panics
///
/// The imports are those of `T`'s `#[nova(import = …)]`, which must name bundled schemas.
/// A reference to `std.Name` without an import under the alias `std` imports the newest
/// bundled standard library as `std`, so `#[nova(type = "std.Uri")]` and the foreign-type
/// impls work without an attribute; `import = "novafile.dev/std@… as std"` pins the version.
///
/// # Panics
///
/// Panics if the schema fails the checks [`crate::parse_schema`] applies: a `#[nova(type)]`
/// naming an unknown type, an alias cycle, a class on a `@key` field, an invalid default,
/// a repeated field name or id, an optional or nullable `@key`, or an enum without variants.
/// Also panics if two different Rust types map to the same definition name, or on a
/// malformed `#[nova(import)]` or one that names a schema that is not bundled.
#[must_use]
pub fn schema_for<T: NovaSchema>() -> Schema {
    let mut generator = SchemaGen::default();
    let ty = T::nova_type(&mut generator);
    let root = if let TypeKind::Named(_) = ty.kind {
        ty
    } else {
        // Non-named roots (e.g. `Vec<Item>`) get a `Root` alias, which keeps their name stable.
        // A user type may already be called `Root` (`Vec<Root>`); then `Root_`, `Root__`, ….
        let mut name = String::from("Root");
        while generator.defs.iter().any(|(n, _)| *n == name) {
            name.push('_');
        }

        generator.defs.push((name.clone(), Def::Alias(ty)));
        TypeRef::new(TypeKind::Named(name))
    };

    let (name, version) = T::nova_schema_id().map_or((None, None), |(n, v)| (Some(n), Some(v)));
    let mut schema = generator.into_schema(Some(root), name, version);
    for text in T::nova_imports() {
        let import = crate::parse::parse_import(text, &schema).unwrap_or_else(|e| panic!("#[nova(import = {text:?})]: {e}"));
        assert!(
            crate::bundled(&import.reference).is_some(),
            "#[nova(import = {text:?})]: a derived schema can only import a bundled schema, such as {}",
            crate::STD_NAME
        );

        schema.imports.push(import);
    }

    if !schema.imports.iter().any(|i| i.alias == STD_ALIAS) && uses_alias(&schema, STD_ALIAS) {
        let std = crate::std_schemas().last().and_then(Schema::reference).expect("a bundled standard library");
        schema.imports.push(crate::model::Import {
            alias: STD_ALIAS.to_owned(),
            reference: std,
        });
    }

    crate::parse::check_defs(&schema)
        .map_err(|message| crate::SchemaError { line: 0, message })
        .and_then(|()| schema.link(&|_| Err(String::new())))
        .unwrap_or_else(|e| panic!("#[derive(NovaSchema)]: {}", e.message));
    schema
}

/// The alias under which derived schemas reach the standard library.
const STD_ALIAS: &str = "std";

/// Whether the root or a definition of `schema` names a type `alias.Name`.
fn uses_alias(schema: &Schema, alias: &str) -> bool {
    /// Whether `t` or a type inside it is a name under `alias`.
    fn names(t: &TypeRef, alias: &str) -> bool {
        match &t.kind {
            TypeKind::Named(n) => n.split_once('.').is_some_and(|(a, _)| a == alias),
            TypeKind::List(e) | TypeKind::Set(e) => names(e, alias),
            TypeKind::Map(k, v) => names(k, alias) || names(v, alias),
            TypeKind::Tuple(ts) | TypeKind::Union(ts) => ts.iter().any(|t| names(t, alias)),
            _ => false,
        }
    }

    schema.root.iter().any(|t| names(t, alias))
        || schema.defs.iter().any(|(_, d)| match d {
            Def::Alias(t) => names(t, alias),
            Def::Enum(vs) => vs.iter().filter_map(|v| v.payload.as_ref()).any(|t| names(t, alias)),
            Def::Record(r) => r.fields.iter().any(|f| names(&f.ty, alias)),
        })
}

/// Apply schema-syntax attributes (`@range(1..10) @len(..64, utf8)`) to a type.
/// Used by the derive macro; panics with a clear message on invalid input.
#[doc(hidden)]
#[must_use]
pub fn __with_attrs(mut ty: TypeRef, attrs: &str, field: &str) -> TypeRef {
    let budget = std::cell::Cell::new(crate::pattern::SCHEMA_BUDGET);
    let mut p = crate::parse::TyParser::new(attrs, &budget);
    let rest = p.attrs(&mut ty).unwrap_or_else(|e| panic!("#[nova] on {field}: {e}"));
    if let Some((a, _)) = rest.first() {
        panic!("#[nova] on {field}: @{a} is not a type attribute");
    }

    ty
}

/// Parse a `#[nova(type = "…")]` type expression; `nullable` for an `Option<…>` field.
/// Used by the derive macro; panics with a clear message on invalid input.
#[doc(hidden)]
#[must_use]
pub fn __type(text: &str, field: &str, nullable: bool) -> TypeRef {
    let mut ty = crate::parse::parse_type(text).unwrap_or_else(|e| panic!("#[nova(type)] on {field}: {e}"));
    ty.nullable |= nullable;
    ty
}

/// Parse a `#[nova(default = "…")]` flow literal.
#[doc(hidden)]
#[must_use]
pub fn __default(text: &str, field: &str) -> novafile_value::Value {
    novafile_text::parse_flow(text).unwrap_or_else(|e| panic!("#[nova(default)] on {field}: {e}"))
}

/// Implements [`NovaSchema`] for types that map to one fixed type kind.
macro_rules! simple {
    ($($t:ty => $k:expr),* $(,)?) => {$(
        impl NovaSchema for $t {
            fn nova_type(_: &mut SchemaGen) -> TypeRef {
                TypeRef::new($k)
            }
        }
    )*};
}

simple! {
    bool => TypeKind::Bool,
    i8 => TypeKind::Int(IntKind::I8), i16 => TypeKind::Int(IntKind::I16), i32 => TypeKind::Int(IntKind::I32), i64 => TypeKind::Int(IntKind::I64),
    u8 => TypeKind::Int(IntKind::U8), u16 => TypeKind::Int(IntKind::U16), u32 => TypeKind::Int(IntKind::U32), u64 => TypeKind::Int(IntKind::U64),
    i128 => TypeKind::Int(IntKind::Int), isize => TypeKind::Int(IntKind::I64), usize => TypeKind::Int(IntKind::U64),
    f32 => TypeKind::Float, f64 => TypeKind::Float,
    String => TypeKind::String, str => TypeKind::String,
    novafile_value::Timestamp => TypeKind::Timestamp, novafile_value::Duration => TypeKind::Duration,
    novafile_value::Decimal => TypeKind::Decimal, novafile_value::Rational => TypeKind::Rational,
    novafile_value::Value => TypeKind::Any,
    () => TypeKind::Null,
    // Serde writes these as strings (human-readable form, which novafile-serde uses).
    std::path::PathBuf => TypeKind::String, std::path::Path => TypeKind::String,
    std::net::IpAddr => TypeKind::String, std::net::Ipv4Addr => TypeKind::String, std::net::Ipv6Addr => TypeKind::String,
    std::net::SocketAddr => TypeKind::String, std::net::SocketAddrV4 => TypeKind::String, std::net::SocketAddrV6 => TypeKind::String,
}

impl NovaSchema for u128 {
    fn nova_type(_: &mut SchemaGen) -> TypeRef {
        let mut t = TypeRef::new(TypeKind::Int(IntKind::Int));
        t.constraints.range = Some((Some(Num::int(0)), None));
        t
    }
}

impl NovaSchema for char {
    fn nova_type(_: &mut SchemaGen) -> TypeRef {
        let mut t = TypeRef::new(TypeKind::String);
        t.constraints.len = Some((Some(1), Some(1)));
        t
    }
}

impl<T: NovaSchema + ?Sized> NovaSchema for &T {
    fn nova_type(g: &mut SchemaGen) -> TypeRef {
        T::nova_type(g)
    }
}

use std::{rc::Rc, sync::Arc};

impl<T: NovaSchema + ?Sized> NovaSchema for Box<T> {
    fn nova_type(g: &mut SchemaGen) -> TypeRef {
        T::nova_type(g)
    }
}

impl<T: NovaSchema + ?Sized> NovaSchema for Rc<T> {
    fn nova_type(g: &mut SchemaGen) -> TypeRef {
        T::nova_type(g)
    }
}

impl<T: NovaSchema + ?Sized> NovaSchema for Arc<T> {
    fn nova_type(g: &mut SchemaGen) -> TypeRef {
        T::nova_type(g)
    }
}

impl<T: NovaSchema> NovaSchema for Option<T> {
    fn nova_type(g: &mut SchemaGen) -> TypeRef {
        let mut t = T::nova_type(g);
        t.nullable = true;
        t
    }

    fn nova_field_optional() -> bool {
        true
    }
}

/// Wrap `t` in a `list<…>` type.
fn list_of(t: TypeRef) -> TypeRef {
    TypeRef::new(TypeKind::List(Box::new(t)))
}

/// Wrap `t` in a `set<…>` type.
fn set_of(t: TypeRef) -> TypeRef {
    TypeRef::new(TypeKind::Set(Box::new(t)))
}

impl<T: NovaSchema> NovaSchema for Vec<T> {
    fn nova_type(g: &mut SchemaGen) -> TypeRef {
        list_of(T::nova_type(g))
    }
}

impl<T: NovaSchema> NovaSchema for [T] {
    fn nova_type(g: &mut SchemaGen) -> TypeRef {
        list_of(T::nova_type(g))
    }
}

impl<T: NovaSchema> NovaSchema for std::collections::VecDeque<T> {
    fn nova_type(g: &mut SchemaGen) -> TypeRef {
        list_of(T::nova_type(g))
    }
}

impl<T: NovaSchema, const N: usize> NovaSchema for [T; N] {
    /// Serde writes arrays as tuples.
    fn nova_type(g: &mut SchemaGen) -> TypeRef {
        let t = T::nova_type(g);
        TypeRef::new(TypeKind::Tuple(vec![t; N]))
    }
}

impl<T: NovaSchema, S> NovaSchema for std::collections::HashSet<T, S> {
    fn nova_type(g: &mut SchemaGen) -> TypeRef {
        set_of(T::nova_type(g))
    }
}

impl<T: NovaSchema> NovaSchema for std::collections::BTreeSet<T> {
    fn nova_type(g: &mut SchemaGen) -> TypeRef {
        set_of(T::nova_type(g))
    }
}

impl<K: NovaSchema, V: NovaSchema, S> NovaSchema for std::collections::HashMap<K, V, S> {
    fn nova_type(g: &mut SchemaGen) -> TypeRef {
        TypeRef::new(TypeKind::Map(Box::new(K::nova_type(g)), Box::new(V::nova_type(g))))
    }
}

impl<K: NovaSchema, V: NovaSchema> NovaSchema for std::collections::BTreeMap<K, V> {
    fn nova_type(g: &mut SchemaGen) -> TypeRef {
        TypeRef::new(TypeKind::Map(Box::new(K::nova_type(g)), Box::new(V::nova_type(g))))
    }
}

/// Implements [`NovaSchema`] for Rust tuples (arity 1 to 8) as NOVA tuple types.
macro_rules! tuples {
    ($(($($n:ident),+)),*) => {$(
        impl<$($n: NovaSchema),+> NovaSchema for ($($n,)+) {
            fn nova_type(g: &mut SchemaGen) -> TypeRef {
                TypeRef::new(TypeKind::Tuple(vec![$($n::nova_type(g)),+]))
            }
        }
    )*};
}

tuples!(
    (A),
    (A, B),
    (A, B, C),
    (A, B, C, D),
    (A, B, C, D, E),
    (A, B, C, D, E, F),
    (A, B, C, D, E, F, G),
    (A, B, C, D, E, F, G, H)
);

/// Implements [`NovaSchema`] for foreign types as standard library types, which
/// [`schema_for`] imports as `std`.
macro_rules! std_types {
    ($($feature:literal: $t:ty => $name:literal),* $(,)?) => {$(
        #[cfg(feature = $feature)]
        impl NovaSchema for $t {
            fn nova_type(_: &mut SchemaGen) -> TypeRef {
                TypeRef::new(TypeKind::Named(format!("{STD_ALIAS}.{}", $name)))
            }
        }
    )*};
}

std_types! {
    // RFC 3986, stricter than the WHATWG URLs of `Url`. A `Url` read from a URI writes a URI
    // back; one built from other text can keep `[ ] | { } ^` and then fails (`type = "string"`).
    "url": url::Url => "Uri",
    // Lower case, hyphenated: what `Uuid` writes. `Uuid` also reads upper case and other
    // forms, which the schema rejects.
    "uuid": uuid::Uuid => "Uuid",
    // SemVer 2.0.0; `semver::Version` reads and writes the same syntax.
    "semver": semver::Version => "SemVer",
}

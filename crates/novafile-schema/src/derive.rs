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
//! by `novafile-serde` resolve against the generated schema.

use crate::model::{Def, IntKind, Num, Schema, TypeKind, TypeRef};
use std::collections::HashSet;

/// Collects named definitions while type expressions are generated.
#[derive(Default)]
pub struct SchemaGen {
    /// Completed definitions, in the order they were finished.
    defs: Vec<(String, Def)>,
    /// Names whose definition is being built; a reference to one of them is a
    /// recursive use and is not built again.
    in_progress: HashSet<String>,
}

impl SchemaGen {
    /// Register a named definition built by `build` (once; recursion-safe)
    /// and return a reference to it.
    pub fn named(&mut self, name: &str, build: impl FnOnce(&mut SchemaGen) -> Def) -> TypeRef {
        let r = TypeRef::new(TypeKind::Named(name.to_owned()));
        if self.in_progress.contains(name) || self.defs.iter().any(|(n, _)| n == name) {
            return r;
        }

        self.in_progress.insert(name.to_owned());
        let def = build(self);
        self.in_progress.remove(name);
        self.defs.push((name.to_owned(), def));
        r
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
    generator.into_schema(Some(root), name, version)
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

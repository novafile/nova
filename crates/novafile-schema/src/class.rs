//! Classified types (proposal 0009): the class a type gives its values, and
//! the positions where a class is not allowed.

use crate::model::{Class, Def, Schema, TypeKind, TypeRef};

/// How many aliases [`Schema::base_kind`] follows before it stops. Alias
/// cycles are rejected by the reference checks; this bound only keeps a
/// lookup on an unchecked schema finite.
const MAX_ALIAS_HOPS: usize = 64;

impl Schema {
    /// The strongest class that a value of type `t` can get from `t` itself:
    /// its own `@personal` or `@secret`, the class of the definition it
    /// names, and the classes along a chain of aliases. For a union, the
    /// strongest class of any member. Classes of fields inside a record do
    /// not count: they apply to the field values, not to the record.
    #[must_use]
    pub fn type_class(&self, t: &TypeRef) -> Option<Class> {
        // Each definition counts once, so aliases repeated across union
        // members cost linear, not exponential, work.
        let mut seen = std::collections::HashSet::new();
        let mut todo = vec![t];
        let mut class = None;
        while let Some(t) = todo.pop() {
            class = class.max(t.constraints.class);
            match &t.kind {
                TypeKind::Union(ms) => todo.extend(ms),
                TypeKind::Named(n) if seen.insert(n.as_str()) => {
                    class = class.max(self.def_class(n));
                    if let Some(Def::Alias(a)) = self.def(n) {
                        todo.push(a);
                    }
                }
                _ => {}
            }
        }

        class
    }

    /// The class that resolving a value against `t` gives it, before any
    /// enclosing value counts: the classes on `t` and on each alias of its
    /// chain, and the class of the record or enum the chain ends at. Union
    /// members count only when the value resolves through them.
    #[must_use]
    pub fn node_class(&self, t: &TypeRef) -> Option<Class> {
        self.alias_chain(t)
            .into_iter()
            .map(|link| {
                let def = match &link.kind {
                    TypeKind::Named(n) => self.def_class(n),
                    _ => None,
                };
                link.constraints.class.max(def)
            })
            .max()
            .flatten()
    }

    /// Whether any type or definition of this schema, imported ones included,
    /// has a class.
    #[must_use]
    pub fn has_classes(&self) -> bool {
        /// Whether `t` or a type argument inside it has a class. The depth is
        /// bounded by the parser's nesting limit.
        fn any(t: &TypeRef) -> bool {
            t.constraints.class.is_some()
                || match &t.kind {
                    TypeKind::List(e) | TypeKind::Set(e) => any(e),
                    TypeKind::Map(k, v) => any(k) || any(v),
                    TypeKind::Tuple(ts) | TypeKind::Union(ts) => ts.iter().any(any),
                    _ => false,
                }
        }

        !self.def_classes.is_empty()
            || self.root.as_ref().is_some_and(any)
            || self.defs.iter().chain(&self.imported).any(|(_, d)| match d {
                Def::Alias(t) => any(t),
                Def::Enum(vs) => vs.iter().filter_map(|v| v.payload.as_ref()).any(any),
                Def::Record(r) => r.fields.iter().any(|f| any(&f.ty)),
            })
    }

    /// The kind that `t` stands for once aliases are followed: `t`'s own kind,
    /// or for a reference to an alias, the kind of the aliased type. A
    /// reference to a record or enum stays [`TypeKind::Named`].
    #[must_use]
    pub fn base_kind<'a>(&'a self, t: &'a TypeRef) -> &'a TypeKind {
        let mut kind = &t.kind;
        for _ in 0..MAX_ALIAS_HOPS {
            let TypeKind::Named(n) = kind else { break };
            let Some(Def::Alias(a)) = self.def(n) else { break };
            kind = &a.kind;
        }

        kind
    }
}

/// Check the positions where a classified type is not allowed (proposal
/// 0009): `@key` fields, map keys and set elements, whose values appear in
/// paths and selectors. Also check that `@pattern` on a reference names an
/// alias of `string`.
pub(crate) fn check_positions(s: &Schema) -> Result<(), String> {
    for (name, d) in &s.defs {
        match d {
            Def::Alias(t) => check_type(s, t).map_err(|m| format!("{name}: {m}"))?,
            Def::Enum(vs) => {
                for v in vs {
                    if let Some(t) = &v.payload {
                        check_type(s, t).map_err(|m| format!("{name}.{}: {m}", v.name))?;
                    }
                }
            }
            Def::Record(r) => {
                for f in &r.fields {
                    let at = |m: String| format!("{name}.{}: {m}", f.name);
                    check_type(s, &f.ty).map_err(at)?;
                    if f.key
                        && let Some(c) = s.type_class(&f.ty)
                    {
                        return Err(at(format!("a @key field cannot have a @{c} type, since keys appear in paths")));
                    }
                }
            }
        }
    }

    match &s.root {
        Some(t) => check_type(s, t).map_err(|m| format!("@root: {m}")),
        None => Ok(()),
    }
}

/// Check one type expression and its type arguments: no classified map key
/// or set element, and `@pattern` only on `string` or an alias of it.
fn check_type(s: &Schema, t: &TypeRef) -> Result<(), String> {
    if t.constraints.pattern.is_some() && *s.base_kind(t) != TypeKind::String {
        return Err(format!("@pattern applies to string, not {}", TypeRef::new(t.kind.clone())));
    }

    match &t.kind {
        TypeKind::Set(e) => {
            if let Some(c) = s.type_class(e) {
                return Err(format!("a set element cannot have a @{c} type, since elements appear in paths"));
            }

            check_type(s, e)
        }
        TypeKind::Map(k, v) => {
            if let Some(c) = s.type_class(k) {
                return Err(format!("a map key cannot have a @{c} type, since keys appear in paths"));
            }

            check_type(s, k).and(check_type(s, v))
        }
        TypeKind::List(e) => check_type(s, e),
        TypeKind::Tuple(ts) | TypeKind::Union(ts) => ts.iter().try_for_each(|t| check_type(s, t)),
        _ => Ok(()),
    }
}

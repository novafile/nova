//! Warnings about union member order (spec §6.5): members an earlier one
//! makes unreachable (`string | timestamp`), and members whose conversions
//! capture values a later member was probably meant for (`timestamp | string`).

use crate::compat::widens;
use crate::model::{Def, Schema, TypeKind, TypeRef};

/// A questionable member order in one union of a schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnionWarning {
    /// The definition holding the union; `None` for `@root`.
    pub def: Option<String>,
    /// The record field or enum variant holding the union, if any.
    pub member: Option<String>,
    /// What is wrong, naming both members.
    pub message: String,
}

impl std::fmt::Display for UnionWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.def, &self.member) {
            (Some(d), Some(m)) => write!(f, "{d}.{m}: {}", self.message),
            (Some(d), None) => write!(f, "{d}: {}", self.message),
            (None, _) => write!(f, "@root: {}", self.message),
        }
    }
}

impl Schema {
    /// Every union in the schema whose member order is probably a mistake
    /// (spec §6.5). Unions are listed in definition order, members as written.
    #[must_use]
    pub fn union_warnings(&self) -> Vec<UnionWarning> {
        let mut out = Vec::new();
        let mut visit = |def: Option<&str>, member: Option<&str>, t: &TypeRef| {
            walk_unions(t, &mut |ms| {
                for message in self.order_problems(ms) {
                    out.push(UnionWarning {
                        def: def.map(str::to_owned),
                        member: member.map(str::to_owned),
                        message,
                    });
                }
            });
        };

        if let Some(r) = &self.root {
            visit(None, None, r);
        }

        for (name, d) in &self.defs {
            match d {
                Def::Alias(t) => visit(Some(name), None, t),
                Def::Record(r) => r.fields.iter().for_each(|f| visit(Some(name), Some(&f.name), &f.ty)),
                Def::Enum(vs) => vs
                    .iter()
                    .filter_map(|v| Some((v, v.payload.as_ref()?)))
                    .for_each(|(v, p)| visit(Some(name), Some(&v.name), p)),
            }
        }

        out
    }

    /// One message per later member that an earlier one shadows or captures
    /// from; each later member is reported at most once.
    fn order_problems(&self, ms: &[TypeRef]) -> Vec<String> {
        let mut out = Vec::new();
        for (j, b) in ms.iter().enumerate().skip(1) {
            for a in &ms[..j] {
                if let Some(m) = self.order_problem(a, b) {
                    out.push(m);
                    break;
                }
            }
        }

        out
    }

    /// Why `a` written before `b` is probably wrong, if it is.
    fn order_problem(&self, a: &TypeRef, b: &TypeRef) -> Option<String> {
        // A literal is one string, so whether it is reachable is decided exactly.
        if let TypeKind::Literal(l) = &self.deref(b).0.kind
            && self.resolve_as(&novafile_value::Value::String(l.clone()), a).is_ok()
        {
            return Some(format!("`{b}` is unreachable: the string resolves to the earlier `{a}`"));
        }

        let (an, bn) = (self.native(a), self.native(b));
        if matches!(self.deref(a).0.kind, TypeKind::Any) || a == b || (an.is_some() && an == bn && widens(b, a)) {
            return Some(format!("`{b}` is unreachable: every value it takes resolves to the earlier `{a}`"));
        }

        // A plain string member takes every string before conversions can.
        if an == Some("string") && !has_constraints(self.deref(a).0) && self.converts(b).contains(&"string") {
            return Some(format!(
                "`{b}` is reached only by typed literals: strings resolve to the earlier `{a}` before they can convert to `{b}`"
            ));
        }

        let k = bn?;
        self.converts(a)
            .contains(&k)
            .then(|| format!("`{a}` captures some {k} values meant for the later `{b}` by converting them; put `{b}` first if that is not intended"))
    }

    /// The kind of value (as [`novafile_value::Value::kind`] names it) that
    /// `t` takes without conversion, if it has one.
    fn native(&self, t: &TypeRef) -> Option<&'static str> {
        let (rt, def) = self.deref(t);
        Some(match (&rt.kind, def) {
            (_, Some(Def::Record(_))) => "record",
            (_, Some(Def::Enum(_))) => "variant",
            (TypeKind::Null, _) => "null",
            (TypeKind::Bool, _) => "bool",
            (TypeKind::Int(_), _) => "int",
            (TypeKind::Float, _) => "float",
            (TypeKind::Decimal, _) => "decimal",
            (TypeKind::Rational, _) => "rational",
            (TypeKind::String, _) => "string",
            (TypeKind::Bytes, _) => "bytes",
            (TypeKind::Timestamp, _) => "timestamp",
            (TypeKind::Duration, _) => "duration",
            (TypeKind::List(_), _) => "list",
            (TypeKind::Set(_), _) => "set",
            (TypeKind::Tuple(_), _) => "tuple",
            (TypeKind::Map(..), _) => "map",
            _ => return None,
        })
    }

    /// The kinds `t` converts from during resolution (spec §6.3).
    fn converts(&self, t: &TypeRef) -> &'static [&'static str] {
        let (rt, def) = self.deref(t);
        match (&rt.kind, def) {
            // Enums also take `{tag: payload}` records, but those rarely overlap a record member.
            (_, Some(Def::Enum(_))) | (TypeKind::Bytes | TypeKind::Timestamp | TypeKind::Duration, _) => &["string"],
            (TypeKind::Float, _) => &["int"],
            (TypeKind::Decimal, _) => &["int", "float", "string", "rational"],
            (TypeKind::Rational, _) => &["int", "decimal", "string"],
            (TypeKind::Set(_) | TypeKind::Tuple(_), _) => &["list"],
            (TypeKind::Map(..), _) => &["record"],
            _ => &[],
        }
    }
}

/// Call `f` with the members of every union written inside `t`.
fn walk_unions(t: &TypeRef, f: &mut impl FnMut(&[TypeRef])) {
    match &t.kind {
        TypeKind::Union(ms) => {
            f(ms);
            for m in ms {
                walk_unions(m, f);
            }
        }
        TypeKind::List(e) | TypeKind::Set(e) => walk_unions(e, f),
        TypeKind::Map(k, v) => {
            walk_unions(k, f);
            walk_unions(v, f);
        }
        TypeKind::Tuple(ts) => {
            for e in ts {
                walk_unions(e, f);
            }
        }
        _ => {}
    }
}

/// Whether `t` narrows its kind with `@range`, `@len` or `@pattern`.
fn has_constraints(t: &TypeRef) -> bool {
    t.constraints.range.is_some() || t.constraints.len.is_some() || t.constraints.pattern.is_some()
}

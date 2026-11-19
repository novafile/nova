//! Schema evolution checks (spec §6.5). "Compatible" means every value valid
//! under the old schema resolves under the new one (new readers, old data).

use crate::model::{Constraints, Def, IntKind, Num, Schema, TypeKind, TypeRef, VariantDef};

/// One difference between two schema versions, as found by [`check_compatibility`].
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// Whether data valid under the old schema may be rejected by the new one.
    pub breaking: bool,
    /// Where the change is: `@root`, a definition name, or `Definition.member`.
    pub at: String,
    /// Human-readable description of the change.
    pub message: String,
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", if self.breaking { "BREAKING" } else { "ok      " }, self.at, self.message)
    }
}

/// The effective numeric range of `t`: its `@range`, narrowed to the bounds
/// of a fixed-width integer kind (`int` adds no bounds).
fn range_of(t: &TypeRef) -> (Option<Num>, Option<Num>) {
    let (mut lo, mut hi) = t.constraints.range.clone().unwrap_or((None, None));
    if let TypeKind::Int(k) = t.kind {
        let (a, b) = k.bounds();
        if k != IntKind::Int {
            let (a, b) = (Num::int(a), Num::int(b));
            lo = Some(match lo {
                Some(l) if a.le(&l) => l,
                _ => a,
            });
            hi = Some(match hi {
                Some(h) if h.le(&b) => h,
                _ => b,
            });
        }
    }

    (lo, hi)
}

/// Does every value of `old` also satisfy `new`, and, through a union, the
/// member at the same position (spec §6.5)? Named types are compared by
/// name; see [`check_compatibility`] for comparing whole schemas.
#[must_use]
pub fn widens(old: &TypeRef, new: &TypeRef) -> bool {
    let none = Schema::default();
    widens_in((&none, &none), old, new, 0)
}

/// [`widens`], expanding aliases to unions through the old and new schema
/// `s` when comparing union positions.
fn widens_in(s: (&Schema, &Schema), old: &TypeRef, new: &TypeRef, depth: usize) -> bool {
    // Mutually recursive aliases under different names could expand forever.
    if depth > 64 {
        return false;
    }

    let widens = |a: &TypeRef, b: &TypeRef| widens_in(s, a, b, depth + 1);
    // Equal types widen; names in them are compared by name, as below.
    if old == new {
        return true;
    }

    if matches!(new.kind, TypeKind::Any) {
        return true;
    }

    // A type that becomes a union counts as its first member. Members keep
    // their positions: only the last one may widen, and more may follow it.
    // `null`, as a member or through `T?`, matches only `null` and does not
    // count for positions; it only has to stay admitted.
    // The same name on both sides is compared by name; its definition is
    // compared once, on its own, which also stops recursive aliases here.
    let same_name = matches!((&old.kind, &new.kind), (TypeKind::Named(a), TypeKind::Named(b)) if a == b);
    let is_union = |sc: &Schema, t: &TypeRef| matches!(sc.deref(t).0.kind, TypeKind::Union(_));
    if !same_name && (is_union(s.0, old) || is_union(s.1, new)) {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        // Unions too large to flatten are treated as breaking.
        let (Some(a_null), Some(b_null)) = (positional_members(s.0, old, &mut a, 0), positional_members(s.1, new, &mut b, 0)) else {
            return false;
        };
        if a_null && !b_null {
            return false;
        }

        let Some(last) = a.len().checked_sub(1) else { return true };
        return b.len() >= a.len() && a[..last] == b[..last] && widens(&a[last], &b[last]);
    }

    // An alias stands for its target. Same names were compared by name above.
    if !same_name {
        let expand = |sc: &Schema, t: &TypeRef| match &t.kind {
            TypeKind::Named(n) if t.constraints == Constraints::default() => match sc.def(n) {
                Some(Def::Alias(a)) => Some(TypeRef {
                    nullable: t.nullable || a.nullable,
                    ..a.clone()
                }),
                _ => None,
            },
            _ => None,
        };

        match (expand(s.0, old), expand(s.1, new)) {
            (None, None) => {}
            (a, b) => return widens(a.as_ref().unwrap_or(old), b.as_ref().unwrap_or(new)),
        }
    }

    if old.nullable && !new.nullable {
        return false;
    }

    // A literal widens to itself, or to a string it satisfies (spec §6.5).
    if let TypeKind::Literal(l) = &old.kind {
        return match &new.kind {
            TypeKind::Literal(m) => l == m,
            TypeKind::String => {
                let c = &new.constraints;
                let len = c.len_unit.measure(l) as u64;
                c.len.is_none_or(|(lo, hi)| lo.is_none_or(|lo| lo <= len) && hi.is_none_or(|hi| len <= hi)) && c.pattern.as_ref().is_none_or(|p| p.is_match(l))
            }
            _ => false,
        };
    }

    let kinds = match (&old.kind, &new.kind) {
        (TypeKind::Int(_), TypeKind::Int(_) | TypeKind::Decimal | TypeKind::Rational) => true,
        (TypeKind::Int(k), TypeKind::Float) => matches!(k, IntKind::I8 | IntKind::I16 | IntKind::I32 | IntKind::U8 | IntKind::U16 | IntKind::U32),
        (TypeKind::List(a), TypeKind::List(b) | TypeKind::Set(b)) | (TypeKind::Set(a), TypeKind::Set(b)) => {
            widens(a, b) && !matches!((&old.kind, &new.kind), (TypeKind::List(_), TypeKind::Set(_)))
        }
        (TypeKind::Map(ak, av), TypeKind::Map(bk, bv)) => widens(ak, bk) && widens(av, bv),
        (TypeKind::Tuple(a), TypeKind::Tuple(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| widens(x, y)),
        (a, b) => a == b,
    };

    if !kinds {
        return false;
    }

    // Numeric ranges must not narrow.
    let (olo, ohi) = range_of(old);
    let (nlo, nhi) = range_of(new);
    let lo_ok = match (olo, nlo) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(o), Some(n)) => n.le(&o),
    };

    let hi_ok = match (ohi, nhi) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(o), Some(n)) => o.le(&n),
    };

    let (olen, nlen) = (old.constraints.len.unwrap_or((None, None)), new.constraints.len.unwrap_or((None, None)));
    let len_ok = nlen.0.is_none_or(|n| olen.0.is_some_and(|o| n <= o)) && nlen.1.is_none_or(|n| olen.1.is_some_and(|o| o <= n));
    // Changing what @len counts is only safe if the new type has no @len.
    let unit_ok = new.constraints.len.is_none() || old.constraints.len.is_none() || old.constraints.len_unit == new.constraints.len_unit;
    // Patterns are compared by text, never by the languages they match (§6.5).
    let pattern_ok = new.constraints.pattern.is_none() || new.constraints.pattern == old.constraints.pattern;
    lo_ok && hi_ok && len_ok && unit_ok && pattern_ok
}

/// Collect into `out` the members of `t` that count for union positions:
/// unions are flattened, also through aliases, `null` members are dropped and
/// the rest made non-nullable. Returns whether `t` admits `null`, or `None`
/// if flattening exceeds [`MAX_MEMBERS`] (aliases repeated across members
/// can multiply the count).
fn positional_members(s: &Schema, t: &TypeRef, out: &mut Vec<TypeRef>, depth: usize) -> Option<bool> {
    if out.len() > MAX_MEMBERS || depth > 64 {
        return None;
    }

    let rt = s.deref(t).0;
    Some(match &rt.kind {
        TypeKind::Union(ms) => ms.iter().try_fold(t.nullable, |n, m| Some(positional_members(s, m, out, depth + 1)? | n))?,
        TypeKind::Null => true,
        _ => {
            out.push(TypeRef { nullable: false, ..t.clone() });
            t.nullable || rt.nullable
        }
    })
}

/// The most members [`positional_members`] flattens a union into.
const MAX_MEMBERS: usize = 1024;

/// List every change from `old` to `new` (spec §6.5), each marked breaking or not.
///
/// Record fields are matched by `@id` first, then by name. Compatibility is
/// judged for new readers of old data, so additions old readers would reject
/// (such as new enum variants) are reported, but not as breaking.
#[must_use]
pub fn check_compatibility(old: &Schema, new: &Schema) -> Vec<Change> {
    let mut out = Vec::new();
    let mut push = |breaking, at: &str, m: String| {
        out.push(Change {
            breaking,
            at: at.to_owned(),
            message: m,
        });
    };

    if old.root != new.root {
        push(
            true,
            "@root",
            format!("root changed from {} to {}", shown(old.root.as_ref()), shown(new.root.as_ref())),
        );
    }

    for (name, od) in &old.defs {
        let Some(nd) = new.def(name) else {
            push(true, name, "definition removed".into());
            continue;
        };

        match (od, nd) {
            (Def::Alias(a), Def::Alias(b)) => {
                if !widens_in((old, new), a, b, 0) {
                    push(true, name, format!("type {a} narrowed or changed to {b}"));
                }
            }
            (Def::Enum(a), Def::Enum(b)) => enum_changes((old, new), name, a, b, &mut push),
            (Def::Record(a), Def::Record(b)) => {
                if a.open && !b.open {
                    push(true, name, "record closed (@open removed)".into());
                }

                for f in &a.fields {
                    let at = format!("{name}.{}", f.name);
                    let g = b.fields.iter().find(|g| f.id.is_some() && g.id == f.id).or_else(|| b.field(&f.name));
                    let Some(g) = g else {
                        if b.open {
                            push(false, &at, "field removed (record is @open, old data still accepted)".into());
                        } else {
                            push(true, &at, "field removed; old data containing it is rejected".into());
                        }

                        continue;
                    };

                    if g.name != f.name {
                        push(
                            true,
                            &at,
                            format!("renamed to {} (same @id; compatible for positional/binary encodings only)", g.name),
                        );
                    }

                    if !widens_in((old, new), &f.ty, &g.ty, 0) {
                        push(true, &at, format!("type {} changed incompatibly to {}", f.ty, g.ty));
                    }

                    if f.id.is_some() && g.id != f.id {
                        let id = g.id.map_or_else(|| "removed".into(), |n| format!("changed to {n}"));
                        push(true, &at, format!("@id {id} (Protobuf uses it as the field number)"));
                    }

                    if (f.optional || f.default.is_some()) && !g.optional && g.default.is_none() {
                        push(true, &at, "field may no longer be absent (optional or default removed)".into());
                    }

                    if f.key != g.key {
                        push(g.key, &at, if g.key { "@key added".into() } else { "@key removed".into() });
                    }
                }

                for g in &b.fields {
                    let existed = a.fields.iter().any(|f| f.name == g.name || (g.id.is_some() && f.id == g.id));
                    if !existed {
                        let at = format!("{name}.{}", g.name);
                        if g.key {
                            // Old elements all get the same default key value (§6.5: add @key is breaking).
                            push(true, &at, "@key field added; existing list elements may now collide".into());
                        } else if g.optional || g.default.is_some() {
                            push(false, &at, "field added".into());
                        } else {
                            push(true, &at, "required field added without a default".into());
                        }
                    }
                }
            }
            _ => push(true, name, "definition kind changed".into()),
        }
    }

    for (name, _) in &new.defs {
        if old.def(name).is_none() {
            push(false, name, "definition added".into());
        }
    }

    out
}

/// Changes between two versions of enum `name`.
///
/// Variants are matched by name. Each variant has a number (its `@id`, or
/// its position without one), which Protobuf puts on the wire, so a kept
/// variant must keep its number and an added one must not take a number
/// that old data uses. Without any `@id` on either side, a removal or an
/// insertion shifts every later variant; that shift is reported once, at the
/// removed or inserted variant, and only variants whose order among the kept
/// ones changed are reported as moved.
fn enum_changes(schemas: (&Schema, &Schema), name: &str, a: &[VariantDef], b: &[VariantDef], push: &mut impl FnMut(bool, &str, String)) {
    const HINT: &str = "Protobuf numbers variants without @id by position; give the variants @id(n) to make reordering safe";
    let ids = a.iter().chain(b).any(|v| v.id.is_some());
    let kept =
        |xs: &[VariantDef], ys: &[VariantDef]| -> Vec<String> { xs.iter().filter(|x| ys.iter().any(|y| y.name == x.name)).map(|x| x.name.clone()).collect() };

    let (kept_a, kept_b) = (kept(a, b), kept(b, a));
    for (i, v) in a.iter().enumerate() {
        let at = format!("{name}.{}", v.name);
        let Some(j) = b.iter().position(|w| w.name == v.name) else {
            let shifts = !ids && a[i + 1..].iter().any(|w| kept_a.contains(&w.name));
            push(
                true,
                &at,
                if shifts {
                    format!("variant removed; later variants shift position ({HINT})")
                } else {
                    "variant removed".into()
                },
            );
            continue;
        };

        let (old_n, new_n) = (v.number(i), b[j].number(j));
        if old_n != new_n {
            if ids {
                push(
                    true,
                    &at,
                    format!("variant number changed from {old_n} to {new_n} (its @id, or its position without one; Protobuf puts it on the wire)"),
                );
            } else if kept_a.iter().position(|n| *n == v.name) != kept_b.iter().position(|n| *n == v.name) {
                push(true, &at, format!("variant moved from position {old_n} to {new_n} ({HINT})"));
            }
        }

        match (&v.payload, &b[j].payload) {
            (None, None) => {}
            (Some(old_p), Some(new_p)) if widens_in(schemas, old_p, new_p, 0) => {}
            _ => push(true, &at, "variant payload changed incompatibly".into()),
        }
    }

    for (j, w) in b.iter().enumerate().filter(|(_, w)| !a.iter().any(|v| v.name == w.name)) {
        let at = format!("{name}.{}", w.name);
        let n = w.number(j);
        match a.iter().enumerate().find(|&(i, v)| v.number(i) == n) {
            Some((_, v)) if ids => push(true, &at, format!("variant added with number {n}, which old variant {} had", v.name)),
            Some((_, v)) => push(
                true,
                &at,
                format!("variant inserted at position {n}, which old variant {} had ({HINT})", v.name),
            ),
            None => push(false, &at, "variant added (old readers will reject it)".into()),
        }
    }
}

/// A root type for messages: the type expression, or `none`.
fn shown(t: Option<&crate::TypeRef>) -> String {
    t.map_or_else(|| "none".into(), ToString::to_string)
}

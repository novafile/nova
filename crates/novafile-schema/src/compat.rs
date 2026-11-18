//! Schema evolution checks (spec §6.5). "Compatible" means every value valid
//! under the old schema resolves under the new one (new readers, old data).

use crate::model::{Def, IntKind, Num, Schema, TypeKind, TypeRef};

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

/// Does every value of `old` also satisfy `new`?
#[must_use]
pub fn widens(old: &TypeRef, new: &TypeRef) -> bool {
    if matches!(new.kind, TypeKind::Any) {
        return true;
    }

    if old.nullable && !new.nullable {
        return false;
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
    lo_ok && hi_ok && len_ok && unit_ok
}

/// List every change from `old` to `new` (spec §6.5), each marked breaking or not.
///
/// Record fields are matched by `@id` first, then by name. Compatibility is
/// judged for new readers of old data, so additions old readers would reject
/// (such as new enum variants) are reported, but not as breaking.
#[must_use]
#[expect(clippy::too_many_lines, reason = "one arm per definition kind, each a flat list of rules")]
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
        push(true, "@root", format!("root changed from {:?} to {:?}", old.root, new.root));
    }

    for (name, od) in &old.defs {
        let Some(nd) = new.def(name) else {
            push(true, name, "definition removed".into());
            continue;
        };

        match (od, nd) {
            (Def::Alias(a), Def::Alias(b)) => {
                if !widens(a, b) {
                    push(true, name, format!("type {a} narrowed or changed to {b}"));
                }
            }
            (Def::Enum(a), Def::Enum(b)) => {
                for (i, v) in a.iter().enumerate() {
                    let at = format!("{name}.{}", v.name);
                    let Some(j) = b.iter().position(|w| w.name == v.name) else {
                        push(true, &at, "variant removed".into());
                        continue;
                    };

                    if i != j {
                        push(
                            true,
                            &at,
                            format!("variant moved from position {} to {} (Protobuf numbers variants by position)", i + 1, j + 1),
                        );
                    }

                    match (&v.payload, &b[j].payload) {
                        (None, None) => {}
                        (Some(old_p), Some(new_p)) if widens(old_p, new_p) => {}
                        _ => push(true, &at, "variant payload changed incompatibly".into()),
                    }
                }

                for w in b.iter().filter(|w| !a.iter().any(|v| v.name == w.name)) {
                    push(false, &format!("{name}.{}", w.name), "variant added (old readers will reject it)".into());
                }
            }
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

                    if !widens(&f.ty, &g.ty) {
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
                        if g.optional || g.default.is_some() {
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

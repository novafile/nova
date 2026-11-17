//! Schema evolution checks (spec §6.5). "Compatible" means every value valid
//! under the old schema resolves under the new one (new readers, old data).

use crate::model::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub breaking: bool,
    pub at: String,
    pub message: String,
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", if self.breaking { "BREAKING" } else { "ok      " }, self.at, self.message)
    }
}

fn range_of(t: &TypeRef) -> (Option<Num>, Option<Num>) {
    let (mut lo, mut hi) = t.constraints.range.unwrap_or((None, None));
    if let TypeKind::Int(k) = t.kind {
        let (a, b) = k.bounds();
        if k != IntKind::Int {
            lo = Some(lo.map_or(Num::Int(a), |l| if Num::Int(a).le(l) { l } else { Num::Int(a) }));
            hi = Some(hi.map_or(Num::Int(b), |h| if h.le(Num::Int(b)) { h } else { Num::Int(b) }));
        }
    }
    (lo, hi)
}

/// Does every value of `old` also satisfy `new`?
pub fn widens(old: &TypeRef, new: &TypeRef) -> bool {
    if matches!(new.kind, TypeKind::Any) {
        return true;
    }
    if old.nullable && !new.nullable {
        return false;
    }
    let kinds = match (&old.kind, &new.kind) {
        (TypeKind::Int(_), TypeKind::Int(_)) => true,
        (TypeKind::Int(_), TypeKind::Decimal | TypeKind::Rational) => true,
        (TypeKind::Decimal, TypeKind::Rational) => true,
        (TypeKind::Int(k), TypeKind::Float) => matches!(k, IntKind::I8 | IntKind::I16 | IntKind::I32 | IntKind::U8 | IntKind::U16 | IntKind::U32),
        (TypeKind::List(a), TypeKind::List(b)) | (TypeKind::Set(a), TypeKind::Set(b)) | (TypeKind::List(a), TypeKind::Set(b)) => {
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
        (Some(o), Some(n)) => n.le(o),
    };
    let hi_ok = match (ohi, nhi) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(o), Some(n)) => o.le(n),
    };
    let (olen, nlen) = (old.constraints.len.unwrap_or((None, None)), new.constraints.len.unwrap_or((None, None)));
    let len_ok = (nlen.0.is_none() || olen.0.is_some_and(|o| nlen.0.unwrap() <= o)) && (nlen.1.is_none() || olen.1.is_some_and(|o| o <= nlen.1.unwrap()));
    lo_ok && hi_ok && len_ok
}

pub fn check_compatibility(old: &Schema, new: &Schema) -> Vec<Change> {
    let mut out = Vec::new();
    let mut push = |breaking, at: &str, m: String| {
        out.push(Change {
            breaking,
            at: at.to_owned(),
            message: m,
        })
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
                for v in a {
                    match b.iter().find(|w| w.name == v.name) {
                        None => push(true, &format!("{name}.{}", v.name), "variant removed".into()),
                        Some(w) => match (&v.payload, &w.payload) {
                            (None, None) => {}
                            (Some(x), Some(y)) if widens(x, y) => {}
                            _ => push(true, &format!("{name}.{}", v.name), "variant payload changed incompatibly".into()),
                        },
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
                    if f.optional && !g.optional && g.default.is_none() {
                        push(true, &at, "optional field became required".into());
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

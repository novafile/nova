//! Structural diff that produces a minimal-ish, conditional patch.

use crate::patch::{Op, OpKind, Patch};
use nova_text::{Path, Segment};
use nova_value::{revision, Value};

/// Produce a patch that turns `a` into `b`. Every change is conditional on
/// the old value, and the patch is pinned to `a`'s revision.
pub fn diff(a: &Value, b: &Value) -> Patch {
    let mut ops = Vec::new();
    walk(a, b, &mut Vec::new(), &mut ops);
    Patch {
        schema: None,
        base: Some(revision(a).to_string()),
        scope: Path::root(),
        ops,
    }
}

/// If every element is a record with a unique scalar `id`/`key`/`name`, use it as the identity.
fn identity_field(a: &[Value], b: &[Value]) -> Option<&'static str> {
    ["id", "key", "name"].into_iter().find(|f| {
        [a, b].iter().all(|l| {
            let mut seen = std::collections::HashSet::new();
            l.iter().all(|v| {
                v.as_record()
                    .and_then(|r| r.get(f))
                    .is_some_and(|k| !k.is_container() && seen.insert(k.clone()))
            })
        })
    })
}

/// Keyed diffing only expresses removals and appends, so `b` must list the
/// surviving elements in `a`'s order, followed by the new ones.
fn order_preserving(a: &[Value], b: &[Value], f: &str) -> bool {
    let key = |v: &Value| v.as_record().unwrap().get(f).unwrap().clone();
    let ka: Vec<Value> = a.iter().map(key).collect();
    let kb: Vec<Value> = b.iter().map(key).collect();
    let mut expected: Vec<Value> = ka.iter().filter(|k| kb.contains(k)).cloned().collect();
    expected.extend(kb.iter().filter(|k| !ka.contains(k)).cloned());
    expected == kb
}

fn walk(a: &Value, b: &Value, path: &mut Vec<Segment>, ops: &mut Vec<Op>) {
    if a == b {
        return;
    }
    let op = |path: &Vec<Segment>, kind| Op {
        path: Path(path.clone()),
        kind,
        line: 0,
    };
    match (a, b) {
        (Value::Record(ra), Value::Record(rb)) => {
            for (k, va) in ra.iter() {
                path.push(Segment::Field(k.clone()));
                match rb.get(k) {
                    Some(vb) => walk(va, vb, path, ops),
                    None => ops.push(op(path, OpKind::Remove(Some(va.clone())))),
                }
                path.pop();
            }
            for (k, vb) in rb.iter() {
                if !ra.contains_key(k) {
                    path.push(Segment::Field(k.clone()));
                    ops.push(op(path, OpKind::Add(vb.clone())));
                    path.pop();
                }
            }
        }
        (Value::List(la), Value::List(lb)) => {
            if let Some(f) = identity_field(la, lb).filter(|f| order_preserving(la, lb, f)) {
                let key = |v: &Value| v.as_record().unwrap().get(f).unwrap().clone();
                for va in la {
                    let k = key(va);
                    path.push(Segment::Match(f.to_owned(), k.clone()));
                    match lb.iter().find(|vb| key(vb) == k) {
                        Some(vb) => walk(va, vb, path, ops),
                        None => ops.push(op(path, OpKind::Remove(None))),
                    }
                    path.pop();
                }
                for vb in lb {
                    if !la.iter().any(|va| key(va) == key(vb)) {
                        ops.push(op(path, OpKind::Add(vb.clone())));
                    }
                }
            } else if la.len() == lb.len() {
                for (i, (va, vb)) in la.iter().zip(lb).enumerate() {
                    path.push(Segment::Index(i as i64));
                    walk(va, vb, path, ops);
                    path.pop();
                }
            } else {
                ops.push(op(
                    path,
                    OpKind::Replace {
                        old: a.clone(),
                        new: b.clone(),
                    },
                ));
            }
        }
        _ => {
            if path.is_empty() {
                ops.push(op(path, OpKind::Set(b.clone())));
            } else {
                ops.push(op(
                    path,
                    OpKind::Replace {
                        old: a.clone(),
                        new: b.clone(),
                    },
                ));
            }
        }
    }
}

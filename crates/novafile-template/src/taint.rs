//! Taint tracking for classified values (proposal 0009.7).
//!
//! A value that comes from an imported document through a `@personal` or
//! `@secret` type is classified, and so is every value that the template
//! derives from it. The evaluator must know this to redact the output.
//!
//! The taint travels *in band*: a classified value is wrapped in a variant
//! whose tag is [`TAG`] followed by the class name, for example
//! `@classified:secret`. The parser cannot make a tag that starts with `@`,
//! so no template or data document can make or remove a wrapper. We chose
//! this over a shadow map of classified paths or `(Value, Option<Class>)`
//! pairs for these reasons:
//!
//! - A wrapper moves with its value through `@let`, references, spreads,
//!   loop variables and nested imports. No extra bookkeeping is necessary.
//! - A construct that does not know about wrappers fails closed. It sees a
//!   variant where it expects a string, list or bool and reports an error,
//!   or it copies the wrapper into the output, where [`strip`] finds it.
//! - The evaluator changes only where it looks inside values: references,
//!   interpolation, conditions, loops, spreads, record keys, set elements
//!   and map keys.
//!
//! Wrappers exist only for classes that the reader mode does not show in
//! every position (see [`shows`]). Thus every wrapper is a value to hide,
//! and the guards in the evaluator do not need to know the mode. At the end,
//! [`strip`] removes the wrappers and returns their output paths, and
//! [`novafile_schema::redact`] puts the placeholders in. That function also
//! applies `Reveal::Paths` to the output paths and makes pseudonyms.

use crate::{TemplateError, err};
use novafile_schema::{Class, ClassMark, Reveal};
use novafile_text::{Path, Segment};
use novafile_value::{Record, Value};

/// Tag prefix of a wrapper; the class name follows it.
const TAG: &str = "@classified:";

/// Maximum nesting depth that [`class_in`] and [`strip`] go down into. Chains
/// of `@let` can nest values more deeply than the parser allows.
const MAX_DEPTH: usize = 1024;

/// Whether the mode `reveal` shows values of class `class` in every output
/// position. Such values need no wrapper. `Reveal::Paths` shows a value only
/// at some output paths, so it shows no class in every position.
pub(crate) fn shows(reveal: &Reveal, class: Class) -> bool {
    matches!((reveal, class), (Reveal::All, _) | (Reveal::Personal, Class::Personal))
}

/// `v` marked with `class`. If `v` already has a wrapper, the stronger class
/// applies.
pub(crate) fn taint(v: Value, class: Class) -> Value {
    let (v, own) = untaint(v);
    let class = own.map_or(class, |c| c.max(class));
    Value::Variant(format!("{TAG}{}", class.name()), Some(Box::new(v)))
}

/// `v` marked with `class` if there is a class.
pub(crate) fn taint_opt(v: Value, class: Option<Class>) -> Value {
    match class {
        Some(c) => taint(v, c),
        None => v,
    }
}

/// The class of the wrapper of `v` and the wrapped value, or `None` if `v`
/// has no wrapper at the top.
pub(crate) fn tainted(v: &Value) -> Option<(Class, &Value)> {
    match v {
        Value::Variant(t, Some(inner)) => Class::from_name(t.strip_prefix(TAG)?).map(|c| (c, &**inner)),
        _ => None,
    }
}

/// `v` without the wrappers at its top, and the strongest class they had.
pub(crate) fn untaint(mut v: Value) -> (Value, Option<Class>) {
    let mut class = None;
    while let Some((c, _)) = tainted(&v) {
        class = class.max(Some(c));
        let Value::Variant(_, Some(inner)) = v else {
            unreachable!("tainted checked the shape")
        };
        v = *inner;
    }

    (v, class)
}

/// The strongest class of any wrapper in `v`, at any depth. A value that
/// nests too deeply to check counts as secret.
pub(crate) fn class_in(v: &Value) -> Option<Class> {
    class_below(v, 0)
}

/// [`class_in`] for a value `depth` levels down.
fn class_below(v: &Value, depth: usize) -> Option<Class> {
    if depth > MAX_DEPTH {
        return Some(Class::Secret);
    }

    let own = tainted(v).map(|(c, _)| c);
    let inner = match v {
        Value::List(l) | Value::Tuple(l) | Value::Set(l) => l.iter().filter_map(|x| class_below(x, depth + 1)).max(),
        Value::Record(r) => r.iter().filter_map(|(_, x)| class_below(x, depth + 1)).max(),
        Value::Map(m) => m
            .iter()
            .flat_map(|(k, x)| [class_below(k, depth + 1), class_below(x, depth + 1)])
            .flatten()
            .max(),
        Value::Variant(_, Some(p)) => class_below(p, depth + 1),
        _ => None,
    };

    own.max(inner)
}

/// Whether the selector `seg`, applied to `v`, compares a classified value.
/// Such a selector is a filter on a classified value (0009.6.1).
pub(crate) fn selects_classified(v: &Value, seg: &Segment) -> bool {
    match (v, seg) {
        (Value::List(l) | Value::Tuple(l) | Value::Set(l), Segment::Match(field, _)) => l
            .iter()
            .any(|e| tainted(e).is_some() || e.as_record().and_then(|r| r.get(field)).is_some_and(|x| class_in(x).is_some())),
        (Value::List(l) | Value::Tuple(l) | Value::Set(l), Segment::Key(_)) => l.iter().any(|e| class_in(e).is_some()),
        (Value::Map(m), _) => m.iter().any(|(k, _)| class_in(k).is_some()),
        _ => false,
    }
}

/// Wrap the value at `path` below `v` with `class`. Apply the paths of
/// [`novafile_schema::Schema::classify`] deepest first, so that no path goes
/// through a wrapper. If a segment selects nothing, the deepest value on the
/// way is wrapped instead, so a mismatch hides more, never less. The
/// recursion is as deep as `path` is long, and paths come from parsed
/// documents, which have a bounded depth.
pub(crate) fn wrap_at(v: &mut Value, path: &[Segment], class: Class) {
    if let Some((seg, rest)) = path.split_first()
        && let Some(next) = child_mut(v, seg)
    {
        wrap_at(next, rest, class);
        return;
    }

    let old = std::mem::replace(v, Value::Null);
    *v = taint(old, class);
}

/// The child of `v` that the concrete segment `seg` names, if it exists.
fn child_mut<'a>(v: &'a mut Value, seg: &Segment) -> Option<&'a mut Value> {
    match (v, seg) {
        (Value::Record(r), Segment::Field(f)) => r.get_mut(f),
        (Value::Variant(t, Some(p)), Segment::Field(f)) => (t == f).then_some(&mut **p),
        (Value::List(l) | Value::Tuple(l) | Value::Set(l), Segment::Index(i)) => l.get_mut(usize::try_from(*i).ok()?),
        (Value::Map(m), Segment::Key(k)) => m.iter_mut().find(|(mk, _)| mk == k).map(|(_, x)| x),
        (Value::Map(m), Segment::Field(f)) => m.iter_mut().find(|(mk, _)| mk.as_str() == Some(f)).map(|(_, x)| x),
        _ => None,
    }
}

/// `v` without wrappers. The output path and class of every wrapper go to
/// `marks`, in the form that [`novafile_schema::redact`] reads.
///
/// # Errors
///
/// Fails if `v` nests more deeply than [`MAX_DEPTH`].
pub(crate) fn strip(v: Value, marks: &mut Vec<ClassMark>) -> Result<Value, TemplateError> {
    strip_at(v, &mut Vec::new(), marks, 0)
}

/// [`strip`] for the value at `path`, `depth` levels down.
fn strip_at(v: Value, path: &mut Vec<Segment>, marks: &mut Vec<ClassMark>, depth: usize) -> Result<Value, TemplateError> {
    if depth > MAX_DEPTH {
        return err("the rendered value nests too deeply");
    }

    let (v, class) = untaint(v);
    if let Some(c) = class {
        marks.push((Path(path.clone()), c));
    }

    let child = |seg: Segment, x: Value, marks: &mut Vec<ClassMark>, path: &mut Vec<Segment>| {
        path.push(seg);
        let r = strip_at(x, path, marks, depth + 1);
        path.pop();
        r
    };

    let indexed = |l: Vec<Value>, marks: &mut Vec<ClassMark>, path: &mut Vec<Segment>| {
        l.into_iter()
            .enumerate()
            .map(|(i, x)| child(Segment::Index(i64::try_from(i).unwrap_or(i64::MAX)), x, marks, path))
            .collect::<Result<Vec<_>, _>>()
    };

    Ok(match v {
        Value::Record(r) => {
            let mut out = Record::new();
            for (k, x) in r.into_entries() {
                let x = child(Segment::Field(k.clone()), x, marks, path)?;
                out.set(k, x);
            }

            Value::Record(out)
        }
        Value::List(l) => Value::List(indexed(l, marks, path)?),
        Value::Tuple(l) => Value::Tuple(indexed(l, marks, path)?),
        Value::Set(l) => Value::Set(indexed(l, marks, path)?),
        Value::Map(m) => {
            // A path cannot name a key; a classified key hides the whole map.
            if let Some(c) = m.iter().filter_map(|(k, _)| class_in(k)).max() {
                marks.push((Path(path.clone()), c));
            }

            let mut out = Vec::with_capacity(m.len());
            for (k, x) in m {
                let k = strip_at(k, &mut Vec::new(), &mut Vec::new(), depth + 1)?;
                let x = child(Segment::Key(k.clone()), x, marks, path)?;
                out.push((k, x));
            }

            Value::Map(out)
        }
        Value::Variant(t, Some(p)) => {
            let p = child(Segment::Field(t.clone()), *p, marks, path)?;
            Value::Variant(t, Some(Box::new(p)))
        }
        other => other,
    })
}

/// `v` without any wrapper, at any depth, and the strongest class they had.
///
/// # Errors
///
/// Fails if `v` nests more deeply than [`MAX_DEPTH`].
pub(crate) fn clear(v: Value) -> Result<(Value, Option<Class>), TemplateError> {
    let class = class_in(&v);
    if class.is_none() {
        return Ok((v, None));
    }

    Ok((strip(v, &mut Vec::new())?, class))
}

/// The error for a template that branches on, or selects by, a classified
/// value (0009.7). The message names the construct, never the value.
pub(crate) fn branches<T>(what: &str) -> Result<T, TemplateError> {
    err(format!(
        "{what} depends on a classified value; this is not allowed when classified values are redacted"
    ))
}

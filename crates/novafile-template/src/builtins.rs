//! The standard library of template language 0.2 (proposal 0006 §0006.4–§0006.5): 40
//! functions with exact, locale-free semantics.
//!
//! Strings are sequences of Unicode scalar values: lengths, indices and slices count scalar
//! values. `lower` and `upper` are the full default case mappings (`UnicodeData.txt`, the
//! unconditional `SpecialCasing.txt` mappings and `Final_Sigma`, no language-specific rules),
//! and `trim` removes `White_Space`; all three use the tables of the Rust standard library
//! (see the crate documentation for the Unicode version).

use crate::eval::Eval;
use crate::ops::{self, Round, scalar_text};
use crate::{Cost, TemplateError};
use base64::Engine;
use novafile_text::{Profile, classify, to_text};
use novafile_value::{Decimal, Record, Value, canonical_bytes};
use sha2::{Digest, Sha256};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

/// The names of the built-in functions, by group: strings, values in general, collections,
/// numbers, conversions, encodings and digests.
pub const BUILTINS: [&str; 40] = [
    "lower",
    "upper",
    "trim",
    "trim_prefix",
    "trim_suffix",
    "starts_with",
    "ends_with",
    "replace",
    "split",
    "join",
    "slice",
    "matches",
    "len",
    "contains",
    "get",
    "default",
    "kind",
    "keys",
    "values",
    "sort",
    "unique",
    "concat",
    "merge",
    "min",
    "max",
    "floor",
    "ceil",
    "round",
    "div",
    "mod",
    "string",
    "int",
    "float",
    "decimal",
    "base64_encode",
    "base64_decode",
    "sha256",
    "revision",
    "to_json",
    "to_nova",
];

/// The step weight of a value passed to or returned from a built-in (§0006.7): its elements,
/// fields or entries, or the started 64-byte blocks of a string or byte string.
fn size(v: &Value) -> u64 {
    let n = match v {
        Value::List(l) | Value::Tuple(l) | Value::Set(l) => l.len(),
        Value::Record(r) => r.len(),
        Value::Map(m) => m.len(),
        Value::String(s) => s.len().div_ceil(64),
        Value::Bytes(b) => b.len().div_ceil(64),
        _ => 0,
    };
    n as u64
}

/// Call built-in `name`, charging its steps and the output it builds.
pub(crate) fn call(ev: &mut Eval<'_>, name: &str, args: Vec<Value>) -> Result<Value, TemplateError> {
    ev.step(1 + args.iter().map(size).sum::<u64>())?;
    let v = apply(ev, name, args).map_err(|m| TemplateError(format!("{name}: {m}")))?;
    ev.step(size(&v))?;
    match &v {
        Value::String(_) | Value::Bytes(_) | Value::List(_) | Value::Tuple(_) | Value::Set(_) | Value::Record(_) | Value::Map(_) => {
            let c = Cost::of(&v);
            ev.charge(c.nodes, c.bytes)?;
        }
        _ => {}
    }

    Ok(v)
}

/// The string argument at 0-based position `i`.
fn s(a: &[Value], i: usize) -> Result<&str, String> {
    match &a[i] {
        Value::String(s) => Ok(s),
        v => Err(format!("argument {} must be a string, found {}", i + 1, v.kind())),
    }
}

/// The int argument at 0-based position `i`.
fn int(a: &[Value], i: usize) -> Result<i128, String> {
    match &a[i] {
        Value::Int(n) => Ok(*n),
        v => Err(format!("argument {} must be an int, found {}", i + 1, v.kind())),
    }
}

/// A non-negative int argument used as an index, saturating at `usize::MAX`.
fn index(a: &[Value], i: usize) -> Result<usize, String> {
    let n = int(a, i)?;
    if n < 0 {
        return Err(format!("argument {} is a negative index", i + 1));
    }

    Ok(usize::try_from(n).unwrap_or(usize::MAX))
}

/// The elements of a list, tuple or set argument, in iteration order (§0006.6).
fn elements(v: Value, i: usize) -> Result<Vec<Value>, String> {
    match v {
        v @ (Value::List(_) | Value::Tuple(_) | Value::Set(_)) => Ok(ops::iterate(v).expect("collection").into_iter().map(|(_, v)| v).collect()),
        v => Err(format!("argument {} must be a list, tuple or set, found {}", i + 1, v.kind())),
    }
}

/// The bytes of a string (UTF-8) or byte-string argument.
fn octets(a: &[Value], i: usize) -> Result<&[u8], String> {
    match &a[i] {
        Value::String(s) => Ok(s.as_bytes()),
        Value::Bytes(b) => Ok(b),
        v => Err(format!("argument {} must be a string or bytes, found {}", i + 1, v.kind())),
    }
}

/// The least (or with `max`, greatest) of `xs` by the order of §0006.2.3.
fn extreme(xs: Vec<Value>, max: bool) -> Result<Value, String> {
    let mut it = xs.into_iter();
    let mut best = it.next().ok_or("needs at least one value")?;
    ops::order(&best, &best)?;
    for x in it {
        let o = ops::order(&x, &best)?;
        if (max && o.is_gt()) || (!max && o.is_lt()) {
            best = x;
        }
    }

    Ok(best)
}

/// The function body of built-in `name`; errors are prefixed with the name by [`call`].
#[expect(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "one arm per built-in function, flat by design; short names for operands"
)]
fn apply(ev: &mut Eval<'_>, name: &str, mut a: Vec<Value>) -> Result<Value, String> {
    let n = a.len();
    let arity = |lo: usize, hi: usize| -> Result<(), String> {
        if (lo..=hi).contains(&n) {
            return Ok(());
        }

        let want = match (lo, hi) {
            (1, 1) => "1 argument".to_owned(),
            (lo, hi) if lo == hi => format!("{lo} arguments"),
            (lo, usize::MAX) => format!("at least {lo} argument{}", if lo == 1 { "" } else { "s" }),
            (lo, hi) => format!("{lo} or {hi} arguments"),
        };
        Err(format!("takes {want}, but {n} were given"))
    };
    let str_ = |x: String| Ok(Value::String(x));

    match name {
        "lower" => {
            arity(1, 1)?;
            str_(s(&a, 0)?.to_lowercase())
        }
        "upper" => {
            arity(1, 1)?;
            str_(s(&a, 0)?.to_uppercase())
        }
        "trim" => {
            arity(1, 1)?;
            // `str::trim` removes code points with the `White_Space` property.
            str_(s(&a, 0)?.trim().to_owned())
        }
        "trim_prefix" => {
            arity(2, 2)?;
            let (x, p) = (s(&a, 0)?, s(&a, 1)?);
            str_(x.strip_prefix(p).unwrap_or(x).to_owned())
        }
        "trim_suffix" => {
            arity(2, 2)?;
            let (x, p) = (s(&a, 0)?, s(&a, 1)?);
            str_(x.strip_suffix(p).unwrap_or(x).to_owned())
        }
        "starts_with" => {
            arity(2, 2)?;
            Ok(Value::Bool(s(&a, 0)?.starts_with(s(&a, 1)?)))
        }
        "ends_with" => {
            arity(2, 2)?;
            Ok(Value::Bool(s(&a, 0)?.ends_with(s(&a, 1)?)))
        }
        "replace" => {
            arity(3, 3)?;
            let (x, old, new) = (s(&a, 0)?, s(&a, 1)?, s(&a, 2)?);
            if old.is_empty() {
                return Err("the string to replace must not be empty".into());
            }

            // Bound the result before building it: the output budget must also bound memory.
            let count = x.matches(old).count();
            ev.reserve(1, x.len() - count * old.len() + count * new.len()).map_err(|e| e.0)?;
            str_(x.replace(old, new))
        }
        "split" => {
            arity(2, 2)?;
            let (x, sep) = (s(&a, 0)?, s(&a, 1)?);
            if sep.is_empty() {
                return Err("the separator must not be empty".into());
            }

            ev.reserve(2 + x.matches(sep).count(), x.len()).map_err(|e| e.0)?;
            Ok(Value::List(x.split(sep).map(|p| Value::String(p.to_owned())).collect()))
        }
        "join" => {
            arity(2, 2)?;
            let sep = s(&a, 1)?.to_owned();
            let texts = elements(a.swap_remove(0), 0)?
                .iter()
                .map(|x| scalar_text(x).ok_or_else(|| format!("cannot join a {}; only scalars have text", x.kind())))
                .collect::<Result<Vec<_>, _>>()?;
            let len = texts.iter().map(String::len).sum::<usize>() + sep.len() * texts.len().saturating_sub(1);
            ev.reserve(1, len).map_err(|e| e.0)?;
            str_(texts.join(&sep))
        }
        "slice" => {
            arity(2, 3)?;
            let start = index(&a, 1)?;
            let end = if n == 3 { Some(index(&a, 2)?) } else { None };
            match &a[0] {
                Value::String(x) => {
                    let len = x.chars().count();
                    let (st, en) = (start.min(len), end.unwrap_or(len).min(len));
                    str_(x.chars().skip(st).take(en.saturating_sub(st)).collect())
                }
                Value::List(l) => {
                    let len = l.len();
                    let (st, en) = (start.min(len), end.unwrap_or(len).min(len));
                    Ok(Value::List(if st < en { l[st..en].to_vec() } else { Vec::new() }))
                }
                v => Err(format!("argument 1 must be a string or list, found {}", v.kind())),
            }
        }
        "matches" => {
            arity(2, 2)?;
            Ok(Value::Bool(ev.is_match(s(&a, 1)?, s(&a, 0)?)?))
        }
        "len" => {
            arity(1, 1)?;
            let len = match &a[0] {
                Value::String(x) => x.chars().count(),
                Value::Bytes(b) => b.len(),
                Value::List(l) | Value::Tuple(l) | Value::Set(l) => l.len(),
                Value::Record(r) => r.len(),
                Value::Map(m) => m.len(),
                v => return Err(format!("argument 1 must be a string, bytes or a collection, found {}", v.kind())),
            };
            Ok(Value::Int(len as i128))
        }
        "contains" => {
            arity(2, 2)?;
            Ok(Value::Bool(match (&a[0], &a[1]) {
                (Value::String(x), Value::String(y)) => x.contains(y.as_str()),
                (Value::String(_), y) => return Err(format!("argument 2 must be a string, found {}", y.kind())),
                (Value::List(l) | Value::Tuple(l) | Value::Set(l), y) => l.contains(y),
                (Value::Record(r), Value::String(k)) => r.contains_key(k),
                (Value::Record(_), y) => return Err(format!("argument 2 must be a string (a record key), found {}", y.kind())),
                (Value::Map(m), y) => m.iter().any(|(k, _)| k == y),
                (x, _) => return Err(format!("argument 1 must be a string or a collection, found {}", x.kind())),
            }))
        }
        "get" => {
            arity(3, 3)?;
            let fallback = a.pop().expect("three arguments");
            Ok(match (&a[0], &a[1]) {
                (Value::Record(r), Value::String(k)) => r.get(k).cloned(),
                (Value::Record(_), k) => return Err(format!("argument 2 must be a string (a record key), found {}", k.kind())),
                (Value::Map(m), k) => m.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()),
                (Value::List(l) | Value::Tuple(l), Value::Int(_)) => l.get(index(&a, 1)?).cloned(),
                (Value::List(_) | Value::Tuple(_), k) => return Err(format!("argument 2 must be an int (an index), found {}", k.kind())),
                (x, _) => return Err(format!("argument 1 must be a record, map, list or tuple, found {}", x.kind())),
            }
            .unwrap_or(fallback))
        }
        "default" => {
            arity(2, 2)?;
            let fallback = a.pop().expect("two arguments");
            Ok(match a.pop().expect("two arguments") {
                Value::Null => fallback,
                x => x,
            })
        }
        "kind" => {
            arity(1, 1)?;
            str_(a[0].kind().to_owned())
        }
        "keys" | "values" => {
            arity(1, 1)?;
            let x = a.pop().expect("one argument");
            if !matches!(x, Value::Record(_) | Value::Map(_)) {
                return Err(format!("argument 1 must be a record or map, found {}", x.kind()));
            }

            let pairs = ops::iterate(x).expect("record or map");
            Ok(Value::List(pairs.into_iter().map(|(k, v)| if name == "keys" { k } else { v }).collect()))
        }
        "sort" => {
            arity(1, 1)?;
            let mut xs = elements(a.pop().expect("one argument"), 0)?;
            if let Some(first) = xs.first() {
                for x in &xs {
                    ops::order(x, first)?;
                }
            }

            xs.sort_by(|x, y| ops::order(x, y).expect("checked"));
            Ok(Value::List(xs))
        }
        "unique" => {
            arity(1, 1)?;
            let l = match a.pop().expect("one argument") {
                Value::List(l) | Value::Tuple(l) => l,
                v => return Err(format!("argument 1 must be a list or tuple, found {}", v.kind())),
            };
            let mut seen = HashSet::new();
            Ok(Value::List(l.into_iter().filter(|v| seen.insert(canonical_bytes(v))).collect()))
        }
        "concat" => {
            arity(1, usize::MAX)?;
            let mut out = Vec::new();
            for (i, x) in a.into_iter().enumerate() {
                match x {
                    Value::List(l) => out.extend(l),
                    v => return Err(format!("argument {} must be a list, found {}", i + 1, v.kind())),
                }
            }

            Ok(Value::List(out))
        }
        "merge" => {
            arity(2, usize::MAX)?;
            let maps = matches!(a[0], Value::Map(_));
            let (mut rec, mut map) = (Record::new(), Vec::<(Value, Value)>::new());
            // Map keys by their canonical encoding, so merging is linear like its step cost.
            let mut at: HashMap<Vec<u8>, usize> = HashMap::new();
            for (i, x) in a.into_iter().enumerate() {
                match x {
                    Value::Record(r) if !maps => r.into_entries().into_iter().for_each(|(k, v)| {
                        rec.set(k, v);
                    }),
                    Value::Map(m) if maps => {
                        for (k, v) in m {
                            match at.entry(canonical_bytes(&k)) {
                                Entry::Occupied(e) => map[*e.get()].1 = v,
                                Entry::Vacant(e) => {
                                    e.insert(map.len());
                                    map.push((k, v));
                                }
                            }
                        }
                    }
                    v => {
                        return Err(format!(
                            "argument {} must be a {}, found {}",
                            i + 1,
                            if maps { "map" } else { "record" },
                            v.kind()
                        ));
                    }
                }
            }

            Ok(if maps { Value::Map(map) } else { Value::Record(rec) })
        }
        "min" | "max" => {
            arity(1, usize::MAX)?;
            let xs = if n == 1 {
                let xs = elements(a.pop().expect("one argument"), 0)?;
                if xs.is_empty() {
                    return Err("needs a non-empty list, tuple or set".into());
                }

                xs
            } else {
                a
            };
            extreme(xs, name == "max")
        }
        "floor" | "ceil" | "round" => {
            arity(1, 1)?;
            let mode = match name {
                "floor" => Round::Floor,
                "ceil" => Round::Ceil,
                _ => Round::Nearest,
            };
            ops::to_int(&a[0], mode)
        }
        "div" | "mod" => {
            arity(2, 2)?;
            let (x, y) = (int(&a, 0)?, int(&a, 1)?);
            if y == 0 {
                return Err("division by zero".into());
            }

            // Floored division: the quotient rounds toward −∞, so `mod` has the sign of `y`.
            let q = x.checked_div(y).map(|q| if x % y != 0 && ((x < 0) != (y < 0)) { q - 1 } else { q });
            ops::int(if name == "div" {
                q
            } else {
                q.and_then(|q| y.checked_mul(q)).and_then(|p| x.checked_sub(p))
            })
        }
        "string" => {
            arity(1, 1)?;
            str_(scalar_text(&a[0]).ok_or_else(|| format!("cannot convert a {} to a string", a[0].kind()))?)
        }
        "int" => {
            arity(1, 1)?;
            match &a[0] {
                Value::Int(i) => Ok(Value::Int(*i)),
                Value::String(x) => match classify(x) {
                    Ok(Value::Int(i)) => Ok(Value::Int(i)),
                    _ => Err(format!("{x:?} is not an integer token")),
                },
                v => Err(format!("cannot convert a {} to an int; use floor, ceil or round", v.kind())),
            }
        }
        "float" => {
            arity(1, 1)?;
            #[expect(clippy::cast_precision_loss, reason = "rounds to nearest, ties to even, as specified")]
            let f = match &a[0] {
                Value::Int(i) => *i as f64,
                Value::Decimal(d) => d.to_f64(),
                Value::Rational(r) => ops::ratio_to_f64(r.numerator(), r.denominator()),
                Value::String(x) => match classify(x) {
                    Ok(Value::Float(f)) => f,
                    Ok(Value::Int(i)) => i as f64,
                    _ => return Err(format!("{x:?} is not a float or integer token")),
                },
                v => return Err(format!("cannot convert a {} to a float", v.kind())),
            };
            Ok(Value::Float(f))
        }
        "decimal" => {
            arity(1, 1)?;
            match &a[0] {
                Value::Int(i) if i.unsigned_abs().to_string().len() <= 38 => Ok(Value::Decimal(Decimal::from_int(*i))),
                Value::Int(_) => Err("the integer has more than 38 digits".into()),
                Value::String(x) => Decimal::parse(x).map(Value::Decimal),
                Value::Float(_) => Err("floats are not converted; write $decimal($string($f)) to go through the shortest text".into()),
                v => Err(format!("cannot convert a {} to a decimal", v.kind())),
            }
        }
        "base64_encode" => {
            arity(1, 1)?;
            str_(base64::engine::general_purpose::STANDARD.encode(octets(&a, 0)?))
        }
        "base64_decode" => {
            arity(1, 1)?;
            // The standard engine requires canonical padding and zero trailing bits.
            base64::engine::general_purpose::STANDARD
                .decode(s(&a, 0)?)
                .map(Value::Bytes)
                .map_err(|e| format!("invalid base64: {e}"))
        }
        "sha256" => {
            arity(1, 1)?;
            let mut hex = String::with_capacity(64);
            for b in Sha256::digest(octets(&a, 0)?) {
                let _ = write!(hex, "{b:02x}");
            }

            str_(hex)
        }
        "revision" => {
            arity(1, 1)?;
            str_(novafile_value::revision(&a[0]).to_string())
        }
        "to_json" => {
            arity(1, 1)?;
            crate::json::to_json(&a[0]).map(Value::String)
        }
        "to_nova" => {
            arity(1, 1)?;
            let t = to_text(&a[0], Profile::Canonical);
            // A flow value is one line; drop its line break. Block text keeps its last one.
            let one_line = t.strip_suffix('\n').filter(|b| !b.contains('\n'));
            str_(one_line.map_or(t.clone(), str::to_owned))
        }
        _ => Err("unknown function".into()),
    }
}

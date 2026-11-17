//! Schema-directed resolution and validation (spec §6.3–6.4).

use crate::model::{Constraints, Def, Num, RecordDef, Schema, TypeKind, TypeRef, VariantDef};
use base64::Engine;
use novafile_text::{Path, Segment};
use novafile_value::{Decimal, Duration, Rational, Record, Timestamp, Value};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub path: Path,
    pub message: String,
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

struct Ctx<'s> {
    schema: &'s Schema,
    path: Vec<Segment>,
    issues: Vec<Issue>,
    apply_defaults: bool,
}

impl<'s> Ctx<'s> {
    fn issue(&mut self, m: impl Into<String>) {
        self.issues.push(Issue {
            path: Path(self.path.clone()),
            message: m.into(),
        });
    }

    fn deref(&self, t: &'s TypeRef) -> (&'s TypeRef, Option<&'s Def>) {
        let mut cur = t;
        loop {
            match &cur.kind {
                TypeKind::Named(n) => match self.schema.def(n) {
                    Some(Def::Alias(a)) => cur = a,
                    Some(d) => return (cur, Some(d)),
                    None => return (cur, None),
                },
                _ => return (cur, None),
            }
        }
    }

    fn check(&mut self, v: Value, t: &'s TypeRef) -> Value {
        if matches!(v, Value::Null) && (t.nullable || matches!(t.kind, TypeKind::Null | TypeKind::Any)) {
            return v;
        }

        let (rt, def) = self.deref(t);
        if matches!(v, Value::Null) && rt.nullable {
            return v;
        }

        let v = match (&rt.kind, def) {
            (TypeKind::Named(_), Some(Def::Record(r))) => self.record(v, r),
            (TypeKind::Named(_), Some(Def::Enum(vs))) => self.variant(v, vs),
            (TypeKind::Named(n), _) => {
                self.issue(format!("unknown type {n}"));
                v
            }
            (kind, _) => self.structural(v, kind),
        };

        // Constraints on the alias chain and the use site.
        self.constraints(&v, &rt.constraints);
        if !std::ptr::eq(rt, t) {
            self.constraints(&v, &t.constraints);
        }

        v
    }

    fn mismatch(&mut self, expected: &str, v: &Value) {
        self.issue(format!("expected {expected}, found {}", v.kind()));
    }

    #[expect(clippy::match_same_arms, reason = "one arm per kind keeps the conversion table readable")]
    #[expect(clippy::cast_precision_loss, reason = "the int-to-float arm only takes integers up to 2^53, exact in f64")]
    fn structural(&mut self, v: Value, kind: &'s TypeKind) -> Value {
        match (kind, v) {
            (TypeKind::Any, v) => v,
            (TypeKind::Null, v @ Value::Null) => v,
            (TypeKind::Bool, v @ Value::Bool(_)) => v,
            (TypeKind::Int(k), v @ Value::Int(i)) => {
                let (lo, hi) = k.bounds();
                if i < lo || i > hi {
                    self.issue(format!("{i} is out of range for {}", k.name()));
                }

                v
            }
            (TypeKind::Float, v @ Value::Float(_)) => v,
            (TypeKind::Float, Value::Int(i)) if i.unsigned_abs() <= 1 << 53 => Value::Float(i as f64),
            (TypeKind::Decimal, v @ Value::Decimal(_)) => v,
            (TypeKind::Decimal, Value::Int(i)) => Value::Decimal(Decimal::from_int(i)),
            (TypeKind::Decimal, Value::Float(f)) if f.is_finite() => Value::Decimal(Decimal::parse(&format!("{f:?}")).expect("finite float")),
            (TypeKind::Decimal, Value::String(s)) => self.lexical(s, "decimal", |s| Decimal::parse(s).map(Value::Decimal)),
            (TypeKind::Decimal, Value::Rational(r)) if r.to_decimal().is_some() => Value::Decimal(r.to_decimal().unwrap()),
            (TypeKind::Rational, v @ Value::Rational(_)) => v,
            (TypeKind::Rational, Value::Int(i)) => Value::Rational(Rational::new(i, 1).expect("nonzero denominator")),
            (TypeKind::Rational, Value::Decimal(d)) => {
                if let Some(r) = decimal_to_rational(d) {
                    Value::Rational(r)
                } else {
                    self.issue("decimal is too large to represent as a rational");
                    Value::Decimal(d)
                }
            }
            (TypeKind::Rational, Value::String(s)) => self.lexical(s, "rational", |s| Rational::parse(s).map(Value::Rational)),
            (TypeKind::String, v @ Value::String(_)) => v,
            (TypeKind::Bytes, v @ Value::Bytes(_)) => v,
            (TypeKind::Bytes, Value::String(s)) => self.lexical(s, "bytes", |s| {
                base64::engine::general_purpose::STANDARD.decode(s).map(Value::Bytes).map_err(|e| e.to_string())
            }),
            (TypeKind::Timestamp, v @ Value::Timestamp(_)) => v,
            (TypeKind::Timestamp, Value::String(s)) => self.lexical(s, "timestamp", |s| Timestamp::parse(s).map(Value::Timestamp)),
            (TypeKind::Duration, v @ Value::Duration(_)) => v,
            (TypeKind::Duration, Value::String(s)) => self.lexical(s, "duration", |s| Duration::parse(s).map(Value::Duration)),
            (TypeKind::List(et), Value::List(items)) => Value::List(self.elements(items, et)),
            (TypeKind::Set(et), Value::Set(items) | Value::List(items)) => {
                let items = self.elements(items, et);
                match Value::set(items.clone()) {
                    Ok(s) => s,
                    Err(e) => {
                        self.issue(e);
                        Value::Set(items)
                    }
                }
            }
            (TypeKind::Tuple(ts), Value::Tuple(items) | Value::List(items)) => {
                if items.len() != ts.len() {
                    self.issue(format!("expected a {}-tuple, found {} elements", ts.len(), items.len()));
                    return Value::Tuple(items);
                }

                let out = items
                    .into_iter()
                    .zip(ts)
                    .enumerate()
                    .map(|(i, (v, t))| {
                        self.path.push(index_segment(i));
                        let r = self.check(v, t);
                        self.path.pop();
                        r
                    })
                    .collect();
                Value::Tuple(out)
            }
            (TypeKind::Map(kt, vt), Value::Map(entries)) => {
                let out = entries
                    .into_iter()
                    .map(|(k, v)| {
                        self.path.push(Segment::Key(k.clone()));
                        let k2 = self.check(k, kt);
                        let v2 = self.check(v, vt);
                        self.path.pop();
                        (k2, v2)
                    })
                    .collect();
                Value::Map(out)
            }
            (TypeKind::Map(kt, vt), Value::Record(r)) => {
                let out = r
                    .into_entries()
                    .into_iter()
                    .map(|(k, v)| {
                        self.path.push(Segment::Field(k.clone()));
                        let k2 = self.check(Value::String(k), kt);
                        let v2 = self.check(v, vt);
                        self.path.pop();
                        (k2, v2)
                    })
                    .collect();
                Value::Map(out)
            }
            (kind, v) => {
                let t = TypeRef::new(kind.clone());
                self.mismatch(&t.to_string(), &v);
                v
            }
        }
    }

    fn lexical(&mut self, s: String, what: &str, f: impl Fn(&str) -> Result<Value, String>) -> Value {
        match f(&s) {
            Ok(v) => v,
            Err(e) => {
                self.issue(format!("string is not a valid {what}: {e}"));
                Value::String(s)
            }
        }
    }

    fn elements(&mut self, items: Vec<Value>, et: &'s TypeRef) -> Vec<Value> {
        let out: Vec<Value> = items
            .into_iter()
            .enumerate()
            .map(|(i, v)| {
                self.path.push(index_segment(i));
                let r = self.check(v, et);
                self.path.pop();
                r
            })
            .collect();
        // @key uniqueness (spec §6.2).
        if let (_, Some(Def::Record(r))) = self.deref(et) {
            let keys = r.key_fields();
            if !keys.is_empty() {
                let mut seen = HashSet::new();
                for (i, v) in out.iter().enumerate() {
                    if let Some(rec) = v.as_record() {
                        let k: Vec<Option<&Value>> = keys.iter().map(|k| rec.get(k)).collect();
                        if !seen.insert(k.clone()) {
                            self.path.push(index_segment(i));
                            self.issue(format!("duplicate @key ({})", keys.join(", ")));
                            self.path.pop();
                        }
                    }
                }
            }
        }

        out
    }

    fn record(&mut self, v: Value, def: &'s RecordDef) -> Value {
        let Value::Record(r) = v else {
            self.mismatch("record", &v);
            return v;
        };

        let mut out = Record::new();
        let mut entries = r.into_entries();
        for f in &def.fields {
            let pos = entries.iter().position(|(k, _)| *k == f.name);
            self.path.push(Segment::Field(f.name.clone()));
            match pos {
                Some(p) => {
                    let (k, v) = entries.remove(p);
                    let v = self.check(v, &f.ty);
                    out.set(k, v);
                }
                None => match &f.default {
                    Some(d) if self.apply_defaults => {
                        let d = self.check(d.clone(), &f.ty);
                        out.set(f.name.clone(), d);
                    }
                    Some(_) => {}
                    None if f.optional => {}
                    None => self.issue("missing required field"),
                },
            }

            self.path.pop();
        }

        for (k, v) in entries {
            if !def.open {
                self.path.push(Segment::Field(k.clone()));
                self.issue("unknown field");
                self.path.pop();
            }

            out.set(k, v);
        }

        Value::Record(out)
    }

    fn variant(&mut self, v: Value, vs: &'s [VariantDef]) -> Value {
        // `{tag: payload}` — how JSON and other formats spell a variant (§6.3).
        let v = match v {
            Value::Record(r) if r.len() == 1 && vs.iter().any(|d| d.payload.is_some() && r.contains_key(&d.name)) => {
                let (tag, payload) = r.into_entries().pop().unwrap();
                Value::Variant(tag, Some(Box::new(payload)))
            }
            other => other,
        };

        match v {
            Value::String(s) => match vs.iter().find(|d| d.name == s) {
                Some(d) if d.payload.is_none() => Value::Variant(s, None),
                Some(_) => {
                    self.issue(format!("variant {s} requires a payload"));
                    Value::String(s)
                }
                None => {
                    self.issue(format!("unknown variant {s:?}; expected one of {}", names(vs)));
                    Value::String(s)
                }
            },
            Value::Variant(tag, payload) => match vs.iter().find(|d| d.name == tag) {
                None => {
                    self.issue(format!("unknown variant {tag:?}; expected one of {}", names(vs)));
                    Value::Variant(tag, payload)
                }
                Some(d) => match (&d.payload, payload) {
                    (None, None) => Value::Variant(tag, None),
                    (Some(t), Some(p)) => {
                        self.path.push(Segment::Field(tag.clone()));
                        let p = self.check(*p, t);
                        self.path.pop();
                        Value::Variant(tag, Some(Box::new(p)))
                    }
                    (None, Some(p)) => {
                        self.issue(format!("variant {tag} takes no payload"));
                        Value::Variant(tag, Some(p))
                    }
                    (Some(_), None) => {
                        self.issue(format!("variant {tag} requires a payload"));
                        Value::Variant(tag, None)
                    }
                },
            },
            other => {
                self.mismatch(&format!("variant ({})", names(vs)), &other);
                other
            }
        }
    }

    fn constraints(&mut self, v: &Value, c: &Constraints) {
        if let Some((lo, hi)) = &c.range
            && let Some(n) = Num::of(v)
        {
            // Exact across kinds; NaN never satisfies a range.
            if lo.as_ref().is_some_and(|lo| !lo.le(&n)) || hi.as_ref().is_some_and(|hi| !n.le(hi)) {
                let b = |o: &Option<Num>| o.as_ref().map(std::string::ToString::to_string).unwrap_or_default();
                self.issue(format!("{n} is outside @range({}..{})", b(lo), b(hi)));
            }
        }

        if let Some((lo, hi)) = c.len {
            let len = match v {
                Value::String(s) => Some(c.len_unit.measure(s)),
                Value::Bytes(b) => Some(b.len()),
                Value::List(l) | Value::Set(l) => Some(l.len()),
                Value::Map(m) => Some(m.len()),
                Value::Record(r) => Some(r.len()),
                _ => None,
            };

            if let Some(len) = len {
                let len = len as u64;
                if lo.is_some_and(|lo| len < lo) || hi.is_some_and(|hi| len > hi) {
                    let unit = match (v, c.len_unit) {
                        (Value::String(_), u) if u != crate::LenUnit::Scalars => format!(", {}", u.name()),
                        _ => String::new(),
                    };

                    self.issue(format!(
                        "length {len} is outside @len({}..{}{unit})",
                        lo.map(|n| n.to_string()).unwrap_or_default(),
                        hi.map(|n| n.to_string()).unwrap_or_default()
                    ));
                }
            }
        }
    }
}

#[expect(clippy::cast_possible_wrap, reason = "an element index is at most isize::MAX")]
fn index_segment(i: usize) -> Segment {
    Segment::Index(i as i64)
}

fn decimal_to_rational(d: Decimal) -> Option<Rational> {
    let (c, e) = (d.coefficient(), d.exponent());
    if e >= 0 {
        Rational::new(c.checked_mul(10i128.checked_pow(e.unsigned_abs())?)?, 1).ok()
    } else {
        Rational::new(c, 10i128.checked_pow(e.unsigned_abs())?).ok()
    }
}

fn names(vs: &[VariantDef]) -> String {
    vs.iter().map(|v| v.name.as_str()).collect::<Vec<_>>().join(" | ")
}

impl Schema {
    /// Resolve `v` against type `t`: apply the permitted lexical conversions
    /// and defaults, and report every violation (spec §6.3).
    ///
    /// # Errors
    ///
    /// Returns every issue found if `v` does not conform to `t`.
    pub fn resolve_as(&self, v: &Value, t: &TypeRef) -> Result<Value, Vec<Issue>> {
        let mut c = Ctx {
            schema: self,
            path: Vec::new(),
            issues: Vec::new(),
            apply_defaults: true,
        };

        let out = c.check(v.clone(), t);
        if c.issues.is_empty() { Ok(out) } else { Err(c.issues) }
    }

    /// Resolve against the `@root` type.
    ///
    /// # Errors
    ///
    /// Returns every issue found if `v` does not conform, or a single issue if
    /// the schema has no `@root`.
    pub fn resolve(&self, v: &Value) -> Result<Value, Vec<Issue>> {
        let t = self.root_type().ok_or_else(|| {
            vec![Issue {
                path: Path::root(),
                message: "schema has no @root".into(),
            }]
        })?;
        self.resolve_as(v, &t)
    }

    /// Validate without materializing defaults.
    ///
    /// # Errors
    ///
    /// Returns every issue found if `v` does not conform, or a single issue if
    /// the schema has no `@root`.
    pub fn validate(&self, v: &Value) -> Result<(), Vec<Issue>> {
        let t = self.root_type().ok_or_else(|| {
            vec![Issue {
                path: Path::root(),
                message: "schema has no @root".into(),
            }]
        })?;
        let mut c = Ctx {
            schema: self,
            path: Vec::new(),
            issues: Vec::new(),
            apply_defaults: false,
        };

        c.check(v.clone(), &t);
        if c.issues.is_empty() { Ok(()) } else { Err(c.issues) }
    }
}

//! Schema-directed resolution and validation (spec §6.3–6.4).

use crate::model::{Class, Constraints, Def, Num, RecordDef, Schema, TypeKind, TypeRef, VariantDef};
use base64::Engine;
use novafile_text::{Path, Segment};
use novafile_value::{Decimal, Duration, Rational, Record, Timestamp, Value};
use std::collections::HashSet;
use std::fmt::Write as _;

/// The path of a classified value and its class, as [`Schema::classify`]
/// reports it (proposal 0009).
pub type ClassMark = (Path, Class);

/// A problem found while resolving or validating a value.
#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    /// Where in the value the problem is.
    pub path: Path,
    /// What is wrong.
    pub message: String,
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// State of one resolution or validation pass.
struct Ctx<'s> {
    /// The schema that named types are looked up in.
    schema: &'s Schema,
    /// Path of the value being checked.
    path: Vec<Segment>,
    /// Problems found so far.
    issues: Vec<Issue>,
    /// Indices into `issues` of `@key` duplicates, which do not take part in
    /// choosing a union member (spec §6.3).
    keyed: Vec<usize>,
    /// How this pass treats defaults, conversions, class paths and required fields.
    pass: Pass,
    /// Nesting of [`Self::check`] calls, capped at [`MAX_CHECK_DEPTH`].
    depth: usize,
    /// Calls to [`Self::check`] left. Overlapping union members can retry the
    /// same value exponentially often, so the work is bounded by input size.
    fuel: usize,
    /// The paths of classified values and their classes (proposal 0009), in
    /// the order their checks finish. A union member that fails takes its
    /// entries back.
    classes: Vec<(Vec<Segment>, Class)>,
    /// The strongest class of the value being checked or any value that
    /// contains it. Issues then leave the value out of their messages.
    inside: Option<Class>,
}

/// How one pass treats defaults, conversions, class paths and required fields.
#[derive(Clone, Copy)]
#[expect(clippy::struct_excessive_bools, reason = "one flag per independent aspect of a pass")]
struct Pass {
    /// Absent fields get their default (resolution), or stay absent (validation).
    apply_defaults: bool,
    /// The lossless conversions of spec §6.3 apply; without them a value must
    /// already have the kind its type names.
    convert: bool,
    /// Class paths follow sets into their resolved order. Otherwise they keep
    /// the source order, as the stored value has it.
    resolved_paths: bool,
    /// Partial validation of one configuration layer (proposal 0010): every
    /// record field is optional and record rules are not checked.
    partial: bool,
}

impl Pass {
    /// Resolution: conversions and defaults (spec §6.3).
    const RESOLVE: Pass = Pass {
        apply_defaults: true,
        convert: true,
        resolved_paths: true,
        partial: false,
    };
    /// Validation: conversions, no defaults (spec §6.4).
    const VALIDATE: Pass = Pass {
        apply_defaults: false,
        ..Pass::RESOLVE
    };
    /// A check of a value as it is: no conversions, no defaults.
    const CHECK: Pass = Pass {
        convert: false,
        ..Pass::VALIDATE
    };
    /// Classification in the coordinates of the stored value.
    const SOURCE: Pass = Pass {
        resolved_paths: false,
        ..Pass::VALIDATE
    };
    /// Partial resolution of a layer: conversions, but no defaults, since a
    /// default would override the value of a lower layer when merged.
    const PARTIAL: Pass = Pass {
        partial: true,
        ..Pass::VALIDATE
    };
}

impl<'s> Ctx<'s> {
    /// `detail` if the value being checked is not classified, else `value`:
    /// messages about a classified value must not show it (proposal 0009).
    fn shown(&self, detail: impl FnOnce() -> String) -> String {
        if self.inside.is_some() { "value".to_owned() } else { detail() }
    }

    /// Record a problem at the current path.
    fn issue(&mut self, m: impl Into<String>) {
        self.issues.push(Issue {
            path: Path(self.path.clone()),
            message: m.into(),
        });
    }

    /// Resolve `v` against `t`: apply permitted conversions, record issues, and
    /// return the possibly converted value. The constraints of both the use site
    /// and the alias target apply.
    fn check(&mut self, v: Value, t: &'s TypeRef) -> Value {
        // Fuel bounds the total work, not the depth: a field whose default is
        // its own record (`a: R = {}`) would otherwise recurse until the stack overflows.
        if self.depth == MAX_CHECK_DEPTH {
            self.issue(format!("resolution nests deeper than {MAX_CHECK_DEPTH} levels"));
            return v;
        }

        self.depth += 1;
        let class = self.schema.node_class(t);
        let outer = self.inside;
        self.inside = outer.max(class);
        let v = self.check_at(v, t);
        self.inside = outer;
        self.depth -= 1;
        if let Some(c) = class {
            self.classes.push((self.path.clone(), c));
        }

        v
    }

    /// [`Self::check`] below the depth guard.
    fn check_at(&mut self, v: Value, t: &'s TypeRef) -> Value {
        if self.fuel == 0 {
            return v;
        }

        self.fuel -= 1;
        if matches!(v, Value::Null) && (t.nullable || matches!(t.kind, TypeKind::Null | TypeKind::Any)) {
            return v;
        }

        let (rt, def) = self.schema.deref(t);
        if matches!(v, Value::Null) && rt.nullable {
            return v;
        }

        let v = match (&rt.kind, def) {
            (TypeKind::Union(ms), _) => {
                // Schemas built in code skip the parser's check for unions that reach themselves.
                let cycle = match &t.kind {
                    TypeKind::Named(n) => self.schema.alias_cycle(n, rt),
                    _ => None,
                };

                match cycle {
                    Some(c) => {
                        self.issue(format!("type alias cycle through {c}"));
                        v
                    }
                    None => self.union(v, ms, t),
                }
            }
            (TypeKind::Named(_), Some(Def::Record(r))) => self.record(v, r),
            (TypeKind::Named(_), Some(Def::Enum(vs))) => self.variant(v, vs),
            // Schemas built in code (derive, JSON Schema import) skip the
            // parser's cycle check, so a bounded deref can stop on an alias.
            (TypeKind::Named(n), _) if self.schema.def(n).is_some() => {
                self.issue(format!("type alias cycle through {n}"));
                v
            }
            (TypeKind::Named(n), _) => {
                self.issue(format!("unknown type {n}"));
                v
            }
            (kind, _) => self.structural(v, kind),
        };

        // Constraints on the use site and every alias of the chain all hold
        // (proposal 0008: a reference to an alias refines it).
        for link in self.schema.alias_chain(t) {
            self.constraints(&v, &link.constraints);
        }

        v
    }

    /// Resolve against the first member that takes `v` without an issue
    /// (spec §6.3). If none does, report one issue at the union's path,
    /// naming it as `shown` and carrying the closest member's issues as
    /// detail (spec §6.4).
    fn union(&mut self, v: Value, members: &'s [TypeRef], shown: &'s TypeRef) -> Value {
        let (mark, kmark, cmark) = (self.issues.len(), self.keyed.len(), self.classes.len());
        let mut failures = Vec::with_capacity(members.len());
        for m in members {
            let r = self.check(v.clone(), m);
            let keyed = self.keyed.len() - kmark;
            if self.issues.len() - mark == keyed || self.fuel == 0 {
                return r;
            }

            failures.push(self.issues.split_off(mark));
            self.keyed.truncate(kmark);
            self.classes.truncate(cmark);
        }

        let mut msg = String::new();
        // A near miss of a classified member must not show the value either.
        let classified = self.inside.is_some() || members.iter().any(|m| self.schema.type_class(m).is_some());
        if !classified && !matches!(v, Value::List(_) | Value::Set(_) | Value::Tuple(_) | Value::Map(_) | Value::Record(_)) {
            msg = format!("{} ", novafile_text::flow_string(&v));
        }

        let _ = write!(msg, "matches no member of {shown}");
        if let Some(i) = self.closest(&v, members) {
            let base = self.path.len();
            let detail: Vec<String> = failures[i]
                .iter()
                .map(|is| {
                    let mut d = match &members[i].kind {
                        TypeKind::Named(n) => format!("{n}: "),
                        _ => String::new(),
                    };

                    if is.path.0.len() > base {
                        let _ = write!(d, "{}: ", Path(is.path.0[base..].to_vec()));
                    }

                    d + &is.message
                })
                .collect();
            let _ = write!(msg, ": {}", detail.join("; "));
        }

        self.issue(msg);
        v
    }

    /// The member whose failure best explains why `v` matched none (spec
    /// §6.4): for a record, the record member declaring most of its keys;
    /// otherwise the first member whose kind accepts `v`.
    fn closest(&self, v: &Value, members: &'s [TypeRef]) -> Option<usize> {
        if let Value::Record(r) = v {
            let score = |m: &TypeRef| match self.schema.deref(m) {
                (_, Some(Def::Record(d))) => r.keys().filter(|k| d.field(k).is_some()).count(),
                _ => 0,
            };

            let best = members.iter().enumerate().max_by_key(|&(i, m)| (score(m), std::cmp::Reverse(i)));
            if let Some((i, m)) = best
                && score(m) > 0
            {
                return Some(i);
            }
        }

        members.iter().position(|m| self.accepts(v, m, &mut Vec::new()))
    }

    /// Whether the kind of `t` can take a value of `v`'s kind, before any
    /// content is checked. `seen` holds the names already tried, so aliases
    /// shared by several union members are expanded once.
    fn accepts(&self, v: &Value, t: &'s TypeRef, seen: &mut Vec<&'s str>) -> bool {
        if matches!(v, Value::Null) && t.nullable {
            return true;
        }

        if let TypeKind::Named(n) = &t.kind {
            if seen.contains(&n.as_str()) {
                return false;
            }

            seen.push(n);
        }

        match (self.schema.deref(t), v) {
            ((_, Some(Def::Record(_))), Value::Record(_)) | ((_, Some(Def::Enum(_))), Value::String(_) | Value::Variant(..) | Value::Record(_)) => true,
            ((rt, None), v) => match (&rt.kind, v) {
                (TypeKind::Union(ms), v) => ms.iter().any(|m| self.accepts(v, m, seen)),
                // A literal explains a failure only for its own string.
                (TypeKind::Literal(l), Value::String(s)) => l == s,
                (TypeKind::Any, _)
                | (TypeKind::Null, Value::Null)
                | (TypeKind::Bool, Value::Bool(_))
                | (TypeKind::Int(_) | TypeKind::Float, Value::Int(_))
                | (TypeKind::Float, Value::Float(_))
                | (TypeKind::Decimal | TypeKind::Rational, Value::Int(_) | Value::Float(_) | Value::Decimal(_) | Value::Rational(_) | Value::String(_))
                | (TypeKind::String, Value::String(_))
                | (TypeKind::Bytes, Value::Bytes(_) | Value::String(_))
                | (TypeKind::Timestamp, Value::Timestamp(_) | Value::String(_))
                | (TypeKind::Duration, Value::Duration(_) | Value::String(_))
                | (TypeKind::List(_), Value::List(_))
                | (TypeKind::Set(_), Value::Set(_) | Value::List(_))
                | (TypeKind::Tuple(_), Value::Tuple(_) | Value::List(_))
                | (TypeKind::Map(..), Value::Map(_) | Value::Record(_)) => true,
                _ => false,
            },
            _ => false,
        }
    }

    /// Record that a value of the `expected` type was required.
    fn mismatch(&mut self, expected: &str, v: &Value) {
        let hint = match v {
            // `port: 80,` in a block is the plain string "80,": a habit from RON or JSON.
            Value::String(s) if s.len() > 1 && s.ends_with(',') => "; block entries end at the line break, drop the trailing `,`",
            _ => "",
        };
        self.issue(format!("expected {expected}, found {}{hint}", v.kind()));
    }

    /// Resolve against a built-in type kind, applying the lexical and numeric
    /// conversions of spec §6.3.
    #[expect(clippy::match_same_arms, reason = "one arm per kind keeps the conversion table readable")]
    #[expect(clippy::cast_precision_loss, reason = "the int-to-float arm only takes integers up to 2^53, exact in f64")]
    #[expect(clippy::too_many_lines, reason = "one arm per kind and conversion")]
    fn structural(&mut self, v: Value, kind: &'s TypeKind) -> Value {
        if !self.pass.convert && converts(kind, &v) {
            self.mismatch(&TypeRef::new(kind.clone()).to_string(), &v);
            return v;
        }

        match (kind, v) {
            (TypeKind::Any, v) => v,
            (TypeKind::Null, v @ Value::Null) => v,
            (TypeKind::Bool, v @ Value::Bool(_)) => v,
            (TypeKind::Int(k), v @ Value::Int(i)) => {
                let (lo, hi) = k.bounds();
                if i < lo || i > hi {
                    let shown = self.shown(|| i.to_string());
                    self.issue(format!("{shown} is out of range for {}", k.name()));
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
            (TypeKind::Literal(l), v @ Value::String(_)) if v.as_str() == Some(l) => v,
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
                let cmark = self.classes.len();
                let items = self.elements(items, et);
                match Value::set(items.clone()) {
                    Ok(s) => {
                        if self.pass.resolved_paths {
                            self.reindex_classes(cmark, &items, &s);
                        }

                        s
                    }
                    Err(e) => {
                        let e = self.shown(|| e);
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
                        let seg = Segment::Key(k.clone());
                        let k2 = self.key(k, kt);
                        self.path.push(seg);
                        let v2 = self.check(v, vt);
                        self.path.pop();
                        (k2, v2)
                    })
                    .collect();
                self.unique_keys(out)
            }
            (TypeKind::Map(kt, vt), Value::Record(r)) => {
                let out = r
                    .into_entries()
                    .into_iter()
                    .map(|(k, v)| {
                        let seg = Segment::Field(k.clone());
                        let k2 = self.key(Value::String(k), kt);
                        self.path.push(seg);
                        let v2 = self.check(v, vt);
                        self.path.pop();
                        (k2, v2)
                    })
                    .collect();
                self.unique_keys(out)
            }
            (kind, v) => {
                let t = TypeRef::new(kind.clone());
                self.mismatch(&t.to_string(), &v);
                v
            }
        }
    }

    /// Point the class entries from `cmark` on at the elements' places in the
    /// set `sorted`, which [`Value::set`] built from `items` in canonical
    /// order: redaction looks values up in the resolved value.
    fn reindex_classes(&mut self, cmark: usize, items: &[Value], sorted: &Value) {
        let Value::Set(sorted) = sorted else { return };
        let at = self.path.len();
        for (path, _) in &mut self.classes[cmark..] {
            if let Some(Segment::Index(i)) = path.get(at)
                && let Some(item) = usize::try_from(*i).ok().and_then(|i| items.get(i))
                && let Some(j) = sorted.iter().position(|x| x == item)
            {
                path[at] = index_segment(j);
            }
        }
    }

    /// A map of `entries`, reporting keys that conversion made equal
    /// (`"1/2"` and `"2/4"` as `rational`), as the set arm does for elements.
    fn unique_keys(&mut self, entries: Vec<(Value, Value)>) -> Value {
        match Value::map(entries.clone()) {
            Ok(m) => m,
            Err(e) => {
                let e = self.shown(|| e);
                self.issue(e);
                Value::Map(entries)
            }
        }
    }

    /// Resolve a map key against `kt`. Its issues are reported at the map,
    /// since a path cannot point at a key or into one (`m[1]` would name the
    /// entry whose key is 1), and say that a key failed.
    fn key(&mut self, k: Value, kt: &'s TypeRef) -> Value {
        let mark = self.issues.len();
        let k = self.check(k, kt);
        for is in &mut self.issues[mark..] {
            is.path.0.truncate(self.path.len());
            is.message.insert_str(0, "key ");
        }

        k
    }

    /// Parse the string `s` with `f` as a `what`; on failure, record an issue
    /// and keep the string.
    fn lexical(&mut self, s: String, what: &str, f: impl Fn(&str) -> Result<Value, String>) -> Value {
        match f(&s) {
            Ok(v) => v,
            Err(e) => {
                let e = self.shown(|| e);
                self.issue(format!("string is not a valid {what}: {e}"));
                Value::String(s)
            }
        }
    }

    /// Resolve each element against `et`, then check `@key` uniqueness if the
    /// elements are records.
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

        // @key uniqueness (spec §6.2), across union members too.
        let keys = self.schema.key_fields_of(et);
        if !keys.is_empty() {
            let mut seen = HashSet::new();
            for (i, v) in out.iter().enumerate() {
                if let Some(rec) = v.as_record() {
                    let k: Vec<Option<&Value>> = keys.iter().map(|k| rec.get(k)).collect();
                    if !seen.insert(k.clone()) {
                        self.keyed.push(self.issues.len());
                        self.path.push(index_segment(i));
                        self.issue(format!("duplicate @key ({})", keys.join(", ")));
                        self.path.pop();
                    }
                }
            }
        }

        out
    }

    /// Resolve a record against its definition: check declared fields, fill
    /// defaults if enabled, and report missing required fields and (unless
    /// `@open`) unknown ones.
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
                    Some(d) if self.pass.apply_defaults => {
                        let d = self.check(d.clone(), &f.ty);
                        out.set(f.name.clone(), d);
                    }
                    Some(_) => {}
                    None if f.optional || self.pass.partial => {}
                    None => self.issue("missing required field"),
                },
            }

            self.path.pop();
        }

        if !self.pass.partial {
            self.rules(&out, def);
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

    /// Report each record rule of `def` that the fields of `r` break
    /// (proposal 0010). A field counts as present unless it is absent or `null`.
    fn rules(&mut self, r: &Record, def: &RecordDef) {
        for rule in &def.rules {
            let present: Vec<&str> = rule
                .fields
                .iter()
                .filter(|f| r.get(f).is_some_and(|v| !matches!(v, Value::Null)))
                .map(String::as_str)
                .collect();
            if !rule.kind.allows(present.len(), rule.fields.len()) {
                let which = if present.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", present.join(", "))
                };
                self.issue(format!("{rule}: {} present{which}; expected {}", present.len(), rule.kind.expected()));
            }
        }
    }

    /// Resolve an enum value given as a bare tag string, a variant, or a
    /// single-entry `{tag: payload}` record.
    fn variant(&mut self, v: Value, vs: &'s [VariantDef]) -> Value {
        // `{tag: payload}` — how JSON and other formats spell a variant (§6.3).
        let v = match v {
            Value::Record(r) if self.pass.convert && r.len() == 1 && vs.iter().any(|d| d.payload.is_some() && r.contains_key(&d.name)) => {
                let (tag, payload) = r.into_entries().pop().unwrap();
                Value::Variant(tag, Some(Box::new(payload)))
            }
            other => other,
        };

        match v {
            Value::String(s) if self.pass.convert => match vs.iter().find(|d| d.name == s) {
                Some(d) if d.payload.is_none() => Value::Variant(s, None),
                Some(_) => {
                    let shown = self.shown(|| s.clone());
                    self.issue(format!("variant {shown} requires a payload"));
                    Value::String(s)
                }
                None => {
                    let shown = self.shown(|| format!("{s:?}"));
                    self.issue(format!("unknown variant {shown}; expected one of {}", names(vs)));
                    Value::String(s)
                }
            },
            Value::Variant(tag, payload) => match vs.iter().find(|d| d.name == tag) {
                None => {
                    let shown = self.shown(|| format!("{tag:?}"));
                    self.issue(format!("unknown variant {shown}; expected one of {}", names(vs)));
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
                        let shown = self.shown(|| tag.clone());
                        self.issue(format!("variant {shown} takes no payload"));
                        Value::Variant(tag, Some(p))
                    }
                    (Some(_), None) => {
                        let shown = self.shown(|| tag.clone());
                        self.issue(format!("variant {shown} requires a payload"));
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

    /// Check `v` against `@range` and `@len`; a constraint that does not apply
    /// to the value's kind passes.
    fn constraints(&mut self, v: &Value, c: &Constraints) {
        if let Some((lo, hi)) = &c.range
            && let Some(n) = Num::of(v)
        {
            // Exact across kinds; NaN never satisfies a range.
            if lo.as_ref().is_some_and(|lo| !lo.le(&n)) || hi.as_ref().is_some_and(|hi| !n.le(hi)) {
                let b = |o: &Option<Num>| o.as_ref().map(std::string::ToString::to_string).unwrap_or_default();
                let shown = self.shown(|| n.to_string());
                self.issue(format!("{shown} is outside @range({}..{})", b(lo), b(hi)));
            }
        }

        if let (Some(p), Value::String(s)) = (&c.pattern, v)
            && !p.is_match(s)
        {
            let shown = self.shown(|| novafile_text::quote(s));
            self.issue(format!("{shown} does not match @pattern({})", novafile_text::quote(p.as_str())));
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

/// How deeply [`Ctx::check`] may nest: well above the 128 levels documents
/// may nest (with a union at each), well below what overflows a 2 MiB stack.
const MAX_CHECK_DEPTH: usize = 384;

/// The number of values in `v`, counting every nested element, entry and payload.
fn nodes(v: &Value) -> usize {
    1 + match v {
        Value::List(l) | Value::Set(l) | Value::Tuple(l) => l.iter().map(nodes).sum(),
        Value::Map(m) => m.iter().map(|(k, x)| nodes(k) + nodes(x)).sum(),
        Value::Record(r) => r.iter().map(|(_, x)| nodes(x)).sum(),
        Value::Variant(_, Some(p)) => nodes(p),
        _ => 0,
    }
}

/// The path segment for element index `i`.
#[expect(clippy::cast_possible_wrap, reason = "an element index is at most isize::MAX")]
fn index_segment(i: usize) -> Segment {
    Segment::Index(i as i64)
}

/// The rational equal to `d`, or `None` if it does not fit in `i128`.
fn decimal_to_rational(d: Decimal) -> Option<Rational> {
    let (c, e) = (d.coefficient(), d.exponent());
    if e >= 0 {
        Rational::new(c.checked_mul(10i128.checked_pow(e.unsigned_abs())?)?, 1).ok()
    } else {
        Rational::new(c, 10i128.checked_pow(e.unsigned_abs())?).ok()
    }
}

/// Variant names joined as `a | b | c`, for messages.
fn names(vs: &[VariantDef]) -> String {
    vs.iter().map(|v| v.name.as_str()).collect::<Vec<_>>().join(" | ")
}

/// Whether resolving `v` against `kind` would convert it (spec §6.3) rather
/// than take it as it is.
fn converts(kind: &TypeKind, v: &Value) -> bool {
    matches!(
        (kind, v),
        (TypeKind::Float, Value::Int(_))
            | (TypeKind::Decimal, Value::Int(_) | Value::Float(_) | Value::String(_) | Value::Rational(_))
            | (TypeKind::Rational, Value::Int(_) | Value::Decimal(_) | Value::String(_))
            | (TypeKind::Bytes | TypeKind::Timestamp | TypeKind::Duration, Value::String(_))
            | (TypeKind::Set(_) | TypeKind::Tuple(_), Value::List(_))
            | (TypeKind::Map(..), Value::Record(_))
    )
}

impl Schema {
    /// Resolve `v` against type `t`: apply the permitted lexical conversions
    /// and defaults, and report every violation (spec §6.3).
    ///
    /// # Errors
    ///
    /// Returns every issue found if `v` does not conform to `t`.
    pub fn resolve_as(&self, v: &Value, t: &TypeRef) -> Result<Value, Vec<Issue>> {
        let (out, issues, _) = self.run(v, t, Pass::RESOLVE);
        if issues.is_empty() { Ok(out) } else { Err(issues) }
    }

    /// Check `v` against type `t` as it is: no conversion (spec §6.3) and no
    /// default applies, so the string `"80"` does not match `u16`. Template
    /// function arguments are checked this way, since they are values already.
    ///
    /// # Errors
    ///
    /// Returns every issue found if `v` does not conform to `t`.
    pub fn check_as(&self, v: &Value, t: &TypeRef) -> Result<(), Vec<Issue>> {
        let (_, issues, _) = self.run(v, t, Pass::CHECK);
        if issues.is_empty() { Ok(()) } else { Err(issues) }
    }

    /// One resolution or validation pass of `v` against `t`.
    fn run(&self, v: &Value, t: &TypeRef, pass: Pass) -> (Value, Vec<Issue>, Vec<ClassMark>) {
        let mut c = Ctx {
            schema: self,
            path: Vec::new(),
            issues: Vec::new(),
            keyed: Vec::new(),
            depth: 0,
            pass,
            fuel: nodes(v).saturating_mul(256).saturating_add(4096),
            classes: Vec::new(),
            inside: None,
        };

        let out = c.check(v.clone(), t);
        if c.fuel == 0 {
            c.issues.push(Issue {
                path: Path::root(),
                message: "resolution needs too much work; union members probably overlap".into(),
            });
        }

        let classes = c.classes.into_iter().map(|(p, class)| (Path(p), class)).collect();
        (out, c.issues, classes)
    }

    /// One pass of `v` against the `@root` type.
    ///
    /// # Errors
    ///
    /// Returns every issue found if `v` does not conform, or a single issue if
    /// the schema has no `@root`.
    fn run_root(&self, v: &Value, pass: Pass) -> Result<(Value, Vec<ClassMark>), Vec<Issue>> {
        let t = self.root_type().ok_or_else(|| {
            vec![Issue {
                path: Path::root(),
                message: "schema has no @root".into(),
            }]
        })?;

        let (out, issues, classes) = self.run(v, &t, pass);
        if issues.is_empty() { Ok((out, classes)) } else { Err(issues) }
    }

    /// Resolve against the `@root` type.
    ///
    /// # Errors
    ///
    /// Returns every issue found if `v` does not conform, or a single issue if
    /// the schema has no `@root`.
    pub fn resolve(&self, v: &Value) -> Result<Value, Vec<Issue>> {
        self.run_root(v, Pass::RESOLVE).map(|(out, _)| out)
    }

    /// Validate without materializing defaults.
    ///
    /// # Errors
    ///
    /// Returns every issue found if `v` does not conform, or a single issue if
    /// the schema has no `@root`.
    pub fn validate(&self, v: &Value) -> Result<(), Vec<Issue>> {
        self.run_root(v, Pass::VALIDATE).map(|_| ())
    }

    /// Resolve one layer of a layered configuration against the `@root` type
    /// (proposal 0010). Every record field is optional, as in TypeScript's
    /// `Partial<T>` applied deeply, and record rules (`@together`, `@one_of`,
    /// `@any_of`) are not checked. Types, patterns, ranges, lengths and closed
    /// records are checked as usual, and conversions apply. Defaults are not
    /// filled in, since they would override lower layers when the layers are
    /// merged. Resolve the merged value with [`Schema::resolve`].
    ///
    /// A union of records takes the first member that the present fields fit,
    /// since no member fails for a missing field.
    ///
    /// # Errors
    ///
    /// Returns every issue found if `v` does not conform, or a single issue if
    /// the schema has no `@root`.
    pub fn resolve_partial(&self, v: &Value) -> Result<Value, Vec<Issue>> {
        self.run_root(v, Pass::PARTIAL).map(|(out, _)| out)
    }

    /// Resolve `v` against the `@root` type like [`Schema::resolve`], and also
    /// return the classified values (proposal 0009): the path of every value
    /// that a `@personal` or `@secret` type classifies, with that class. A
    /// path may lie below another one; the outer class then applies too.
    ///
    /// # Errors
    ///
    /// As [`Schema::resolve`].
    pub fn classify(&self, v: &Value) -> Result<(Value, Vec<ClassMark>), Vec<Issue>> {
        self.run_root(v, Pass::RESOLVE)
    }

    /// [`Schema::classify`] for one layer of a layered configuration: the
    /// value as [`Schema::resolve_partial`] gives it, and its classified values.
    ///
    /// # Errors
    ///
    /// As [`Schema::resolve_partial`].
    pub fn classify_partial(&self, v: &Value) -> Result<(Value, Vec<ClassMark>), Vec<Issue>> {
        self.run_root(v, Pass::PARTIAL)
    }

    /// The classified paths of `v` in its own coordinates, as stored: set
    /// elements keep their source order and nothing is converted. Sinks that
    /// show or compare the stored value (queries, diffs, patch conflicts,
    /// editors) redact `v` itself with these paths; [`Schema::classify`]
    /// gives paths into the resolved value instead (proposal 0009).
    ///
    /// # Errors
    ///
    /// As [`Schema::resolve`].
    pub fn classify_source(&self, v: &Value) -> Result<Vec<ClassMark>, Vec<Issue>> {
        self.run_root(v, Pass::SOURCE).map(|(_, classes)| classes)
    }
}

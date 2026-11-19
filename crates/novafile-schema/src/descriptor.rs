//! Schemas as NOVA data (spec §6.8).
//!
//! [`Schema::to_value`] produces the *descriptor* of a schema: an ordinary
//! NOVA value described by the meta-schema [`meta_schema`]
//! (`nova-schema@0.3.2`). [`Schema::from_value`] turns a descriptor back
//! into a schema by rendering its canonical text and parsing that, so
//! descriptors are validated exactly like hand-written schemas.

use crate::model::{Def, FieldDef, IntKind, LenUnit, Num, RecordDef, Schema, TypeKind, TypeRef, VariantDef};
use novafile_value::{Record, Value};

/// Source of the meta-schema.
pub const META_SCHEMA_SRC: &str = include_str!("../meta/nova-schema.novs");

/// The meta-schema, which validates descriptors (including its own).
///
/// # Panics
///
/// Panics if the embedded meta-schema source fails to parse (a build defect
/// that the test suite rules out).
#[must_use]
pub fn meta_schema() -> Schema {
    crate::parse_schema(META_SCHEMA_SRC).expect("the meta-schema is valid")
}

/// A record value with the given entries, in order.
fn rec(entries: Vec<(&str, Value)>) -> Value {
    let mut r = Record::new();
    for (k, v) in entries {
        r.set(k, v);
    }

    Value::Record(r)
}

/// A string value.
fn s(x: &str) -> Value {
    Value::String(x.to_owned())
}

/// A variant value without payload, as used for enum tags such as `kind`.
fn tag(x: &str) -> Value {
    Value::Variant(x.to_owned(), None)
}

/// The descriptor of a type expression (meta-schema record `Type`).
fn type_value(t: &TypeRef) -> Value {
    let mut e: Vec<(&str, Value)> = Vec::new();
    let kind = match &t.kind {
        TypeKind::Any => "any",
        TypeKind::Null => "null",
        TypeKind::Bool => "bool",
        TypeKind::Int(k) => k.name(),
        TypeKind::Float => "float",
        TypeKind::Decimal => "decimal",
        TypeKind::Rational => "rational",
        TypeKind::String => "string",
        TypeKind::Bytes => "bytes",
        TypeKind::Timestamp => "timestamp",
        TypeKind::Duration => "duration",
        TypeKind::List(_) => "list",
        TypeKind::Set(_) => "set",
        TypeKind::Map(..) => "map",
        TypeKind::Tuple(_) => "tuple",
        TypeKind::Named(_) => "named",
        TypeKind::Union(_) => "union",
        TypeKind::Literal(_) => "literal",
    };

    e.push(("kind", tag(kind)));
    match &t.kind {
        TypeKind::Named(n) => e.push(("ref", s(n))),
        TypeKind::Literal(l) => e.push(("literal", s(l))),
        TypeKind::List(i) | TypeKind::Set(i) => e.push(("item", type_value(i))),
        TypeKind::Map(k, v) => {
            e.push(("key", type_value(k)));
            e.push(("value", type_value(v)));
        }
        TypeKind::Tuple(ts) | TypeKind::Union(ts) => e.push(("items", Value::List(ts.iter().map(type_value).collect()))),
        _ => {}
    }

    if t.nullable {
        e.push(("nullable", Value::Bool(true)));
    }

    if let Some((lo, hi)) = &t.constraints.range {
        let mut r = Vec::new();
        if let Some(lo) = lo {
            r.push(("min", lo.0.clone()));
        }

        if let Some(hi) = hi {
            r.push(("max", hi.0.clone()));
        }

        e.push(("range", rec(r)));
    }

    if let Some((lo, hi)) = t.constraints.len {
        let mut l = Vec::new();
        if let Some(lo) = lo {
            l.push(("min", Value::Int(i128::from(lo))));
        }

        if let Some(hi) = hi {
            l.push(("max", Value::Int(i128::from(hi))));
        }

        if t.constraints.len_unit != LenUnit::Scalars {
            l.push(("unit", tag(t.constraints.len_unit.name())));
        }

        e.push(("len", rec(l)));
    }

    if let Some(p) = &t.constraints.pattern {
        e.push(("pattern", s(p.as_str())));
    }

    rec(e)
}

impl Schema {
    /// The descriptor of this schema (definitions in canonical, name order).
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut top: Vec<(&str, Value)> = Vec::new();
        if let Some(n) = &self.name {
            top.push(("name", s(n)));
        }

        if let Some(v) = &self.version {
            top.push(("version", Value::String(v.to_string())));
        }

        if let Some(r) = &self.root {
            top.push(("root", type_value(r)));
        }

        let mut defs: Vec<&(String, Def)> = self.defs.iter().collect();
        defs.sort_by(|a, b| a.0.cmp(&b.0));
        let defs = defs
            .into_iter()
            .map(|(name, def)| {
                let body = match def {
                    Def::Alias(t) => ("alias", type_value(t)),
                    Def::Enum(vs) => (
                        "enum",
                        Value::List(
                            vs.iter()
                                .map(|v| {
                                    let mut e = vec![("name", s(&v.name))];
                                    if let Some(p) = &v.payload {
                                        e.push(("payload", type_value(p)));
                                    }

                                    if let Some(id) = v.id {
                                        e.push(("id", Value::Int(i128::from(id))));
                                    }

                                    rec(e)
                                })
                                .collect(),
                        ),
                    ),
                    Def::Record(r) => {
                        let fields = r
                            .fields
                            .iter()
                            .map(|f| {
                                let mut e = vec![("name", s(&f.name)), ("type", type_value(&f.ty))];
                                if f.optional {
                                    e.push(("optional", Value::Bool(true)));
                                }

                                if let Some(d) = &f.default {
                                    e.push(("default", d.clone()));
                                }

                                if f.key {
                                    e.push(("key", Value::Bool(true)));
                                }

                                if let Some(id) = f.id {
                                    e.push(("id", Value::Int(i128::from(id))));
                                }

                                if f.deprecated {
                                    e.push(("deprecated", Value::Bool(true)));
                                }

                                rec(e)
                            })
                            .collect();
                        let mut e = Vec::new();
                        if r.open {
                            e.push(("open", Value::Bool(true)));
                        }

                        e.push(("fields", Value::List(fields)));
                        ("record", rec(e))
                    }
                };

                rec(vec![("name", s(name)), body])
            })
            .collect();
        top.push(("defs", Value::List(defs)));
        rec(top)
    }

    /// Rebuild a schema from its descriptor. The descriptor is first resolved
    /// against the meta-schema, then checked like schema text.
    ///
    /// # Errors
    ///
    /// Fails if the descriptor does not conform to the meta-schema, has a
    /// malformed definition, or describes an invalid schema (bad names,
    /// references, ids, cycles or defaults).
    pub fn from_value(v: &Value) -> Result<Schema, String> {
        let v = meta_schema().resolve(v).map_err(|issues| {
            format!(
                "invalid schema descriptor:\n  {}",
                issues.iter().map(std::string::ToString::to_string).collect::<Vec<_>>().join("\n  ")
            )
        })?;
        let r = v.as_record().ok_or("descriptor must be a record")?;
        let budget = std::cell::Cell::new(crate::pattern::SCHEMA_BUDGET);
        let value_type = |t: &Value| value_type(t, &budget);
        let text = |k: &str| r.get(k).and_then(Value::as_str).map(str::to_owned);
        let mut schema = Schema {
            name: text("name"),
            version: None,
            root: r.get("root").map(value_type).transpose()?,
            defs: Vec::new(),
            extensions: Vec::new(),
        };

        if let Some(ver) = text("version") {
            schema.version = Some(crate::Version::parse(&ver)?);
        }

        for d in r.get("defs").and_then(Value::as_list).into_iter().flatten() {
            let d = d.as_record().ok_or("definition must be a record")?;
            let name = d.get("name").and_then(Value::as_str).ok_or("definition without name")?.to_owned();
            let at = |what: &str| format!("defs[name={name}]: {what}");
            let bodies = ["record", "enum", "alias"].iter().filter(|k| d.contains_key(k)).count();
            if bodies != 1 {
                return Err(at("needs exactly one of `record`, `enum` and `alias`"));
            }

            let def = if let Some(t) = d.get("alias") {
                Def::Alias(value_type(t).map_err(|e| at(&e))?)
            } else if let Some(Value::List(vs)) = d.get("enum") {
                Def::Enum(
                    vs.iter()
                        .map(|v| {
                            let v = v.as_record().ok_or("variant must be a record")?;
                            Ok(VariantDef {
                                name: v.get("name").and_then(Value::as_str).ok_or("variant without name")?.to_owned(),
                                payload: v.get("payload").map(value_type).transpose()?,
                                id: match v.get("id") {
                                    Some(Value::Int(id)) => Some(u32::try_from(*id).map_err(|_| "id out of range")?),
                                    _ => None,
                                },
                            })
                        })
                        .collect::<Result<_, String>>()
                        .map_err(|e| at(&e))?,
                )
            } else {
                let rd = d.get("record").and_then(Value::as_record).ok_or_else(|| at("`record` must be a record"))?;
                let flag = |r: &Record, k: &str| matches!(r.get(k), Some(Value::Bool(true)));
                let fields = rd
                    .get("fields")
                    .and_then(Value::as_list)
                    .into_iter()
                    .flatten()
                    .map(|f| {
                        let f = f.as_record().ok_or("field must be a record")?;
                        Ok(FieldDef {
                            name: f.get("name").and_then(Value::as_str).ok_or("field without name")?.to_owned(),
                            ty: value_type(f.get("type").ok_or("field without type")?)?,
                            optional: flag(f, "optional"),
                            default: f.get("default").cloned(),
                            key: flag(f, "key"),
                            id: match f.get("id") {
                                Some(Value::Int(id)) => Some(u32::try_from(*id).map_err(|_| "id out of range")?),
                                _ => None,
                            },
                            deprecated: flag(f, "deprecated"),
                        })
                    })
                    .collect::<Result<_, String>>()
                    .map_err(|e| at(&e))?;
                Def::Record(RecordDef {
                    fields,
                    open: flag(rd, "open"),
                })
            };

            schema.defs.push((name, def));
        }

        // Validate exactly like schema text (names, references, ids, cycles, defaults).
        crate::parse_schema(&schema.to_canonical_text()).map_err(|e| format!("invalid schema descriptor: {}", e.message))
    }
}

/// Read a type expression back from its descriptor; the inverse of [`type_value`].
fn value_type(v: &Value, budget: &std::cell::Cell<usize>) -> Result<TypeRef, String> {
    let r = v.as_record().ok_or("type must be a record")?;
    let kind = match r.get("kind") {
        Some(Value::Variant(k, None) | Value::String(k)) => k.as_str(),
        _ => return Err("type without kind".into()),
    };

    let sub = |k: &str| -> Result<Box<TypeRef>, String> { Ok(Box::new(value_type(r.get(k).ok_or_else(|| format!("{kind} type needs `{k}`"))?, budget)?)) };
    let tk = match kind {
        "any" => TypeKind::Any,
        "null" => TypeKind::Null,
        "bool" => TypeKind::Bool,
        "float" => TypeKind::Float,
        "decimal" => TypeKind::Decimal,
        "rational" => TypeKind::Rational,
        "string" => TypeKind::String,
        "bytes" => TypeKind::Bytes,
        "timestamp" => TypeKind::Timestamp,
        "duration" => TypeKind::Duration,
        "list" => TypeKind::List(sub("item")?),
        "set" => TypeKind::Set(sub("item")?),
        "map" => TypeKind::Map(sub("key")?, sub("value")?),
        "tuple" | "union" => {
            let items: Vec<TypeRef> = r
                .get("items")
                .and_then(Value::as_list)
                .ok_or_else(|| format!("{kind} type needs `items`"))?
                .iter()
                .map(|t| value_type(t, budget))
                .collect::<Result<_, _>>()?;
            if kind == "tuple" {
                TypeKind::Tuple(items)
            } else {
                // Schema text cannot express these, so the canonical text would change meaning.
                if items.len() < 2 {
                    return Err("a union needs at least two `items`".into());
                }

                if matches!(r.get("nullable"), Some(Value::Bool(true))) || ["range", "len", "pattern"].iter().any(|k| r.contains_key(k)) {
                    return Err("a union carries no `nullable`, `range`, `len` or `pattern`; its members do".into());
                }

                if items.iter().any(|m| matches!(m.kind, TypeKind::Union(_))) {
                    return Err("a union member cannot be a union; list its members in place".into());
                }

                TypeKind::Union(items)
            }
        }
        "literal" => {
            if ["range", "len", "pattern"].iter().any(|k| r.contains_key(k)) {
                return Err("a literal type carries no `range`, `len` or `pattern`".into());
            }

            TypeKind::Literal(r.get("literal").and_then(Value::as_str).ok_or("literal type needs `literal`")?.to_owned())
        }
        "named" => TypeKind::Named(r.get("ref").and_then(Value::as_str).ok_or("named type needs `ref`")?.to_owned()),
        other => TypeKind::Int(IntKind::from_name(other).ok_or_else(|| format!("unknown kind {other:?}"))?),
    };

    if kind != "literal" && r.contains_key("literal") {
        return Err(format!("only a literal type has `literal`, not {kind}"));
    }

    let mut t = TypeRef::new(tk);
    t.nullable = matches!(r.get("nullable"), Some(Value::Bool(true)));
    if let Some(Value::Record(rg)) = r.get("range") {
        let bound = |k: &str| rg.get(k).map(|v| Num::of(v).ok_or_else(|| format!("range {k} must be a number"))).transpose();
        t.constraints.range = Some((bound("min")?, bound("max")?));
    }

    if let Some(Value::Record(l)) = r.get("len") {
        let n = |k: &str| match l.get(k) {
            Some(Value::Int(i)) => u64::try_from(*i).map(Some).map_err(|_| format!("len {k} out of range")),
            _ => Ok(None),
        };

        t.constraints.len = Some((n("min")?, n("max")?));
        if let Some(Value::Variant(u, None)) = l.get("unit") {
            t.constraints.len_unit = LenUnit::from_name(u).ok_or("unknown len unit")?;
        }
    }

    if let Some(p) = r.get("pattern") {
        if kind != "string" {
            return Err(format!("`pattern` applies to string, not {kind}"));
        }

        let p = p.as_str().ok_or("pattern must be a string")?;
        t.constraints.pattern = Some(crate::Pattern::new_within(p, budget).map_err(|e| format!("pattern {}: {e}", novafile_text::quote(p)))?);
    }

    Ok(t)
}

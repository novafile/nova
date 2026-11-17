//! NOVA schema → JSON Schema (2020-12). See the crate documentation for the mapping.

use novafile_schema::{Constraints, Def, FieldDef, IntKind, LenUnit, Num, RecordDef, Schema, TypeKind, TypeRef, VariantDef};
use novafile_value::Value;
use novafile_value::schemars_support::patterns;
use serde_json::{Map, Value as Json, json};

/// The meta-schema URI written as `$schema`.
pub const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// Pattern for the string form of non-finite floats written by `novafile-json`.
const NON_FINITE: &str = "^(NaN|-?inf)$";

/// Convert a NOVA schema to a JSON Schema (2020-12) describing the JSON that
/// `novafile-json` writes for resolved documents (and `serde_json` for the
/// corresponding Rust types).
///
/// Named definitions go to `$defs`, the `@root` becomes a top-level `$ref`,
/// and a named schema gets `$id: urn:nova:name@version` and a matching
/// `title`. Constraints JSON Schema cannot check are kept as `x-nova-*`
/// annotations so that [`crate::from_json_schema`] restores them.
#[must_use]
pub fn to_json_schema(schema: &Schema) -> Json {
    let mut root = Map::new();
    root.insert("$schema".into(), DRAFT_2020_12.into());
    if let Some(r) = schema.reference() {
        root.insert("$id".into(), format!("urn:nova:{r}").into());
        root.insert("title".into(), r.to_string().into());
    }
    if let Some(r) = &schema.root {
        root.insert("$ref".into(), def_ref(r).into());
    }
    let mut defs = Map::new();
    for (name, def) in &schema.defs {
        let s = match def {
            Def::Alias(t) => ty(schema, t),
            Def::Record(r) => record(schema, r),
            Def::Enum(vs) => variants(schema, vs),
        };
        defs.insert(name.clone(), s);
    }
    if !defs.is_empty() {
        root.insert("$defs".into(), Json::Object(defs));
    }
    Json::Object(root)
}

fn def_ref(name: &str) -> String {
    format!("#/$defs/{name}")
}

fn record(schema: &Schema, r: &RecordDef) -> Json {
    let mut props = Map::new();
    for f in &r.fields {
        props.insert(f.name.clone(), field(schema, f));
    }
    // A field with a default may be absent from unresolved documents.
    let required: Vec<&str> = r
        .fields
        .iter()
        .filter(|f| !f.optional && f.default.is_none())
        .map(|f| f.name.as_str())
        .collect();
    let mut o = Map::new();
    o.insert("type".into(), "object".into());
    o.insert("properties".into(), Json::Object(props));
    if !required.is_empty() {
        o.insert("required".into(), json!(required));
    }
    o.insert("additionalProperties".into(), Json::Bool(r.open));
    let names: Vec<&str> = r.fields.iter().map(|f| f.name.as_str()).collect();
    if names.windows(2).any(|w| w[0] > w[1]) {
        // JSON objects are unordered; keep the declared order for the round trip.
        o.insert("x-nova-order".into(), json!(names));
    }
    Json::Object(o)
}

fn field(schema: &Schema, f: &FieldDef) -> Json {
    let s = ty(schema, &f.ty);
    if f.default.is_none() && !f.deprecated && f.id.is_none() && !f.key {
        return s;
    }
    let mut o = match s {
        Json::Object(o) => o,
        _ => Map::new(),
    };
    if let Some(d) = &f.default {
        o.insert("default".into(), json_of(d));
    }
    if f.deprecated {
        o.insert("deprecated".into(), true.into());
    }
    if let Some(id) = f.id {
        o.insert("x-nova-id".into(), id.into());
    }
    if f.key {
        o.insert("x-nova-key".into(), true.into());
        o.insert(
            "description".into(),
            "Key field (@key): values must be unique among the elements of a list (not checked by JSON Schema).".into(),
        );
    }
    Json::Object(o)
}

/// An enum as serde's externally tagged form: runs of unit variants become
/// `{"type": "string", "enum": [...]}`, payload variants single-key objects.
fn variants(schema: &Schema, vs: &[VariantDef]) -> Json {
    let mut alts: Vec<Json> = Vec::new();
    let mut units: Vec<&str> = Vec::new();
    let flush = |units: &mut Vec<&str>, alts: &mut Vec<Json>| {
        if !units.is_empty() {
            alts.push(json!({"type": "string", "enum": units}));
            units.clear();
        }
    };
    for v in vs {
        match &v.payload {
            None => units.push(&v.name),
            Some(p) => {
                flush(&mut units, &mut alts);
                let mut props = Map::new();
                props.insert(v.name.clone(), ty(schema, p));
                alts.push(json!({
                    "type": "object",
                    "properties": props,
                    "required": [v.name],
                    "additionalProperties": false
                }));
            }
        }
    }
    flush(&mut units, &mut alts);
    match <[Json; 1]>::try_from(alts) {
        Ok([one]) => one,
        Err(alts) => json!({ "oneOf": alts }),
    }
}

/// The JSON Schema for a type at a use site.
fn ty(schema: &Schema, t: &TypeRef) -> Json {
    let mut s = kind(schema, &t.kind);
    constrain(schema, t, &mut s);
    if t.nullable {
        s = nullable(s);
    }
    s
}

fn nullable(s: Json) -> Json {
    match s {
        Json::Bool(true) => s,
        Json::Object(mut o) => match o.get_mut("type") {
            Some(Json::String(t)) if t != "null" => {
                let t = std::mem::take(t);
                o.insert("type".into(), json!([t, "null"]));
                Json::Object(o)
            }
            Some(Json::Array(ts)) => {
                if !ts.iter().any(|t| t == "null") {
                    ts.push("null".into());
                }
                Json::Object(o)
            }
            Some(_) => Json::Object(o),
            None => json!({"anyOf": [o, {"type": "null"}]}),
        },
        other => json!({"anyOf": [other, {"type": "null"}]}),
    }
}

fn int_format(k: IntKind) -> Option<&'static str> {
    Some(match k {
        IntKind::Int => return None,
        IntKind::I8 => "int8",
        IntKind::I16 => "int16",
        IntKind::I32 => "int32",
        IntKind::I64 => "int64",
        IntKind::U8 => "uint8",
        IntKind::U16 => "uint16",
        IntKind::U32 => "uint32",
        IntKind::U64 => "uint64",
    })
}

fn int_json(i: i128) -> Json {
    if let Ok(v) = i64::try_from(i) {
        v.into()
    } else if let Ok(v) = u64::try_from(i) {
        v.into()
    } else {
        #[expect(clippy::cast_precision_loss, reason = "beyond 64 bits JSON has only binary64 numbers")]
        let f = i as f64;
        f.into()
    }
}

fn kind(schema: &Schema, k: &TypeKind) -> Json {
    match k {
        TypeKind::Any => Json::Bool(true),
        TypeKind::Null => json!({"type": "null"}),
        TypeKind::Bool => json!({"type": "boolean"}),
        TypeKind::Int(k) => {
            let mut o = json!({"type": "integer"});
            if let Some(f) = int_format(*k) {
                let (lo, hi) = k.bounds();
                o["format"] = f.into();
                o["minimum"] = int_json(lo);
                o["maximum"] = int_json(hi);
            }
            o
        }
        TypeKind::Float => json!({"type": ["number", "string"], "pattern": NON_FINITE, "x-nova-kind": "float"}),
        TypeKind::Decimal => json!({"type": "string", "pattern": patterns::DECIMAL, "x-nova-kind": "decimal"}),
        TypeKind::Rational => json!({"type": "string", "pattern": patterns::RATIONAL, "x-nova-kind": "rational"}),
        TypeKind::String => json!({"type": "string"}),
        TypeKind::Bytes => json!({"type": "string", "contentEncoding": "base64", "x-nova-kind": "bytes"}),
        TypeKind::Timestamp => json!({"type": "string", "format": "date-time", "x-nova-kind": "timestamp"}),
        TypeKind::Duration => json!({"type": "string", "pattern": patterns::DURATION, "x-nova-kind": "duration"}),
        TypeKind::List(t) => json!({"type": "array", "items": ty(schema, t)}),
        TypeKind::Set(t) => json!({"type": "array", "items": ty(schema, t), "uniqueItems": true}),
        TypeKind::Tuple(ts) => {
            let items: Vec<Json> = ts.iter().map(|t| ty(schema, t)).collect();
            json!({"type": "array", "prefixItems": items, "items": false, "minItems": ts.len()})
        }
        TypeKind::Map(k, v) => {
            // novafile-json writes a map value as an array of [key, value] pairs.
            let pair = json!({"type": "array", "prefixItems": [ty(schema, k), ty(schema, v)], "items": false, "minItems": 2});
            let pairs = json!({"type": "array", "items": pair});
            if matches!(schema.deref(k).0.kind, TypeKind::String) {
                // A string-keyed map is also written as an object: by serde_json,
                // and by novafile-json for text documents, where it is a record until resolved.
                let mut object = json!({"type": "object", "additionalProperties": ty(schema, v)});
                if k.constraints != Constraints::default() || matches!(k.kind, TypeKind::Named(_)) {
                    object["propertyNames"] = ty(schema, k);
                }
                json!({"anyOf": [object, pairs], "x-nova-kind": "map"})
            } else {
                let mut pairs = pairs;
                pairs["x-nova-kind"] = "map".into();
                pairs
            }
        }
        TypeKind::Named(n) => json!({"$ref": def_ref(n)}),
    }
}

/// What a constraint applies to, after following aliases.
enum Target {
    Int(Option<(i128, i128)>),
    Float,
    ExactNumber,
    Text(bool),
    Items,
    Properties,
    /// A string-keyed map: an object or an array of pairs.
    ItemsOrProperties,
    Other,
}

fn target(schema: &Schema, t: &TypeRef) -> Target {
    let (rt, def) = schema.deref(t);
    match (&rt.kind, def) {
        (TypeKind::Int(k), _) => Target::Int((*k != IntKind::Int && !matches!(t.kind, TypeKind::Named(_))).then(|| k.bounds())),
        (TypeKind::Float, _) => Target::Float,
        (TypeKind::Decimal | TypeKind::Rational, _) => Target::ExactNumber,
        (TypeKind::String, _) => Target::Text(true),
        (TypeKind::Bytes, _) => Target::Text(false),
        (TypeKind::Map(k, _), _) if matches!(schema.deref(k).0.kind, TypeKind::String) => Target::ItemsOrProperties,
        (TypeKind::List(_) | TypeKind::Set(_) | TypeKind::Map(..), _) => Target::Items,
        (_, Some(Def::Record(_))) => Target::Properties,
        _ => Target::Other,
    }
}

fn constrain(schema: &Schema, t: &TypeRef, s: &mut Json) {
    let c = &t.constraints;
    if *c == Constraints::default() {
        return;
    }
    let Json::Object(obj) = s else { return };
    let target = target(schema, t);
    if let Some((lo, hi)) = &c.range {
        match target {
            Target::Int(bounds) => {
                if let Some(lo) = lo.as_ref().and_then(|n| int_bound(n, true)) {
                    let lo = bounds.map_or(lo, |(klo, _)| lo.max(klo));
                    obj.insert("minimum".into(), int_json(lo));
                }
                if let Some(hi) = hi.as_ref().and_then(|n| int_bound(n, false)) {
                    let hi = bounds.map_or(hi, |(_, khi)| hi.min(khi));
                    obj.insert("maximum".into(), int_json(hi));
                }
            }
            Target::Float => {
                if let Some(lo) = lo {
                    obj.insert("minimum".into(), num_json(lo));
                }
                if let Some(hi) = hi {
                    obj.insert("maximum".into(), num_json(hi));
                }
            }
            _ => {
                let bound = |n: &Option<Num>| n.as_ref().map(std::string::ToString::to_string).unwrap_or_default();
                obj.insert("x-nova-range".into(), format!("{}..{}", bound(lo), bound(hi)).into());
            }
        }
    }
    if let Some((lo, hi)) = c.len {
        // Each keyword only constrains instances of its own JSON type.
        let keys: &[(&str, &str)] = match target {
            Target::Text(true) if c.len_unit == LenUnit::Scalars => &[("minLength", "maxLength")],
            Target::Items => &[("minItems", "maxItems")],
            Target::Properties => &[("minProperties", "maxProperties")],
            Target::ItemsOrProperties => &[("minProperties", "maxProperties"), ("minItems", "maxItems")],
            _ => &[],
        };
        if let [_, ..] = keys {
            for (min, max) in keys {
                if let Some(lo) = lo {
                    obj.insert((*min).into(), lo.into());
                }
                if let Some(hi) = hi {
                    obj.insert((*max).into(), hi.into());
                }
            }
        } else {
            let bound = |n: Option<u64>| n.map(|n| n.to_string()).unwrap_or_default();
            let unit = if c.len_unit == LenUnit::Scalars {
                String::new()
            } else {
                format!(", {}", c.len_unit.name())
            };
            obj.insert("x-nova-len".into(), format!("{}..{}{unit}", bound(lo), bound(hi)).into());
        }
    }
}

/// The tightest integer bound implied by an exact numeric bound.
#[expect(clippy::cast_possible_truncation, reason = "float-to-int `as` saturates, clamping out-of-range bounds")]
fn int_bound(n: &Num, lower: bool) -> Option<i128> {
    match &n.0 {
        Value::Int(i) => Some(*i),
        Value::Decimal(d) => d.to_i128().or_else(|| {
            let f = d.to_f64();
            Some(if lower { f.ceil() } else { f.floor() } as i128)
        }),
        Value::Rational(r) => {
            let (q, rem) = (r.numerator().div_euclid(r.denominator()), r.numerator().rem_euclid(r.denominator()));
            Some(if lower && rem != 0 { q + 1 } else { q })
        }
        Value::Float(f) => Some(if lower { f.ceil() } else { f.floor() } as i128),
        _ => None,
    }
}

/// A bound as a JSON number (decimals and rationals via binary64).
fn num_json(n: &Num) -> Json {
    match &n.0 {
        Value::Int(i) => int_json(*i),
        Value::Decimal(d) => d.to_i128().map_or_else(|| d.to_f64().into(), int_json),
        Value::Rational(r) => r.to_f64().into(),
        Value::Float(f) => (*f).into(),
        _ => Json::Null,
    }
}

/// A NOVA value as JSON, via `novafile-json`.
fn json_of(v: &Value) -> Json {
    let (text, _) = novafile_json::to_json_string(v, false);
    serde_json::from_str(&text).unwrap_or(Json::Null)
}

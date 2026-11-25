//! NOVA schema → JSON Schema (2020-12). See the crate documentation for the mapping.

use novafile_schema::{Class, Constraints, Def, FieldDef, IntKind, LenUnit, Num, RecordDef, Schema, TypeKind, TypeRef, VariantDef};
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
/// and a named schema gets `x-nova-schema: name@version` and a matching
/// `title`; a qualified name also gets `$id: https://{name}/{version}.schema.json`
/// (spec §11.2), while a local name has no global identity to state. Constraints JSON Schema cannot check are kept as `x-nova-*`
/// annotations so that [`crate::from_json_schema`] restores them.
#[must_use]
pub fn to_json_schema(schema: &Schema) -> Json {
    let mut root = Map::new();
    root.insert("$schema".into(), DRAFT_2020_12.into());
    if let Some(r) = schema.reference() {
        if novafile_schema::schema_name_kind(&r.name) == Some(novafile_schema::NameKind::Qualified) {
            root.insert("$id".into(), format!("https://{}/{}.schema.json", r.name, r.version).into());
        }

        root.insert("title".into(), r.to_string().into());
        root.insert("x-nova-schema".into(), r.to_string().into());
    }

    if let Some(r) = &schema.root {
        // A named root is a `$ref`; any other root type is written inline.
        if let Json::Object(o) = ty(schema, r) {
            root.extend(o);
        }
    }

    let mut defs = Map::new();
    let used = used_imports(schema);
    let imported = schema.imported.iter().filter(|(n, _)| used.contains(n.as_str()));
    for (name, def) in schema.defs.iter().chain(imported) {
        let mut s = match def {
            Def::Alias(t) => ty(schema, t),
            Def::Record(r) => record(schema, r),
            Def::Enum(vs) => variants(schema, vs),
        };

        if !matches!(def, Def::Alias(_)) {
            classify(&mut s, schema.def_class(name));
        }

        if let Some(std) = std_type(schema, name) {
            mark_std(&mut s, std);
        }

        defs.insert(name.clone(), s);
    }

    if !defs.is_empty() {
        root.insert("$defs".into(), Json::Object(defs));
    }

    Json::Object(root)
}

/// The names of the imported definitions that the root and the local
/// definitions use, directly or through other definitions.
fn used_imports(schema: &Schema) -> std::collections::HashSet<&str> {
    let mut seen = std::collections::HashSet::new();
    let mut todo: Vec<&TypeRef> = schema.root.iter().collect();
    for (_, d) in &schema.defs {
        def_types(d, &mut todo);
    }

    while let Some(t) = todo.pop() {
        match &t.kind {
            TypeKind::Named(n) => {
                if seen.insert(n.as_str())
                    && let Some(d) = schema.def(n)
                {
                    def_types(d, &mut todo);
                }
            }
            TypeKind::List(e) | TypeKind::Set(e) => todo.push(e),
            TypeKind::Map(k, v) => todo.extend([&**k, &**v]),
            TypeKind::Tuple(ts) | TypeKind::Union(ts) => todo.extend(ts),
            _ => {}
        }
    }

    seen
}

/// Push the type expressions that the definition `d` contains.
fn def_types<'a>(d: &'a Def, out: &mut Vec<&'a TypeRef>) {
    match d {
        Def::Alias(t) => out.push(t),
        Def::Enum(vs) => out.extend(vs.iter().filter_map(|v| v.payload.as_ref())),
        Def::Record(r) => out.extend(r.fields.iter().map(|f| &f.ty)),
    }
}

/// The type name in the standard library (`Uuid`) of the imported
/// definition `name` (`std.Uuid`), if its alias imports the standard library.
fn std_type<'a>(schema: &Schema, name: &'a str) -> Option<&'a str> {
    let (alias, ty) = name.split_once('.')?;
    let std = schema.imports.iter().any(|i| i.alias == alias && i.reference.name == novafile_schema::STD_NAME);
    (std && !ty.contains('.')).then_some(ty)
}

/// Add `x-nova-type` to the schema of the standard library type `ty`, and
/// the JSON Schema `format` that matches it, if there is one (proposal 0008).
fn mark_std(s: &mut Json, ty: &str) {
    let Json::Object(o) = s else { return };
    let format = match ty {
        "Uuid" => Some("uuid"),
        "Uri" => Some("uri"),
        "UriReference" => Some("uri-reference"),
        "Email" => Some("email"),
        _ => None,
    };

    if let Some(f) = format {
        o.insert("format".into(), f.into());
    }

    o.insert("x-nova-type".into(), format!("{}.{ty}", novafile_schema::STD_NAME).into());
}

/// Add the class of a type or definition (proposal 0009): `x-nova-class`,
/// and `writeOnly: true` for `@secret`. An `any` schema (`true`) becomes `{}`
/// so that it can hold the keywords.
fn classify(s: &mut Json, class: Option<Class>) {
    let Some(class) = class else { return };
    if *s == Json::Bool(true) {
        *s = Json::Object(Map::new());
    }

    let Json::Object(o) = s else { return };
    if class == Class::Secret {
        o.insert("writeOnly".into(), true.into());
    }

    o.insert("x-nova-class".into(), class.name().into());
}

/// The `$ref` target of a named definition. Definition names, imported
/// ones (`std.Uuid`) included, have no `/` or `~`, so they need no escaping.
fn def_ref(name: &str) -> String {
    format!("#/$defs/{name}")
}

/// A record as a JSON object schema. Fields with a default are not `required`.
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

/// The schema of a field, with its default, deprecation, `@id` and `@key` attached.
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
        // `const` alone would still reject the `null` that `type` now admits.
        if let Some(c) = s.get_mut("const").map(Json::take) {
            s["enum"] = json!([c, null]);
            s.as_object_mut().expect("has const").remove("const");
        }

        s = nullable(s);
    }

    classify(&mut s, t.constraints.class);
    s
}

/// Let a schema also accept `null`: via its `type` when it has one, else via `anyOf`.
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

/// The JSON Schema `format` of a sized integer kind; `None` for `int`.
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

/// An integer bound as a JSON number, and whether the number is exact.
///
/// Beyond 64 bits JSON has only binary64 numbers. The float is then rounded
/// outward (up for a maximum, down for a minimum), so the bound never rejects
/// a value that NOVA accepts.
fn int_json(i: i128, upper: bool) -> (Json, bool) {
    if let Ok(v) = i64::try_from(i) {
        return (v.into(), true);
    }

    if let Ok(v) = u64::try_from(i) {
        return (v.into(), true);
    }

    #[expect(clippy::cast_precision_loss, reason = "beyond 64 bits JSON has only binary64 numbers; rounded outward below")]
    let f = i as f64;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "f is integral and near i, so the cast is exact or saturates at an i128 limit, both on the correct side"
    )]
    let back = f as i128;
    let f = match back.cmp(&i) {
        std::cmp::Ordering::Less if upper => f.next_up(),
        std::cmp::Ordering::Greater if !upper => f.next_down(),
        _ => f,
    };

    (f.into(), back == i)
}

/// The `@range` body `lo..hi`, with an open end left empty.
fn range_text(lo: Option<&Num>, hi: Option<&Num>) -> String {
    let bound = |n: Option<&Num>| n.map(std::string::ToString::to_string).unwrap_or_default();
    format!("{}..{}", bound(lo), bound(hi))
}

/// The JSON Schema for a type kind, before constraints and nullability.
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
                // Fixed-width kinds fit 64 bits, so these bounds are exact.
                o["minimum"] = int_json(lo, false).0;
                o["maximum"] = int_json(hi, true).0;
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
        TypeKind::Union(ms) => {
            let lits: Option<Vec<&str>> = ms
                .iter()
                .map(|m| match &m.kind {
                    TypeKind::Literal(l) if !m.nullable => Some(l.as_str()),
                    _ => None,
                })
                .collect();
            match lits {
                // Literals are disjoint, so their order needs no ordered choice.
                Some(lits) => json!({"type": "string", "enum": lits, "x-nova-kind": "literal"}),
                // JSON Schema has no ordered choice; `anyOf` accepts the same values.
                None => json!({"anyOf": ms.iter().map(|t| ty(schema, t)).collect::<Vec<_>>()}),
            }
        }
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
        // The marker keeps identifier literals from reading back as an enum.
        TypeKind::Literal(l) => json!({"type": "string", "const": l, "x-nova-kind": "literal"}),
    }
}

/// What a constraint applies to, after following aliases.
enum Target {
    /// An integer, with the bounds of its sized kind (`None` for `int` or an alias).
    Int(Option<(i128, i128)>),
    /// A float.
    Float,
    /// A decimal or rational, written as a string.
    ExactNumber,
    /// A string (`true`) or bytes (`false`).
    Text(bool),
    /// A list, set, or map without string keys.
    Items,
    /// A record.
    Properties,
    /// A string-keyed map: an object or an array of pairs.
    ItemsOrProperties,
    /// Anything else; constraints become `x-nova-*` annotations.
    Other,
}

/// Classify `t` for constraint mapping.
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

/// Add the `@range`, `@len` and `@pattern` constraints of `t` to `s`, as JSON Schema
/// keywords where they apply and as `x-nova-range`/`x-nova-len` otherwise.
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
                let mut exact = true;
                if let Some(lo) = lo.as_ref().and_then(|n| int_bound(n, true)) {
                    let (min, min_exact) = int_json(bounds.map_or(lo, |(klo, _)| lo.max(klo)), false);
                    obj.insert("minimum".into(), min);
                    exact &= min_exact;
                }

                if let Some(hi) = hi.as_ref().and_then(|n| int_bound(n, false)) {
                    let (max, max_exact) = int_json(bounds.map_or(hi, |(_, khi)| hi.min(khi)), true);
                    obj.insert("maximum".into(), max);
                    exact &= max_exact;
                }

                // A rounded bound is looser than the range; keep the exact range for the way back.
                if !exact {
                    obj.insert("x-nova-range".into(), range_text(lo.as_ref(), hi.as_ref()).into());
                }
            }
            Target::Float => {
                if let Some(lo) = lo {
                    obj.insert("minimum".into(), num_json(lo, false));
                }

                if let Some(hi) = hi {
                    obj.insert("maximum".into(), num_json(hi, true));
                }
            }
            _ => {
                obj.insert("x-nova-range".into(), range_text(lo.as_ref(), hi.as_ref()).into());
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

    // The parser allows @pattern only on `string`; the exact text rides along
    // so reading the JSON Schema back restores it (spec §11.2).
    if let Some(p) = &c.pattern {
        obj.insert("pattern".into(), p.to_ecmascript().into());
        obj.insert("x-nova-pattern".into(), p.as_str().into());
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

/// A bound as a JSON number (decimals and rationals via binary64). `upper`
/// says which way a wide integer rounds, see [`int_json`].
fn num_json(n: &Num, upper: bool) -> Json {
    match &n.0 {
        Value::Int(i) => int_json(*i, upper).0,
        Value::Decimal(d) => d.to_i128().map_or_else(|| d.to_f64().into(), |i| int_json(i, upper).0),
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

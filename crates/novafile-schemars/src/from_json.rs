//! JSON Schema → NOVA schema. See the crate documentation for the mapping.

use crate::{Conversion, Error};
use novafile_schema::{
    Class, Constraints, Def, FieldDef, IntKind, LenUnit, Num, RecordDef, RecordRule, RuleKind, Schema, TypeKind, TypeRef, VariantDef, Version,
};
use novafile_value::{Decimal, Value};
use serde_json::{Map, Value as Json};
use std::collections::{HashMap, HashSet};

/// A JSON object (schema).
type Obj = Map<String, Json>;

/// Keywords that carry no type information and are dropped without a warning
/// (or are read at field level).
const ANNOTATIONS: &[&str] = &[
    "$schema",
    "$id",
    "x-nova-schema",
    "$anchor",
    "$dynamicAnchor",
    "$comment",
    "$defs",
    "definitions",
    "$vocabulary",
    "title",
    "description",
    "examples",
    "example",
    "readOnly",
    "writeOnly",
    "default",
    "deprecated",
    "x-nova-id",
    "x-nova-key",
    "x-nova-order",
    "x-nova-class",
    "x-nova-type",
];

/// Keywords this converter understands (in the positions where they apply).
const HANDLED: &[&str] = &[
    "type",
    "$ref",
    "properties",
    "required",
    "additionalProperties",
    "unevaluatedProperties",
    "patternProperties",
    "propertyNames",
    "items",
    "prefixItems",
    "additionalItems",
    "unevaluatedItems",
    "uniqueItems",
    "minItems",
    "maxItems",
    "minProperties",
    "maxProperties",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minLength",
    "maxLength",
    "format",
    "contentEncoding",
    "contentMediaType",
    "pattern",
    "enum",
    "const",
    "oneOf",
    "anyOf",
    "allOf",
    "x-nova-kind",
    "x-nova-len",
    "x-nova-pattern",
    "x-nova-range",
];

/// The JSON Schema type name of `null`.
const NULL_TYPE: &str = "null";

/// Convert a JSON Schema document; `name` and `version` override what `$id` or `title` say.
pub(crate) fn convert(json: &Json, name: Option<&str>, version: Option<&str>) -> Result<Conversion, Error> {
    let root_obj = match json {
        Json::Object(o) => o.clone(),
        Json::Bool(true) => Obj::new(),
        Json::Bool(false) => return Err(Error::InvalidJsonSchema("the root schema is `false`, which matches nothing".into())),
        other => return Err(Error::InvalidJsonSchema(format!("expected an object or boolean, found {other}"))),
    };

    let mut c = Conv {
        doc: json,
        names: HashMap::new(),
        used: HashSet::new(),
        defs: Vec::new(),
        warnings: Vec::new(),
        root_name: String::new(),
        depth: 0,
        patterns: std::cell::Cell::new(novafile_schema::SCHEMA_BUDGET),
        inlined: HashMap::new(),
        def_classes: Vec::new(),
        def_docs: Vec::new(),
        uses_std: false,
    };

    let mut schema = Schema::default();
    let (n, v) = schema_ref(json, name, version, &mut c.warnings)?;
    schema.name = n;
    schema.version = v;

    // Name every definition first, so that `$ref`s resolve in any order.
    let mut def_list: Vec<(String, String, &Json)> = Vec::new();
    for key in ["$defs", "definitions"] {
        if let Some(Json::Object(defs)) = root_obj.get(key) {
            for (k, s) in defs {
                let pointer = format!("#/{key}/{}", escape_pointer(k));
                // A standard library type stands for itself, not for a local copy.
                if let Some(std) = s.as_object().and_then(|o| c.std_type(o, &pointer)) {
                    c.names.insert(pointer, std);
                    continue;
                }

                let nova = c.fresh(&sanitize_def_name(k));
                c.names.insert(pointer.clone(), nova.clone());
                def_list.push((pointer, nova, s));
            }
        }
    }

    // The root: a bare `$ref` to a definition, nothing (no @root), or an inline schema.
    let structural: Vec<&String> = root_obj.keys().filter(|k| is_structural(k)).collect();
    let root_ref = root_obj
        .get("$ref")
        .and_then(Json::as_str)
        .map(normalize_ref)
        .filter(|r| structural.len() == 1 && c.names.contains_key(r));
    if let Some(r) = root_ref {
        let n = c.names[&r].clone();
        c.root_name.clone_from(&n);
        schema.root = Some(TypeRef::new(TypeKind::Named(n)));
    } else if !structural.is_empty() || matches!(json, Json::Bool(true)) {
        let title = root_obj
            .get("title")
            .and_then(Json::as_str)
            .filter(|t| novafile_schema::SchemaRef::parse(t).is_err());
        let n = c.fresh(&title.map_or_else(|| "Root".into(), sanitize_def_name));
        c.root_name.clone_from(&n);
        schema.root = Some(TypeRef::new(TypeKind::Named(n.clone())));
        let d = c.def_of(&Json::Object(root_obj.clone()), "#", &n);
        c.defs.push((n, d));
    }

    for (pointer, nova, s) in def_list {
        let d = c.def_of(s, &pointer, &nova);
        c.defs.push((nova, d));
    }

    schema.defs = std::mem::take(&mut c.defs);
    schema.def_classes = std::mem::take(&mut c.def_classes);
    schema.def_docs = std::mem::take(&mut c.def_docs);
    // Canonical order (spec §6.6), so the result does not depend on traversal order.
    schema.defs.sort_by(|a, b| a.0.cmp(&b.0));
    break_alias_cycles(&mut schema, &mut c.warnings);
    if c.uses_std {
        import_std(&mut schema);
    }

    resolve_defaults(&mut schema, &mut c.warnings);
    Ok(Conversion { schema, warnings: c.warnings })
}

/// The schema name, and the version if it has one, that an `https:` `$id`
/// gives (spec §11.2): its host and path, lowercased, form a qualified name; a
/// trailing `/{version}.schema.json` or `/{version}.json` gives the version, and
/// any other `.json` stays part of the name.
fn name_from_https_id(id: &str) -> Option<(String, Option<Version>)> {
    let rest = id.strip_prefix("https://")?.trim_end_matches('#').to_ascii_lowercase();
    if rest.contains(['?', '#', ':', '%']) {
        return None;
    }

    for suffix in [".schema.json", ".json"] {
        if let Some((name, v)) = rest.strip_suffix(suffix).and_then(|r| r.rsplit_once('/'))
            && let Ok(v) = Version::parse(v)
            && novafile_schema::schema_name_kind(name) == Some(novafile_schema::NameKind::Qualified)
        {
            return Some((name.to_owned(), Some(v)));
        }
    }

    (novafile_schema::schema_name_kind(&rest) == Some(novafile_schema::NameKind::Qualified)).then_some((rest, None))
}

/// Work out the schema name and version from the arguments, `x-nova-schema`,
/// `$id` (an `https:` URL, or the legacy `urn:nova:name@version`) or `title`.
fn schema_ref(json: &Json, name: Option<&str>, version: Option<&str>, warnings: &mut Vec<String>) -> Result<(Option<String>, Option<Version>), Error> {
    let reference = |s: &str| novafile_schema::SchemaRef::parse(s).ok().map(|r| (r.name, Some(r.version)));
    let text = |k: &str| json.get(k).and_then(Json::as_str);
    let from_doc = text("x-nova-schema")
        .and_then(reference)
        .or_else(|| text("$id").and_then(name_from_https_id))
        .or_else(|| text("$id").and_then(|s| s.strip_prefix("urn:nova:")).and_then(reference))
        .or_else(|| text("title").and_then(reference));
    let Some(name) = name.map(str::to_owned).or_else(|| from_doc.as_ref().map(|(n, _)| n.clone())) else {
        if let Some(v) = version {
            Version::parse(v).map_err(Error::InvalidVersion)?;
            warnings.push("#: a version without a schema name is ignored; the schema is anonymous".into());
        }

        return Ok((None, None));
    };

    if !novafile_schema::valid_schema_name(&name) {
        return Err(Error::InvalidName(name));
    }

    let version = match (version, from_doc) {
        (Some(v), _) => Version::parse(v).map_err(Error::InvalidVersion)?,
        (None, Some((_, Some(v)))) => v,
        (None, _) => {
            warnings.push("#: no schema version given; using 0.1.0".into());
            Version::new(0, 1, 0)
        }
    };

    Ok((Some(name), Some(version)))
}

/// Keywords that say something about the instance.
fn is_structural(k: &str) -> bool {
    !ANNOTATIONS.contains(&k)
}

/// State of one conversion.
struct Conv<'a> {
    /// The whole document, for resolving `$ref`s that are not definitions.
    doc: &'a Json,
    /// JSON pointer (`#/$defs/X`) → NOVA definition name.
    names: HashMap<String, String>,
    /// Definition names already taken.
    used: HashSet<String>,
    /// Definitions created so far.
    defs: Vec<(String, Def)>,
    /// Warnings collected so far.
    warnings: Vec<String>,
    /// Definition name of the root schema, the target of `$ref: "#"`; empty if none.
    root_name: String,
    /// Current nesting depth, bounded to stop runaway recursion.
    depth: usize,
    /// Compiled-pattern bytes left for this schema.
    patterns: std::cell::Cell<usize>,
    /// Types already converted for `$ref`s that point outside the definitions,
    /// so a pointer used twice per level is not converted 2^depth times.
    inlined: HashMap<String, TypeRef>,
    /// `@personal` or `@secret` on the record and enum definitions created so far.
    def_classes: Vec<(String, Class)>,
    /// The `description` of the definitions created so far, as their documentation.
    def_docs: Vec<(String, String)>,
    /// Whether a type refers to the standard library, so that the schema needs its import.
    uses_std: bool,
}

/// What a (non-nullable) schema maps to at definition level.
enum Shape {
    /// A record definition.
    Record,
    /// An enum definition.
    Enum,
    /// A type used inline (or an alias).
    Other,
}

impl Conv<'_> {
    /// Record a warning about the construct at JSON pointer `at`.
    fn warn(&mut self, at: &str, m: impl AsRef<str>) {
        self.warnings.push(format!("{at}: {}", m.as_ref()));
    }

    /// Reserve a definition name based on `base`.
    fn fresh(&mut self, base: &str) -> String {
        let mut n = base.to_owned();
        let mut i = 2;
        while self.used.contains(&n) {
            n = format!("{base}{i}");
            i += 1;
        }

        self.used.insert(n.clone());
        n
    }

    /// Convert a schema that becomes the definition `name`.
    fn def_of(&mut self, s: &Json, at: &str, name: &str) -> Def {
        if let Some(d) = description(s) {
            self.def_docs.push((name.to_owned(), d));
        }

        if let Json::Object(o) = s {
            let (o, nullable) = strip_null(o);
            if !nullable {
                let d = match shape(&o) {
                    Shape::Record => Def::Record(self.record(&o, at, name)),
                    Shape::Enum => Def::Enum(self.enum_variants(&o, at, name)),
                    Shape::Other => return Def::Alias(self.ty(s, at, &format!("{name}Inner"))),
                };

                // An alias gets its class from `ty`, on the aliased type.
                if let Some(c) = self.class(&o, at) {
                    self.def_classes.push((name.to_owned(), c));
                }

                return d;
            }
        }

        Def::Alias(self.ty(s, at, &format!("{name}Inner")))
    }

    /// Create a definition for an inline record or enum shape.
    fn new_def(&mut self, o: &Obj, at: &str, hint: &str) -> TypeRef {
        let name = self.fresh(hint);
        let d = match shape(o) {
            Shape::Record => Def::Record(self.record(o, at, &name)),
            Shape::Enum => Def::Enum(self.enum_variants(o, at, &name)),
            Shape::Other => unreachable!("new_def is only called for record and enum shapes"),
        };

        self.defs.push((name.clone(), d));
        TypeRef::new(TypeKind::Named(name))
    }

    /// The variants of a schema for which [`enum_plan`] succeeds.
    fn enum_variants(&mut self, o: &Obj, at: &str, name: &str) -> Vec<VariantDef> {
        let (key, plan) = enum_plan(o).expect("enum_variants is only called for enum shapes");
        let mut out = Vec::new();
        for p in plan {
            match p {
                Alt::Units(tags) => out.extend(tags.iter().map(|t| VariantDef {
                    name: t.clone(),
                    payload: None,
                    id: None,
                    doc: None,
                })),
                Alt::Payload(tag, payload, i) => {
                    let at = format!("{at}/{key}/{i}/properties/{}", escape_pointer(&tag));
                    let t = self.ty(payload, &at, &format!("{name}{}", pascal(&tag)));
                    out.push(VariantDef {
                        name: tag,
                        payload: Some(t),
                        id: None,
                        doc: None,
                    });
                }
            }
        }

        out
    }

    /// Whether a record schema accepts fields it does not declare, with a warning
    /// for each keyword on extra fields that NOVA does not enforce, and for each
    /// keyword that the record drops.
    fn record_openness(&mut self, o: &Obj, at: &str) -> bool {
        let mut open = false;
        for k in ["additionalProperties", "unevaluatedProperties"] {
            match o.get(k) {
                None | Some(Json::Bool(false)) => {}
                Some(Json::Bool(true)) => open = true,
                Some(_) => {
                    open = true;
                    self.warn(at, format!("{k} schema is not enforced on extra fields; the record is @open"));
                }
            }
        }

        if o.contains_key("patternProperties") {
            open = true;
            self.warn(at, "patternProperties are not enforced; the record is @open");
        }

        for k in ["minProperties", "maxProperties", "propertyNames"] {
            if o.contains_key(k) {
                self.warn(at, format!("{k} on a record is not enforced"));
            }
        }

        // `allOf` holds the record rules when `x-nova-rules` keeps them (proposal 0010).
        let rules: &[&str] = if o.contains_key("x-nova-rules") { &["allOf", "x-nova-rules"] } else { &[] };
        let handled = [
            "type",
            "properties",
            "required",
            "additionalProperties",
            "unevaluatedProperties",
            "patternProperties",
            "minProperties",
            "maxProperties",
            "propertyNames",
        ];
        self.unsupported(o, at, &[&handled[..], rules].concat());

        open
    }

    /// A record definition from an object schema (or a tagged union).
    fn record(&mut self, o: &Obj, at: &str, name: &str) -> RecordDef {
        if let Some((tag, alts)) = tagged_alternatives(o) {
            return self.tagged_record(o, &tag, &alts, at, name);
        }

        let empty = Obj::new();
        let props = o.get("properties").and_then(Json::as_object).unwrap_or(&empty);
        let required: Vec<&str> = o
            .get("required")
            .and_then(Json::as_array)
            .map(|r| r.iter().filter_map(Json::as_str).collect())
            .unwrap_or_default();
        let mut open = self.record_openness(o, at);

        // Declared order: x-nova-order, else `required` order, then the rest alphabetically.
        let mut order: Vec<String> = Vec::new();
        if let Some(Json::Array(a)) = o.get("x-nova-order") {
            order.extend(a.iter().filter_map(Json::as_str).map(str::to_owned));
        }

        order.extend(required.iter().map(|s| (*s).to_owned()));
        order.extend(props.keys().cloned());
        let mut seen = HashSet::new();
        order.retain(|k| seen.insert(k.clone()) && (props.contains_key(k) || required.contains(&k.as_str())));

        let mut fields = Vec::new();
        let mut ids = HashSet::new();
        for fname in order {
            let fat = format!("{at}/properties/{}", escape_pointer(&fname));
            if !valid_name(&fname) {
                self.warn(
                    &fat,
                    format!("{fname:?} is not a valid NOVA field name; the field is dropped and the record is @open"),
                );
                open = true;
                continue;
            }

            let fs = props.get(&fname).unwrap_or(&Json::Bool(true));
            let ty = self.ty(fs, &fat, &format!("{name}{}", pascal(&fname)));
            let meta = |k: &str| fs.as_object().and_then(|o| o.get(k));
            let mut f = FieldDef {
                name: fname.clone(),
                ty,
                optional: !required.contains(&fname.as_str()),
                default: None,
                key: false,
                id: None,
                deprecated: meta("deprecated") == Some(&Json::Bool(true)),
                doc: description(fs),
            };

            if let Some(d) = meta("default") {
                match novafile_json::from_json_str(&d.to_string()) {
                    Ok(v) => f.default = Some(v),
                    Err(e) => self.warn(&fat, format!("default dropped: {e}")),
                }
            }

            if let Some(id) = meta("x-nova-id") {
                match id.as_u64().and_then(|n| u32::try_from(n).ok()) {
                    Some(n) if ids.insert(n) => f.id = Some(n),
                    _ => self.warn(&fat, format!("x-nova-id {id} dropped (not a unique u32)")),
                }
            }

            if meta("x-nova-key") == Some(&Json::Bool(true)) {
                if f.optional || f.ty.nullable || f.ty.constraints.class.is_some() {
                    self.warn(&fat, "x-nova-key dropped: key fields must be required, non-null and not @personal or @secret");
                } else {
                    f.key = true;
                }
            }

            fields.push(f);
        }

        let mut r = RecordDef {
            fields,
            open,
            rules: Vec::new(),
        };
        r.rules = self.rules(o, at, &r);
        r
    }

    /// The record rules in `"x-nova-rules"` (proposal 0010). A rule that is
    /// malformed or names a field that is missing, required or has a default
    /// is dropped with a warning. JSON Schema's own `dependentRequired`,
    /// `oneOf` and `anyOf` are not read back into rules.
    fn rules(&mut self, o: &Obj, at: &str, r: &RecordDef) -> Vec<RecordRule> {
        let mut out = Vec::new();
        for (i, j) in o.get("x-nova-rules").and_then(Json::as_array).into_iter().flatten().enumerate() {
            let kind = j.get("kind").and_then(Json::as_str).and_then(RuleKind::from_name);
            let fields: Option<Vec<String>> = j
                .get("fields")
                .and_then(Json::as_array)
                .map(|fs| fs.iter().filter_map(|f| f.as_str().map(str::to_owned)).collect());
            let usable = |fs: &[String]| fs.len() >= 2 && fs.iter().all(|f| r.field(f).is_some_and(|d| d.optional && d.default.is_none() && !d.key));
            match (kind, fields) {
                (Some(kind), Some(fields)) if usable(&fields) => out.push(RecordRule { kind, fields }),
                _ => self.warn(
                    &format!("{at}/x-nova-rules/{i}"),
                    "record rule dropped: it must name two or more optional fields of the record",
                ),
            }
        }

        out
    }

    /// Internally or adjacently tagged enums: one record holding the tag and
    /// the union of the other fields (all optional).
    fn tagged_record(&mut self, o: &Obj, tag: &str, alts: &[(usize, &Obj)], at: &str, name: &str) -> RecordDef {
        let key = if o.contains_key("oneOf") { "oneOf" } else { "anyOf" };
        let tags: Vec<String> = alts.iter().filter_map(|(_, a)| const_str(a.get("properties")?.get(tag)?)).collect();
        self.warn(
            at,
            format!(
                "internally/adjacently tagged union (tag {tag:?}: {}) mapped to a record whose other fields are optional",
                tags.join(" | ")
            ),
        );
        let field = |name: &str, ty: TypeRef, optional: bool| FieldDef {
            name: name.to_owned(),
            ty,
            optional,
            default: None,
            key: false,
            id: None,
            deprecated: false,
            doc: None,
        };

        let mut fields = vec![field(tag, TypeRef::new(TypeKind::String), false)];
        let mut open = false;
        for (i, a) in alts {
            if !matches!(a.get("additionalProperties"), None | Some(Json::Bool(false))) {
                open = true;
            }

            let Some(props) = a.get("properties").and_then(Json::as_object) else {
                continue;
            };

            for (k, s) in props {
                if k == tag {
                    continue;
                }

                let fat = format!("{at}/{key}/{i}/properties/{}", escape_pointer(k));
                if !valid_name(k) {
                    self.warn(
                        &fat,
                        format!("{k:?} is not a valid NOVA field name; the field is dropped and the record is @open"),
                    );
                    open = true;
                    continue;
                }

                let ty = self.ty(s, &fat, &format!("{name}{}", pascal(k)));
                match fields.iter_mut().find(|f| f.name == *k) {
                    Some(f) if f.ty != ty => {
                        if !matches!(f.ty.kind, TypeKind::Any) {
                            self.warn(&fat, format!("field {k:?} has different types in different variants; using any"));
                        }

                        f.ty = TypeRef::new(TypeKind::Any);
                    }
                    Some(_) => {}
                    None => fields.push(field(k, ty, true)),
                }
            }
        }

        RecordDef {
            fields,
            open,
            rules: Vec::new(),
        }
    }

    /// Warn about keywords that are dropped. `handled` lists the keywords
    /// the caller dealt with.
    fn unsupported(&mut self, o: &Obj, at: &str, handled: &[&str]) {
        for k in o.keys() {
            let k = k.as_str();
            if handled.contains(&k) || ANNOTATIONS.contains(&k) || (k.starts_with("x-") && !k.starts_with("x-nova-")) {
                continue;
            }

            let what = match k {
                "const" | "pattern" | "multipleOf" | "format" | "propertyNames" | "uniqueItems" => "is not enforced",
                _ if HANDLED.contains(&k) => "does not apply here and was dropped",
                _ => "is not supported and was dropped",
            };

            self.warn(at, format!("{k} {what}"));
        }
    }

    /// A type in a nested position (element, key, value, tuple member): the
    /// schema language cannot attach constraints there, so constrained types
    /// are hoisted into an alias definition.
    fn nested(&mut self, s: &Json, at: &str, hint: &str) -> TypeRef {
        let t = self.ty(s, at, hint);
        if t.constraints == Constraints::default() {
            return t;
        }

        let name = self.fresh(hint);
        let nullable = t.nullable;
        let mut alias = t;
        alias.nullable = false;
        self.defs.push((name.clone(), Def::Alias(alias)));
        TypeRef {
            kind: TypeKind::Named(name),
            nullable,
            constraints: Constraints::default(),
        }
    }

    /// The NOVA type for a schema. Record and enum shapes become new
    /// definitions named after `hint`.
    fn ty(&mut self, s: &Json, at: &str, hint: &str) -> TypeRef {
        self.depth += 1;
        let t = if self.depth > 64 {
            self.warn(at, "schema nested too deeply; using any");
            any()
        } else {
            match s {
                Json::Bool(true) => any(),
                Json::Bool(false) => {
                    self.warn(at, "`false` schema (matches nothing) mapped to any");
                    any()
                }
                Json::Object(o) => {
                    let (o, nullable) = strip_null(o);
                    let mut t = self.ty_object(&o, at, hint);
                    t.nullable |= nullable;
                    if let Some(c) = self.class(&o, at) {
                        t.constraints.class = Some(c);
                    }

                    t
                }
                other => {
                    self.warn(at, format!("{other} is not a schema; using any"));
                    any()
                }
            }
        };

        self.depth -= 1;
        t
    }

    /// `o` has had its `null` alternatives removed.
    fn ty_object(&mut self, o: &Obj, at: &str, hint: &str) -> TypeRef {
        let only = |keys: &[&str]| !o.keys().any(|k| is_structural(k) && !keys.contains(&k.as_str()));
        // `x-nova-type` wins over the `pattern`, `format` and `type` that restate it.
        if let Some(n) = self.std_type(o, at) {
            let mut t = TypeRef::new(TypeKind::Named(n));
            self.constraints(&mut t, o, at);
            return t;
        }

        // References, with sibling constraints (2020-12).
        if let Some(r) = o.get("$ref").and_then(Json::as_str) {
            let mut t = self.reference(r, at, hint);
            self.unsupported(o, at, CONSTRAINT_KEYWORDS);
            self.constraints(&mut t, o, at);
            return t;
        }

        match shape(o) {
            Shape::Record | Shape::Enum => return self.new_def(o, at, hint),
            Shape::Other => {}
        }

        // NOVA kind markers (before combinators: a string-keyed map is an `anyOf`).
        if let Some(k) = o.get("x-nova-kind").and_then(Json::as_str) {
            if let Some(t) = self.nova_kind(k, o, at, hint) {
                return t;
            }

            self.warn(at, format!("unknown or malformed x-nova-kind {k:?} ignored"));
        }

        // Combinators.
        if let Some(Json::Array(all)) = o.get("allOf") {
            if all.len() == 1 && only(&["allOf"]) {
                return self.ty(&all[0], &format!("{at}/allOf/0"), hint);
            }

            self.warn(at, "allOf with several schemas (or with sibling keywords) is not supported; using any");
            return any();
        }

        for key in ["oneOf", "anyOf"] {
            let Some(Json::Array(alts)) = o.get(key) else { continue };
            let alts: Vec<(usize, &Json)> = alts.iter().enumerate().filter(|(_, a)| !is_null_schema(a)).collect();
            if alts.len() == 1 && only(&[key]) {
                return self.ty(alts[0].1, &format!("{at}/{key}/{}", alts[0].0), hint);
            }

            self.warn(
                at,
                format!("{key} that is not a serde externally tagged enum (e.g. an untagged enum) mapped to any"),
            );
            return any();
        }

        // Strings that are not all variant names become a union of literals.
        if let Some(Json::Array(vals)) = o.get("enum")
            && o.get("type").is_none_or(|t| t == "string")
            && let Some(mut ms) = vals
                .iter()
                .map(|v| Some(TypeRef::new(TypeKind::Literal(v.as_str()?.to_owned()))))
                .collect::<Option<Vec<_>>>()
            && !ms.is_empty()
        {
            return if ms.len() == 1 {
                ms.pop().unwrap()
            } else {
                TypeRef::new(TypeKind::Union(ms))
            };
        }

        // Non-variant enums and constants keep their base type.
        if let Some(Json::Array(vals)) = o.get("enum") {
            self.warn(
                at,
                "enum whose values are not all valid variant names was mapped to its base type; the enum constraint is dropped",
            );
            let ty = o.get("type").cloned().or_else(|| type_of_values(vals));
            return self.typed(o, ty.as_ref(), at, hint, &["enum"]);
        }

        let ty = o
            .get("type")
            .cloned()
            .or_else(|| infer_type(o))
            .or_else(|| o.get("const").and_then(|v| type_of_values(std::slice::from_ref(v))));
        self.typed(o, ty.as_ref(), at, hint, &[])
    }

    /// Convert by `type` (a string, or an array of type names without `null`).
    fn typed(&mut self, o: &Obj, ty: Option<&Json>, at: &str, hint: &str, handled: &[&str]) -> TypeRef {
        let name = match ty {
            None => {
                self.unsupported(o, at, handled);
                return any();
            }
            Some(Json::String(s)) => s.clone(),
            Some(Json::Array(a)) => {
                let names: Vec<&str> = a.iter().filter_map(Json::as_str).filter(|t| *t != NULL_TYPE).collect();
                match names.as_slice() {
                    [one] => (*one).to_owned(),
                    [] => NULL_TYPE.to_owned(),
                    many => {
                        self.warn(at, format!("a value of several types ({}) mapped to any", many.join(", ")));
                        return any();
                    }
                }
            }
            Some(other) => {
                self.warn(at, format!("invalid type {other}; using any"));
                return any();
            }
        };

        let mut handled: Vec<&str> = handled.to_vec();
        handled.push("type");
        let mut t = match name.as_str() {
            "null" => TypeRef::new(TypeKind::Null),
            "boolean" => TypeRef::new(TypeKind::Bool),
            "integer" => {
                handled.extend(["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum", "format", "x-nova-range"]);
                self.unsupported(o, at, &handled);
                return self.integer(o, at);
            }
            "number" => {
                handled.extend(NUMBER_KEYWORDS);
                // schemars marks f32/f64 with these; both are NOVA floats.
                if matches!(o.get("format").and_then(Json::as_str), Some("float" | "double")) {
                    handled.push("format");
                }

                TypeRef::new(TypeKind::Float)
            }
            "string" => {
                handled.extend(STRING_KEYWORDS);
                self.string(o, at)
            }
            "array" => {
                handled.extend(ARRAY_KEYWORDS);
                self.array(o, at, hint)
            }
            "object" => {
                handled.extend(MAP_KEYWORDS);
                self.map(o, at, hint)
            }
            other => {
                self.warn(at, format!("unknown type {other:?}; using any"));
                return any();
            }
        };

        self.unsupported(o, at, &handled);
        self.constraints(&mut t, o, at);
        if matches!(t.kind, TypeKind::Literal(_)) && t.constraints != Constraints::default() {
            t.constraints = Constraints::default();
            self.warn(at, "constraints next to a string const are dropped; the literal fixes the value");
        }

        t
    }

    /// The type a `$ref` points to; unresolvable references become `any`.
    fn reference(&mut self, r: &str, at: &str, hint: &str) -> TypeRef {
        let r = normalize_ref(r);
        if r == "#" && !self.root_name.is_empty() {
            return TypeRef::new(TypeKind::Named(self.root_name.clone()));
        }

        if let Some(n) = self.names.get(&r) {
            return TypeRef::new(TypeKind::Named(n.clone()));
        }

        if let Some(t) = self.inlined.get(&r) {
            return t.clone();
        }

        if let Some(target) = r.strip_prefix('#').and_then(|p| self.doc.pointer(p)) {
            let target = target.clone();
            let t = self.ty(&target, &r, hint);
            self.inlined.insert(r, t.clone());
            return t;
        }

        self.warn(at, format!("$ref {r:?} cannot be resolved (only local references are supported); using any"));
        any()
    }

    /// A type marked with `x-nova-kind`; `None` if the marker is unknown or malformed.
    fn nova_kind(&mut self, k: &str, o: &Obj, at: &str, hint: &str) -> Option<TypeRef> {
        let kind = match k {
            "timestamp" => TypeKind::Timestamp,
            "duration" => TypeKind::Duration,
            "decimal" => TypeKind::Decimal,
            "rational" => TypeKind::Rational,
            "bytes" => TypeKind::Bytes,
            "float" => TypeKind::Float,
            "map" => {
                // An array of [key, value] pairs, or for string keys
                // `anyOf: [object, pairs]` (as written by to_json_schema).
                let object = o
                    .get("anyOf")
                    .and_then(Json::as_array)
                    .and_then(|alts| alts.iter().enumerate().find(|(_, a)| a.get("type").and_then(Json::as_str) == Some("object")));
                let mut t = if let Some((i, obj)) = object {
                    let at = format!("{at}/anyOf/{i}");
                    let vt = match obj.get("additionalProperties") {
                        Some(v) => self.nested(v, &format!("{at}/additionalProperties"), &format!("{hint}Value")),
                        None => any(),
                    };

                    let kt = match obj.get("propertyNames") {
                        Some(k) => {
                            let k = self.unclassified(k, &format!("{at}/propertyNames"), "map keys");
                            self.nested(&k, &format!("{at}/propertyNames"), &format!("{hint}Key"))
                        }
                        None => TypeRef::new(TypeKind::String),
                    };

                    TypeRef::new(TypeKind::Map(Box::new(kt), Box::new(vt)))
                } else {
                    let pair = o.get("items").and_then(Json::as_object)?;
                    let [k, v] = pair.get("prefixItems").and_then(Json::as_array)?.as_slice() else {
                        return None;
                    };

                    let k = self.unclassified(k, &format!("{at}/items/prefixItems/0"), "map keys");
                    let kt = self.nested(&k, &format!("{at}/items/prefixItems/0"), &format!("{hint}Key"));
                    let vt = self.nested(v, &format!("{at}/items/prefixItems/1"), &format!("{hint}Value"));
                    TypeRef::new(TypeKind::Map(Box::new(kt), Box::new(vt)))
                };

                len(&mut t, o, "minItems", "maxItems");
                len(&mut t, o, "minProperties", "maxProperties");
                return Some(t);
            }
            "literal" => {
                // `const`, or `enum` with an optional `null` (as written by to_json_schema).
                let vals: Vec<&Json> = match (o.get("const"), o.get("enum")) {
                    (Some(c), _) => vec![c],
                    (None, Some(Json::Array(vs))) => vs.iter().collect(),
                    _ => return None,
                };
                let mut ms: Vec<TypeRef> = vals
                    .iter()
                    .filter(|v| !v.is_null())
                    .map(|v| Some(TypeRef::new(TypeKind::Literal(v.as_str()?.to_owned()))))
                    .collect::<Option<_>>()?;
                let nullable = ms.len() < vals.len();
                return match ms.len() {
                    0 => None,
                    1 => {
                        let mut t = ms.pop().unwrap();
                        t.nullable = nullable;
                        Some(t)
                    }
                    _ if nullable => None,
                    _ => Some(TypeRef::new(TypeKind::Union(ms))),
                };
            }
            _ => return None,
        };

        let mut t = TypeRef::new(kind);
        // `minimum`/`maximum` only constrain the numeric form of a float.
        self.constraints(&mut t, o, at);
        Some(t)
    }

    /// An integer type: the kind from `format` or from bounds that match a
    /// sized kind exactly, other bounds as `@range`.
    fn integer(&mut self, o: &Obj, at: &str) -> TypeRef {
        let format = o.get("format").and_then(Json::as_str);
        let from_format = format.and_then(|f| match f {
            "int8" => Some(IntKind::I8),
            "int16" => Some(IntKind::I16),
            "int32" => Some(IntKind::I32),
            "int64" => Some(IntKind::I64),
            "uint8" => Some(IntKind::U8),
            "uint16" => Some(IntKind::U16),
            "uint32" => Some(IntKind::U32),
            "uint64" => Some(IntKind::U64),
            "int" | "uint" | "int128" | "uint128" | "integer" => Some(IntKind::Int),
            _ => None,
        });
        if let (Some(f), None) = (format, from_format) {
            self.warn(at, format!("integer format {f:?} is not enforced"));
        }

        let lo = int_bound(o, "minimum", "exclusiveMinimum", true);
        let hi = int_bound(o, "maximum", "exclusiveMaximum", false);
        let kind = from_format.unwrap_or_else(|| {
            [
                IntKind::I8,
                IntKind::I16,
                IntKind::I32,
                IntKind::I64,
                IntKind::U8,
                IntKind::U16,
                IntKind::U32,
                IntKind::U64,
            ]
            .into_iter()
            .find(|k| (lo, hi) == (Some(k.bounds().0), Some(k.bounds().1)))
            .unwrap_or(IntKind::Int)
        });
        let mut t = TypeRef::new(TypeKind::Int(kind));
        let (klo, khi) = kind.bounds();
        let redundant = |b: Option<i128>, k: i128| b.is_none() || (kind != IntKind::Int && b == Some(k));
        if !(redundant(lo, klo) && redundant(hi, khi)) {
            t.constraints.range = Some((lo.map(Num::int), hi.map(Num::int)));
        }

        if let Some(r) = o.get("x-nova-range").and_then(Json::as_str) {
            self.nova_range(&mut t, r, at);
        }

        t
    }

    /// A string, or bytes/timestamp/duration by `contentEncoding` or `format`.
    fn string(&mut self, o: &Obj, at: &str) -> TypeRef {
        let format = o.get("format").and_then(Json::as_str);
        let encoding = o.get("contentEncoding").and_then(Json::as_str);
        let kind = match (format, encoding) {
            (_, Some("base64")) => TypeKind::Bytes,
            (Some("date-time"), _) => TypeKind::Timestamp,
            (Some("duration"), _) => TypeKind::Duration,
            _ => TypeKind::String,
        };

        if let Some(e) = encoding.filter(|e| *e != "base64") {
            self.warn(at, format!("contentEncoding {e:?} is not supported; using string"));
        }

        if let (TypeKind::String, Some(Json::String(c))) = (&kind, o.get("const")) {
            return TypeRef::new(TypeKind::Literal(c.clone()));
        }

        let std = format.and_then(std_of_format).filter(|_| kind == TypeKind::String);
        if let Some(f) = format.filter(|f| !matches!(*f, "date-time" | "duration") && std.is_none()) {
            self.warn(at, format!("format {f:?} is not enforced"));
        }

        let mut t = TypeRef::new(kind);
        if let Some(n) = std {
            self.warn(
                at,
                format!(
                    "format {:?} mapped to std.{n}, which is narrower than the JSON Schema format",
                    format.unwrap_or_default()
                ),
            );
            self.uses_std = true;
            t.kind = TypeKind::Named(format!("std.{n}"));
        }

        if matches!(t.kind, TypeKind::String | TypeKind::Named(_))
            && let Some(p) = read_pattern(o)
        {
            match novafile_schema::Pattern::new_within(&p, &self.patterns) {
                Ok(p) => t.constraints.pattern = Some(p),
                Err(e) => self.warn(at, format!("pattern is not enforced: {e}")),
            }
        } else if o.contains_key("pattern") {
            self.warn(at, "pattern is not enforced");
        }

        if let Some(v) = o.get("const") {
            self.warn(at, format!("const {v} is not enforced"));
        }

        t
    }

    /// A tuple (fixed-length `prefixItems`/array-form `items`), set or list.
    fn array(&mut self, o: &Obj, at: &str, hint: &str) -> TypeRef {
        let min = o.get("minItems").and_then(Json::as_u64);
        let max = o.get("maxItems").and_then(Json::as_u64);
        // Tuples: 2020-12 `prefixItems`, draft-07 array-form `items`.
        let (prefix, key, rest) = match (o.get("prefixItems"), o.get("items")) {
            (Some(Json::Array(p)), rest) => (Some(p), "prefixItems", rest.or_else(|| o.get("unevaluatedItems"))),
            (_, Some(Json::Array(p))) => (Some(p), "items", o.get("additionalItems")),
            _ => (None, "", None),
        };

        if let Some(p) = prefix {
            let n = p.len() as u64;
            let fixed = rest == Some(&Json::Bool(false)) || max == Some(n);
            if fixed {
                if min.unwrap_or(0) < n {
                    self.warn(at, format!("{key} allows arrays shorter than {n}; the NOVA tuple requires all {n} elements"));
                }

                let ts = p
                    .iter()
                    .enumerate()
                    .map(|(i, s)| self.nested(s, &format!("{at}/{key}/{i}"), &format!("{hint}{i}")))
                    .collect();
                return TypeRef::new(TypeKind::Tuple(ts));
            }

            self.warn(at, format!("{key} without a fixed length mapped to list<any>"));
            return TypeRef::new(TypeKind::List(Box::new(any())));
        }

        let set = o.get("uniqueItems") == Some(&Json::Bool(true));
        let item = match o.get("items") {
            Some(s) if set => {
                let s = self.unclassified(s, &format!("{at}/items"), "set elements");
                self.nested(&s, &format!("{at}/items"), &format!("{hint}Item"))
            }
            Some(s) => self.nested(s, &format!("{at}/items"), &format!("{hint}Item")),
            None => any(),
        };

        let mut t = if set {
            TypeRef::new(TypeKind::Set(Box::new(item)))
        } else {
            TypeRef::new(TypeKind::List(Box::new(item)))
        };

        len(&mut t, o, "minItems", "maxItems");
        t
    }

    /// A map for an object schema without properties; integer keys come from a digits-only `patternProperties` pattern.
    fn map(&mut self, o: &Obj, at: &str, hint: &str) -> TypeRef {
        let mut key = TypeRef::new(TypeKind::String);
        let mut value = any();
        match (o.get("additionalProperties"), o.get("patternProperties").and_then(Json::as_object)) {
            (None | Some(Json::Bool(false)), Some(pp)) if pp.len() == 1 => {
                let (pat, s) = pp.iter().next().expect("one entry");
                if matches!(pat.as_str(), r"^\d+$" | r"^-?\d+$" | "^[0-9]+$" | "^-?[0-9]+$") {
                    key = TypeRef::new(TypeKind::Int(IntKind::Int));
                } else {
                    self.warn(at, format!("patternProperties pattern {pat:?} is not enforced on keys"));
                }

                value = self.nested(s, &format!("{at}/patternProperties/{}", escape_pointer(pat)), &format!("{hint}Value"));
            }
            (_, Some(_)) => self.warn(at, "patternProperties are not supported; using map<string, any>"),
            (Some(s @ Json::Object(_)), None) => value = self.nested(s, &format!("{at}/additionalProperties"), &format!("{hint}Value")),
            (Some(Json::Bool(false)), None) => self.warn(at, "an object that allows no properties mapped to map<string, any>"),
            _ => {}
        }

        if let Some(pn) = o.get("propertyNames") {
            let mut pn = self.unclassified(pn, &format!("{at}/propertyNames"), "map keys");
            if let Some(p) = pn.as_object_mut() {
                p.entry("type").or_insert_with(|| "string".into());
            }

            let k = self.nested(&pn, &format!("{at}/propertyNames"), &format!("{hint}Key"));
            if matches!(key.kind, TypeKind::String) {
                key = k;
            }
        }

        let mut t = TypeRef::new(TypeKind::Map(Box::new(key), Box::new(value)));
        len(&mut t, o, "minProperties", "maxProperties");
        t
    }

    /// Range and length constraints for scalars and referenced types.
    fn constraints(&mut self, t: &mut TypeRef, o: &Obj, at: &str) {
        if o.contains_key("minLength") || o.contains_key("maxLength") {
            len(t, o, "minLength", "maxLength");
        }

        if matches!(t.kind, TypeKind::Named(_)) {
            len(t, o, "minItems", "maxItems");
            len(t, o, "minProperties", "maxProperties");
        }

        let lo = o.get("minimum").and_then(Json::as_number).map(num);
        let hi = o.get("maximum").and_then(Json::as_number).map(num);
        let xlo = o.get("exclusiveMinimum").and_then(Json::as_number).map(num);
        let xhi = o.get("exclusiveMaximum").and_then(Json::as_number).map(num);
        if xlo.is_some() || xhi.is_some() || o.get("exclusiveMinimum") == Some(&Json::Bool(true)) || o.get("exclusiveMaximum") == Some(&Json::Bool(true)) {
            self.warn(at, "exclusive bound on a float is kept inclusive");
        }

        let (lo, hi) = (lo.or(xlo), hi.or(xhi));
        if lo.is_some() || hi.is_some() {
            t.constraints.range = Some((lo, hi));
        }

        if let Some(r) = o.get("x-nova-range").and_then(Json::as_str) {
            self.nova_range(t, r, at);
        }

        if let Some(l) = o.get("x-nova-len").and_then(Json::as_str) {
            match novafile_schema::parse_type(&format!("string @len({l})")) {
                Ok(p) => {
                    t.constraints.len = p.constraints.len;
                    t.constraints.len_unit = p.constraints.len_unit;
                }
                Err(e) => self.warn(at, format!("x-nova-len {l:?} ignored: {e}")),
            }
        }
    }

    /// The class of a type or definition (proposal 0009), from `x-nova-class`
    /// only. `writeOnly` states the direction of data flow, not sensitivity, and
    /// a wrong `@secret` cannot be removed without a breaking change.
    fn class(&mut self, o: &Obj, at: &str) -> Option<Class> {
        if let Some(c) = o.get("x-nova-class") {
            match c.as_str().and_then(Class::from_name) {
                Some(c) => return Some(c),
                None => self.warn(at, format!("unknown x-nova-class {c} ignored")),
            }
        }

        None
    }

    /// `s` without a class, for positions that cannot have one (`what`: set
    /// elements, map keys), with a warning if it had one.
    fn unclassified(&mut self, s: &Json, at: &str, what: &str) -> Json {
        let mut s = s.clone();
        // shortcut: only a class on the schema itself is dropped, not one that a `$ref` reaches.
        if let Some(o) = s.as_object_mut()
            && o.remove("x-nova-class").is_some()
        {
            self.warn(
                at,
                format!("{what} cannot be @personal or @secret, since they appear in paths; the class is dropped"),
            );
        }

        s
    }

    /// The standard library type (`std.Uuid`) that `x-nova-type` names
    /// (`novafile.dev/std.Uuid`), if any. Warns on a name that is not a type
    /// of the bundled standard library.
    fn std_type(&mut self, o: &Obj, at: &str) -> Option<String> {
        let x = o.get("x-nova-type")?;
        let ty = x
            .as_str()
            .and_then(|x| x.strip_prefix(novafile_schema::STD_NAME)?.strip_prefix('.'))
            .filter(|ty| latest_std().def(ty).is_some());
        if ty.is_none() {
            self.warn(at, format!("x-nova-type {x} is not a type of {} and is ignored", novafile_schema::STD_NAME));
        }

        self.uses_std |= ty.is_some();
        ty.map(|ty| format!("{STD_ALIAS}.{ty}"))
    }

    /// Apply an `x-nova-range` annotation, warning if it does not parse.
    fn nova_range(&mut self, t: &mut TypeRef, r: &str, at: &str) {
        match novafile_schema::parse_type(&format!("decimal @range({r})")) {
            Ok(p) => t.constraints.range = p.constraints.range,
            Err(e) => self.warn(at, format!("x-nova-range {r:?} ignored: {e}")),
        }
    }
}

/// Keywords handled next to a `$ref`.
const CONSTRAINT_KEYWORDS: &[&str] = &[
    "$ref",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "minProperties",
    "maxProperties",
    "x-nova-range",
    "x-nova-len",
];
/// Keywords handled for `type: number`.
const NUMBER_KEYWORDS: &[&str] = &["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum", "x-nova-range"];
/// Keywords handled for `type: string`.
const STRING_KEYWORDS: &[&str] = &[
    "minLength",
    "maxLength",
    "format",
    "contentEncoding",
    "contentMediaType",
    "pattern",
    "const",
    "x-nova-len",
    "x-nova-pattern",
];
/// Keywords handled for `type: array`.
const ARRAY_KEYWORDS: &[&str] = &[
    "items",
    "prefixItems",
    "additionalItems",
    "unevaluatedItems",
    "uniqueItems",
    "minItems",
    "maxItems",
];
/// Keywords handled for `type: object` without properties (a map).
const MAP_KEYWORDS: &[&str] = &[
    "additionalProperties",
    "unevaluatedProperties",
    "patternProperties",
    "propertyNames",
    "minProperties",
    "maxProperties",
];

/// The alias under which a converted schema imports the standard library.
const STD_ALIAS: &str = "std";

/// The newest bundled version of the standard library.
fn latest_std() -> &'static Schema {
    novafile_schema::std_schemas().last().expect("at least one standard library version is bundled")
}

/// The standard library type for a JSON Schema string `format` (proposal 0008).
fn std_of_format(f: &str) -> Option<&'static str> {
    Some(match f {
        "uuid" => "Uuid",
        "uri" => "Uri",
        "uri-reference" => "UriReference",
        "email" => "Email",
        _ => return None,
    })
}

/// Import the newest bundled standard library as `std` and link its
/// definitions. A scratch schema does the linking, so the reference checks
/// do not run before the defaults are resolved.
/// The `description` of a schema as documentation, without the note that
/// [`crate::to_json_schema`] adds to `@key` fields.
fn description(s: &Json) -> Option<String> {
    let d = s.as_object()?.get("description")?.as_str()?;
    let d = d.strip_suffix(crate::to_json::KEY_NOTE).unwrap_or(d).trim_end();
    (!d.is_empty()).then(|| d.to_owned())
}

/// Import the bundled standard library into `schema` under [`STD_ALIAS`].
fn import_std(schema: &mut Schema) {
    let reference = latest_std().reference().expect("the standard library has a name and version");
    let mut scratch = Schema::default();
    scratch.imports.push(novafile_schema::Import {
        alias: STD_ALIAS.into(),
        reference,
    });
    scratch.link(&|_| Err(String::new())).expect("the bundled standard library links");
    schema.imports = scratch.imports;
    schema.imported = scratch.imported;
    schema.def_classes.extend(scratch.def_classes);
    schema.def_docs.extend(scratch.def_docs);
}

/// Set `@len` from a pair of min/max keywords, if either is present.
fn len(t: &mut TypeRef, o: &Obj, min: &str, max: &str) {
    let lo = o.get(min).and_then(Json::as_u64);
    let hi = o.get(max).and_then(Json::as_u64);
    if lo.is_some() || hi.is_some() {
        t.constraints.len = Some((lo, hi));
        t.constraints.len_unit = LenUnit::Scalars;
    }
}

/// Does this (non-nullable) schema become a record or an enum definition?
fn shape(o: &Obj) -> Shape {
    if o.contains_key("$ref") || o.contains_key("x-nova-kind") {
        return Shape::Other;
    }

    if enum_plan(o).is_some() {
        return Shape::Enum;
    }

    let ty = o.get("type").and_then(Json::as_str);
    if (ty == Some("object") || (ty.is_none() && !o.contains_key("type"))) && (o.contains_key("properties") || o.contains_key("required")) {
        return Shape::Record;
    }

    if tagged_alternatives(o).is_some() {
        return Shape::Record;
    }

    Shape::Other
}

/// One alternative of an externally tagged enum.
enum Alt<'j> {
    /// Unit variants, from a string `enum` or `const`.
    Units(Vec<String>),
    /// Tag, payload schema, index of the alternative.
    Payload(String, &'j Json, usize),
}

/// If `o` is a string `enum`, or a `oneOf`/`anyOf` shaped like a serde
/// externally tagged enum, how to build its variants. Pure: nothing is
/// converted unless every alternative fits.
fn enum_plan(o: &Obj) -> Option<(&'static str, Vec<Alt<'_>>)> {
    let (key, plan) = if let Some(Json::Array(vals)) = o.get("enum") {
        if o.get("type").and_then(Json::as_str).is_some_and(|t| t != "string") {
            return None;
        }

        let tags: Vec<String> = vals.iter().map(|v| v.as_str().map(str::to_owned)).collect::<Option<_>>()?;
        ("enum", vec![Alt::Units(tags)])
    } else {
        let (key, alts) = ["oneOf", "anyOf"].into_iter().find_map(|k| Some((k, o.get(k)?.as_array()?)))?;
        let mut plan = Vec::new();
        for (i, a) in alts.iter().enumerate().filter(|(_, a)| !is_null_schema(a)) {
            let a = a.as_object()?;
            if let Some(t) = a.get("const") {
                if a.get("type").and_then(Json::as_str).is_some_and(|t| t != "string") {
                    return None;
                }

                plan.push(Alt::Units(vec![t.as_str()?.to_owned()]));
            } else if let Some(Json::Array(vals)) = a.get("enum") {
                plan.push(Alt::Units(vals.iter().map(|v| v.as_str().map(str::to_owned)).collect::<Option<_>>()?));
            } else {
                let props = a.get("properties")?.as_object()?;
                let req = a.get("required")?.as_array()?;
                let closed = a.get("additionalProperties") == Some(&Json::Bool(false)) || a.get("unevaluatedProperties") == Some(&Json::Bool(false));
                if props.len() != 1 || req.len() != 1 || !closed {
                    return None;
                }

                let (tag, payload) = props.iter().next()?;
                if req[0].as_str() != Some(tag) {
                    return None;
                }

                plan.push(Alt::Payload(tag.clone(), payload, i));
            }
        }

        (key, plan)
    };

    let names: Vec<&str> = plan
        .iter()
        .flat_map(|a| match a {
            Alt::Units(ts) => ts.iter().map(String::as_str).collect::<Vec<_>>(),
            Alt::Payload(t, _, _) => vec![t.as_str()],
        })
        .collect();
    let unique: HashSet<&str> = names.iter().copied().collect();
    (!names.is_empty() && unique.len() == names.len() && names.iter().all(|n| valid_name(n))).then_some((key, plan))
}

/// If every `oneOf`/`anyOf` alternative is an object with a common required
/// property whose schema is a string `const` (or one-element `enum`), return
/// that tag property and the alternatives (with their indices).
fn tagged_alternatives(o: &Obj) -> Option<(String, Vec<(usize, &Obj)>)> {
    let alts = o.get("oneOf").or_else(|| o.get("anyOf"))?.as_array()?;
    let alts: Vec<(usize, &Obj)> = alts
        .iter()
        .enumerate()
        .filter(|(_, a)| !is_null_schema(a))
        .map(|(i, a)| a.as_object().map(|a| (i, a)))
        .collect::<Option<_>>()?;
    let (_, first) = alts.first()?;
    let tag = first.get("properties")?.as_object()?.keys().find(|k| {
        alts.iter().all(|(_, a)| {
            let required = a
                .get("required")
                .and_then(Json::as_array)
                .is_some_and(|r| r.iter().any(|x| x.as_str() == Some(k.as_str())));
            required && a.get("properties").and_then(|p| p.get(k.as_str())).and_then(const_str).is_some()
        })
    })?;
    valid_name(tag).then(|| (tag.clone(), alts))
}

/// The string of a `const` or a one-element `enum`.
fn const_str(s: &Json) -> Option<String> {
    if let Some(c) = s.get("const").and_then(Json::as_str) {
        return Some(c.to_owned());
    }

    match s.get("enum")?.as_array()?.as_slice() {
        [one] => one.as_str().map(str::to_owned),
        _ => None,
    }
}

/// The `any` type.
fn any() -> TypeRef {
    TypeRef::new(TypeKind::Any)
}

/// NOVA field and variant names: `[A-Za-z_][A-Za-z0-9_-]*`.
fn valid_name(n: &str) -> bool {
    n.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A definition name: `[A-Z][A-Za-z0-9_]*`. Invalid characters are dropped
/// and the letter after them is capitalized (`my-type` → `MyType`).
fn sanitize_def_name(s: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(if upper { c.to_ascii_uppercase() } else { c });
            upper = false;
        } else {
            upper = true;
        }
    }

    if !out.starts_with(|c: char| c.is_ascii_uppercase()) {
        out.insert(0, 'T');
    }

    out
}

/// `snake_case`/`kebab-case` → `PascalCase`, for generated definition names.
fn pascal(s: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(if upper { c.to_ascii_uppercase() } else { c });
            upper = false;
        } else {
            upper = true;
        }
    }

    out
}

/// Whether `s` is `{"type": "null"}`, possibly with annotations.
fn is_null_schema(s: &Json) -> bool {
    s.get("type").and_then(Json::as_str) == Some(NULL_TYPE) && s.as_object().is_some_and(|o| o.keys().all(|k| k == "type" || !is_structural(k)))
}

/// Remove `null` from `type` arrays, `enum`s and `anyOf`/`oneOf`; report whether it was there.
fn strip_null(o: &Obj) -> (Obj, bool) {
    let mut o = o.clone();
    let mut nullable = false;
    if let Some(Json::Array(ts)) = o.get_mut("type")
        && ts.len() > 1
        && ts.iter().any(|t| t == NULL_TYPE)
    {
        ts.retain(|t| t != NULL_TYPE);
        nullable = true;
        if ts.len() == 1 {
            let t = ts[0].clone();
            o.insert("type".into(), t);
        }
    }

    if let Some(Json::Array(vals)) = o.get_mut("enum")
        && vals.len() > 1
        && vals.iter().any(Json::is_null)
    {
        vals.retain(|v| !v.is_null());
        nullable = true;
    }

    for k in ["anyOf", "oneOf"] {
        if let Some(Json::Array(alts)) = o.get_mut(k)
            && alts.len() > 1
            && alts.iter().any(is_null_schema)
        {
            alts.retain(|a| !is_null_schema(a));
            nullable = true;
        }
    }

    (o, nullable)
}

/// The `type` implied by type-specific keywords when `type` is missing.
fn infer_type(o: &Obj) -> Option<Json> {
    let has = |ks: &[&str]| ks.iter().any(|k| o.contains_key(*k));
    let t = if has(&[
        "properties",
        "required",
        "additionalProperties",
        "patternProperties",
        "propertyNames",
        "minProperties",
        "maxProperties",
    ]) {
        "object"
    } else if has(&["items", "prefixItems", "uniqueItems", "minItems", "maxItems"]) {
        "array"
    } else if has(&["minLength", "maxLength", "pattern", "format", "contentEncoding"]) {
        "string"
    } else if has(&["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum", "multipleOf"]) {
        "number"
    } else {
        return None;
    };

    Some(Json::String(t.into()))
}

/// The common `type` of `enum`/`const` values (integers widen to `number`).
fn type_of_values(vals: &[Json]) -> Option<Json> {
    let names: HashSet<&str> = vals
        .iter()
        .filter(|v| !v.is_null())
        .map(|v| match v {
            Json::Bool(_) => "boolean",
            Json::Number(n) if n.is_f64() => "number",
            Json::Number(_) => "integer",
            Json::String(_) => "string",
            Json::Array(_) => "array",
            Json::Object(_) => "object",
            Json::Null => NULL_TYPE,
        })
        .collect();
    match names.len() {
        1 => names.into_iter().next().map(|n| Json::String(n.into())),
        2 if names.contains("integer") && names.contains("number") => Some(Json::String("number".into())),
        _ => None,
    }
}

/// A JSON number as an exact bound: integers stay integers, others become
/// decimals via their shortest round-trip digits.
fn num(n: &serde_json::Number) -> Num {
    if let Some(i) = n.as_i64() {
        return Num::int(i.into());
    }

    if let Some(u) = n.as_u64() {
        return Num::int(u.into());
    }

    let f = n.as_f64().unwrap_or(0.0);
    match Decimal::parse(&format!("{f:?}")) {
        Ok(d) => match d.to_i128() {
            Some(i) => Num::int(i),
            None => Num(Value::Decimal(d)),
        },
        Err(_) => Num(Value::Float(f)),
    }
}

/// An inclusive integer bound from `minimum`/`exclusiveMinimum` (or the
/// maximum pair), also accepting draft-04 boolean `exclusiveMinimum`.
fn int_bound(o: &Obj, incl: &str, excl: &str, lower: bool) -> Option<i128> {
    let bound = |v: &Json, exclusive: bool| -> Option<i128> {
        let n = v.as_number()?;
        match n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from)) {
            Some(i) if exclusive => Some(if lower { i + 1 } else { i - 1 }),
            Some(i) => Some(i),
            None => {
                let f = n.as_f64()?;
                // Fractional bounds round inwards; whole ones step past when exclusive.
                let step = exclusive && f.fract() == 0.0;
                #[expect(clippy::cast_possible_truncation, reason = "float-to-int `as` saturates, clamping out-of-range bounds")]
                Some(if lower {
                    f.ceil() as i128 + i128::from(step)
                } else {
                    f.floor() as i128 - i128::from(step)
                })
            }
        }
    };

    let a = o.get(incl).and_then(|v| bound(v, o.get(excl) == Some(&Json::Bool(true))));
    let b = o.get(excl).filter(|v| v.is_number()).and_then(|v| bound(v, true));
    match (a, b) {
        (Some(a), Some(b)) => Some(if lower { a.max(b) } else { a.min(b) }),
        (a, b) => a.or(b),
    }
}

/// Decode the percent-escapes of a URI-fragment reference (`#/$defs/My%20Type`).
fn normalize_ref(r: &str) -> String {
    let b = r.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 3 <= b.len()
            && let Some(x) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(x);
            i += 3;
            continue;
        }

        out.push(b[i]);
        i += 1;
    }

    match String::from_utf8(out) {
        Ok(s) if s.is_empty() => "#".into(),
        Ok(s) => s,
        Err(_) => r.to_owned(),
    }
}

/// Escape a key for use in a JSON pointer (RFC 6901).
fn escape_pointer(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

/// Replace one alias in each alias cycle (`$ref` chains that never reach a
/// type, which JSON Schema allows) with `any`, so the result is valid schema
/// language.
fn break_alias_cycles(schema: &mut Schema, warnings: &mut Vec<String>) {
    for i in 0..schema.defs.len() {
        let name = &schema.defs[i].0;
        let mut seen = HashSet::new();
        let mut cur = &schema.defs[i].1;
        let cyclic = loop {
            let Def::Alias(TypeRef { kind: TypeKind::Named(n), .. }) = cur else {
                break false;
            };
            // A cycle not through `name` is broken when one of its members comes up.
            if n == name || !seen.insert(n) {
                break n == name;
            }

            match schema.def(n) {
                Some(d) => cur = d,
                None => break false,
            }
        };

        if cyclic {
            warnings.push(format!("{name}: $ref cycle that never reaches a type; using any"));
            schema.defs[i].1 = Def::Alias(TypeRef::new(TypeKind::Any));
        }
    }
}

/// Resolve each field default against its type (so `"active"` becomes the
/// variant `!active` and RFC 3339 strings become timestamps); drop those that
/// do not resolve. A field with a default is written `name: T = d`: it may be
/// absent either way, so `name?:` would add nothing.
fn resolve_defaults(schema: &mut Schema, warnings: &mut Vec<String>) {
    let snapshot = schema.clone();
    for (name, def) in &mut schema.defs {
        let Def::Record(r) = def else { continue };
        for f in &mut r.fields {
            let Some(d) = &f.default else { continue };
            match snapshot.resolve_as(d, &f.ty) {
                Ok(v) => {
                    f.default = Some(v);
                    f.optional = false;
                }
                Err(issues) => {
                    let why = issues.iter().map(std::string::ToString::to_string).collect::<Vec<_>>().join("; ");
                    warnings.push(format!(
                        "{name}.{}: default {} dropped: {why}",
                        f.name,
                        novafile_json::to_json_string(d, false).0
                    ));
                    f.default = None;
                }
            }
        }
    }
}

/// The I-Regexp for a string schema's `pattern` (spec §11.2):
/// `x-nova-pattern` if present; else an anchored `^(?:q)$` or `^q$` with
/// `[^\n\r]` turned back into `.`; else an unanchored search, spelled out
/// with `(.|\n|\r)*` on each open end. `None` if there is no pattern, or
/// `q` uses anchors itself (they would turn into literal characters).
fn read_pattern(o: &Obj) -> Option<String> {
    const ANY: &str = "(.|\\n|\\r)*";
    if let Some(p) = o.get("x-nova-pattern").and_then(Json::as_str) {
        return Some(p.to_owned());
    }

    let p = o.get("pattern")?.as_str()?;
    let (start, p) = p.strip_prefix('^').map_or((false, p), |q| (true, q));
    let (end, p) = match p.strip_suffix('$') {
        Some(q) if !q.ends_with('\\') => (true, q),
        _ => (false, p),
    };

    let wrapped = start && end && p.starts_with("(?:") && p.ends_with(')');
    let q = if wrapped { &p[3..p.len() - 1] } else { p };
    // Back to I-Regexp: `[^\n\r]` is `.`, `\$` the literal `$`. Any other `^`
    // or `$` outside a class is an anchor, which a whole-string match cannot
    // express, and so is a top-level `|` next to a single pair of anchors.
    let (mut out, mut class, mut depth, mut alt) = (String::new(), false, 0i32, false);
    let q = q.replace("[^\\n\\r]", ".");
    let mut cs = q.chars();
    while let Some(c) = cs.next() {
        match c {
            '\\' => {
                match cs.next()? {
                    '$' => out.push('$'),
                    n => {
                        out.push('\\');
                        out.push(n);
                    }
                }

                continue;
            }
            '[' if !class => class = true,
            ']' if class => class = false,
            '^' | '$' if !class => return None,
            '(' if !class => depth += 1,
            ')' if !class => depth -= 1,
            '|' if !class && depth == 0 => alt = true,
            _ => {}
        }

        out.push(c);
    }

    if alt && (start || end) && !wrapped {
        return None;
    }

    let q = out;
    Some(match (start, end) {
        (true, true) => q,
        (true, false) => format!("({q}){ANY}"),
        (false, true) => format!("{ANY}({q})"),
        (false, false) => format!("{ANY}({q}){ANY}"),
    })
}

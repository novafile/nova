//! The NOVA schema language (spec §6): optional, structural types with
//! constraints, stable field identities, schema-directed resolution of text
//! values, `@key` uniqueness and evolution checks.

mod compat;
pub mod derive;
mod descriptor;
mod model;
mod parse;
mod pattern;
mod positional;
mod resolve;
mod version;
mod warn;

pub use compat::{Change, check_compatibility, widens};
pub use derive::{NovaSchema, SchemaGen, schema_for};
pub use descriptor::{META_SCHEMA_SRC, meta_schema};
pub use model::*;
#[cfg(feature = "derive")]
pub use novafile_derive::NovaSchema;
pub use parse::{SchemaError, parse_schema, parse_type};
pub use pattern::{Pattern, SCHEMA_BUDGET};
pub use resolve::Issue;
pub use version::{ChangeKind, PreId, SchemaRef, Version, valid_schema_name};
pub use warn::UnionWarning;

impl Schema {
    /// Canonical rendering used for fingerprints.
    #[must_use]
    pub fn to_canonical_text(&self) -> String {
        self.to_string()
    }

    /// The canonical text with the `@x-…` extension directives kept after the
    /// `@schema` line: what tools write when they put a schema back in a file.
    #[must_use]
    pub fn to_source_text(&self) -> String {
        let canon = self.to_canonical_text();
        let ext: String = self.extensions.iter().flat_map(|e| ["@", e.as_str(), "\n"]).collect();
        match canon.split_once('\n') {
            Some((first, rest)) if first.starts_with("@schema ") => format!("{first}\n{ext}{rest}"),
            _ => ext + &canon,
        }
    }

    /// The first 64 bits of SHA-256 over the canonical text, as 16 hex digits (spec §6.6).
    #[must_use]
    pub fn fingerprint_hex(&self) -> String {
        novafile_value::revision(&novafile_value::Value::String(self.to_canonical_text())).hex()[..16].to_owned()
    }

    /// `name@version#<16 hex>` (spec §6.6), or `anonymous#<hex>`.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        match self.reference() {
            Some(r) => format!("{r}#{}", self.fingerprint_hex()),
            None => format!("anonymous#{}", self.fingerprint_hex()),
        }
    }

    /// `name@version`, if the schema is named.
    #[must_use]
    pub fn reference(&self) -> Option<SchemaRef> {
        Some(SchemaRef {
            name: self.name.clone()?,
            version: self.version.clone()?,
            fingerprint: None,
        })
    }

    /// Check a document's `@schema` reference against this schema (spec §6.7):
    /// same name, a version this schema can read, and a matching fingerprint if given.
    ///
    /// # Errors
    ///
    /// Fails, with the reason, if `reference` is malformed, this schema has no
    /// name and version, or the name, version or fingerprint does not match.
    pub fn accepts_reference(&self, reference: &str) -> Result<(), String> {
        let r = SchemaRef::parse(reference)?;
        let mine = self.reference().ok_or("an anonymous schema cannot check @schema references")?;
        if r.name != mine.name {
            return Err(format!("document is for schema {:?}, not {:?}", r.name, mine.name));
        }

        if !mine.version.can_read(&r.version) {
            return Err(format!("schema {mine} cannot read data written for {}@{} (spec §6.7)", r.name, r.version));
        }

        if let Some(fp) = &r.fingerprint
            && r.version.precedence(&mine.version) == std::cmp::Ordering::Equal
            && *fp != self.fingerprint_hex()
        {
            return Err(format!("fingerprint mismatch: document expects #{fp}, schema is #{}", self.fingerprint_hex()));
        }

        Ok(())
    }

    /// Canonical text without the `@schema` line: equal iff no semantic change.
    fn body_text(&self) -> String {
        let mut s = self.clone();
        s.name = None;
        s.version = None;
        s.to_canonical_text()
    }
}

/// Result of checking a new schema version against an old one (spec §6.7).
#[derive(Clone, Debug)]
pub struct VersionCheck {
    /// Every difference found between the two schemas.
    pub changes: Vec<Change>,
    /// The overall classification of `changes`.
    pub kind: ChangeKind,
    /// The old schema's version.
    pub old: Version,
    /// The new schema's version.
    pub new: Version,
    /// The smallest acceptable new version.
    pub minimum: Version,
    /// Problems with the version bump; empty when it is acceptable.
    pub problems: Vec<String>,
}

/// Classify the changes from `old` to `new` and verify the `SemVer` bump.
///
/// # Errors
///
/// Fails if either schema lacks `@schema name@version` or the names differ.
/// An unacceptable bump is reported in [`VersionCheck::problems`] instead.
pub fn check_versions(old: &Schema, new: &Schema) -> Result<VersionCheck, String> {
    let (o, n) = (
        old.reference().ok_or("old schema has no `@schema name@version`")?,
        new.reference().ok_or("new schema has no `@schema name@version`")?,
    );

    if o.name != n.name {
        return Err(format!("schema names differ: {} vs {}", o.name, n.name));
    }

    let changes = check_compatibility(old, new);
    let kind = if changes.iter().any(|c| c.breaking) {
        ChangeKind::Breaking
    } else if old.body_text() != new.body_text() {
        ChangeKind::Compatible
    } else {
        ChangeKind::None
    };

    let minimum = o.version.required_bump(kind);
    let mut problems = Vec::new();
    if n.version.precedence(&o.version) != std::cmp::Ordering::Greater {
        problems.push(format!("new version {} must be greater than {}", n.version, o.version));
    } else if n.version.is_prerelease() {
        // Pre-releases carry no compatibility promise; any increase is fine.
    } else {
        let ok = match kind {
            // A breaking change must leave the old compatibility range.
            ChangeKind::Breaking => !n.version.can_read(&o.version),
            // Additions need at least a minor bump (0.y: patch), and must stay
            // readable unless they over-bump to a major (0.y: minor) release.
            ChangeKind::Compatible => {
                let (nv, ov) = (&n.version, &o.version);
                nv.precedence(&minimum) != std::cmp::Ordering::Less
                    && (nv.major != ov.major || (nv.major == 0 && nv.minor != ov.minor) || nv.can_read(ov) || (ov.major == 0 && ov.minor == 0))
            }
            ChangeKind::None => true,
        };

        if !ok {
            problems.push(match kind {
                ChangeKind::Breaking => format!("breaking changes require at least {minimum}, got {}", n.version),
                _ => format!("these changes require at least {minimum}, got {}", n.version),
            });
        }
    }

    Ok(VersionCheck {
        changes,
        kind,
        old: o.version,
        new: n.version,
        minimum,
        problems,
    })
}

impl Schema {
    /// The alias through which `t` reaches the alias `name` again without
    /// passing through a container, record or enum (spec §6.1), if any.
    /// Such a cycle makes a schema invalid; [`parse_schema`] rejects it, and
    /// resolution reports it for schemas built in code.
    #[must_use]
    pub fn alias_cycle(&self, name: &str, t: &TypeRef) -> Option<String> {
        fn reach<'a>(s: &'a Schema, name: &str, t: &'a TypeRef, seen: &mut Vec<&'a str>) -> Option<String> {
            match &t.kind {
                TypeKind::Named(n) if n == name => Some(n.clone()),
                TypeKind::Named(n) if !seen.contains(&n.as_str()) => {
                    seen.push(n);
                    match s.def(n) {
                        Some(Def::Alias(a)) => reach(s, name, a, seen),
                        _ => None,
                    }
                }
                TypeKind::Union(ms) => ms.iter().find_map(|m| reach(s, name, m, seen)),
                _ => None,
            }
        }

        reach(self, name, t, &mut Vec::new())
    }

    /// The record definitions among the members of `t` if it is a union,
    /// flattened through aliases to unions (spec §6.2), or `t`'s own record.
    #[must_use]
    pub fn member_records<'a>(&'a self, t: &'a TypeRef) -> Vec<(&'a str, &'a RecordDef)> {
        /// Adds the records below `t`; `seen` names each definition once, so
        /// aliases shared by several members are not expanded again.
        fn collect<'a>(s: &'a Schema, t: &'a TypeRef, out: &mut Vec<(&'a str, &'a RecordDef)>, seen: &mut Vec<&'a str>) {
            if let TypeKind::Named(n) = &t.kind {
                if seen.contains(&n.as_str()) {
                    return;
                }

                seen.push(n);
            }

            match s.deref(t) {
                (TypeRef { kind: TypeKind::Union(ms), .. }, _) => ms.iter().for_each(|m| collect(s, m, out, seen)),
                (TypeRef { kind: TypeKind::Named(n), .. }, Some(Def::Record(r))) if !out.iter().any(|(m, _)| m == n) => out.push((n, r)),
                _ => {}
            }
        }

        let mut out = Vec::new();
        collect(self, t, &mut out, &mut Vec::new());
        out
    }

    /// The `@key` field names of the elements of a list or set of `t`: those
    /// of a record, or, for a union, of its keyed record members (spec §6.2).
    #[must_use]
    pub fn key_fields_of<'a>(&'a self, t: &'a TypeRef) -> Vec<&'a str> {
        self.member_records(t)
            .into_iter()
            .map(|(_, r)| r.key_fields())
            .find(|k| !k.is_empty())
            .unwrap_or_default()
    }

    /// Follow aliases to the underlying type (and the definition, if named).
    #[must_use]
    pub fn deref<'a>(&'a self, t: &'a TypeRef) -> (&'a TypeRef, Option<&'a Def>) {
        let mut cur = t;
        for _ in 0..64 {
            match &cur.kind {
                TypeKind::Named(n) => match self.def(n) {
                    Some(Def::Alias(a)) => cur = a,
                    Some(d) => return (cur, Some(d)),
                    None => return (cur, None),
                },
                _ => return (cur, None),
            }
        }

        (cur, None)
    }

    /// The declared type at a concrete path below the root type (fields and
    /// indices only), together with the field definition when the last
    /// segment names a record field. Used by editors for hover and completion.
    /// Below a union, the first member the segment fits is followed.
    #[must_use]
    pub fn type_at(&self, path: &novafile_text::Path) -> Option<(TypeRef, Option<FieldDef>)> {
        let mut cur = (self.root_type()?, None);
        for seg in &path.0 {
            cur = self.step(&cur.0, seg, &mut Vec::new())?;
        }

        Some(cur)
    }

    /// The type one path segment below `t`, for [`Self::type_at`]. `seen`
    /// holds the names already tried below a union, so each is tried once.
    fn step(&self, t: &TypeRef, seg: &novafile_text::Segment, seen: &mut Vec<String>) -> Option<(TypeRef, Option<FieldDef>)> {
        use novafile_text::Segment;
        if let TypeKind::Named(n) = &t.kind {
            if seen.contains(n) {
                return None;
            }

            seen.push(n.clone());
        }

        let (t, def) = self.deref(t);
        Some(match (seg, &t.kind, def) {
            (Segment::Field(name), _, Some(Def::Record(r))) => {
                let f = r.field(name)?.clone();
                (f.ty.clone(), Some(f))
            }
            (_, TypeKind::Union(ms), _) => return ms.iter().find_map(|m| self.step(m, seg, seen)),
            (Segment::Field(_) | Segment::Key(_), TypeKind::Map(_, v), _) => ((**v).clone(), None),
            (Segment::Index(_) | Segment::Match(..) | Segment::Key(_) | Segment::All | Segment::Filter(..), TypeKind::List(e) | TypeKind::Set(e), _) => {
                ((**e).clone(), None)
            }
            (Segment::Index(i), TypeKind::Tuple(ts), _) => (ts.get(usize::try_from(*i).ok()?)?.clone(), None),
            (_, TypeKind::Any, _) => (TypeRef::new(TypeKind::Any), None),
            _ => return None,
        })
    }
}

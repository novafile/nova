//! The NOVA schema language (spec §6): optional, structural types with
//! constraints, stable field identities, schema-directed resolution of text
//! values, `@key` uniqueness and evolution checks.

mod compat;
mod model;
mod parse;
mod resolve;
mod version;

pub use compat::{check_compatibility, widens, Change};
pub use model::*;
pub use parse::{parse_schema, parse_type, SchemaError};
pub use resolve::Issue;
pub use version::{valid_schema_name, ChangeKind, PreId, SchemaRef, Version};

impl Schema {
    /// Canonical rendering used for fingerprints.
    pub fn to_canonical_text(&self) -> String {
        self.to_string()
    }

    /// The first 64 bits of SHA-256 over the canonical text, as 16 hex digits (spec §6.6).
    pub fn fingerprint_hex(&self) -> String {
        nova_value::revision(&nova_value::Value::String(self.to_canonical_text())).hex()[..16].to_owned()
    }

    /// `name@version#<16 hex>` (spec §6.6), or `anonymous#<hex>`.
    pub fn fingerprint(&self) -> String {
        match self.reference() {
            Some(r) => format!("{r}#{}", self.fingerprint_hex()),
            None => format!("anonymous#{}", self.fingerprint_hex()),
        }
    }

    /// `name@version`, if the schema is named.
    pub fn reference(&self) -> Option<SchemaRef> {
        Some(SchemaRef {
            name: self.name.clone()?,
            version: self.version.clone()?,
            fingerprint: None,
        })
    }

    /// Check a document's `@schema` reference against this schema (spec §6.7):
    /// same name, a version this schema can read, and a matching fingerprint if given.
    pub fn accepts_reference(&self, reference: &str) -> Result<(), String> {
        let r = SchemaRef::parse(reference)?;
        let mine = self.reference().ok_or("an anonymous schema cannot check @schema references")?;
        if r.name != mine.name {
            return Err(format!("document is for schema {:?}, not {:?}", r.name, mine.name));
        }
        if !mine.version.can_read(&r.version) {
            return Err(format!("schema {mine} cannot read data written for {}@{} (spec §6.7)", r.name, r.version));
        }
        if let Some(fp) = &r.fingerprint {
            if r.version == mine.version && *fp != self.fingerprint_hex() {
                return Err(format!("fingerprint mismatch: document expects #{fp}, schema is #{}", self.fingerprint_hex()));
            }
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
    pub changes: Vec<Change>,
    pub kind: ChangeKind,
    pub old: Version,
    pub new: Version,
    /// The smallest acceptable new version.
    pub minimum: Version,
    /// Problems with the version bump; empty when it is acceptable.
    pub problems: Vec<String>,
}

/// Classify the changes from `old` to `new` and verify the SemVer bump.
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
            // Additions need at least a minor bump (0.y: patch), and must stay readable.
            ChangeKind::Compatible => {
                n.version.precedence(&minimum) != std::cmp::Ordering::Less
                    && (n.version.major != o.version.major || n.version.can_read(&o.version) || (o.version.major == 0 && o.version.minor == 0))
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

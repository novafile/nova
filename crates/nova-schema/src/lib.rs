//! The NOVA schema language (spec §6): optional, structural types with
//! constraints, stable field identities, schema-directed resolution of text
//! values, `@key` uniqueness and evolution checks.

mod compat;
mod model;
mod parse;
mod resolve;

pub use compat::{check_compatibility, widens, Change};
pub use model::*;
pub use parse::{parse_schema, parse_type, SchemaError};
pub use resolve::Issue;

impl Schema {
    /// Canonical rendering used for fingerprints.
    pub fn to_canonical_text(&self) -> String {
        self.to_string()
    }

    /// `name#<16 hex>`: the first 64 bits of SHA-256 over the canonical text (spec §6.6).
    pub fn fingerprint(&self) -> String {
        let h = nova_value::revision(&nova_value::Value::String(self.to_canonical_text())).hex();
        format!("{}#{}", self.name.as_deref().unwrap_or("anonymous"), &h[..16])
    }
}

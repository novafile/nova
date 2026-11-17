//! `schemars::JsonSchema` for the value types (feature `schemars`).
//!
//! The schemas describe the *natural* serde form of each type (the form
//! `serde_json` produces, see [`crate::serde_support`]): timestamps,
//! durations, decimals and rationals are strings. Each carries an
//! `x-nova-kind` marker so that `novafile-schemars` maps it back to the exact
//! NOVA kind instead of `string`.
//!
//! | Type | JSON Schema |
//! | --- | --- |
//! | [`Timestamp`] | `{"type": "string", "format": "date-time", "x-nova-kind": "timestamp"}` |
//! | [`Duration`] | `{"type": "string", "pattern": …, "x-nova-kind": "duration"}` |
//! | [`Decimal`] | `{"type": "string", "pattern": …, "x-nova-kind": "decimal"}` |
//! | [`Rational`] | `{"type": "string", "pattern": …, "x-nova-kind": "rational"}` |
//! | [`Value`] | `true` (any value) |

use crate::{Decimal, Duration, Rational, Timestamp, Value};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use std::borrow::Cow;

/// Patterns for the string forms of the text-backed kinds. They accept the
/// canonical text that NOVA writes (and, for durations, ISO 8601 as well).
pub mod patterns {
    /// `1.5`, `-3`, `1e-7`.
    pub const DECIMAL: &str = r"^-?[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?$";
    /// `22/7`, `-1/2`.
    pub const RATIONAL: &str = r"^-?(0|[1-9][0-9]*)/[1-9][0-9]*$";
    /// `1h30m`, `1.5s`, `-2d`, or ISO 8601 `PT1H30M`.
    pub const DURATION: &str = r"^-?(P[0-9WDTHMS.,]+|([0-9]+(\.[0-9]{1,9})?[dhms])+)$";
}

macro_rules! text_schema {
    ($ty:ty, $name:literal, $schema:tt) => {
        impl JsonSchema for $ty {
            fn inline_schema() -> bool {
                true
            }

            fn schema_name() -> Cow<'static, str> {
                $name.into()
            }

            fn schema_id() -> Cow<'static, str> {
                concat!("novafile_value::", $name).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!($schema)
            }
        }
    };
}

text_schema!(Timestamp, "Timestamp", {
    "type": "string",
    "format": "date-time",
    "x-nova-kind": "timestamp"
});
text_schema!(Duration, "Duration", {
    "type": "string",
    "pattern": patterns::DURATION,
    "x-nova-kind": "duration"
});
text_schema!(Decimal, "Decimal", {
    "type": "string",
    "pattern": patterns::DECIMAL,
    "x-nova-kind": "decimal"
});
text_schema!(Rational, "Rational", {
    "type": "string",
    "pattern": patterns::RATIONAL,
    "x-nova-kind": "rational"
});

impl JsonSchema for Value {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "Value".into()
    }

    fn schema_id() -> Cow<'static, str> {
        "novafile_value::Value".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        Schema::from(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_carry_nova_kinds() {
        let s = schemars::schema_for!(Timestamp);
        assert_eq!(s.get("x-nova-kind").and_then(|v| v.as_str()), Some("timestamp"));
        assert_eq!(s.get("format").and_then(|v| v.as_str()), Some("date-time"));
        let mut generator = SchemaGenerator::default();
        assert_eq!(generator.subschema_for::<Value>().as_bool(), Some(true));
        let d = generator.subschema_for::<Decimal>();
        assert_eq!(d.get("pattern").and_then(|v| v.as_str()), Some(patterns::DECIMAL));
    }
}

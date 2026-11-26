//! NOVA, the Notation for Objects, Values & Agents: one precise semantic model with several interchangeable
//! representations. This crate re-exports the reference implementation;
//! see the [specification](https://github.com/novafile/spec/blob/main/SPEC.md).
//!
//! | Module | Crate | Feature |
//! | --- | --- | --- |
//! | [`value`] | `novafile-value`: value model, canonical encoding, revisions | always |
//! | [`text`] | `novafile-text`: text parser and writers, paths, formatting | always |
//! | [`binary`] | `novafile-binary`: frames and streams | always |
//! | [`schema`] | `novafile-schema`: schemas, resolution, SemVer, descriptors | always (`derive` for `#[derive(NovaSchema)]`; `url`, `uuid`, `semver` map those crates' types to `std.Uri`, `std.Uuid`, `std.SemVer`) |
//! | [`patch`] | `novafile-patch`: queries, patches, diffs | always |
//! | [`serde`] | `novafile-serde` | `serde` (default) |
//! | [`facet`] | `novafile-facet` | `facet` |
//! | [`json`], [`formats`] | `novafile-json`, `novafile-formats` (YAML, TOML, CBOR) | `formats` |
//! | [`proto`] | `novafile-proto` | `proto` |
//! | [`template`] | `novafile-template` | `template` |
//! | [`lint`] | `novafile-lint` | `lint` |
//! | [`schemars`] | `novafile-schemars` | `schemars` |
//!
//! ```
//! let v = novafile::text::parse("services:\n  - id: api\n    replicas: 5\n").unwrap();
//! let r = novafile::patch::get(&v, "services[id=api].replicas").unwrap();
//! assert_eq!(r.value, novafile::Value::Int(5));
//! ```

#[cfg(feature = "binary")]
pub use novafile_binary as binary;
#[cfg(feature = "patch")]
pub use novafile_patch as patch;
#[cfg(feature = "schema")]
pub use novafile_schema as schema;
pub use novafile_text as text;
pub use novafile_value as value;
pub use novafile_value::{Record, Value};

#[cfg(feature = "derive")]
pub use novafile_schema::NovaSchema;

#[cfg(feature = "facet")]
pub use novafile_facet as facet;
#[cfg(feature = "formats")]
pub use novafile_formats as formats;
#[cfg(feature = "formats")]
pub use novafile_json as json;
#[cfg(feature = "lint")]
pub use novafile_lint as lint;
#[cfg(feature = "proto")]
pub use novafile_proto as proto;
#[cfg(feature = "schemars")]
pub use novafile_schemars as schemars;
#[cfg(feature = "serde")]
pub use novafile_serde as serde;
#[cfg(feature = "template")]
pub use novafile_template as template;

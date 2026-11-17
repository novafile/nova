//! YAML, TOML and CBOR adapters for the NOVA value model (spec §11.2).
//!
//! Every adapter follows the discipline of `novafile-json` (spec §11.1):
//! conversion is lossless wherever the target format can represent a NOVA
//! kind, and every mapping that is not is reported to the caller as a
//! [`Warning`] (or refused with an error where no sensible mapping exists,
//! such as `null` in TOML). Callers that want "strict" conversion simply
//! treat a non-empty warning list as a failure.
//!
//! - [`yaml`]: YAML 1.2 core schema, aliases expanded under the §9.5 output
//!   budget, merge keys and non-core tags rejected.
//! - [`toml`]: TOML 1.0 via the `toml` crate.
//! - [`cbor`]: a self-contained RFC 8949 codec, including core deterministic
//!   encoding (§4.2.1).

pub mod cbor;
pub mod toml;
pub mod yaml;

/// A lossy mapping performed while converting a NOVA value to another
/// format. `path` uses the same notation as `novafile-json` (`.key`, `[i]`;
/// empty for the root).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", if self.path.is_empty() { "." } else { &self.path }, self.message)
    }
}

/// Collects warnings while tracking the path of the value being written.
#[derive(Default)]
pub(crate) struct Reporter {
    pub(crate) path: String,
    pub(crate) warnings: Vec<Warning>,
}

impl Reporter {
    pub(crate) fn warn(&mut self, message: &str) {
        self.warnings.push(Warning {
            path: self.path.clone(),
            message: message.to_owned(),
        });
    }

    /// Run `f` with `.key` appended to the current path.
    pub(crate) fn field<T>(&mut self, key: &str, f: impl FnOnce(&mut Self) -> T) -> T {
        let n = self.path.len();
        self.path.push('.');
        self.path.push_str(key);
        let r = f(self);
        self.path.truncate(n);
        r
    }

    /// Run `f` with `[i]` appended to the current path.
    pub(crate) fn index<T>(&mut self, i: usize, f: impl FnOnce(&mut Self) -> T) -> T {
        let n = self.path.len();
        self.path.push_str(&format!("[{i}]"));
        let r = f(self);
        self.path.truncate(n);
        r
    }
}

/// Maximum nesting depth accepted by every decoder in this crate (spec §12).
pub(crate) const MAX_DEPTH: usize = 128;

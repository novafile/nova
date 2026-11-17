//! The error type shared by the serializer and the deserializer.

use std::fmt;

/// A (de)serialization error, with the path of the offending value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// Human-readable description, without the path.
    message: String,
    /// Path segments, outermost first (`services`, `[1]`, `port`).
    path: Vec<String>,
}

impl Error {
    /// An error with the given message and an empty path.
    pub fn new(m: impl Into<String>) -> Self {
        Error {
            message: m.into(),
            path: Vec::new(),
        }
    }

    /// The message, without the path.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The path as NOVA path syntax, e.g. `services[1].port`.
    #[must_use]
    pub fn path(&self) -> String {
        let mut s = String::new();
        for seg in &self.path {
            if !seg.starts_with('[') && !s.is_empty() {
                s.push('.');
            }

            s.push_str(seg);
        }

        s
    }

    /// Prepend a path segment while the error propagates outwards.
    pub(crate) fn at(mut self, seg: String) -> Self {
        self.path.insert(0, seg);
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path(), self.message)
        }
    }
}

impl std::error::Error for Error {}

impl serde::ser::Error for Error {
    fn custom<T: fmt::Display>(m: T) -> Self {
        Error::new(m.to_string())
    }
}

impl serde::de::Error for Error {
    fn custom<T: fmt::Display>(m: T) -> Self {
        Error::new(m.to_string())
    }
}

/// The path segment for a record field, quoted when the key is not a bare name.
pub(crate) fn field_seg(k: &str) -> String {
    novafile_text::key_text(k)
}

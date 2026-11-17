use std::fmt;

/// A (de)serialization error, with the path of the offending value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    message: String,
    /// Path segments, outermost first (`services`, `[1]`, `port`).
    path: Vec<String>,
}

impl Error {
    pub fn new(m: impl Into<String>) -> Self {
        Error {
            message: m.into(),
            path: Vec::new(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// The path as NOVA path syntax, e.g. `services[1].port`.
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

    pub(crate) fn at(mut self, seg: String) -> Self {
        self.path.insert(0, seg);
        self
    }

    pub(crate) fn at_field(self, key: &str) -> Self {
        self.at(nova_text::key_text(key))
    }

    pub(crate) fn at_index(self, i: usize) -> Self {
        self.at(format!("[{i}]"))
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

/// Errors reported by facet-reflect carry facet's own path; ours is tracked
/// separately in NOVA path syntax, so only the kind is kept.
impl From<facet_reflect::ReflectError> for Error {
    fn from(e: facet_reflect::ReflectError) -> Self {
        Error::new(e.kind.to_string())
    }
}

impl From<facet_reflect::VariantError> for Error {
    fn from(e: facet_reflect::VariantError) -> Self {
        Error::new(e.to_string())
    }
}

impl From<facet_reflect::AllocError> for Error {
    fn from(e: facet_reflect::AllocError) -> Self {
        Error::new(format!("cannot allocate {}: {}", e.shape, e.operation))
    }
}

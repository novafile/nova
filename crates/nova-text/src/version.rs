//! Versions of the NOVA language and the template language (spec §13.1).

use std::fmt;

/// A `MAJOR.MINOR` language version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LangVersion {
    pub major: u32,
    pub minor: u32,
}

/// The NOVA language version this implementation supports (data, patch, query and schema syntax).
pub const NOVA_VERSION: LangVersion = LangVersion { major: 0, minor: 1 };
/// The template language version this implementation supports (spec §9).
pub const TEMPLATE_VERSION: LangVersion = LangVersion { major: 0, minor: 1 };

impl LangVersion {
    /// Parse `MAJOR.MINOR` (decimal, no leading zeros).
    pub fn parse(s: &str) -> Result<LangVersion, String> {
        let err = || format!("invalid language version {s:?} (expected MAJOR.MINOR, e.g. 0.1)");
        let (a, b) = s.split_once('.').ok_or_else(err)?;
        let num = |t: &str| -> Result<u32, String> {
            if t.is_empty() || !t.bytes().all(|c| c.is_ascii_digit()) || (t.len() > 1 && t.starts_with('0')) {
                return Err(err());
            }
            t.parse().map_err(|_| err())
        };
        Ok(LangVersion {
            major: num(a)?,
            minor: num(b)?,
        })
    }

    /// Can an implementation of `self` read a document declaring `declared`?
    /// Same major and declared minor ≤ supported minor; while the major is 0,
    /// minors must match exactly.
    pub fn supports(self, declared: LangVersion) -> bool {
        self.major == declared.major
            && if self.major == 0 {
                self.minor == declared.minor
            } else {
                declared.minor <= self.minor
            }
    }

    /// Parse `text` and check it against `self`; `what` names the language in errors.
    pub fn check(self, text: &str, what: &str) -> Result<LangVersion, String> {
        let v = LangVersion::parse(text.trim())?;
        if !self.supports(v) {
            return Err(format!("this document requires {what} {v}, but this implementation supports {what} {self}"));
        }
        Ok(v)
    }
}

impl fmt::Display for LangVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        let v = |s| LangVersion::parse(s).unwrap();
        assert!(v("0.1").supports(v("0.1")));
        assert!(!v("0.2").supports(v("0.1")));
        assert!(v("1.3").supports(v("1.0")) && v("1.3").supports(v("1.3")));
        assert!(!v("1.3").supports(v("1.4")) && !v("1.3").supports(v("2.0")));
        for bad in ["1", "1.2.3", "01.1", "1.x", "", "v1.0"] {
            assert!(LangVersion::parse(bad).is_err(), "{bad}");
        }
    }
}

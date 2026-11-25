//! Semantic Versioning 2.0.0 (<https://semver.org>) for schemas (spec §6.7).

use std::cmp::Ordering;
use std::fmt;

/// One dot-separated pre-release identifier.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PreId {
    /// Digits only; compares numerically and below alphanumeric identifiers.
    Num(u64),
    /// Contains a letter or `-`; compares in ASCII order.
    Alpha(String),
}

/// A `SemVer` 2.0.0 version.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Version {
    /// `MAJOR`: incremented for breaking changes (from 1.0.0 on).
    pub major: u64,
    /// `MINOR`: incremented for compatible additions (breaking changes in 0.y.z).
    pub minor: u64,
    /// `PATCH`: incremented for everything else.
    pub patch: u64,
    /// Pre-release identifiers after `-`; empty for a release.
    pub pre: Vec<PreId>,
    /// Build metadata: ignored for precedence and compatibility.
    pub build: Vec<String>,
}

/// Parse a numeric identifier: ASCII digits, no leading zeros; `what`
/// names it in errors.
fn numeric(s: &str, what: &str) -> Result<u64, String> {
    if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
        return Err(format!("{what} must be a non-negative integer, got {s:?}"));
    }

    if s.len() > 1 && s.starts_with('0') {
        return Err(format!("{what} must not have leading zeros, got {s:?}"));
    }

    s.parse().map_err(|_| format!("{what} is too large: {s:?}"))
}

/// Whether `s` is a non-empty run of `[0-9A-Za-z-]`.
fn ident_ok(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

impl Version {
    /// The release `major.minor.patch`, without pre-release or build metadata.
    #[must_use]
    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Version {
            major,
            minor,
            patch,
            pre: Vec::new(),
            build: Vec::new(),
        }
    }

    /// Parse strictly per `SemVer` 2.0.0: `MAJOR.MINOR.PATCH[-PRE][+BUILD]`.
    ///
    /// # Errors
    ///
    /// Fails if `s` is not a valid `SemVer` 2.0.0 version (e.g. missing parts,
    /// leading zeros, empty or invalid identifiers).
    pub fn parse(s: &str) -> Result<Version, String> {
        let ctx = |m: String| format!("invalid SemVer 2.0.0 version {s:?}: {m}");
        let (rest, build) = match s.split_once('+') {
            Some((r, b)) => {
                let ids: Vec<String> = b.split('.').map(str::to_owned).collect();
                if !ids.iter().all(|i| ident_ok(i)) {
                    return Err(ctx("build metadata identifiers must be non-empty [0-9A-Za-z-]".into()));
                }

                (r, ids)
            }
            None => (s, Vec::new()),
        };

        let (core, pre) = match rest.split_once('-') {
            Some((c, p)) => {
                let mut ids = Vec::new();
                for id in p.split('.') {
                    if !ident_ok(id) {
                        return Err(ctx("pre-release identifiers must be non-empty [0-9A-Za-z-]".into()));
                    }

                    if id.bytes().all(|c| c.is_ascii_digit()) {
                        ids.push(PreId::Num(numeric(id, "numeric pre-release identifier").map_err(ctx)?));
                    } else {
                        ids.push(PreId::Alpha(id.to_owned()));
                    }
                }

                (c, ids)
            }
            None => (rest, Vec::new()),
        };

        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() != 3 {
            return Err(ctx("expected MAJOR.MINOR.PATCH".into()));
        }

        Ok(Version {
            major: numeric(parts[0], "major").map_err(ctx)?,
            minor: numeric(parts[1], "minor").map_err(ctx)?,
            patch: numeric(parts[2], "patch").map_err(ctx)?,
            pre,
            build,
        })
    }

    /// Whether the version has pre-release identifiers.
    #[must_use]
    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    /// `SemVer` precedence (build metadata ignored).
    #[must_use]
    pub fn precedence(&self, other: &Version) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(&other.pre) {
                        let o = match (a, b) {
                            (PreId::Num(x), PreId::Num(y)) => x.cmp(y),
                            (PreId::Num(_), PreId::Alpha(_)) => Ordering::Less,
                            (PreId::Alpha(_), PreId::Num(_)) => Ordering::Greater,
                            (PreId::Alpha(x), PreId::Alpha(y)) => x.cmp(y),
                        };

                        if o != Ordering::Equal {
                            return o;
                        }
                    }

                    self.pre.len().cmp(&other.pre.len())
                }
            })
    }

    /// Same version, ignoring build metadata.
    fn same_release(&self, o: &Version) -> bool {
        self.precedence(o) == Ordering::Equal
    }

    /// Can a reader at version `self` read data written against `data`?
    /// (spec §6.7: same major and reader ≥ data; 0.y.z: same minor;
    /// 0.0.z and pre-releases: exact.)
    #[must_use]
    pub fn can_read(&self, data: &Version) -> bool {
        if self.is_prerelease() || data.is_prerelease() || (self.major == 0 && self.minor == 0) || (data.major == 0 && data.minor == 0) {
            return self.same_release(data);
        }

        if self.major != data.major {
            return false;
        }

        if self.major == 0 && self.minor != data.minor {
            return false;
        }

        self.precedence(data) != Ordering::Less
    }

    /// The smallest release that permits a change of the given kind after `self`.
    ///
    /// A part at `u64::MAX` carries into the next higher part, so `1.0.MAX`
    /// needs at least `1.1.0`. Past `MAX.MAX.MAX` no release exists; the result
    /// is then `MAX.MAX.MAX`, which no new version can exceed.
    #[must_use]
    pub fn required_bump(&self, kind: ChangeKind) -> Version {
        let (ma, mi, pa) = (self.major, self.minor, self.patch);
        match (kind, ma, mi) {
            (ChangeKind::None, ..) => next_patch(ma, mi, pa),
            // In 0.0.z every release is potentially breaking.
            (_, 0, 0) => next_patch(0, 0, pa),
            (ChangeKind::Breaking, 0, m) => next_minor(0, m),
            (ChangeKind::Compatible, 0, m) => next_patch(0, m, pa),
            (ChangeKind::Breaking, ma, _) => next_major(ma),
            (ChangeKind::Compatible, ma, mi) => next_minor(ma, mi),
        }
    }
}

/// The smallest version after `major.minor.patch` in the same minor series, or the next minor.
fn next_patch(major: u64, minor: u64, patch: u64) -> Version {
    patch.checked_add(1).map_or_else(|| next_minor(major, minor), |p| Version::new(major, minor, p))
}

/// The smallest version after every `major.minor.*`, or the next major.
fn next_minor(major: u64, minor: u64) -> Version {
    minor.checked_add(1).map_or_else(|| next_major(major), |m| Version::new(major, m, 0))
}

/// The smallest version after every `major.*.*`, saturated at `MAX.MAX.MAX`.
fn next_major(major: u64) -> Version {
    major
        .checked_add(1)
        .map_or_else(|| Version::new(u64::MAX, u64::MAX, u64::MAX), |m| Version::new(m, 0, 0))
}

/// Classification of the difference between two schema versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    /// No semantic change (formatting, comments, documentation).
    None,
    /// Backward-compatible changes only.
    Compatible,
    /// At least one breaking change.
    Breaking,
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            let p: Vec<String> = self
                .pre
                .iter()
                .map(|i| match i {
                    PreId::Num(n) => n.to_string(),
                    PreId::Alpha(a) => a.clone(),
                })
                .collect();
            write!(f, "-{}", p.join("."))?;
        }

        if !self.build.is_empty() {
            write!(f, "+{}", self.build.join("."))?;
        }

        Ok(())
    }
}

/// A schema reference: `name@version[#fingerprint]` (spec §6.7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaRef {
    /// Schema name (see [`valid_schema_name`]).
    pub name: String,
    /// The referenced version.
    pub version: Version,
    /// 16 lowercase hex digits pinning the exact schema
    /// (see [`crate::Schema::fingerprint_hex`]); optional.
    pub fingerprint: Option<String>,
}

/// The two kinds of schema name (spec §6.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameKind {
    /// Its first segment is a domain name (`acme.example/billing/invoice`), owned
    /// through that domain.
    Qualified,
    /// No `.` in the first segment (`infrastructure`): meaningful within a project.
    Local,
}

/// The kind of the schema name `n`, or `None` if it is not a valid name
/// (spec §6.7, proposal 0005 §0005.2.1). There is exactly one spelling per
/// name, so names compare as plain strings.
#[must_use]
pub fn schema_name_kind(n: &str) -> Option<NameKind> {
    /// `[a-z0-9]`.
    fn alnum(c: u8) -> bool {
        c.is_ascii_lowercase() || c.is_ascii_digit()
    }

    /// A path segment: an alphanumeric, then alphanumerics, `-`, `_` or `.`.
    fn segment(s: &str) -> bool {
        s.bytes().next().is_some_and(alnum) && s.bytes().all(|c| alnum(c) || b"-_.".contains(&c))
    }

    /// A DNS label: 1 to 63 alphanumerics and inner `-`.
    fn label(l: &str) -> bool {
        let b = l.as_bytes();
        (1..=63).contains(&b.len()) && alnum(b[0]) && alnum(b[b.len() - 1]) && b.iter().all(|&c| alnum(c) || c == b'-')
    }

    if n.is_empty() || n.len() > 255 {
        return None;
    }

    let mut segs = n.split('/');
    let first = segs.next()?;
    let rest: Vec<&str> = segs.collect();
    if !rest.iter().all(|s| segment(s)) {
        return None;
    }

    if first.contains('.') {
        let labels: Vec<&str> = first.split('.').collect();
        // The last label starts with a letter, so an IP address is not a host.
        let tld_ok = labels.last().is_some_and(|l| l.as_bytes()[..].first().is_some_and(u8::is_ascii_lowercase));
        (first.len() <= 253 && labels.iter().all(|l| label(l)) && tld_ok && !rest.is_empty()).then_some(NameKind::Qualified)
    } else {
        let b = first.as_bytes();
        (b.first().is_some_and(u8::is_ascii_lowercase) && b.iter().all(|&c| alnum(c) || c == b'-' || c == b'_')).then_some(NameKind::Local)
    }
}

/// Whether `n` is a valid schema name, qualified or local (see [`schema_name_kind`]).
#[must_use]
pub fn valid_schema_name(n: &str) -> bool {
    schema_name_kind(n).is_some()
}

impl SchemaRef {
    /// Parse a reference `name@MAJOR.MINOR.PATCH[#fingerprint]`.
    ///
    /// # Errors
    ///
    /// Fails if the name, the version or the 16-hex-digit fingerprint is invalid,
    /// or the `@` separator is missing.
    pub fn parse(s: &str) -> Result<SchemaRef, String> {
        let (body, fp) = match s.split_once('#') {
            Some((b, f)) => {
                if f.len() != 16 || !f.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) {
                    return Err(format!("invalid fingerprint {f:?} (expected 16 lowercase hex digits)"));
                }

                (b, Some(f.to_owned()))
            }
            None => (s, None),
        };

        let (name, ver) = body
            .rsplit_once('@')
            .ok_or_else(|| format!("schema reference {s:?} must be `name@MAJOR.MINOR.PATCH`"))?;
        if !valid_schema_name(name) {
            return Err(format!(
                "invalid schema name {name:?} (a local name such as `infrastructure`, or a domain and a path such as `acme.example/billing/invoice`)"
            ));
        }

        Ok(SchemaRef {
            name: name.to_owned(),
            version: Version::parse(ver)?,
            fingerprint: fp,
        })
    }
}

impl fmt::Display for SchemaRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)?;
        if let Some(fp) = &self.fingerprint {
            write!(f, "#{fp}")?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn schema_names_follow_the_grammar() {
        use NameKind::{Local, Qualified};
        for (n, k) in [
            ("infrastructure", Some(Local)),
            ("acme/order", Some(Local)),
            ("acme.example/billing/invoice", Some(Qualified)),
            ("github.com/alice/pddl/domain", Some(Qualified)),
            ("xn--bcher-kva.example/catalog", Some(Qualified)),
            ("3m.com/parts", Some(Qualified)),
            ("json.schemastore.org/package.json", Some(Qualified)),
            ("Acme.example/x", None),
            ("acme.example", None),
            ("my.schema", None),
            ("acme.example/", None),
            ("acme.example//x", None),
            ("acme.example/./x", None),
            ("10.0.0.1/x", None),
            ("-acme.example/x", None),
            ("acme_corp.example/x", None),
            ("1abc", None),
            ("a/-b", None),
            ("", None),
        ] {
            assert_eq!(schema_name_kind(n), k, "{n:?}");
        }

        let long = format!("a.example/{}", "x".repeat(246));
        assert_eq!(long.len(), 256);
        assert_eq!(schema_name_kind(&long), None);
        assert!(schema_name_kind(&long[..255]).is_some());
    }

    #[test]
    fn parses_strictly() {
        for ok in [
            "0.0.0",
            "1.2.3",
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-0.3.7",
            "1.0.0-x.7.z.92",
            "1.0.0+20130313144700",
            "1.0.0-beta+exp.sha.5114f85",
            "1.0.0-x-y-z.--",
        ] {
            assert_eq!(v(ok).to_string(), ok);
        }

        for bad in [
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "v1.2.3",
            "1.2.3-",
            "1.2.3-01",
            "1.2.3-a..b",
            "1.2.3+",
            "1.2.3+a_b",
            " 1.2.3",
            "-1.2.3",
        ] {
            assert!(Version::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn precedence_matches_semver_spec() {
        // Example chain from semver.org §11.
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for w in chain.windows(2) {
            assert_eq!(v(w[0]).precedence(&v(w[1])), Ordering::Less, "{} < {}", w[0], w[1]);
        }

        assert_eq!(v("1.0.0+a").precedence(&v("1.0.0+b")), Ordering::Equal);
    }

    #[test]
    fn compatibility() {
        assert!(v("1.4.0").can_read(&v("1.2.9")));
        assert!(!v("1.2.0").can_read(&v("1.4.0")));
        assert!(!v("2.0.0").can_read(&v("1.9.0")));
        assert!(v("0.3.5").can_read(&v("0.3.1")));
        assert!(!v("0.4.0").can_read(&v("0.3.1")));
        assert!(!v("0.0.2").can_read(&v("0.0.1")));
        assert!(!v("1.1.0").can_read(&v("1.1.0-rc.1")));
        assert!(v("1.1.0-rc.1+b").can_read(&v("1.1.0-rc.1")));
    }

    #[test]
    fn bumps() {
        assert_eq!(v("1.2.3").required_bump(ChangeKind::Breaking).to_string(), "2.0.0");
        assert_eq!(v("1.2.3").required_bump(ChangeKind::Compatible).to_string(), "1.3.0");
        assert_eq!(v("1.2.3").required_bump(ChangeKind::None).to_string(), "1.2.4");
        assert_eq!(v("0.2.3").required_bump(ChangeKind::Breaking).to_string(), "0.3.0");
        assert_eq!(v("0.2.3").required_bump(ChangeKind::Compatible).to_string(), "0.2.4");
        assert_eq!(v("0.0.3").required_bump(ChangeKind::Breaking).to_string(), "0.0.4");
        // A part at u64::MAX carries into the next part instead of overflowing.
        let max = u64::MAX;
        assert_eq!(v(&format!("1.0.{max}")).required_bump(ChangeKind::None).to_string(), "1.1.0");
        assert_eq!(v(&format!("1.{max}.0")).required_bump(ChangeKind::Compatible).to_string(), "2.0.0");
        let top = format!("{max}.{max}.{max}");
        assert_eq!(v(&format!("{max}.0.0")).required_bump(ChangeKind::Breaking).to_string(), top);
    }

    #[test]
    fn refs() {
        let r = SchemaRef::parse("infrastructure@1.2.0#0123456789abcdef").unwrap();
        assert_eq!((r.name.as_str(), r.version.to_string()), ("infrastructure", "1.2.0".into()));
        assert_eq!(r.to_string(), "infrastructure@1.2.0#0123456789abcdef");
        for bad in ["infrastructure", "infrastructure/v1", "Infra@1.0.0", "x@1.0", "x@1.0.0#zz"] {
            assert!(SchemaRef::parse(bad).is_err(), "{bad}");
        }
    }
}

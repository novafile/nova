//! Paths and selectors (spec §7): a small, non-Turing-complete language for
//! addressing parts of a value.
//!
//! ```text
//! path     = [ first ( "." name | "." projection | "[" selector "]" )* ]
//! first    = name | "[" selector "]"
//! name     = ident | quoted-string
//! selector = "*" | int | ident "=" flow-value | flow-value
//!          | "?" path ("=" | "!=" | "<" | "<=" | ">" | ">=") flow-value
//!          | "?" path | "?!" path | "?" path "in" list
//! projection = "{" name ("," name)* "}"      (last segment only)
//! ```

use crate::parser::Parser;
use crate::writer::{flow_string, key_text};
use novafile_value::Value;
use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    /// `.name` — a record field.
    Field(String),
    /// `[n]` — a list/tuple index; negative counts from the end.
    Index(i64),
    /// `[field=value]` — the unique list element whose record field equals value.
    Match(String, Value),
    /// `[value]` — a map key, set element, the unique equal list element, or (string) record field.
    Key(Value),
    /// `[*]` — every element (queries only).
    All,
    /// `[?path op value]` — every element for which the comparison holds (queries only).
    Filter(Path, CmpOp, Value),
    /// `.{a,b}` — keep only these fields (queries only, last segment).
    Project(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Path(pub Vec<Segment>);

/// Comparison operator in a filter selector (spec §7.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `[?p]`: the path resolves.
    Exists,
    /// `[?!p]`: the path does not resolve.
    Absent,
    /// `[?p in [a, b]]`: the value equals (numerically, as `=`) one of the list's elements.
    In,
}

impl CmpOp {
    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            CmpOp::Eq => "=",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
            CmpOp::Exists | CmpOp::Absent => "",
            CmpOp::In => " in ",
        }
    }

    /// Does `left op right` hold? Numbers compare by numeric value across
    /// int, float, decimal and rational; strings by code point; timestamps
    /// and durations chronologically (a string operand is parsed as the
    /// other side's kind). Values that cannot be ordered only support `=`
    /// and `!=`; ordering comparisons on them are false.
    #[must_use]
    pub fn holds(self, left: &Value, right: &Value) -> bool {
        use std::cmp::Ordering;
        let ord: Option<Ordering> = compare(left, right);
        match self {
            CmpOp::Eq => ord.map_or(left == right, |o| o == Ordering::Equal),
            CmpOp::Ne => !ord.map_or(left == right, |o| o == Ordering::Equal),
            CmpOp::Lt => ord == Some(Ordering::Less),
            CmpOp::Le => matches!(ord, Some(Ordering::Less | Ordering::Equal)),
            CmpOp::Gt => ord == Some(Ordering::Greater),
            CmpOp::Ge => matches!(ord, Some(Ordering::Greater | Ordering::Equal)),
            CmpOp::Exists => true,
            CmpOp::Absent => false,
            CmpOp::In => matches!(right, Value::List(opts) if opts.iter().any(|o| CmpOp::Eq.holds(left, o))),
        }
    }
}

fn compare(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    use novafile_value::{Duration, Timestamp};
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        (Value::Timestamp(x), Value::Timestamp(y)) => Some(x.cmp(y)),
        (Value::Timestamp(x), Value::String(s)) => Timestamp::parse(s).ok().map(|y| x.cmp(&y)),
        (Value::String(s), Value::Timestamp(y)) => Timestamp::parse(s).ok().map(|x| x.cmp(y)),
        (Value::Duration(x), Value::Duration(y)) => Some(x.cmp(y)),
        (Value::Duration(x), Value::String(s)) => Duration::parse(s).ok().map(|y| x.cmp(&y)),
        (Value::String(s), Value::Duration(y)) => Duration::parse(s).ok().map(|x| x.cmp(y)),
        _ => novafile_value::numeric_cmp(a, b),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PathError {
    NotFound(Path),
    Ambiguous(Path, usize),
    Type { at: Path, expected: &'static str, found: &'static str },
    NotSingular(Path),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::NotFound(p) => write!(f, "path {p} not found"),
            PathError::Ambiguous(p, n) => write!(f, "selector {p} matches {n} elements; expected exactly one"),
            PathError::Type { at, expected, found } => write!(f, "at {at}: expected {expected}, found {found}"),
            PathError::NotSingular(p) => write!(f, "path {p} uses `[*]` or a projection, which is not allowed here"),
        }
    }
}

impl std::error::Error for PathError {}

pub(crate) fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

pub(crate) fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

impl Path {
    #[must_use]
    pub fn root() -> Path {
        Path(Vec::new())
    }

    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// True if the path addresses exactly one location (no `[*]`, no projection).
    #[must_use]
    pub fn is_singular(&self) -> bool {
        !self.0.iter().any(|s| matches!(s, Segment::All | Segment::Filter(..) | Segment::Project(_)))
    }

    #[must_use]
    pub fn join(&self, other: &Path) -> Path {
        Path(self.0.iter().chain(other.0.iter()).cloned().collect())
    }

    #[must_use]
    pub fn parent(&self) -> Option<(Path, &Segment)> {
        let (last, rest) = self.0.split_last()?;
        Some((Path(rest.to_vec()), last))
    }

    /// Parse a complete path; empty or `.` is the root.
    ///
    /// # Errors
    ///
    /// Returns a message if `s` is not a well-formed path in its entirety.
    pub fn parse(s: &str) -> Result<Path, String> {
        let s = s.trim();
        if s.is_empty() || s == "." {
            return Ok(Path::root());
        }

        let (p, n) = Path::parse_prefix(s)?;
        if n != s.len() {
            return Err(format!("unexpected {:?} in path", &s[n..]));
        }

        Ok(p)
    }

    /// Parse a path at the start of `s`, stopping at the first character that
    /// cannot continue it. Returns the path and the number of bytes consumed.
    ///
    /// # Errors
    ///
    /// Returns a message if a segment is malformed or a projection is not the
    /// last segment.
    #[expect(
        clippy::many_single_char_names,
        reason = "byte-scanner convention: s source, b bytes, i cursor, c byte, n length"
    )]
    pub fn parse_prefix(s: &str) -> Result<(Path, usize), String> {
        let b = s.as_bytes();
        let mut i = 0;
        let mut segs = Vec::new();
        let mut first = true;
        loop {
            let c = b.get(i).copied();
            match c {
                Some(b'[') => {
                    i += 1;
                    let (seg, n) = parse_selector(&s[i..])?;
                    i += n;
                    segs.push(seg);
                }
                Some(b'.') if !first => {
                    i += 1;
                    if let Some(b'{') = b.get(i) {
                        i += 1;
                        let mut fields = Vec::new();
                        loop {
                            while b.get(i) == Some(&b' ') {
                                i += 1;
                            }

                            let (name, n) = parse_name(&s[i..])?;
                            i += n;
                            fields.push(name);
                            while b.get(i) == Some(&b' ') {
                                i += 1;
                            }

                            match b.get(i) {
                                Some(b',') => i += 1,
                                Some(b'}') => {
                                    i += 1;
                                    break;
                                }
                                _ => return Err("expected `,` or `}` in projection".into()),
                            }
                        }

                        segs.push(Segment::Project(fields));
                    } else {
                        let (name, n) = parse_name(&s[i..])?;
                        i += n;
                        segs.push(Segment::Field(name));
                    }
                }
                Some(c) if first && (is_ident_start(c) || c == b'"') => {
                    let (name, n) = parse_name(&s[i..])?;
                    i += n;
                    segs.push(Segment::Field(name));
                }
                _ => break,
            }

            first = false;
        }

        if let Some(pos) = segs.iter().position(|s| matches!(s, Segment::Project(_)))
            && pos + 1 != segs.len()
        {
            return Err("a projection must be the last path segment".into());
        }

        Ok((Path(segs), i))
    }

    /// Resolve a singular path to the value it addresses.
    ///
    /// # Errors
    ///
    /// Returns a [`PathError`] if the path is not singular, a step does not exist,
    /// a selector is ambiguous, or a step meets a value of the wrong kind.
    pub fn get<'v>(&self, root: &'v Value) -> Result<&'v Value, PathError> {
        let mut cur = root;
        for (i, seg) in self.0.iter().enumerate() {
            let here = || Path(self.0[..i].to_vec());
            let upto = || Path(self.0[..=i].to_vec());
            cur = match seg {
                Segment::All | Segment::Filter(..) | Segment::Project(_) => return Err(PathError::NotSingular(self.clone())),
                _ => step(cur, seg, &here, &upto)?,
            };
        }

        Ok(cur)
    }
}

/// One navigation step (shared with novafile-patch's mutable resolution).
///
/// # Errors
///
/// Returns a [`PathError`] if `seg` does not exist in `cur`, is ambiguous,
/// does not fit the kind of `cur`, or is not singular.
pub fn step<'v>(cur: &'v Value, seg: &Segment, here: &dyn Fn() -> Path, upto: &dyn Fn() -> Path) -> Result<&'v Value, PathError> {
    let ty = |expected| PathError::Type {
        at: here(),
        expected,
        found: cur.kind(),
    };

    match seg {
        Segment::Field(name) => match cur {
            Value::Record(r) => r.get(name).ok_or_else(|| PathError::NotFound(upto())),
            Value::Map(m) => m
                .iter()
                .find(|(k, _)| k.as_str() == Some(name))
                .map(|(_, v)| v)
                .ok_or_else(|| PathError::NotFound(upto())),
            _ => Err(ty("record")),
        },
        Segment::Index(i) => match cur {
            Value::List(l) | Value::Tuple(l) => resolve_index(*i, l.len()).map(|i| &l[i]).ok_or_else(|| PathError::NotFound(upto())),
            Value::Map(m) => m
                .iter()
                .find(|(k, _)| *k == Value::Int(i128::from(*i)))
                .map(|(_, v)| v)
                .ok_or_else(|| PathError::NotFound(upto())),
            _ => Err(ty("list")),
        },
        Segment::Match(field, want) => match cur {
            Value::List(l) => {
                let idx = match_indices(l, field, want);
                match idx.len() {
                    1 => Ok(&l[idx[0]]),
                    0 => Err(PathError::NotFound(upto())),
                    n => Err(PathError::Ambiguous(upto(), n)),
                }
            }
            _ => Err(ty("list")),
        },
        Segment::Key(k) => match cur {
            Value::Map(m) => m.iter().find(|(mk, _)| mk == k).map(|(_, v)| v).ok_or_else(|| PathError::NotFound(upto())),
            Value::Set(s) => s.iter().find(|e| *e == k).ok_or_else(|| PathError::NotFound(upto())),
            Value::List(l) => {
                let idx: Vec<usize> = l.iter().enumerate().filter(|(_, e)| *e == k).map(|(i, _)| i).collect();
                match idx.len() {
                    1 => Ok(&l[idx[0]]),
                    0 => Err(PathError::NotFound(upto())),
                    n => Err(PathError::Ambiguous(upto(), n)),
                }
            }
            Value::Record(r) => k.as_str().and_then(|k| r.get(k)).ok_or_else(|| PathError::NotFound(upto())),
            _ => Err(ty("map")),
        },
        Segment::All | Segment::Filter(..) | Segment::Project(_) => Err(PathError::NotSingular(upto())),
    }
}

#[must_use]
pub fn resolve_index(i: i64, len: usize) -> Option<usize> {
    if i < 0 {
        len.checked_sub(usize::try_from(i.unsigned_abs()).ok()?)
    } else {
        usize::try_from(i).ok().filter(|&idx| idx < len)
    }
}

#[must_use]
pub fn match_indices(list: &[Value], field: &str, want: &Value) -> Vec<usize> {
    list.iter()
        .enumerate()
        .filter(|(_, v)| v.as_record().and_then(|r| r.get(field)) == Some(want))
        .map(|(i, _)| i)
        .collect()
}

fn parse_name(s: &str) -> Result<(String, usize), String> {
    let b = s.as_bytes();
    match b.first() {
        Some(b'"') => {
            let mut p = Parser::new(s, false);
            let v = p.parse_quoted_literal().map_err(|e| e.message)?;
            Ok((v, p.pos))
        }
        Some(&c) if is_ident_start(c) => {
            let n = b.iter().take_while(|&&c| is_ident_char(c)).count();
            Ok((s[..n].to_owned(), n))
        }
        _ => Err(format!("expected a field name at {:?}", s.chars().take(12).collect::<String>())),
    }
}

#[expect(
    clippy::many_single_char_names,
    reason = "byte-scanner convention: s source, b bytes, j/k cursors, n length, p parser, v value"
)]
fn parse_selector(s: &str) -> Result<(Segment, usize), String> {
    let b = s.as_bytes();
    let close = |i: usize| -> Result<usize, String> {
        let mut j = i;
        while b.get(j) == Some(&b' ') {
            j += 1;
        }

        if b.get(j) == Some(&b']') {
            Ok(j + 1)
        } else {
            Err("expected `]` to close selector".into())
        }
    };

    if b.first() == Some(&b'*') {
        return Ok((Segment::All, close(1)?));
    }

    // ?path op value | ?path | ?!path | ?path in [..]
    if b.first() == Some(&b'?') {
        let mut j = 1;
        let negated = b.get(j) == Some(&b'!');
        if negated {
            j += 1;
        }

        while b.get(j) == Some(&b' ') {
            j += 1;
        }

        let (lhs, n) = Path::parse_prefix(&s[j..])?;
        if lhs.is_root() || !lhs.is_singular() {
            return Err("a filter needs a field path, e.g. [?replicas>3]".into());
        }

        j += n;
        while b.get(j) == Some(&b' ') {
            j += 1;
        }

        let rest = &s[j..];
        if rest.starts_with(']') {
            let op = if negated { CmpOp::Absent } else { CmpOp::Exists };
            return Ok((Segment::Filter(lhs, op, Value::Null), close(j)?));
        }

        if negated {
            return Err("`[?!path]` tests absence and takes no comparison".into());
        }

        if let Some(r) = rest.strip_prefix("in").filter(|r| r.starts_with([' ', '['])) {
            let k = j + 2 + (r.len() - r.trim_start().len());
            let mut p = Parser::new(&s[k..], false);
            let v = p.parse_flow_value().map_err(|e| e.message)?;
            if !matches!(v, Value::List(_)) {
                return Err("`in` needs a list, e.g. [?tier in [web, api]]".into());
            }

            let end = close(k + p.pos)?;
            return Ok((Segment::Filter(lhs, CmpOp::In, v), end));
        }

        let (op, len) = [
            ("!=", CmpOp::Ne),
            ("<=", CmpOp::Le),
            (">=", CmpOp::Ge),
            ("==", CmpOp::Eq),
            ("=", CmpOp::Eq),
            ("<", CmpOp::Lt),
            (">", CmpOp::Gt),
        ]
        .into_iter()
        .find(|(t, _)| rest.starts_with(t))
        .map(|(t, o)| (o, t.len()))
        .ok_or("expected a comparison operator (= != < <= > >= in) in filter")?;
        j += len;
        while b.get(j) == Some(&b' ') {
            j += 1;
        }

        let mut p = Parser::new(&s[j..], false);
        let v = p.parse_flow_value().map_err(|e| e.message)?;
        let end = close(j + p.pos)?;
        return Ok((Segment::Filter(lhs, op, v), end));
    }

    // ident '=' value
    if b.first().is_some_and(|&c| is_ident_start(c)) {
        let n = b.iter().take_while(|&&c| is_ident_char(c)).count();
        let mut j = n;
        while b.get(j) == Some(&b' ') {
            j += 1;
        }

        if b.get(j) == Some(&b'=') {
            j += 1;
            while b.get(j) == Some(&b' ') {
                j += 1;
            }

            let mut p = Parser::new(&s[j..], false);
            let v = p.parse_flow_value().map_err(|e| e.message)?;
            let end = close(j + p.pos)?;
            return Ok((Segment::Match(s[..n].to_owned(), v), end));
        }
    }

    let mut p = Parser::new(s, false);
    let v = p.parse_flow_value().map_err(|e| e.message)?;
    let end = close(p.pos)?;
    Ok((
        match v {
            Value::Int(i) => i64::try_from(i).map_or(Segment::Key(Value::Int(i)), Segment::Index),
            other => Segment::Key(other),
        },
        end,
    ))
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str(".");
        }

        for (i, seg) in self.0.iter().enumerate() {
            match seg {
                Segment::Field(n) => {
                    if i > 0 {
                        f.write_str(".")?;
                    }

                    f.write_str(&key_text(n))?;
                }
                Segment::Index(n) => write!(f, "[{n}]")?,
                Segment::Match(k, v) => write!(f, "[{}={}]", key_text(k), flow_string(v))?,
                Segment::Key(v) => write!(f, "[{}]", flow_string(v))?,
                Segment::All => f.write_str("[*]")?,
                Segment::Filter(p, CmpOp::Exists, _) => write!(f, "[?{p}]")?,
                Segment::Filter(p, CmpOp::Absent, _) => write!(f, "[?!{p}]")?,
                Segment::Filter(p, op, v) => write!(f, "[?{p}{}{}]", op.symbol(), flow_string(v))?,
                Segment::Project(fs) => write!(f, ".{{{}}}", fs.iter().map(|s| key_text(s)).collect::<Vec<_>>().join(","))?,
            }
        }

        Ok(())
    }
}

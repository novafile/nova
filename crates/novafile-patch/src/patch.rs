//! The patch language (spec §7.4–7.6).
//!
//! ```text
//! @patch [schema-id] [sha256:<rev>]
//! @base sha256:<rev>            optional; document must still be at this revision
//! @scope <path>                 optional; all op paths are relative to it
//! <path>: <old> -> <new>        replace, only if the current value equals <old>
//! = <path>: <value>             set (insert or replace)
//! + <path>: <value>             add: append to list / insert into set / new field / insert at index
//! - <path>[: <old>]             remove (optionally only if it equals <old>)
//! ? <path>[: <value>]           test that the path exists (and equals <value>)
//! ```

use crate::nav::get_mut;
use novafile_text::path::{match_indices, resolve_index};
use novafile_text::{flow_string, Parser, Path, PathError, Segment};
use novafile_value::{revision, Revision, Value};
use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum OpKind {
    Replace { old: Value, new: Value },
    Set(Value),
    Add(Value),
    Remove(Option<Value>),
    Test(Option<Value>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub path: Path,
    pub kind: OpKind,
    pub line: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Patch {
    pub schema: Option<String>,
    pub base: Option<String>,
    pub scope: Path,
    pub ops: Vec<Op>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PatchError {
    Syntax {
        line: usize,
        col: usize,
        message: String,
    },
    /// The document is no longer at the patch's base revision.
    StaleBase {
        expected: String,
        actual: Revision,
    },
    Scope(PathError),
    /// An operation failed; nothing was applied.
    Op {
        index: usize,
        line: usize,
        op: String,
        message: String,
    },
    /// The patched document failed validation; nothing was applied.
    Invalid(Vec<String>),
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatchError::Syntax { line, col, message } => write!(f, "{line}:{col}: {message}"),
            PatchError::StaleBase { expected, actual } => write!(f, "conflict: patch expects base {expected} but the document is at {actual}"),
            PatchError::Scope(e) => write!(f, "scope: {e}"),
            PatchError::Op { index, line, op, message } => write!(f, "op {} (line {line}) `{op}`: {message}", index + 1),
            PatchError::Invalid(issues) => write!(f, "patched document is invalid:\n  {}", issues.join("\n  ")),
        }
    }
}

impl std::error::Error for PatchError {}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = &self.path;
        match &self.kind {
            OpKind::Replace { old, new } => write!(f, "{p}: {} -> {}", flow_string(old), flow_string(new)),
            OpKind::Set(v) => write!(f, "= {p}: {}", flow_string(v)),
            OpKind::Add(v) => write!(f, "+ {p}: {}", flow_string(v)),
            OpKind::Remove(None) => write!(f, "- {p}"),
            OpKind::Remove(Some(v)) => write!(f, "- {p}: {}", flow_string(v)),
            OpKind::Test(None) => write!(f, "? {p}"),
            OpKind::Test(Some(v)) => write!(f, "? {p}: {}", flow_string(v)),
        }
    }
}

impl fmt::Display for Patch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@patch")?;
        if let Some(s) = &self.schema {
            write!(f, " {s}")?;
        }
        writeln!(f)?;
        if let Some(b) = &self.base {
            writeln!(f, "@base {b}")?;
        }
        if !self.scope.is_root() {
            writeln!(f, "@scope {}", self.scope)?;
        }
        for op in &self.ops {
            writeln!(f, "{op}")?;
        }
        Ok(())
    }
}

pub fn parse_patch(src: &str) -> Result<Patch, PatchError> {
    let mut p = Parser::new(src, false);
    let syn = |p: &Parser, pos: usize, m: String| {
        let e = p.error_at(pos, m);
        PatchError::Syntax {
            line: e.line,
            col: e.col,
            message: e.message,
        }
    };
    let from = |e: novafile_text::ParseError| PatchError::Syntax {
        line: e.line,
        col: e.col,
        message: e.message,
    };
    let mut patch = Patch::default();
    let mut seen_header = false;
    loop {
        p.skip_blank_lines();
        if p.at_eof() {
            break;
        }
        let start = p.pos;
        let line = p.error_at(start, "").line;
        let rest_line = src[start..].lines().next().unwrap_or("");
        if let Some(d) = rest_line.strip_prefix('@') {
            if !patch.ops.is_empty() {
                return Err(syn(&p, start, "directives must precede operations".into()));
            }
            let d = d.split(" #").next().unwrap().trim();
            let (name, arg) = d.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((d, ""));
            match name {
                "patch" => {
                    seen_header = true;
                    for tok in arg.split_whitespace() {
                        if tok.starts_with("sha256:") {
                            patch.base = Some(tok.to_owned());
                        } else if patch.schema.is_none() {
                            patch.schema = Some(tok.to_owned());
                        } else {
                            return Err(syn(&p, start, format!("unexpected {tok:?} in @patch")));
                        }
                    }
                }
                "nova" => {
                    if seen_header || patch.base.is_some() || patch.schema.is_some() {
                        return Err(syn(&p, start, "@nova must be the first directive".into()));
                    }
                    novafile_text::version::NOVA_VERSION.check(arg, "NOVA").map_err(|m| syn(&p, start, m))?;
                }
                "base" => patch.base = Some(arg.to_owned()),
                "schema" => patch.schema = Some(arg.to_owned()),
                "scope" => patch.scope = Path::parse(arg).map_err(|m| syn(&p, start, m))?,
                other if other.starts_with("x-") => {}
                other => return Err(syn(&p, start, format!("unknown patch directive @{other}"))),
            }
            p.pos = start + rest_line.len();
            p.end_line().map_err(from)?;
            continue;
        }
        if !seen_header {
            return Err(syn(&p, start, "a patch must start with `@patch`".into()));
        }
        let sigil = match p_peek(&p) {
            Some(c @ (b'=' | b'+' | b'-' | b'?')) if src.as_bytes().get(p.pos + 1) == Some(&b' ') => {
                p.pos += 2;
                Some(c)
            }
            _ => None,
        };
        p.skip_inline_ws();
        let (path, n) = Path::parse_prefix(&src[p.pos..]).map_err(|m| syn(&p, p.pos, m))?;
        if !path.is_singular() {
            return Err(syn(&p, p.pos, "patch paths may not use `[*]` or projections".into()));
        }
        p.pos += n;
        p.skip_inline_ws();
        let has_value = p_peek(&p) == Some(b':');
        let value = if has_value {
            p.pos += 1;
            p.skip_inline_ws();
            Some(p.parse_flow_value().map_err(from)?)
        } else {
            None
        };
        let kind = match (sigil, value) {
            (None, Some(old)) => {
                p.skip_inline_ws();
                if !src[p.pos..].starts_with("->") {
                    return Err(syn(&p, p.pos, "expected `-> <new value>`; use `= path: value` for an unconditional set".into()));
                }
                p.pos += 2;
                p.skip_inline_ws();
                OpKind::Replace {
                    old,
                    new: p.parse_flow_value().map_err(from)?,
                }
            }
            (None, None) => return Err(syn(&p, start, "expected `path: old -> new` or an operation (= + - ?)".into())),
            (Some(b'='), Some(v)) => OpKind::Set(v),
            (Some(b'+'), Some(v)) => OpKind::Add(v),
            (Some(b'-'), v) => OpKind::Remove(v),
            (Some(b'?'), v) => OpKind::Test(v),
            (Some(c), None) => return Err(syn(&p, start, format!("`{}` requires `: <value>`", c as char))),
            _ => unreachable!(),
        };
        p.end_line().map_err(from)?;
        patch.ops.push(Op { path, kind, line });
    }
    if !seen_header {
        return Err(PatchError::Syntax {
            line: 1,
            col: 1,
            message: "a patch must start with `@patch`".into(),
        });
    }
    Ok(patch)
}

fn p_peek(p: &Parser) -> Option<u8> {
    // Parser keeps its byte view private; re-derive from the shared source.
    p.peek_byte()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    pub value: Value,
    pub revision: Revision,
    pub base: Revision,
}

/// Apply a patch atomically. `validate` runs on the result before it is
/// accepted (e.g. schema validation); any failure leaves `doc` untouched.
pub fn apply(doc: &Value, patch: &Patch, validate: &dyn Fn(&Value) -> Result<(), Vec<String>>) -> Result<Applied, PatchError> {
    let base = revision(doc);
    if let Some(expected) = &patch.base {
        if !base.matches(expected) {
            return Err(PatchError::StaleBase {
                expected: expected.clone(),
                actual: base,
            });
        }
    }
    if !patch.scope.is_root() {
        patch.scope.get(doc).map_err(PatchError::Scope)?;
    }
    let mut work = doc.clone();
    for (index, op) in patch.ops.iter().enumerate() {
        let full = patch.scope.join(&op.path);
        apply_op(&mut work, &full, &op.kind).map_err(|message| PatchError::Op {
            index,
            line: op.line,
            op: op.to_string(),
            message,
        })?;
    }
    validate(&work).map_err(PatchError::Invalid)?;
    Ok(Applied {
        revision: revision(&work),
        value: work,
        base,
    })
}

fn show(v: &Value) -> String {
    let s = flow_string(v);
    if s.len() > 60 {
        format!("{}…", &s[..s.char_indices().nth(57).map(|(i, _)| i).unwrap_or(s.len())])
    } else {
        s
    }
}

pub(crate) fn apply_op(root: &mut Value, path: &Path, kind: &OpKind) -> Result<(), String> {
    let e = |e: PathError| e.to_string();
    match kind {
        OpKind::Test(want) => {
            let cur = path.get(root).map_err(e)?;
            match want {
                Some(w) if cur != w => Err(format!("test failed: expected {}, found {}", show(w), show(cur))),
                _ => Ok(()),
            }
        }
        OpKind::Replace { old, new } => {
            let cur = get_mut(root, path).map_err(e)?;
            if cur != old {
                return Err(format!("conflict: expected {}, found {}", show(old), show(cur)));
            }
            *cur = new.clone();
            Ok(())
        }
        OpKind::Set(v) => {
            let Some((parent, last)) = path.parent() else {
                *root = v.clone();
                return Ok(());
            };
            let target = get_mut(root, &parent).map_err(e)?;
            match (target, last) {
                (Value::Record(r), Segment::Field(k)) => {
                    r.set(k.clone(), v.clone());
                }
                (Value::Record(r), Segment::Key(Value::String(k))) => {
                    r.set(k.clone(), v.clone());
                }
                (Value::Map(m), seg @ (Segment::Field(_) | Segment::Key(_) | Segment::Index(_))) => {
                    let k = seg_key(seg);
                    match m.iter_mut().find(|(mk, _)| *mk == k) {
                        Some((_, slot)) => *slot = v.clone(),
                        None => m.push((k, v.clone())),
                    }
                }
                (t, _) => *get_mut(t, &Path(vec![last.clone()])).map_err(e)? = v.clone(),
            }
            Ok(())
        }
        OpKind::Add(v) => {
            // Existing list/set at the path: append / insert.
            if let Ok(t) = get_mut(root, path) {
                match t {
                    Value::List(l) => {
                        l.push(v.clone());
                        return Ok(());
                    }
                    Value::Set(s) => {
                        if s.contains(v) {
                            return Err(format!("set already contains {}", show(v)));
                        }
                        s.push(v.clone());
                        return Ok(());
                    }
                    _ => return Err(format!("{path} already exists; use `=` to replace it")),
                }
            }
            let (parent, last) = path.parent().ok_or("cannot add at the document root")?;
            let target = get_mut(root, &parent).map_err(e)?;
            match (target, last) {
                (Value::Record(r), Segment::Field(k)) => r.insert(k.clone(), v.clone()).map_err(|e| e.to_string()),
                (Value::List(l), Segment::Index(i)) => {
                    let len = l.len();
                    let idx = if *i == len as i64 {
                        len
                    } else {
                        resolve_index(*i, len).ok_or(format!("index {i} is out of bounds for insert (len {len})"))?
                    };
                    l.insert(idx, v.clone());
                    Ok(())
                }
                (Value::Map(m), seg @ (Segment::Field(_) | Segment::Key(_) | Segment::Index(_))) => {
                    m.push((seg_key(seg), v.clone()));
                    Ok(())
                }
                (t, _) => Err(format!("cannot add into a {}", t.kind())),
            }
        }
        OpKind::Remove(want) => {
            if let Some(w) = want {
                let cur = path.get(root).map_err(e)?;
                if cur != w {
                    return Err(format!("conflict: expected {}, found {}", show(w), show(cur)));
                }
            }
            let (parent, last) = path.parent().ok_or("cannot remove the document root")?;
            let target = get_mut(root, &parent).map_err(e)?;
            let nf = || PathError::NotFound(path.clone()).to_string();
            match (target, last) {
                (Value::Record(r), Segment::Field(k)) => r.remove(k).map(|_| ()).ok_or_else(nf),
                (Value::Record(r), Segment::Key(Value::String(k))) => r.remove(k).map(|_| ()).ok_or_else(nf),
                (Value::List(l), Segment::Index(i)) => {
                    let idx = resolve_index(*i, l.len()).ok_or_else(nf)?;
                    l.remove(idx);
                    Ok(())
                }
                (Value::List(l), Segment::Match(f, w)) => {
                    let idx = match_indices(l, f, w);
                    match idx.len() {
                        1 => {
                            l.remove(idx[0]);
                            Ok(())
                        }
                        0 => Err(nf()),
                        n => Err(PathError::Ambiguous(path.clone(), n).to_string()),
                    }
                }
                (Value::List(l), Segment::Key(k)) => {
                    let idx: Vec<usize> = l.iter().enumerate().filter(|(_, e)| *e == k).map(|(i, _)| i).collect();
                    match idx.len() {
                        1 => {
                            l.remove(idx[0]);
                            Ok(())
                        }
                        0 => Err(nf()),
                        n => Err(PathError::Ambiguous(path.clone(), n).to_string()),
                    }
                }
                (Value::Set(s), Segment::Key(k)) => {
                    let idx = s.iter().position(|x| x == k).ok_or_else(nf)?;
                    s.remove(idx);
                    Ok(())
                }
                (Value::Map(m), seg @ (Segment::Field(_) | Segment::Key(_) | Segment::Index(_))) => {
                    let k = seg_key(seg);
                    let idx = m.iter().position(|(mk, _)| *mk == k).ok_or_else(nf)?;
                    m.remove(idx);
                    Ok(())
                }
                (Value::Tuple(_), _) => Err("tuples have a fixed length; elements cannot be removed".into()),
                (t, _) => Err(format!("cannot remove from a {}", t.kind())),
            }
        }
    }
}

fn seg_key(seg: &Segment) -> Value {
    match seg {
        Segment::Field(s) => Value::String(s.clone()),
        Segment::Index(i) => Value::Int(*i as i128),
        Segment::Key(k) => k.clone(),
        _ => unreachable!(),
    }
}

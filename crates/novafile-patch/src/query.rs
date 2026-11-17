//! Queries and projections (spec §7.3): `@get path`.

use novafile_text::path::step;
use novafile_text::{CmpOp, Path, PathError, Profile, Segment, document_to_text};
use novafile_value::{Record, Revision, Value, revision};

/// Evaluate a path that may contain `[*]` and a final projection.
pub fn select(root: &Value, path: &Path) -> Result<Value, PathError> {
    sel(root, &path.0, 0, path)
}

fn sel(cur: &Value, segs: &[Segment], depth: usize, full: &Path) -> Result<Value, PathError> {
    let Some((seg, rest)) = segs.split_first() else {
        return Ok(cur.clone());
    };
    let here = || Path(full.0[..depth].to_vec());
    let upto = || Path(full.0[..=depth].to_vec());
    match seg {
        Segment::Filter(..) => {
            // Consecutive filters combine with AND.
            let n = segs.iter().take_while(|s| matches!(s, Segment::Filter(..))).count();
            let (filters, rest) = segs.split_at(n);
            let elems: Vec<&Value> = match cur {
                Value::List(l) | Value::Tuple(l) | Value::Set(l) => l.iter().collect(),
                Value::Map(m) => m.iter().map(|(_, v)| v).collect(),
                other => {
                    return Err(PathError::Type {
                        at: here(),
                        expected: "list",
                        found: other.kind(),
                    });
                }
            };
            let keep = |e: &&Value| {
                filters.iter().all(|f| match f {
                    Segment::Filter(lhs, CmpOp::Absent, _) => lhs.get(e).is_err(),
                    Segment::Filter(lhs, op, want) => lhs.get(e).map(|got| op.holds(got, want)).unwrap_or(false),
                    _ => unreachable!(),
                })
            };
            Ok(Value::List(
                elems
                    .into_iter()
                    .filter(keep)
                    .map(|e| sel(e, rest, depth + n, full))
                    .collect::<Result<_, _>>()?,
            ))
        }
        Segment::All => {
            let elems: Vec<&Value> = match cur {
                Value::List(l) | Value::Tuple(l) | Value::Set(l) => l.iter().collect(),
                Value::Record(r) => r.iter().map(|(_, v)| v).collect(),
                Value::Map(m) => m.iter().map(|(_, v)| v).collect(),
                other => {
                    return Err(PathError::Type {
                        at: here(),
                        expected: "container",
                        found: other.kind(),
                    });
                }
            };
            Ok(Value::List(elems.into_iter().map(|e| sel(e, rest, depth + 1, full)).collect::<Result<_, _>>()?))
        }
        Segment::Project(fields) => project(cur, fields, &here()),
        _ => sel(step(cur, seg, &here, &upto)?, rest, depth + 1, full),
    }
}

fn project(v: &Value, fields: &[String], at: &Path) -> Result<Value, PathError> {
    match v {
        Value::Record(r) => {
            let mut out = Record::new();
            for f in fields {
                if let Some(x) = r.get(f) {
                    out.set(f.clone(), x.clone());
                }
            }
            Ok(Value::Record(out))
        }
        Value::List(l) => Ok(Value::List(l.iter().map(|e| project(e, fields, at)).collect::<Result<_, _>>()?)),
        other => Err(PathError::Type {
            at: at.clone(),
            expected: "record",
            found: other.kind(),
        }),
    }
}

/// The answer to a `@get` request.
#[derive(Clone, Debug, PartialEq)]
pub struct Response {
    /// Revision of the whole document the answer was taken from.
    pub revision: Revision,
    /// The single location the answer came from, if any — usable as `@scope` in a patch.
    pub scope: Option<Path>,
    pub value: Value,
}

impl Response {
    pub fn to_text(&self, profile: Profile) -> String {
        let rev = self.revision.to_string();
        let scope = self.scope.as_ref().map(|s| s.to_string());
        let mut d: Vec<(&str, &str)> = vec![("revision", &rev)];
        if let Some(s) = &scope
            && s != "."
        {
            d.push(("scope", s));
        }
        document_to_text(&d, &self.value, profile)
    }
}

/// Parse `@get <path>` (or a bare path) and evaluate it.
pub fn get(root: &Value, request: &str) -> Result<Response, String> {
    let req = request.trim();
    let p = req.strip_prefix("@get").or_else(|| req.strip_prefix("@select")).unwrap_or(req);
    let path = Path::parse(p).map_err(|e| format!("invalid path: {e}"))?;
    let value = select(root, &path).map_err(|e| e.to_string())?;
    let mut base = path.clone();
    if matches!(base.0.last(), Some(Segment::Project(_))) {
        base.0.pop();
    }
    let scope = base.is_singular().then_some(base);
    Ok(Response {
        revision: revision(root),
        scope,
        value,
    })
}

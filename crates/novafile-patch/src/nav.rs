//! Mutable navigation with the same semantics as `novafile_text::path::step`.

use novafile_text::path::{match_indices, resolve_index};
use novafile_text::{Path, PathError, Segment};
use novafile_value::Value;

pub fn get_mut<'v>(root: &'v mut Value, path: &Path) -> Result<&'v mut Value, PathError> {
    let mut cur = root;
    for (i, seg) in path.0.iter().enumerate() {
        let here = Path(path.0[..i].to_vec());
        let upto = Path(path.0[..=i].to_vec());
        let found = cur.kind();
        let ty = |expected| PathError::Type {
            at: here.clone(),
            expected,
            found,
        };
        cur = match (seg, cur) {
            (Segment::Field(n), Value::Record(r)) => r.get_mut(n).ok_or(PathError::NotFound(upto))?,
            (Segment::Field(n), Value::Map(m)) => m
                .iter_mut()
                .find(|(k, _)| k.as_str() == Some(n))
                .map(|(_, v)| v)
                .ok_or(PathError::NotFound(upto))?,
            (Segment::Field(_), _) => return Err(ty("record")),
            (Segment::Index(n), Value::List(l) | Value::Tuple(l)) => {
                let len = l.len();
                resolve_index(*n, len).map(move |i| &mut l[i]).ok_or(PathError::NotFound(upto))?
            }
            (Segment::Index(n), Value::Map(m)) => {
                let k = Value::Int(*n as i128);
                m.iter_mut().find(|(mk, _)| *mk == k).map(|(_, v)| v).ok_or(PathError::NotFound(upto))?
            }
            (Segment::Index(_), _) => return Err(ty("list")),
            (Segment::Match(f, want), Value::List(l)) => {
                let idx = match_indices(l, f, want);
                match idx.len() {
                    1 => &mut l[idx[0]],
                    0 => return Err(PathError::NotFound(upto)),
                    n => return Err(PathError::Ambiguous(upto, n)),
                }
            }
            (Segment::Match(..), _) => return Err(ty("list")),
            (Segment::Key(k), Value::Map(m)) => m.iter_mut().find(|(mk, _)| mk == k).map(|(_, v)| v).ok_or(PathError::NotFound(upto))?,
            (Segment::Key(Value::String(k)), Value::Record(r)) => r.get_mut(k).ok_or(PathError::NotFound(upto))?,
            (Segment::Key(k), Value::List(l)) => {
                let idx: Vec<usize> = l.iter().enumerate().filter(|(_, e)| *e == k).map(|(i, _)| i).collect();
                match idx.len() {
                    1 => &mut l[idx[0]],
                    0 => return Err(PathError::NotFound(upto)),
                    n => return Err(PathError::Ambiguous(upto, n)),
                }
            }
            (Segment::Key(_), Value::Set(_)) => return Err(ty("map (set elements cannot be modified in place)")),
            (Segment::Key(_), _) => return Err(ty("map")),
            (Segment::All | Segment::Filter(..) | Segment::Project(_), _) => return Err(PathError::NotSingular(path.clone())),
        };
    }
    Ok(cur)
}

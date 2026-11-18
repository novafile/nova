//! Column specs for NOVA Lines table mode (spec §4.13), e.g. `{ts, req{method, path}, ..}`: parsing
//! support, rows of cells in both directions, and a heuristic that picks columns for records.

use crate::writer::{flow_string, key_text};
use novafile_value::{Record, Value};
use std::fmt;

/// One entry of a column spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Column {
    /// A plain column: the cell is the field's value.
    Field(String),
    /// A nested column `name{…}`: a tuple cell is read positionally against the sub-columns.
    Nested(String, Vec<Column>),
    /// The rest column `..`: an empty cell or a record of the remaining fields.
    Rest,
}

impl Column {
    /// The field name of a plain or nested column; `None` for the rest column.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Column::Field(n) | Column::Nested(n, _) => Some(n),
            Column::Rest => None,
        }
    }
}

/// Check one brace level: names are unique, and `..` occurs at most once and only last.
///
/// # Errors
///
/// Returns a message naming the first violation.
pub fn validate_columns(cols: &[Column]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for (i, c) in cols.iter().enumerate() {
        match c {
            Column::Rest if i + 1 != cols.len() => return Err("`..` must be the last column of its braces".into()),
            Column::Rest => {}
            Column::Field(n) | Column::Nested(n, _) => {
                if !seen.insert(n.as_str()) {
                    return Err(format!("duplicate column {n:?}"));
                }
            }
        }

        if let Column::Nested(n, subs) = c {
            if subs.is_empty() {
                return Err(format!("nested column {n:?} has no sub-columns"));
            }

            validate_columns(subs)?;
        }
    }

    Ok(())
}

/// Writes `{a, b{c, d}, ..}`, quoting names that are not identifiers.
pub struct Columns<'a>(pub &'a [Column]);

impl fmt::Display for Columns<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{")?;
        for (i, c) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }

            match c {
                Column::Field(n) => f.write_str(&key_text(n))?,
                Column::Nested(n, subs) => write!(f, "{}{}", key_text(n), Columns(subs))?,
                Column::Rest => f.write_str("..")?,
            }
        }

        f.write_str("}")
    }
}

/// Tag of the `!_` marker for an absent sub-field inside a nested-column tuple.
const ABSENT: &str = "_";

/// Is `v` the `!_` marker?
fn is_absent(v: &Value) -> bool {
    matches!(v, Value::Variant(tag, None) if tag == ABSENT)
}

/// The record a row denotes: declared fields in column order, then the rest fields.
///
/// # Errors
///
/// Fails if the row has the wrong number of cells, a nested-column tuple has the wrong
/// number of elements, a rest cell is not a record, or a rest field repeats a declared
/// column of its level.
pub fn decode_row(cols: &[Column], cells: Vec<Option<Value>>) -> Result<Record, String> {
    if cells.len() != cols.len() {
        return Err(format!("row has {} cells; the columns declare {}", cells.len(), cols.len()));
    }

    decode_level(cols, cells, "")
}

/// Decode one brace level; `at` prefixes field names in error messages.
fn decode_level(cols: &[Column], cells: Vec<Option<Value>>, at: &str) -> Result<Record, String> {
    let mut out = Record::with_capacity(cols.len());
    for (col, cell) in cols.iter().zip(cells) {
        let Some(v) = cell else { continue };
        match col {
            Column::Field(name) => insert(&mut out, name, v, at)?,
            Column::Nested(name, subs) => {
                let v = match v {
                    Value::Tuple(items) if items.len() == subs.len() => {
                        let cells = items.into_iter().map(|e| (!is_absent(&e)).then_some(e)).collect();
                        Value::Record(decode_level(subs, cells, &format!("{at}{name}."))?)
                    }
                    Value::Tuple(items) => {
                        return Err(format!(
                            "{at}{name}: tuple has {} elements; the column declares {} sub-columns",
                            items.len(),
                            subs.len()
                        ));
                    }
                    other => other,
                };

                insert(&mut out, name, v, at)?;
            }
            Column::Rest => {
                let Value::Record(rest) = v else {
                    return Err(format!("{at}..: the rest cell must be empty or a record, found {}", v.kind()));
                };

                for (k, v) in rest.into_entries() {
                    if cols.iter().any(|c| c.name() == Some(k.as_str())) {
                        return Err(format!("{at}{k}: a declared column cannot also appear in the rest cell"));
                    }

                    insert(&mut out, &k, v, at)?;
                }
            }
        }
    }

    Ok(out)
}

/// Insert a field; duplicates can only come from a malformed rest record.
fn insert(r: &mut Record, name: &str, v: Value, at: &str) -> Result<(), String> {
    r.insert(name, v).map_err(|_| format!("{at}{name}: duplicate field"))
}

/// The cells that write `r` under `cols`: the inverse of [`decode_row`].
///
/// A nested-column value that does not fit its sub-columns (a record with extra fields
/// and no `..`, or any non-record) is written as a literal cell.
///
/// # Errors
///
/// Fails if `r` has a field no column covers and there is no `..`, or if a nested column
/// holds a tuple, which would be read back positionally.
pub fn encode_row(cols: &[Column], r: &Record) -> Result<Vec<Option<Value>>, String> {
    encode_level(cols, r, "")
}

/// Encode one brace level; `at` prefixes field names in error messages.
fn encode_level(cols: &[Column], r: &Record, at: &str) -> Result<Vec<Option<Value>>, String> {
    if !cols.contains(&Column::Rest)
        && let Some(extra) = r.keys().find(|k| !cols.iter().any(|c| c.name() == Some(k.as_str())))
    {
        return Err(format!("{at}{extra}: no column for this field (add it, or `..`)"));
    }

    cols.iter()
        .map(|col| match col {
            Column::Field(name) => Ok(r.get(name).cloned()),
            Column::Nested(name, subs) => match r.get(name) {
                None => Ok(None),
                Some(Value::Tuple(_)) => Err(format!("{at}{name}: a tuple cannot be written under a nested column")),
                Some(Value::Record(sub)) if fits(subs, sub) => {
                    let cells = encode_level(subs, sub, &format!("{at}{name}."))?;
                    Ok(Some(Value::Tuple(
                        cells.into_iter().map(|c| c.unwrap_or_else(|| Value::Variant(ABSENT.into(), None))).collect(),
                    )))
                }
                Some(other) => Ok(Some(other.clone())),
            },
            Column::Rest => {
                let mut rest = Record::new();
                for (k, v) in r.iter().filter(|(k, _)| !cols.iter().any(|c| c.name() == Some(k.as_str()))) {
                    rest.insert(k.clone(), v.clone()).map_err(|_| format!("{at}{k}: duplicate field"))?;
                }

                Ok((!rest.is_empty()).then_some(Value::Record(rest)))
            }
        })
        .collect()
}

/// Can `r` be written as a positional tuple under `subs`? Every field needs a column (or
/// a `..`), no nested sub-column may hold a tuple, and the `!_` marker must not be a value.
fn fits(subs: &[Column], r: &Record) -> bool {
    let has_rest = subs.contains(&Column::Rest);
    r.iter().all(|(k, v)| {
        if is_absent(v) {
            return false;
        }

        match subs.iter().find(|c| c.name() == Some(k.as_str())) {
            // A record that does not fit the deeper columns is written as a literal element.
            Some(Column::Nested(..)) => !matches!(v, Value::Tuple(_)),
            Some(_) => true,
            None => has_rest,
        }
    })
}

/// Write the cells of a row as one line: flow values separated by `, `, empty cells empty.
///
/// # Errors
///
/// Fails for a row whose only cell is empty: it would be a blank line, which readers skip.
pub fn row_line(cells: &[Option<Value>]) -> Result<String, String> {
    if cells.len() == 1 && cells[0].is_none() {
        return Err("a one-column row with an empty cell would be a blank line".into());
    }

    Ok(cells
        .iter()
        .map(|c| c.as_ref().map(flow_string).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(", "))
}

/// Pick columns for `records`, or `None` when value mode suits them better.
///
/// A key present in at least half the records becomes a column, in first-seen order. A
/// column whose present values are all records gets nested sub-columns, chosen the same
/// way. Any remaining field goes into a trailing `..`. Value mode is chosen when the items
/// are not all records, when no key qualifies, or when a record would write as a blank line.
#[must_use]
pub fn infer_columns(records: &[Value]) -> Option<Vec<Column>> {
    let recs: Vec<&Record> = records
        .iter()
        .map(|v| match v {
            Value::Record(r) => Some(r),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let cols = infer_level(&recs)?;
    recs.iter().all(|r| encode_row(&cols, r).is_ok_and(|c| row_line(&c).is_ok())).then_some(cols)
}

/// Columns for one level of records (see [`infer_columns`]).
fn infer_level(recs: &[&Record]) -> Option<Vec<Column>> {
    let mut keys: Vec<(&str, usize)> = Vec::new();
    for r in recs {
        for k in r.keys() {
            match keys.iter_mut().find(|(n, _)| n == k) {
                Some((_, c)) => *c += 1,
                None => keys.push((k, 1)),
            }
        }
    }

    let mut cols: Vec<Column> = keys
        .iter()
        .filter(|(_, c)| 2 * c >= recs.len())
        .map(|(k, _)| {
            let subs: Option<Vec<&Record>> = recs
                .iter()
                .filter_map(|r| r.get(k))
                .map(|v| match v {
                    Value::Record(sub) => Some(sub),
                    _ => None,
                })
                .collect();
            match subs.as_deref().and_then(infer_level) {
                Some(nested) => Column::Nested((*k).to_owned(), nested),
                None => Column::Field((*k).to_owned()),
            }
        })
        .collect();
    if cols.is_empty() {
        return None;
    }

    if keys.iter().any(|(_, c)| 2 * c < recs.len()) {
        cols.push(Column::Rest);
    }

    Some(cols)
}

//! Record streams of frames (proposal 0002 §4.3): a META `!header` frame, data frames
//! holding batches of records, and a META `!end` frame.
//!
//! Headers and segments are the NOVA Lines ones ([`novafile_text::lines`]): a binary stream
//! and a `.novl` file carry the same metadata, so either converts to the other.

use crate::{Frame, FrameOptions, FrameReader, encode_frame};
use novafile_text::lines::{Header, Mode, Positional, Segment, Status, WRITTEN_VERSION, check_version};
use novafile_text::{Column, validate_columns};
use novafile_value::{Record, Value, canonical_bytes};
use std::collections::HashSet;

/// Records per data frame a writer puts in one batch.
pub const BATCH: usize = 1024;

/// Does `buf` start with a META frame, so it should be read with [`read_stream`]?
#[must_use]
pub fn is_stream(buf: &[u8]) -> bool {
    crate::decode_frame(buf).is_ok_and(|(f, _)| f.meta)
}

/// The `!header{…}` value for `h`; `batched` declares list payloads.
fn header_value(h: &Header, batched: bool) -> Value {
    let mut r = Record::new();
    let mut set = |k: &str, v: Value| {
        r.set(k, v);
    };
    set("nova", Value::String(h.nova.clone().unwrap_or_else(|| WRITTEN_VERSION.into())));
    if let Some(s) = &h.schema {
        set("schema", Value::String(s.clone()));
    }

    if let Some(k) = &h.key {
        set("key", Value::String(k.clone()));
    }

    match &h.mode {
        Mode::Values => {}
        Mode::Table(cols) => set("columns", columns_value(cols)),
        Mode::Positional => set("mode", Value::String("positional".into())),
    }

    if batched {
        set("batched", Value::Bool(true));
    }

    for (name, arg) in &h.extensions {
        set(name, Value::String(arg.clone()));
    }

    Value::Variant("header".into(), Some(Box::new(Value::Record(r))))
}

/// Columns as a `!header` list: a string per plain column, `{name: [..]}` per nested
/// column, `!rest` for `..`.
fn columns_value(cols: &[Column]) -> Value {
    Value::List(
        cols.iter()
            .map(|c| match c {
                Column::Field(n) => Value::String(n.clone()),
                Column::Nested(n, subs) => {
                    let mut r = Record::new();
                    r.set(n.clone(), columns_value(subs));
                    Value::Record(r)
                }
                Column::Rest => Value::Variant("rest".into(), None),
            })
            .collect(),
    )
}

/// The inverse of [`columns_value`], validated like `@columns`.
fn columns_of(v: &Value) -> Result<Vec<Column>, String> {
    let Value::List(items) = v else {
        return Err(format!("!header columns must be a list, found {}", v.kind()));
    };

    let cols = items
        .iter()
        .map(|c| match c {
            Value::String(n) => Ok(Column::Field(n.clone())),
            Value::Record(r) if r.len() == 1 => {
                let (n, subs) = r.iter().next().ok_or("empty nested column")?;
                Ok(Column::Nested(n.clone(), columns_of(subs)?))
            }
            Value::Variant(t, None) if t == "rest" => Ok(Column::Rest),
            other => Err(format!("!header column must be a string, a one-field record or !rest, found {}", other.kind())),
        })
        .collect::<Result<Vec<_>, String>>()?;
    validate_columns(&cols)?;
    Ok(cols)
}

/// Parse a `!header` payload into a header and its `batched` flag.
fn header_of(v: Option<&Value>) -> Result<(Header, bool), String> {
    let Some(Value::Record(r)) = v else {
        return Err("!header needs a record payload".into());
    };

    let text = |k: &str| -> Result<Option<String>, String> {
        match r.get(k) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(o) => Err(format!("!header {k} must be a string, found {}", o.kind())),
        }
    };
    let nova = text("nova")?.ok_or("!header needs nova")?;
    check_version(&nova)?;
    let mut h = Header {
        nova: Some(nova),
        schema: text("schema")?,
        key: text("key")?,
        ..Header::default()
    };
    match (r.get("columns"), text("mode")?.as_deref()) {
        (Some(_), Some(_)) => return Err("!header cannot have both columns and mode".into()),
        (Some(c), None) => h.mode = Mode::Table(columns_of(c)?),
        (None, Some("positional")) => h.mode = Mode::Positional,
        (None, Some(m)) => return Err(format!("unknown !header mode {m:?}; streams know `positional`")),
        (None, None) => {}
    }

    let batched = match r.get("batched") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(o) => return Err(format!("!header batched must be a bool, found {}", o.kind())),
    };
    for (k, v) in r.iter().filter(|(k, _)| k.starts_with("x-")) {
        h.extensions
            .push((k.clone(), v.as_str().map_or_else(|| novafile_text::flow_string(v), str::to_owned)));
    }

    Ok((h, batched))
}

/// Write `records` as a frame stream: `!header`, data frames of up to [`BATCH`] records,
/// `!end{count}`. In positional mode `positional` encodes each record.
///
/// # Errors
///
/// Fails if the header is positional without `positional`, or a record does not match it.
pub fn write_stream(records: &[Value], header: &Header, positional: Option<&dyn Positional>, opts: FrameOptions) -> Result<Vec<u8>, String> {
    let meta = |value| Frame {
        schema: None,
        value,
        meta: true,
    };
    let mut out = encode_frame(&meta(header_value(header, true)), opts);
    for batch in records.chunks(BATCH) {
        let items = match (&header.mode, positional) {
            (Mode::Positional, Some(p)) => batch.iter().map(|v| p.encode_item(v)).collect::<Result<_, _>>()?,
            (Mode::Positional, None) => return Err("a positional stream needs its schema".into()),
            _ => batch.to_vec(),
        };
        out.extend(encode_frame(
            &Frame {
                schema: None,
                value: Value::List(items),
                meta: false,
            },
            opts,
        ));
    }

    let mut end = Record::new();
    end.set("count", Value::Int(records.len() as i128));
    out.extend(encode_frame(&meta(Value::Variant("end".into(), Some(Box::new(Value::Record(end))))), opts));
    Ok(out)
}

/// Everything read from a frame stream.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamRead {
    /// The records of all segments, in order.
    pub records: Vec<Value>,
    /// Problems, by byte offset of the frame they were found in; the frame was skipped.
    pub errors: Vec<(usize, String)>,
    /// One entry per segment.
    pub segments: Vec<Segment>,
}

/// State of the segment being read.
#[derive(Default)]
struct Current {
    /// Its header.
    header: Header,
    /// Data frames carry lists of records.
    batched: bool,
    /// Records read.
    count: usize,
    /// Set by `!end`: status and root.
    ended: Option<(Status, Option<String>)>,
    /// Canonical encodings of the `@key` values seen.
    keys: HashSet<Vec<u8>>,
    /// Any frame was seen in this segment.
    started: bool,
}

impl Current {
    /// The finished segment.
    fn close(self) -> Segment {
        let (status, revision) = self.ended.unwrap_or((Status::Open, None));
        Segment {
            header: self.header,
            count: self.count,
            status,
            revision,
        }
    }

    /// Apply a META frame's payload. Unknown variants are skipped.
    fn meta(&mut self, v: Value, done: &mut Vec<Segment>) -> Result<(), String> {
        let Value::Variant(tag, payload) = v else {
            return Err(format!("a META frame holds a variant, found {}", v.kind()));
        };

        match tag.as_str() {
            "header" => {
                let (header, batched) = header_of(payload.as_deref())?;
                if self.started {
                    done.push(std::mem::take(self).close());
                }

                *self = Current {
                    header,
                    batched,
                    started: true,
                    ..Current::default()
                };
            }
            "end" => {
                let Some(Value::Record(r)) = payload.as_deref() else {
                    return Err("!end needs a record payload".into());
                };

                let n = match r.get("count") {
                    Some(Value::Int(n)) => *n,
                    _ => return Err("!end needs an integer count".into()),
                };
                let root = match r.get("root") {
                    None => None,
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(o) => return Err(format!("!end root must be a string, found {}", o.kind())),
                };
                let status = if usize::try_from(n) == Ok(self.count) {
                    Status::Complete
                } else {
                    Status::Corrupt(format!("!end declares {n} records, but {} were read", self.count))
                };
                self.ended = Some((status, root));
            }
            _ => {}
        }

        Ok(())
    }

    /// Take the records of a data frame; on error none are taken.
    fn data(&mut self, v: Value, positional: Option<&dyn Positional>) -> Result<Vec<Value>, String> {
        if self.ended.is_some() {
            return Err("data frame after !end; start a new segment with !header".into());
        }

        self.started = true;
        let items = match v {
            Value::List(items) if self.batched => items,
            _ if self.batched => return Err("a batched stream's data frames hold lists".into()),
            v => vec![v],
        };
        let items = match (&self.header.mode, positional) {
            (Mode::Positional, Some(p)) => items.iter().map(|v| p.decode_item(v)).collect::<Result<Vec<_>, _>>()?,
            (Mode::Positional, None) => return Err("this stream is positional; its schema is needed".into()),
            _ => items,
        };
        if let Some(key) = &self.header.key {
            let mut fresh = HashSet::new();
            for v in &items {
                let k = match v {
                    Value::Record(r) => r.get(key),
                    _ => None,
                }
                .ok_or_else(|| format!("record has no key field {key:?}"))?;
                let bytes = canonical_bytes(k);
                if self.keys.contains(&bytes) || !fresh.insert(bytes) {
                    return Err(format!("duplicate key {key} value {}", novafile_text::flow_string(k)));
                }
            }

            self.keys.extend(fresh);
        }

        self.count += items.len();
        Ok(items)
    }
}

/// Read a frame stream. With `recover`, corrupt frames and invalid records are reported and
/// skipped (spec §5.5); otherwise reading stops at the first error. A stream without
/// `!header` reads its data frames as unbatched records.
#[must_use]
pub fn read_stream(buf: &[u8], recover: bool, positional: Option<&dyn Positional>) -> StreamRead {
    let mut frames = FrameReader::new(buf);
    if recover {
        frames = frames.recovering();
    }

    let mut out = StreamRead {
        records: Vec::new(),
        errors: Vec::new(),
        segments: Vec::new(),
    };
    let mut cur = Current::default();
    loop {
        let at = frames.offset();
        let Some(frame) = frames.next() else { break };
        let result = match frame {
            Ok(f) if f.meta => cur.meta(f.value, &mut out.segments),
            Ok(f) => cur.data(f.value, positional).map(|v| out.records.extend(v)),
            Err((_, e)) => Err(e.to_string()),
        };
        if let Err(m) = result {
            out.errors.push((at, m));
            if !recover {
                break;
            }
        }
    }

    out.segments.push(cur.close());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use novafile_text::parse_flow;

    /// `n` small records.
    fn records(n: i64) -> Vec<Value> {
        (0..n).map(|i| parse_flow(&format!("{{id: {i}, tags: [a, b]}}")).unwrap()).collect()
    }

    #[test]
    fn round_trip_with_batches_and_nested_columns() {
        let header = Header {
            key: Some("id".into()),
            mode: Mode::Table(novafile_text::parse_columns("{id, req{m, p}, ..}").unwrap()),
            ..Header::default()
        };
        let recs = records(2500);
        let bytes = write_stream(&recs, &header, None, FrameOptions::default());
        let bytes = bytes.unwrap();
        assert!(is_stream(&bytes));
        assert_eq!(crate::FrameReader::new(&bytes).count(), 1 + 3 + 1, "header, 3 batches, end");

        let r = read_stream(&bytes, false, None);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.records, recs);
        assert_eq!(r.segments.len(), 1);
        assert_eq!(r.segments[0].status, Status::Complete);
        assert_eq!(r.segments[0].header.mode, header.mode);
        assert_eq!(r.segments[0].header.key.as_deref(), Some("id"));
    }

    #[test]
    fn segments_unknown_meta_and_statuses() {
        let h = Header::default();
        let mut bytes = write_stream(&records(2), &h, None, FrameOptions::default()).unwrap();
        // An unknown META variant is skipped.
        bytes.extend(encode_frame(
            &Frame {
                schema: None,
                value: Value::Variant("dict".into(), None),
                meta: true,
            },
            FrameOptions::default(),
        ));
        let mut open = write_stream(&records(3), &h, None, FrameOptions::default()).unwrap();
        // Drop the `!end` frame: the last segment is open.
        let frames: Vec<usize> = {
            let mut rd = crate::FrameReader::new(&open);
            let mut at = Vec::new();
            while rd.next().is_some() {
                at.push(rd.offset());
            }
            at
        };
        open.truncate(frames[frames.len() - 2]);
        bytes.extend(open);

        let r = read_stream(&bytes, true, None);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.records.len(), 5);
        assert_eq!(r.segments.len(), 2);
        assert_eq!(r.segments[0].status, Status::Complete);
        assert_eq!(r.segments[1].status, Status::Open);
    }

    #[test]
    fn duplicate_key_and_bad_header() {
        let h = Header {
            key: Some("id".into()),
            ..Header::default()
        };
        let mut recs = records(2);
        recs.push(recs[0].clone());
        // Written unchecked; the reader rejects the batch holding the duplicate.
        let bytes = write_stream(&recs, &h, None, FrameOptions::default()).unwrap();
        let r = read_stream(&bytes, true, None);
        assert!(r.errors[0].1.contains("duplicate key"), "{:?}", r.errors);
        assert!(matches!(r.segments[0].status, Status::Corrupt(_)));

        let bad = encode_frame(
            &Frame {
                schema: None,
                value: Value::Variant("header".into(), Some(Box::new(parse_flow("{nova: \"9.9\"}").unwrap()))),
                meta: true,
            },
            FrameOptions::default(),
        );
        assert!(read_stream(&bad, true, None).errors[0].1.contains("requires NOVA 9.9"));
    }
}

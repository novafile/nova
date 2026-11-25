//! NOVA Lines (`.novl`, spec §4.13): one record per line, in value, table or positional mode.

use crate::columns::{Column, Columns, decode_row, encode_row, infer_columns, row_line};
use crate::parser::Parser;
use crate::version::LangVersion;
use crate::{ParseError, flow_string, key_text, parse_columns, parse_flow, parse_row};
use novafile_value::{Value, canonical_bytes};
use std::collections::HashSet;
use std::fmt;
use std::io::{self, BufRead, Write};

/// Language versions a NOVA Lines reader accepts in `@nova`. NOVA Lines is a 0.2 feature;
/// `0.1` is accepted as a leniency before 1.0.
const ACCEPTED: [LangVersion; 2] = [LangVersion { major: 0, minor: 1 }, LangVersion { major: 0, minor: 2 }];

/// Check a stream's declared `@nova` (or `!header` `nova`) version.
///
/// # Errors
///
/// Fails if `arg` is not a version, or not one a stream reader here accepts.
pub fn check_version(arg: &str) -> Result<(), String> {
    if ACCEPTED.contains(&LangVersion::parse(arg)?) {
        Ok(())
    } else {
        Err(format!("this stream requires NOVA {arg}; stream readers here accept 0.1 and 0.2"))
    }
}

/// The version a writer declares in `@nova`.
pub const WRITTEN_VERSION: &str = "0.2";

/// A schema positional segments are written and read with (`@mode positional`).
///
/// `novafile-schema` implements this for its `Schema`; the text crate cannot depend on it.
pub trait Positional {
    /// The reference a positional segment's `@schema` must declare: name, version and
    /// fingerprint, e.g. `infra@1.1.0#9ec6026aaf6a108c`.
    fn fingerprint(&self) -> String;

    /// One named element of the schema's list `@root` in its positional form.
    ///
    /// # Errors
    ///
    /// Fails if `@root` is not a list, or `v` does not match its element type.
    fn encode_item(&self, v: &Value) -> Result<Value, String>;

    /// One positional element of the schema's list `@root` as a named, validated value.
    ///
    /// # Errors
    ///
    /// Fails if `@root` is not a list, or `v` does not have the element's positional shape.
    fn decode_item(&self, v: &Value) -> Result<Value, String>;
}

/// Check that a positional segment declares exactly `schema`'s fingerprint.
///
/// Positional records only make sense with the schema they were written for, so
/// NOVA Lines and binary streams both apply this check.
///
/// # Errors
///
/// Returns a message when `declared` is missing, has no fingerprint, or names
/// another schema.
pub fn check_fingerprint(declared: Option<&str>, schema: &dyn Positional) -> Result<(), String> {
    let declared = declared.unwrap_or("");
    let expected = schema.fingerprint();
    if declared.contains('#') && declared == expected {
        Ok(())
    } else {
        Err(format!(
            "@mode positional needs @schema with exactly this schema's fingerprint, {expected}; found {declared:?}"
        ))
    }
}

/// How record lines are read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Each line is one flow value.
    #[default]
    Values,
    /// Each line is a table row under these columns.
    Table(Vec<Column>),
    /// Each line is one element of the schema's list `@root`, in the positional profile.
    Positional,
}

/// The header directives in effect for a segment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    /// `@nova`: the declared language version, as written.
    pub nova: Option<String>,
    /// `@schema`: the schema reference, as written.
    pub schema: Option<String>,
    /// `@key`: the top-level field whose values are unique within the segment.
    pub key: Option<String>,
    /// `@columns` or `@mode positional`, as last declared.
    pub mode: Mode,
    /// `@x-…` directives: name without `@`, and argument.
    pub extensions: Vec<(String, String)>,
}

/// How a segment ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// `@end` was present and its count matches.
    Complete,
    /// The input ended without `@end`: the producer may still be writing, or stopped.
    Open,
    /// `@end` was present but contradicts the records read.
    Corrupt(String),
}

/// One segment of a stream: header, record count and status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    /// The header as it stood at the last record.
    pub header: Header,
    /// Records read in this segment (invalid lines excluded).
    pub count: usize,
    /// Whether `@end` confirmed the segment.
    pub status: Status,
    /// The `tree:` revision from `@end`, if given. It is not verified.
    pub revision: Option<String>,
}

/// A line that could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineError {
    /// 1-based line number.
    pub line: usize,
    /// What is wrong with the line.
    pub message: String,
}

impl fmt::Display for LineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for LineError {}

/// What the reader yields per line that is not blank, a comment or a directive.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// A record.
    Record(Value),
    /// A line that was skipped; in strict mode the reader stops after it.
    Error(LineError),
}

/// Reads NOVA Lines from any buffered input, line by line.
///
/// Iterating yields records and errors. Once it returns `None`, [`Reader::segments`] holds
/// one [`Segment`] per segment of the input.
#[expect(clippy::struct_excessive_bools, reason = "independent per-segment and per-stream flags, not a state machine")]
pub struct Reader<'s, R> {
    /// The input.
    input: R,
    /// Stop after the first error.
    strict: bool,
    /// The schema positional segments are decoded with.
    schema: Option<&'s dyn Positional>,
    /// Lines read so far.
    line: usize,
    /// Header of the current segment.
    header: Header,
    /// Records read in the current segment.
    count: usize,
    /// `@end` of the current segment: status and revision.
    ended: Option<(Status, Option<String>)>,
    /// Canonical encodings of the `@key` values seen in the current segment.
    keys: HashSet<Vec<u8>>,
    /// Finished segments.
    segments: Vec<Segment>,
    /// No more items: end of input, a read failure, or an error in strict mode.
    done: bool,
    /// A header directive of the current segment was invalid: its records are skipped.
    rejected: bool,
    /// A record line of the current segment was seen, read or skipped. Skipped
    /// ones leave `count` at 0, and `@nova`/`@schema` must still start a new segment.
    started: bool,
}

impl<'s, R: BufRead> Reader<'s, R> {
    /// A recovering reader: invalid lines are reported and skipped. `schema` is needed for
    /// positional segments, which must name exactly that schema.
    pub fn new(input: R, schema: Option<&'s dyn Positional>) -> Self {
        Reader {
            input,
            strict: false,
            schema,
            line: 0,
            header: Header::default(),
            count: 0,
            ended: None,
            keys: HashSet::new(),
            segments: Vec::new(),
            done: false,
            rejected: false,
            started: false,
        }
    }

    /// Stop at the first invalid line.
    #[must_use]
    pub fn strict(mut self) -> Self {
        self.strict = true;
        self
    }

    /// The segments read so far, including the current one once the input has ended.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// An error for the current line.
    fn error(&self, message: impl Into<String>) -> LineError {
        LineError {
            line: self.line,
            message: message.into(),
        }
    }

    /// Close the current segment and start a fresh one.
    fn close_segment(&mut self) {
        let (status, revision) = self.ended.take().unwrap_or((Status::Open, None));
        self.segments.push(Segment {
            header: std::mem::take(&mut self.header),
            count: self.count,
            status,
            revision,
        });
        self.count = 0;
        self.keys.clear();
        self.rejected = false;
        self.started = false;
    }

    /// Apply a directive line (`name` without `@`, and its argument).
    fn directive(&mut self, name: &str, arg: &str) -> Result<(), LineError> {
        if matches!(name, "nova" | "schema") && (self.started || self.ended.is_some()) {
            self.close_segment();
        } else if self.ended.is_some() {
            return Err(self.error(format!("@{name} after @end; only @nova or @schema may start a new segment")));
        }

        match name {
            "nova" => {
                check_version(arg).map_err(|m| self.error(m))?;
                self.header.nova = Some(arg.to_owned());
            }
            "schema" => self.header.schema = Some(arg.to_owned()),
            "key" => {
                let key = if arg.starts_with('"') {
                    quoted_arg(arg).map_err(|e| self.error(e.message))?
                } else if arg.is_empty() || arg.contains(['.', '[', ']', ' ']) {
                    return Err(self.error(format!("@key needs a top-level field name, found {arg:?}")));
                } else {
                    arg.to_owned()
                };

                self.header.key = Some(key);
            }
            "columns" => {
                if self.header.mode == Mode::Positional {
                    return Err(self.error("@columns cannot be used together with @mode positional"));
                }

                let cols = parse_columns(arg).map_err(|e| self.error(e.message))?;
                self.header.mode = Mode::Table(cols);
            }
            "mode" => {
                if arg != "positional" {
                    return Err(self.error(format!("unknown @mode {arg:?}; NOVA Lines knows `positional`")));
                }

                if matches!(self.header.mode, Mode::Table(_)) {
                    return Err(self.error("@mode positional cannot be used together with @columns"));
                }

                let schema = self.schema.ok_or_else(|| self.error("@mode positional needs the schema it was written for"))?;
                check_fingerprint(self.header.schema.as_deref(), schema).map_err(|m| self.error(m))?;

                self.header.mode = Mode::Positional;
            }
            "end" => {
                let mut parts = arg.split_whitespace();
                let n: usize = parts
                    .next()
                    .and_then(|n| n.parse().ok())
                    .ok_or_else(|| self.error("@end needs a record count"))?;
                let revision = parts.next().map(str::to_owned);
                if parts.next().is_some() {
                    return Err(self.error("@end takes a count and an optional revision"));
                }

                let status = if n == self.count {
                    Status::Complete
                } else {
                    Status::Corrupt(format!("@end declares {n} records, but {} were read", self.count))
                };
                self.ended = Some((status, revision));
            }
            _ => self.header.extensions.push((name.to_owned(), arg.to_owned())),
        }

        Ok(())
    }

    /// Decode a record line under the current header.
    fn record(&mut self, line: &str) -> Result<Value, LineError> {
        self.started = true;
        if self.ended.is_some() {
            return Err(self.error("record after @end; start a new segment with @nova or @schema"));
        }

        if self.rejected {
            return Err(self.error("record under an invalid header directive; skipped until the next segment"));
        }

        let v = match &self.header.mode {
            Mode::Values => parse_flow(line).map_err(|e| self.error(e.message))?,
            Mode::Table(cols) => {
                let cells = parse_row(line).map_err(|e| self.error(e.message))?;
                Value::Record(decode_row(cols, cells).map_err(|m| self.error(m))?)
            }
            Mode::Positional => {
                let v = parse_flow(line).map_err(|e| self.error(e.message))?;
                // `schema` was checked when @mode positional was accepted.
                let schema = self.schema.ok_or_else(|| self.error("@mode positional needs a schema"))?;
                schema.decode_item(&v).map_err(|m| self.error(m))?
            }
        };

        if let Some(key) = &self.header.key {
            let k = match &v {
                Value::Record(r) => r.get(key),
                _ => None,
            }
            .ok_or_else(|| self.error(format!("record has no @key field {key:?}")))?;
            if !self.keys.insert(canonical_bytes(k)) {
                return Err(self.error(format!("duplicate @key {key} value {}", flow_string(k))));
            }
        }

        self.count += 1;
        Ok(v)
    }
}

/// The text of the quoted string literal that fills all of `arg`, with escapes resolved.
/// A trailing comment is allowed.
///
/// # Errors
///
/// Returns a [`ParseError`] if `arg` is not one well-formed string literal.
fn quoted_arg(arg: &str) -> Result<String, ParseError> {
    let mut p = Parser::new(arg, false);
    let text = p.parse_quoted_literal()?;
    p.end_line()?;
    if !p.at_eof() {
        return Err(p.error_at(p.pos, "unexpected content after the quoted argument"));
    }

    Ok(text)
}

/// The directive on `line`, if it is one: `@` at column 1, a directive name, then a space or
/// the end of the line. Returns the name (without `@`) and the trimmed argument.
fn directive_of(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('@')?;
    let end = rest.find([' ', '\t']).unwrap_or(rest.len());
    let name = &rest[..end];
    let known = matches!(name, "nova" | "schema" | "key" | "columns" | "mode" | "end") || name.starts_with("x-");
    let arg = rest[end..].trim();
    // A column spec or a quoted key may contain ` #`; their parsers handle comments themselves.
    let own_comments = name == "columns" || name == "key" && arg.starts_with('"');
    let arg = match arg.find(" #") {
        Some(i) if !own_comments => arg[..i].trim_end(),
        _ => arg,
    };
    known.then_some((name, arg))
}

impl<R: BufRead> Iterator for Reader<'_, R> {
    type Item = Item;

    fn next(&mut self) -> Option<Item> {
        while !self.done {
            let mut buf = String::new();
            match self.input.read_line(&mut buf) {
                Ok(0) => {
                    self.close_segment();
                    self.done = true;
                    return None;
                }
                Ok(_) => {}
                Err(e) => {
                    self.done = true;
                    return Some(Item::Error(self.error(format!("read failed: {e}"))));
                }
            }

            self.line += 1;
            let line = buf.trim_end_matches(['\n', '\r']);
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let result = match directive_of(line) {
                Some((name, arg)) => match self.directive(name, arg) {
                    Ok(()) => continue,
                    Err(e) => {
                        self.rejected = true;
                        Err(e)
                    }
                },
                None => self.record(line),
            };

            return Some(match result {
                Ok(v) => Item::Record(v),
                Err(e) => {
                    if self.strict {
                        self.done = true;
                        self.close_segment();
                    }

                    Item::Error(e)
                }
            });
        }

        None
    }
}

/// Everything read from a complete input: the records, the skipped lines and the segments.
#[derive(Clone, Debug, PartialEq)]
pub struct Read {
    /// The records of all segments, in order: the list the stream denotes.
    pub records: Vec<Value>,
    /// Lines that were skipped.
    pub errors: Vec<LineError>,
    /// One entry per segment.
    pub segments: Vec<Segment>,
}

/// Read a whole stream in recovering mode (see [`Reader`]).
///
/// # Errors
///
/// This function itself does not fail; read errors are reported in [`Read::errors`].
#[must_use]
pub fn read_str(src: &str, schema: Option<&dyn Positional>) -> Read {
    let mut reader = Reader::new(src.as_bytes(), schema);
    let mut records = Vec::new();
    let mut errors = Vec::new();
    for item in &mut reader {
        match item {
            Item::Record(v) => records.push(v),
            Item::Error(e) => errors.push(e),
        }
    }

    Read {
        records,
        errors,
        segments: reader.segments.clone(),
    }
}

/// How [`Writer`] writes records.
#[derive(Clone, Copy)]
pub enum WriteMode<'s> {
    /// One flow value per line.
    Values,
    /// Table rows; the columns are given to [`Writer::new`].
    Table,
    /// Positional elements of this schema's list `@root`.
    Positional(&'s dyn Positional),
}

/// Writes NOVA Lines: the header first, then one line per record, then `@end`.
pub struct Writer<'s, W: Write> {
    /// The output.
    out: W,
    /// Columns in table mode.
    columns: Option<Vec<Column>>,
    /// The positional schema.
    schema: Option<&'s dyn Positional>,
    /// Records written.
    count: usize,
}

/// A record [`Writer::write`] could not write.
#[derive(Debug)]
pub enum WriteError {
    /// The record does not fit the stream's mode.
    Record(String),
    /// The output failed.
    Io(io::Error),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::Record(m) => f.write_str(m),
            WriteError::Io(e) => write!(f, "write failed: {e}"),
        }
    }
}

impl std::error::Error for WriteError {}

impl From<io::Error> for WriteError {
    fn from(e: io::Error) -> Self {
        WriteError::Io(e)
    }
}

impl<'s, W: Write> Writer<'s, W> {
    /// Write the header: `@nova`, `@schema` (the schema's fingerprint in positional mode),
    /// `@key`, and `@columns` or `@mode positional`. `columns` is required in table mode
    /// and ignored otherwise.
    ///
    /// # Errors
    ///
    /// Fails if the output fails, or if table mode has no columns.
    pub fn new(mut out: W, mode: WriteMode<'s>, columns: Option<Vec<Column>>, schema: Option<&str>, key: Option<&str>) -> Result<Self, WriteError> {
        writeln!(out, "@nova {WRITTEN_VERSION}")?;
        let positional = match mode {
            WriteMode::Positional(s) => Some(s),
            _ => None,
        };
        match (positional, schema) {
            (Some(s), _) => writeln!(out, "@schema {}", s.fingerprint())?,
            (None, Some(s)) => writeln!(out, "@schema {s}")?,
            (None, None) => {}
        }

        if let Some(k) = key {
            writeln!(out, "@key {}", key_text(k))?;
        }

        let columns = match mode {
            WriteMode::Table => {
                let cols = columns.ok_or_else(|| WriteError::Record("table mode needs columns".into()))?;
                writeln!(out, "@columns {}", Columns(&cols))?;
                Some(cols)
            }
            WriteMode::Positional(_) => {
                writeln!(out, "@mode positional")?;
                None
            }
            WriteMode::Values => None,
        };

        Ok(Writer {
            out,
            columns,
            schema: positional,
            count: 0,
        })
    }

    /// Write one record as one line.
    ///
    /// # Errors
    ///
    /// Fails if the record does not fit the mode (a non-record in table mode, a field
    /// without a column, a value that does not match the positional schema), or if the
    /// output fails.
    pub fn write(&mut self, v: &Value) -> Result<(), WriteError> {
        let line = if let Some(cols) = &self.columns {
            let Value::Record(r) = v else {
                return Err(WriteError::Record(format!("table mode writes records, not {}", v.kind())));
            };

            row_line(&encode_row(cols, r).map_err(WriteError::Record)?).map_err(WriteError::Record)?
        } else if let Some(s) = self.schema {
            flow_string(&s.encode_item(v).map_err(WriteError::Record)?)
        } else {
            flow_string(v)
        };

        writeln!(self.out, "{line}")?;
        self.count += 1;
        Ok(())
    }

    /// Write `@end <count>` and return the output.
    ///
    /// # Errors
    ///
    /// Fails if the output fails.
    pub fn finish(mut self) -> Result<W, WriteError> {
        writeln!(self.out, "@end {}", self.count)?;
        Ok(self.out)
    }
}

/// Write `records` as a complete stream. In table mode without `columns`, columns are
/// inferred ([`infer_columns`]); value mode is used when none fit.
///
/// # Errors
///
/// Fails if a record does not fit the mode.
pub fn write_string(
    records: &[Value],
    mode: WriteMode<'_>,
    columns: Option<Vec<Column>>,
    schema: Option<&str>,
    key: Option<&str>,
) -> Result<String, WriteError> {
    let (mode, columns) = match (mode, columns) {
        (WriteMode::Table, None) => match infer_columns(records) {
            Some(cols) => (WriteMode::Table, Some(cols)),
            None => (WriteMode::Values, None),
        },
        other => other,
    };

    let mut w = Writer::new(Vec::new(), mode, columns, schema, key)?;
    for r in records {
        w.write(r)?;
    }

    let bytes = w.finish()?;
    Ok(String::from_utf8(bytes).unwrap_or_default())
}

//! YAML adapter (spec §11.2).
//!
//! # YAML → NOVA
//!
//! Input is read with `yaml-rust2`'s event parser; values are built here so
//! that tag handling and alias expansion stay under NOVA's control.
//!
//! Plain scalars are resolved with the **YAML 1.2 core schema**:
//!
//! | Plain scalar | NOVA |
//! | --- | --- |
//! | empty, `~`, `null`, `Null`, `NULL` | null |
//! | `true`, `True`, `TRUE`, `false`, `False`, `FALSE` | bool |
//! | `[-+]?[0-9]+`, `0o[0-7]+`, `0x[0-9a-fA-F]+` | int (exact, signed 128-bit; overflow is an error) |
//! | `[-+]?(\.[0-9]+ \| [0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?` | float |
//! | `[-+]?.inf` (`.Inf`, `.INF`), `.nan` (`.NaN`, `.NAN`) | float |
//! | anything else | string |
//!
//! One deliberate deviation: a decimal number whose integer part has a
//! leading zero (`0777`, `007`, `00.5`) stays a **string**. YAML 1.1 reads
//! `0777` as octal and YAML 1.2 as decimal; NOVA refuses to guess. Other
//! YAML 1.1 forms (`yes`/`no`/`on`/`off`, `0b1010`, sexagesimal `1:20`,
//! dates) are strings, as the core schema requires.
//!
//! Quoted and block scalars are strings. Mappings whose keys are all
//! strings become records; any other mapping becomes a map. Duplicate keys
//! are errors. Sequences become lists.
//!
//! Tags: the core tags `!!str`, `!!int`, `!!float`, `!!bool`, `!!null`,
//! `!!seq` and `!!map` are honoured, as is the non-specific tag `!`. The
//! only other accepted tag is `!!binary` (base64 → bytes), so that bytes
//! written by [`to_string`] read back losslessly. Every other tag is an
//! error, never guessed at.
//!
//! Aliases are expanded, subject to an **output budget** of
//! [`NODE_BUDGET`] value nodes per stream, counting every node copied by an
//! alias (spec §9.5, §12): "billion laughs" documents fail quickly with an
//! error. Merge keys (`<<`) are rejected. Nesting is limited to 128 levels.
//!
//! # NOVA → YAML
//!
//! Output is block-style YAML written by this module. Strings are plain
//! only when unambiguous; anything that a YAML 1.2 *or* YAML 1.1 reader
//! might resolve to another type (`yes`, `0777`, `1.5`, `null`, dates, …)
//! is double-quoted. Lossy mappings are reported as [`Warning`]s:
//!
//! | NOVA | YAML | Warning |
//! | --- | --- | --- |
//! | null, bool, int, string, list, record | native | — |
//! | float | native, incl. `.inf`, `-.inf`, `.nan`, `-0.0` | — |
//! | decimal, rational, timestamp, duration | canonical text as a string | yes |
//! | bytes | `!!binary` base64 scalar | yes (not a core-schema type) |
//! | tuple, set | sequence | yes |
//! | map with a non-string key | mapping with those keys (complex `?` keys for collections) | — |
//! | map with only string keys (or empty) | mapping | yes (reads back as a record) |
//! | unit variant | string (the tag) | yes |
//! | variant with payload | single-key mapping `tag: payload` | yes |

use std::collections::HashMap;
use std::fmt::Display;
use std::str::Chars;

use base64::Engine;
use novafile_value::{Record, Value};
use yaml_rust2::parser::{Event, Parser, Tag};
use yaml_rust2::scanner::{Marker, TScalarStyle};

use crate::{Reporter, Warning, MAX_DEPTH};

/// Maximum number of value nodes a YAML stream may expand to, counting
/// every node produced by alias expansion (spec §9.5).
pub const NODE_BUDGET: usize = 1_000_000;

const CORE: &str = "tag:yaml.org,2002:";

/// Parse a single-document YAML stream. An empty stream is `null`; a
/// stream with more than one document is an error (see [`from_str_multi`]).
pub fn from_str(s: &str) -> Result<Value, String> {
    let mut l = Loader::new(s);
    let first = l.document()?;
    if l.document()?.is_some() {
        return Err("YAML: input contains more than one document (use from_str_multi)".into());
    }
    Ok(first.unwrap_or(Value::Null))
}

/// Parse every document of a YAML stream. The node budget applies to the
/// stream as a whole.
pub fn from_str_multi(s: &str) -> Result<Vec<Value>, String> {
    let mut l = Loader::new(s);
    let mut out = Vec::new();
    while let Some(v) = l.document()? {
        out.push(v);
    }
    Ok(out)
}

fn err_at(m: &Marker, msg: impl Display) -> String {
    format!("YAML line {} column {}: {msg}", m.line(), m.col() + 1)
}

struct Loader<'a> {
    parser: Parser<Chars<'a>>,
    /// Completed anchored nodes with their node counts.
    anchors: HashMap<usize, (Value, usize)>,
    nodes: usize,
    depth: usize,
}

/// How a node's tag constrains it.
enum TagKind {
    None,
    NonSpecific,
    Core(String),
}

impl<'a> Loader<'a> {
    fn new(s: &'a str) -> Self {
        Loader {
            parser: Parser::new_from_str(s),
            anchors: HashMap::new(),
            nodes: 0,
            depth: 0,
        }
    }

    fn next(&mut self) -> Result<(Event, Marker), String> {
        self.parser.next_token().map_err(|e| format!("YAML: {e}"))
    }

    /// The next document, or `None` at the end of the stream.
    fn document(&mut self) -> Result<Option<Value>, String> {
        loop {
            let (ev, m) = self.next()?;
            match ev {
                Event::StreamStart | Event::Nothing => {}
                Event::StreamEnd => return Ok(None),
                Event::DocumentStart => {
                    self.anchors.clear();
                    let (ev, m) = self.next()?;
                    let (v, _) = self.node(ev, m)?;
                    let (end, m) = self.next()?;
                    if end != Event::DocumentEnd {
                        return Err(err_at(&m, "expected the end of the document"));
                    }
                    return Ok(Some(v));
                }
                other => return Err(err_at(&m, format!("unexpected event {other:?}"))),
            }
        }
    }

    fn charge(&mut self, n: usize, m: &Marker) -> Result<(), String> {
        self.nodes += n;
        if self.nodes > NODE_BUDGET {
            return Err(err_at(
                m,
                format!("document expands to more than {NODE_BUDGET} nodes (alias expansion budget, spec §9.5); refusing to continue"),
            ));
        }
        Ok(())
    }

    fn tag(tag: Option<Tag>, m: &Marker) -> Result<TagKind, String> {
        let Some(t) = tag else { return Ok(TagKind::None) };
        if t.handle.is_empty() && t.suffix == "!" {
            return Ok(TagKind::NonSpecific);
        }
        if t.handle == CORE && matches!(t.suffix.as_str(), "str" | "int" | "float" | "bool" | "null" | "seq" | "map" | "binary") {
            return Ok(TagKind::Core(t.suffix));
        }
        let shown = if t.handle == CORE {
            format!("!!{}", t.suffix)
        } else {
            format!("{}{}", t.handle, t.suffix)
        };
        Err(err_at(
            m,
            format!("unsupported tag {shown}: only the YAML 1.2 core tags (!!str, !!int, !!float, !!bool, !!null, !!seq, !!map) and !!binary are accepted"),
        ))
    }

    fn enter(&mut self, m: &Marker) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(err_at(m, format!("nesting deeper than {MAX_DEPTH} levels")));
        }
        Ok(())
    }

    fn remember(&mut self, anchor: usize, v: &Value, size: usize) {
        if anchor != 0 {
            self.anchors.insert(anchor, (v.clone(), size));
        }
    }

    /// Build the node that starts with `ev`; returns it with its node count.
    fn node(&mut self, ev: Event, m: Marker) -> Result<(Value, usize), String> {
        match ev {
            Event::Alias(id) => {
                let Some(&(_, n)) = self.anchors.get(&id) else {
                    return Err(err_at(&m, "alias refers to a node that is not complete (recursive alias)"));
                };
                // Charge the budget before copying, so a bomb fails before
                // it grows.
                self.charge(n, &m)?;
                Ok((self.anchors[&id].0.clone(), n))
            }
            Event::Scalar(text, style, anchor, tag) => {
                self.charge(1, &m)?;
                let v = scalar(text, style, Self::tag(tag, &m)?).map_err(|e| err_at(&m, e))?;
                self.remember(anchor, &v, 1);
                Ok((v, 1))
            }
            Event::SequenceStart(anchor, tag) => {
                match Self::tag(tag, &m)? {
                    TagKind::None | TagKind::NonSpecific => {}
                    TagKind::Core(s) if s == "seq" => {}
                    TagKind::Core(s) => return Err(err_at(&m, format!("tag !!{s} cannot be applied to a sequence"))),
                }
                self.enter(&m)?;
                self.charge(1, &m)?;
                let (mut items, mut size) = (Vec::new(), 1);
                loop {
                    let (ev, m) = self.next()?;
                    if ev == Event::SequenceEnd {
                        break;
                    }
                    let (v, n) = self.node(ev, m)?;
                    size += n;
                    items.push(v);
                }
                self.depth -= 1;
                let v = Value::List(items);
                self.remember(anchor, &v, size);
                Ok((v, size))
            }
            Event::MappingStart(anchor, tag) => {
                match Self::tag(tag, &m)? {
                    TagKind::None | TagKind::NonSpecific => {}
                    TagKind::Core(s) if s == "map" => {}
                    TagKind::Core(s) => return Err(err_at(&m, format!("tag !!{s} cannot be applied to a mapping"))),
                }
                self.enter(&m)?;
                self.charge(1, &m)?;
                let (mut entries, mut size) = (Vec::new(), 1);
                loop {
                    let (ev, km) = self.next()?;
                    if ev == Event::MappingEnd {
                        break;
                    }
                    if matches!(&ev, Event::Scalar(t, TScalarStyle::Plain, _, None) if t == "<<") {
                        return Err(err_at(
                            &km,
                            "merge keys (`<<`) are not supported: NOVA has no implicit merging; use template spreads (spec §9) or patches (spec §7.4) instead",
                        ));
                    }
                    let (k, kn) = self.node(ev, km)?;
                    let (ev, vm) = self.next()?;
                    let (v, vn) = self.node(ev, vm)?;
                    size += kn + vn;
                    entries.push((k, v, km));
                }
                self.depth -= 1;
                let v = if entries.iter().all(|(k, _, _)| matches!(k, Value::String(_))) {
                    let mut r = Record::new();
                    for (k, v, km) in entries {
                        let Value::String(k) = k else { unreachable!() };
                        if r.insert(k.clone(), v).is_err() {
                            return Err(err_at(&km, format!("duplicate key {k:?}")));
                        }
                    }
                    Value::Record(r)
                } else {
                    Value::map(entries.into_iter().map(|(k, v, _)| (k, v)).collect()).map_err(|e| err_at(&m, e))?
                };
                self.remember(anchor, &v, size);
                Ok((v, size))
            }
            other => Err(err_at(&m, format!("unexpected event {other:?}"))),
        }
    }
}

fn scalar(text: String, style: TScalarStyle, tag: TagKind) -> Result<Value, String> {
    let core = match tag {
        TagKind::NonSpecific => return Ok(Value::String(text)),
        TagKind::None if style == TScalarStyle::Plain => return resolve_plain(&text),
        TagKind::None => return Ok(Value::String(text)),
        TagKind::Core(c) => c,
    };
    let bad = || format!("{text:?} is not a valid !!{core}");
    match core.as_str() {
        "str" => Ok(Value::String(text)),
        "null" if is_null(&text) => Ok(Value::Null),
        "bool" => parse_bool(&text).map(Value::Bool).ok_or_else(bad),
        "int" => parse_int(&text)?.map(Value::Int).ok_or_else(bad),
        "float" => parse_float(&text, true)?.map(Value::Float).ok_or_else(bad),
        "binary" => {
            let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            base64::engine::general_purpose::STANDARD
                .decode(clean)
                .map(Value::Bytes)
                .map_err(|e| format!("invalid !!binary: {e}"))
        }
        "seq" | "map" => Err(format!("tag !!{core} cannot be applied to a scalar")),
        _ => Err(bad()),
    }
}

fn is_null(s: &str) -> bool {
    matches!(s, "" | "~" | "null" | "Null" | "NULL")
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" | "True" | "TRUE" => Some(true),
        "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}

/// Core-schema integer, or `None` if `s` is not one. Out-of-range integers
/// are errors (spec §2.1).
fn parse_int(s: &str) -> Result<Option<i128>, String> {
    let range = || format!("integer {s} exceeds the supported range");
    let radix = |digits: &str, r: u32| -> Result<Option<i128>, String> {
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(r)) {
            return Ok(None);
        }
        i128::from_str_radix(digits, r).map(Some).map_err(|_| range())
    };
    if let Some(d) = s.strip_prefix("0o") {
        return radix(d, 8);
    }
    if let Some(d) = s.strip_prefix("0x") {
        return radix(d, 16);
    }
    let digits = s.strip_prefix(['-', '+']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) || (digits.len() > 1 && digits.starts_with('0')) {
        return Ok(None);
    }
    s.parse::<i128>().map(Some).map_err(|_| range())
}

/// Core-schema float, or `None`. With `allow_int`, integer spellings are
/// accepted too (used for `!!float 1`); otherwise the caller has already
/// ruled them out.
fn parse_float(s: &str, allow_int: bool) -> Result<Option<f64>, String> {
    let unsigned = s.strip_prefix(['-', '+']).unwrap_or(s);
    if matches!(unsigned, ".inf" | ".Inf" | ".INF") {
        return Ok(Some(if s.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY }));
    }
    if matches!(s, ".nan" | ".NaN" | ".NAN") {
        return Ok(Some(f64::NAN));
    }
    let b = unsigned.as_bytes();
    let digits = |j: &mut usize| {
        let st = *j;
        while b.get(*j).is_some_and(u8::is_ascii_digit) {
            *j += 1;
        }
        *j - st
    };
    let mut j = 0;
    let int_digits = digits(&mut j);
    if int_digits > 1 && b[0] == b'0' {
        return Ok(None); // leading zero: ambiguous between YAML 1.1 and 1.2
    }
    let mut fractional = false;
    if b.get(j) == Some(&b'.') {
        j += 1;
        fractional = true;
        if digits(&mut j) == 0 && int_digits == 0 {
            return Ok(None);
        }
    } else if int_digits == 0 {
        return Ok(None);
    }
    if matches!(b.get(j), Some(b'e' | b'E')) {
        j += 1;
        fractional = true;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if digits(&mut j) == 0 {
            return Ok(None);
        }
    }
    if j != b.len() || !(fractional || allow_int) {
        return Ok(None);
    }
    let f: f64 = s.parse().map_err(|_| format!("invalid float {s}"))?;
    if f.is_infinite() {
        return Err(format!("float {s} overflows binary64"));
    }
    Ok(Some(f))
}

/// Resolve a plain scalar with the YAML 1.2 core schema.
fn resolve_plain(s: &str) -> Result<Value, String> {
    if is_null(s) {
        return Ok(Value::Null);
    }
    if let Some(b) = parse_bool(s) {
        return Ok(Value::Bool(b));
    }
    if let Some(i) = parse_int(s)? {
        return Ok(Value::Int(i));
    }
    if let Some(f) = parse_float(s, false)? {
        return Ok(Value::Float(f));
    }
    Ok(Value::String(s.to_owned()))
}

/// YAML 1.1 booleans that 1.1 readers would not treat as strings.
fn is_yaml11_bool(s: &str) -> bool {
    matches!(
        s,
        "y" | "Y" | "yes" | "Yes" | "YES" | "n" | "N" | "no" | "No" | "NO" | "on" | "On" | "ON" | "off" | "Off" | "OFF"
    )
}

/// Whether `s` can be written as a plain scalar that every YAML 1.1 and
/// 1.2 core-schema reader reads back as the same string. Deliberately
/// conservative.
fn plain_ok(s: &str) -> bool {
    let Some(first) = s.chars().next() else { return false };
    (first.is_alphanumeric() || first == '_' || first == '/')
        && !first.is_ascii_digit()
        && !s.ends_with(' ')
        && s.chars().all(|c| c.is_alphanumeric() || " _-./()+@$;".contains(c))
        && !is_yaml11_bool(s)
        && matches!(resolve_plain(s), Ok(Value::String(_)))
}

fn double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{feff}') => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn string_scalar(s: &str) -> String {
    if plain_ok(s) {
        s.to_owned()
    } else {
        double_quoted(s)
    }
}

/// Longest implicit key YAML permits is 1024 characters; use explicit
/// `? key` entries beyond a safe margin.
const MAX_IMPLICIT_KEY: usize = 1000;

/// Serialize to block-style YAML, reporting every lossy mapping.
pub fn to_string(v: &Value) -> (String, Vec<Warning>) {
    let mut w = Writer {
        out: String::new(),
        rep: Reporter::default(),
    };
    w.node(v, 0, Pos::Top);
    (w.out, w.rep.warnings)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pos {
    /// At the start of the document.
    Top,
    /// After `- `, `? ` or an explicit `: `: a block may start on this line.
    Inline,
    /// After `key:`: scalars need a space, blocks a new line.
    AfterKey,
}

struct Writer {
    out: String,
    rep: Reporter,
}

impl Writer {
    fn pad(&mut self, n: usize) {
        self.out.extend(std::iter::repeat_n(' ', n));
    }

    /// Start a block collection at `indent`.
    fn open_block(&mut self, indent: usize, pos: Pos) {
        if pos == Pos::AfterKey {
            self.out.push('\n');
            self.pad(indent);
        }
    }

    fn inline(&mut self, text: &str, pos: Pos) {
        if pos == Pos::AfterKey {
            self.out.push(' ');
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    /// Write `key:` for a string key, using an explicit key if too long.
    fn key(&mut self, k: &str, indent: usize) {
        let text = string_scalar(k);
        if text.chars().count() > MAX_IMPLICIT_KEY {
            self.out.push_str("? ");
            self.out.push_str(&text);
            self.out.push('\n');
            self.pad(indent);
        } else {
            self.out.push_str(&text);
        }
        self.out.push(':');
    }

    fn node(&mut self, v: &Value, indent: usize, pos: Pos) {
        match v {
            Value::List(l) | Value::Tuple(l) | Value::Set(l) => {
                match v {
                    Value::Tuple(_) => self.rep.warn("tuple written as a sequence"),
                    Value::Set(_) => self.rep.warn("set written as a sequence"),
                    _ => {}
                }
                if l.is_empty() {
                    return self.inline("[]", pos);
                }
                self.open_block(indent, pos);
                for (i, e) in l.iter().enumerate() {
                    if i > 0 {
                        self.pad(indent);
                    }
                    self.out.push_str("- ");
                    self.indexed(i, |w| w.node(e, indent + 2, Pos::Inline));
                }
            }
            Value::Record(r) => {
                if r.is_empty() {
                    return self.inline("{}", pos);
                }
                self.open_block(indent, pos);
                for (i, (k, e)) in r.iter().enumerate() {
                    if i > 0 {
                        self.pad(indent);
                    }
                    self.key(k, indent);
                    self.field_node(k, e, indent);
                }
            }
            Value::Map(m) => {
                if m.iter().all(|(k, _)| matches!(k, Value::String(_))) {
                    self.rep.warn("map with only string keys written as a mapping (reads back as a record)");
                }
                if m.is_empty() {
                    return self.inline("{}", pos);
                }
                self.open_block(indent, pos);
                for (i, (k, e)) in m.iter().enumerate() {
                    if i > 0 {
                        self.pad(indent);
                    }
                    self.indexed(i, |w| w.entry(k, e, indent));
                }
            }
            Value::Variant(t, Some(p)) => {
                self.rep.warn("variant written as a single-key mapping");
                self.open_block(indent, pos);
                self.key(t, indent);
                self.field_node(t, p, indent);
            }
            other => {
                let text = self.scalar(other);
                self.inline(&text, pos);
            }
        }
    }

    /// The value of field `k`, after its `key:`.
    fn field_node(&mut self, k: &str, e: &Value, indent: usize) {
        let n = self.rep.path.len();
        self.rep.path.push('.');
        self.rep.path.push_str(k);
        self.node(e, indent + 2, Pos::AfterKey);
        self.rep.path.truncate(n);
    }

    fn indexed(&mut self, i: usize, f: impl FnOnce(&mut Self)) {
        let n = self.rep.path.len();
        self.rep.path.push_str(&format!("[{i}]"));
        f(self);
        self.rep.path.truncate(n);
    }

    /// One map entry: `key: value`, or `? key` / `: value` for keys that
    /// are block collections or too long for an implicit key.
    fn entry(&mut self, k: &Value, e: &Value, indent: usize) {
        let block_key = match k {
            Value::List(l) | Value::Tuple(l) | Value::Set(l) => !l.is_empty(),
            Value::Record(r) => !r.is_empty(),
            Value::Map(m) => !m.is_empty(),
            Value::Variant(_, Some(_)) => true,
            _ => false,
        };
        if !block_key {
            let text = self.scalar(k);
            if text.chars().count() <= MAX_IMPLICIT_KEY {
                self.out.push_str(&text);
                self.out.push(':');
                return self.node(e, indent + 2, Pos::AfterKey);
            }
            self.out.push_str("? ");
            self.out.push_str(&text);
            self.out.push('\n');
        } else {
            self.out.push_str("? ");
            self.node(k, indent + 2, Pos::Inline);
        }
        self.pad(indent);
        self.out.push_str(": ");
        self.node(e, indent + 2, Pos::Inline);
    }

    /// Text of a value written on one line.
    fn scalar(&mut self, v: &Value) -> String {
        match v {
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) if f.is_nan() => ".nan".into(),
            Value::Float(f) if f.is_infinite() => if *f > 0.0 { ".inf" } else { "-.inf" }.into(),
            Value::Float(f) => format!("{f:?}"),
            Value::String(s) => string_scalar(s),
            Value::Decimal(d) => {
                self.rep.warn("decimal written as a string");
                string_scalar(&d.to_string())
            }
            Value::Rational(r) => {
                self.rep.warn("rational written as a \"n/d\" string");
                string_scalar(&r.to_string())
            }
            Value::Timestamp(t) => {
                self.rep.warn("timestamp written as an RFC 3339 string");
                string_scalar(&t.to_string())
            }
            Value::Duration(d) => {
                self.rep.warn("duration written as a string");
                string_scalar(&d.to_string())
            }
            Value::Bytes(b) => {
                self.rep.warn("bytes written as a !!binary base64 scalar (not a YAML 1.2 core-schema type)");
                format!("!!binary {}", double_quoted(&base64::engine::general_purpose::STANDARD.encode(b)))
            }
            Value::Variant(t, None) => {
                self.rep.warn("unit variant written as a string");
                string_scalar(t)
            }
            // Empty collections (non-empty ones are handled as blocks).
            Value::List(_) | Value::Tuple(_) | Value::Set(_) | Value::Record(_) | Value::Map(_) | Value::Variant(_, Some(_)) => {
                let mut w = Writer {
                    out: String::new(),
                    rep: std::mem::take(&mut self.rep),
                };
                w.node(v, 0, Pos::Top);
                self.rep = w.rep;
                w.out.trim_end().to_owned()
            }
        }
    }
}

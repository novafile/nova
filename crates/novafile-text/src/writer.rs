//! Text writer with three profiles (spec §4.10).

use crate::parser::{RESERVED_START, classify};
use crate::path::{is_ident_char, is_ident_start};
use base64::Engine;
use novafile_value::{Value, canonical_bytes};
use std::fmt::Write as _;

/// Output profile. All profiles produce text that parses back to an equal value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// Block layout for people: YAML-like, keys in stored order.
    Human,
    /// LLM-oriented: uniform record lists become tables, flow lists without spaces.
    Compact,
    /// Deterministic: Human layout with keys, sets and maps in canonical order.
    Canonical,
}

const FLOW_WIDTH: usize = 80;

pub(crate) fn is_ident(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && is_ident_start(b[0]) && b.iter().all(|&c| is_ident_char(c))
}

/// Key as written: bare identifier or quoted string.
#[must_use]
pub fn key_text(k: &str) -> String {
    if is_ident(k) { k.to_owned() } else { quote(k) }
}

#[must_use]
pub fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }

    o.push('"');
    o
}

fn common_unsafe(s: &str) -> bool {
    s.is_empty()
        || RESERVED_START.contains(&s.as_bytes()[0])
        || s.starts_with("...")
        || s.chars().any(char::is_control)
        || !matches!(classify(s), Ok(Value::String(_)))
}

/// Can `s` be written as a bare flow word?
pub(crate) fn word_safe(s: &str) -> bool {
    !common_unsafe(s) && !s.ends_with(':') && !s.bytes().any(|c| c.is_ascii_whitespace() || b",[]{}()\"".contains(&c))
}

/// Can `s` be written as a plain block scalar?
pub(crate) fn plain_safe(s: &str) -> bool {
    !common_unsafe(s) && s.trim() == s && !s.ends_with(':') && !s.contains(": ") && !s.contains(" #") && s != "-" && !s.starts_with("- ")
}

fn float_text(f: f64) -> String {
    if f.is_nan() {
        "@float(\"nan\")".into()
    } else if f.is_infinite() {
        if f > 0.0 { "@float(\"inf\")".into() } else { "@float(\"-inf\")".into() }
    } else {
        format!("{f:?}")
    }
}

fn scalar_text(v: &Value) -> Option<String> {
    Some(match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => float_text(*f),
        Value::Decimal(d) => format!("@decimal(\"{d}\")"),
        Value::Rational(r) => r.to_string(),
        Value::Timestamp(t) => format!("@timestamp(\"{t}\")"),
        Value::Duration(d) => format!("@duration(\"{d}\")"),
        Value::Bytes(b) => format!("@bytes(\"{}\")", base64::engine::general_purpose::STANDARD.encode(b)),
        _ => return None,
    })
}

/// Single-line flow rendering (spec §4.7).
#[must_use]
pub fn flow_string(v: &Value) -> String {
    let mut s = String::new();
    write_flow(&mut s, v, ", ");
    s
}

fn write_flow(o: &mut String, v: &Value, sep: &str) {
    if let Some(t) = scalar_text(v) {
        o.push_str(&t);
        return;
    }

    let seq = |o: &mut String, items: &[Value]| {
        for (i, it) in items.iter().enumerate() {
            if i > 0 {
                o.push_str(sep);
            }

            write_flow(o, it, sep);
        }
    };

    match v {
        Value::String(s) => o.push_str(&if word_safe(s) { s.clone() } else { quote(s) }),
        Value::List(l) => {
            o.push('[');
            seq(o, l);
            o.push(']');
        }
        Value::Set(l) => {
            o.push_str("@set[");
            seq(o, l);
            o.push(']');
        }
        Value::Tuple(l) => {
            o.push('(');
            seq(o, l);
            if l.len() == 1 {
                o.push(',');
            }

            o.push(')');
        }
        Value::Record(r) => {
            o.push('{');
            for (i, (k, v)) in r.iter().enumerate() {
                if i > 0 {
                    o.push_str(sep);
                }

                o.push_str(&key_text(k));
                o.push_str(": ");
                write_flow(o, v, sep);
            }

            o.push('}');
        }
        Value::Map(m) => {
            o.push_str("@map{");
            for (i, (k, v)) in m.iter().enumerate() {
                if i > 0 {
                    o.push_str(sep);
                }

                write_flow(o, k, sep);
                o.push_str(" => ");
                write_flow(o, v, sep);
            }

            o.push('}');
        }
        Value::Variant(t, None) => {
            o.push('!');
            o.push_str(t);
        }
        Value::Variant(t, Some(p)) => {
            o.push('!');
            o.push_str(t);
            o.push('(');
            write_flow(o, p, sep);
            o.push(')');
        }
        _ => unreachable!(),
    }
}

/// Put sets and maps (recursively) in canonical order and sort record keys.
pub fn canonicalize(v: &mut Value) {
    fn walk(v: &mut Value) {
        match v {
            Value::List(l) | Value::Tuple(l) => {
                for x in l.iter_mut() {
                    walk(x);
                }
            }
            Value::Set(l) => {
                for x in l.iter_mut() {
                    walk(x);
                }

                l.sort_by_cached_key(canonical_bytes);
            }
            Value::Map(m) => {
                for (k, v) in m.iter_mut() {
                    walk(k);
                    walk(v);
                }

                m.sort_by_cached_key(|(k, _)| canonical_bytes(k));
            }
            Value::Record(r) => r.iter_mut().for_each(|(_, v)| walk(v)),
            Value::Variant(_, Some(p)) => walk(p),
            _ => {}
        }
    }

    v.sort_keys();
    walk(v);
}

pub(crate) struct W<'t> {
    pub(crate) out: String,
    pub(crate) compact: bool,
    /// Comments, blank lines and spellings to carry over (document-preserving formatting).
    pub(crate) trivia: Option<&'t crate::format::Trivia>,
    /// Path of the value being written (for trivia lookups).
    pub(crate) path: Vec<crate::path::Segment>,
}

impl<'t> W<'t> {
    pub(crate) fn new(compact: bool, trivia: Option<&'t crate::format::Trivia>) -> Self {
        W {
            out: String::new(),
            compact,
            trivia,
            path: Vec::new(),
        }
    }

    fn here(&self) -> String {
        crate::path::Path(self.path.clone()).to_string()
    }

    /// Blank line and leading comments for the current path.
    fn emit_leading(&mut self, indent: usize) {
        let Some(t) = self.trivia else { return };
        let here = self.here();
        if t.blank_before.contains(&here) && !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }

        if let Some(cs) = t.leading.get(&here) {
            for c in cs {
                self.pad(indent);
                self.out.push_str(c);
                self.out.push('\n');
            }
        }
    }

    /// Comments attached anywhere below the current path, for values written inline.
    fn emit_hoisted(&mut self, indent: usize) {
        let Some(t) = self.trivia else { return };
        let here = self.here();
        let below = |p: &String| p.len() > here.len() && p.starts_with(&here) && (here == "." || matches!(p.as_bytes()[here.len()], b'.' | b'['));
        let mut found: Vec<(&usize, &String)> = Vec::new();
        for (p, cs) in &t.leading {
            if below(p) {
                found.extend(cs.iter().map(|c| (t.order.get(p).unwrap_or(&0), c)));
            }
        }

        for (p, c) in &t.trailing {
            if below(p) {
                found.push((t.order.get(p).unwrap_or(&0), c));
            }
        }

        found.sort();
        for (_, c) in found {
            self.pad(indent);
            self.out.push_str(c);
            self.out.push('\n');
        }
    }

    /// Append the current path's trailing comment to the first line written since `mark`.
    fn emit_trailing(&mut self, mark: usize) {
        let Some(t) = self.trivia else { return };
        if let Some(c) = t.trailing.get(&self.here()) {
            let at = self.out[mark..].find('\n').map_or(self.out.len(), |i| mark + i);
            self.out.insert_str(at, &format!("  {c}"));
        }
    }

    /// The author's spelling of a scalar, if it still denotes the current value.
    fn spelling(&self, v: &Value) -> Option<String> {
        let t = self.trivia?;
        let (text, orig) = t.spelling.get(&self.here())?;
        (t.keep_spelling && orig == v).then(|| text.clone())
    }

    fn sep(&self) -> &'static str {
        if self.compact { "," } else { ", " }
    }

    fn pad(&mut self, n: usize) {
        self.out.extend(std::iter::repeat_n(' ', n));
    }

    fn flow(&self, v: &Value) -> String {
        let mut s = String::new();
        write_flow(&mut s, v, self.sep());
        s
    }

    /// Is this a non-empty container that should be laid out as a block?
    fn wants_block(&self, v: &Value, used: usize) -> bool {
        match v {
            Value::Record(r) => !r.is_empty(),
            Value::List(l) => !l.is_empty() && (l.iter().any(Value::is_container) || used + self.flow(v).len() > FLOW_WIDTH || self.table_fields(l).is_some()),
            Value::String(s) => block_string_ok(s),
            Value::Variant(_, Some(p)) => matches!(&**p, Value::Record(r) if !r.is_empty()) || matches!(&**p, Value::List(l) if !l.is_empty()),
            _ => false,
        }
    }

    fn table_fields(&self, l: &[Value]) -> Option<Vec<String>> {
        let was_table = self.trivia.is_some_and(|t| t.tables.contains(&self.here()));
        if !(was_table || (self.compact && l.len() >= 2)) || l.is_empty() {
            return None;
        }

        let mut fields: Vec<String> = Vec::new();
        let mut present = 0usize;
        for v in l {
            let r = v.as_record()?;
            if r.is_empty() {
                return None;
            }

            for (k, v) in r.iter() {
                if v.is_container() {
                    return None;
                }

                if !fields.contains(k) {
                    fields.push(k.clone());
                }

                present += 1;
            }
        }

        // Require a mostly-dense table; sparse data reads better as records.
        (fields.len() <= 64 && present * 2 >= fields.len() * l.len()).then_some(fields)
    }

    /// The text the writer would put after `key: ` / `- ` if `v` stays on one line.
    pub(crate) fn inline_text(&self, v: &Value, used: usize) -> Option<String> {
        (!self.wants_block(v, used) && !matches!(v, Value::List(l) if self.table_fields(l).is_some()))
            .then(|| self.spelling(v).unwrap_or_else(|| self.scalar_line(v)))
    }

    fn scalar_line(&self, v: &Value) -> String {
        match v {
            Value::String(s) if plain_safe(s) => s.clone(),
            _ => self.flow(v),
        }
    }

    /// Write the part after `key:` or `-` (the marker is already written).
    fn value_after_marker(&mut self, v: &Value, indent: usize, used: usize) {
        if !self.wants_block(v, used) {
            self.out.push(' ');
            let s = self.spelling(v).unwrap_or_else(|| self.scalar_line(v));
            self.out.push_str(&s);
            self.out.push('\n');
            return;
        }

        match v {
            Value::String(s) => {
                self.out.push_str(if s.ends_with('\n') { " |\n" } else { " |-\n" });
                for line in s.strip_suffix('\n').unwrap_or(s).split('\n') {
                    if !line.is_empty() {
                        self.pad(indent + 2);
                        self.out.push_str(line);
                    }

                    self.out.push('\n');
                }
            }
            Value::Variant(t, Some(p)) => {
                let _ = writeln!(self.out, " !{t}");
                self.block(p, indent + 2);
            }
            _ => {
                self.out.push('\n');
                self.block(v, indent + 2);
            }
        }
    }

    /// Write a container as a block at `indent`.
    fn block(&mut self, v: &Value, indent: usize) {
        match v {
            Value::Record(r) => {
                for (k, v) in r.iter() {
                    self.entry(k, v, indent, true);
                }
            }
            Value::List(l) => {
                for (i, it) in l.iter().enumerate() {
                    #[expect(clippy::cast_possible_wrap, reason = "in-memory indices/lengths are at most isize::MAX, which fits in i64")]
                    self.path.push(crate::path::Segment::Index(i as i64));
                    self.item(it, indent);
                    self.path.pop();
                }
            }
            _ => unreachable!(),
        }
    }

    pub(crate) fn entry(&mut self, k: &str, v: &Value, indent: usize, pad: bool) {
        self.path.push(crate::path::Segment::Field(k.to_owned()));
        let key = key_text(k);
        let table = match v {
            Value::List(l) => self.table_fields(l),
            _ => None,
        };

        if pad {
            self.emit_leading(indent);
            if table.is_none() && v.is_container() && !self.wants_block(v, indent + key.len() + 2) {
                self.emit_hoisted(indent);
            }

            self.pad(indent);
        }

        let mark = self.out.len();
        if let (Value::List(l), Some(fields)) = (v, &table) {
            self.out.push_str(&key);
            self.table(l, fields, indent);
        } else {
            self.out.push_str(&key);
            self.out.push(':');
            self.value_after_marker(v, indent, indent + key.len() + 2);
        }

        self.emit_trailing(mark);
        self.path.pop();
    }

    fn table(&mut self, l: &[Value], fields: &[String], indent: usize) {
        let header: Vec<String> = fields.iter().map(|f| key_text(f)).collect();
        let _ = writeln!(self.out, "[{}]{{{}}}:", l.len(), header.join(","));
        for (i, row) in l.iter().enumerate() {
            let r = row.as_record().unwrap();
            #[expect(clippy::cast_possible_wrap, reason = "in-memory indices/lengths are at most isize::MAX, which fits in i64")]
            self.path.push(crate::path::Segment::Index(i as i64));
            self.emit_leading(indent + 2);
            self.pad(indent + 2);
            let mark = self.out.len();
            let cells: Vec<String> = fields
                .iter()
                .map(|f| match r.get(f) {
                    Some(v) => {
                        self.path.push(crate::path::Segment::Field(f.clone()));
                        let s = self.spelling(v).unwrap_or_else(|| self.flow(v));
                        self.path.pop();
                        s
                    }
                    None => String::new(),
                })
                .collect();
            self.out.push_str(&cells.join(self.sep()));
            self.out.push('\n');
            self.emit_trailing(mark);
            self.path.pop();
        }
    }

    pub(crate) fn item(&mut self, v: &Value, indent: usize) {
        self.emit_leading(indent);
        if v.is_container() && !matches!(v, Value::Record(r) if !r.is_empty()) && !self.wants_block(v, indent + 2) {
            self.emit_hoisted(indent);
        }

        self.pad(indent);
        let mark = self.out.len();
        self.item_body(v, indent);
        self.emit_trailing(mark);
    }

    fn item_body(&mut self, v: &Value, indent: usize) {
        self.out.push('-');
        match v {
            Value::Record(r) if !r.is_empty() => {
                self.out.push(' ');
                for (i, (k, v)) in r.iter().enumerate() {
                    self.entry(k, v, indent + 2, i > 0);
                }
            }
            Value::List(_) if self.wants_block(v, indent + 2) => {
                // Anonymous nested lists are always written as block lists.
                self.out.push('\n');
                self.block(v, indent + 2);
            }
            _ => self.value_after_marker(v, indent, indent + 2),
        }
    }
}

fn block_string_ok(s: &str) -> bool {
    s.contains('\n')
        && !s.ends_with("\n\n")
        && !s.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
        && !s.strip_suffix('\n').unwrap_or(s).split('\n').any(|l| l.trim().is_empty() && !l.is_empty())
        && !s.strip_suffix('\n').unwrap_or(s).ends_with('\n')
        && !s.strip_suffix('\n').unwrap_or(s).is_empty()
}

/// Render a value as a NOVA text document body.
#[must_use]
pub fn to_text(v: &Value, profile: Profile) -> String {
    let mut owned;
    let v = if profile == Profile::Canonical {
        owned = v.clone();
        canonicalize(&mut owned);
        &owned
    } else {
        v
    };

    let mut w = W::new(profile == Profile::Compact, None);
    write_body(&mut w, v);
    w.out
}

pub(crate) fn write_body(w: &mut W, v: &Value) {
    match v {
        Value::Record(r) if !r.is_empty() => w.block(v, 0),
        Value::List(l) if !l.is_empty() => {
            if let Some(fields) = w.table_fields(l) {
                w.table(l, &fields, 0);
                // Top-level tables are written `[N]{...}:` with rows at indent 2.
            } else {
                w.block(v, 0);
            }
        }
        _ => {
            let s = w.flow(v);
            w.out.push_str(&s);
            w.out.push('\n');
        }
    }
}

/// Render directives followed by a body.
#[must_use]
pub fn document_to_text(directives: &[(&str, &str)], v: &Value, profile: Profile) -> String {
    let mut out = String::new();
    for (n, a) in directives {
        out.push('@');
        out.push_str(n);
        if !a.is_empty() {
            out.push(' ');
            out.push_str(a);
        }

        out.push('\n');
    }

    if !directives.is_empty() && profile != Profile::Compact {
        out.push('\n');
    }

    out.push_str(&to_text(v, profile));
    out
}

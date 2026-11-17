//! Text writer with three profiles (spec §4.10).

use crate::parser::{classify, RESERVED_START};
use crate::path::{is_ident_char, is_ident_start};
use base64::Engine;
use nova_value::{canonical_bytes, Value};

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
pub fn key_text(k: &str) -> String {
    if is_ident(k) {
        k.to_owned()
    } else {
        quote(k)
    }
}

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
            c if (c as u32) < 0x20 || c == '\u{7f}' => o.push_str(&format!("\\u{:04x}", c as u32)),
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
        || s.chars().any(|c| c.is_control())
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
        if f > 0.0 {
            "@float(\"inf\")".into()
        } else {
            "@float(\"-inf\")".into()
        }
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
    v.sort_keys();
    fn walk(v: &mut Value) {
        match v {
            Value::List(l) | Value::Tuple(l) => l.iter_mut().for_each(walk),
            Value::Set(l) => {
                l.iter_mut().for_each(walk);
                l.sort_by_cached_key(canonical_bytes);
            }
            Value::Map(m) => {
                m.iter_mut().for_each(|(k, v)| {
                    walk(k);
                    walk(v)
                });
                m.sort_by_cached_key(|(k, _)| canonical_bytes(k));
            }
            Value::Record(r) => r.iter_mut().for_each(|(_, v)| walk(v)),
            Value::Variant(_, Some(p)) => walk(p),
            _ => {}
        }
    }
    walk(v);
}

struct W {
    out: String,
    compact: bool,
}

impl W {
    fn sep(&self) -> &'static str {
        if self.compact {
            ","
        } else {
            ", "
        }
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
        if !self.compact || l.len() < 2 {
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
            let s = self.scalar_line(v);
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
                self.out.push_str(&format!(" !{t}\n"));
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
                for it in l {
                    self.item(it, indent);
                }
            }
            _ => unreachable!(),
        }
    }

    fn entry(&mut self, k: &str, v: &Value, indent: usize, pad: bool) {
        if pad {
            self.pad(indent);
        }
        let key = key_text(k);
        if let Value::List(l) = v {
            if let Some(fields) = self.table_fields(l) {
                self.out.push_str(&key);
                self.table(l, &fields, indent);
                return;
            }
        }
        self.out.push_str(&key);
        self.out.push(':');
        self.value_after_marker(v, indent, indent + key.len() + 2);
    }

    fn table(&mut self, l: &[Value], fields: &[String], indent: usize) {
        let header: Vec<String> = fields.iter().map(|f| key_text(f)).collect();
        self.out.push_str(&format!("[{}]{{{}}}:\n", l.len(), header.join(",")));
        for row in l {
            let r = row.as_record().unwrap();
            self.pad(indent + 2);
            let cells: Vec<String> = fields.iter().map(|f| r.get(f).map(|v| self.flow(v)).unwrap_or_default()).collect();
            self.out.push_str(&cells.join(self.sep()));
            self.out.push('\n');
        }
    }

    fn item(&mut self, v: &Value, indent: usize) {
        self.pad(indent);
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
pub fn to_text(v: &Value, profile: Profile) -> String {
    let mut owned;
    let v = if profile == Profile::Canonical {
        owned = v.clone();
        canonicalize(&mut owned);
        &owned
    } else {
        v
    };
    let mut w = W {
        out: String::new(),
        compact: profile == Profile::Compact,
    };
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
    w.out
}

/// Render directives followed by a body.
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

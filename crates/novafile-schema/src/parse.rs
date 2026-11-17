//! Schema language parser (spec §6.1).

use crate::model::*;
use novafile_text::Parser;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for SchemaError {}

/// Cursor over a single type expression.
pub(crate) struct TyParser<'a> {
    s: &'a str,
    pub(crate) i: usize,
}

impl<'a> TyParser<'a> {
    pub(crate) fn new(s: &'a str) -> Self {
        TyParser { s, i: 0 }
    }

    fn b(&self) -> &[u8] {
        self.s.as_bytes()
    }

    fn peek(&self) -> Option<u8> {
        self.b().get(self.i).copied()
    }

    fn ws(&mut self) {
        while self.peek() == Some(b' ') {
            self.i += 1;
        }
    }

    pub(crate) fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }

    fn ident(&mut self) -> Result<&'a str, String> {
        let st = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_') {
            self.i += 1;
        }
        if st == self.i {
            return Err(format!("expected a type name at {:?}", self.rest()));
        }
        Ok(&self.s[st..self.i])
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn need(&mut self, c: u8) -> Result<(), String> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(format!("expected `{}` at {:?}", c as char, self.rest()))
        }
    }

    /// A type argument (inside `<…>` or a tuple), which may carry type-level
    /// attributes: `list<string @len(1..64)>`.
    fn inner(&mut self) -> Result<TypeRef, String> {
        let mut t = self.ty()?;
        let field_attrs = self.attrs(&mut t)?;
        if let Some((a, _)) = field_attrs.first() {
            return Err(format!("@{a} is only valid on record fields"));
        }
        Ok(t)
    }

    /// `type := atom ('[]' | '?')*` followed by type-level attributes.
    pub(crate) fn ty(&mut self) -> Result<TypeRef, String> {
        let mut t = self.atom()?;
        loop {
            if self.s[self.i..].starts_with("[]") {
                self.i += 2;
                t = TypeRef::new(TypeKind::List(Box::new(t)));
            } else if self.peek() == Some(b'?') {
                self.i += 1;
                t.nullable = true;
            } else {
                break;
            }
        }
        Ok(t)
    }

    fn atom(&mut self) -> Result<TypeRef, String> {
        self.ws();
        if self.eat(b'(') {
            let mut items = Vec::new();
            loop {
                self.ws();
                if self.eat(b')') {
                    break;
                }
                items.push(self.inner()?);
                if !self.eat(b',') {
                    self.need(b')')?;
                    break;
                }
            }
            return Ok(TypeRef::new(TypeKind::Tuple(items)));
        }
        let name = self.ident()?;
        let kind = match name {
            "any" => TypeKind::Any,
            "null" => TypeKind::Null,
            "bool" => TypeKind::Bool,
            "float" => TypeKind::Float,
            "decimal" => TypeKind::Decimal,
            "rational" => TypeKind::Rational,
            "string" => TypeKind::String,
            "bytes" => TypeKind::Bytes,
            "timestamp" => TypeKind::Timestamp,
            "duration" => TypeKind::Duration,
            "list" | "set" => {
                self.need(b'<')?;
                let t = Box::new(self.inner()?);
                self.need(b'>')?;
                if name == "list" { TypeKind::List(t) } else { TypeKind::Set(t) }
            }
            "map" => {
                self.need(b'<')?;
                let k = Box::new(self.inner()?);
                self.need(b',')?;
                let v = Box::new(self.inner()?);
                self.need(b'>')?;
                TypeKind::Map(k, v)
            }
            other => match IntKind::from_name(other) {
                Some(k) => TypeKind::Int(k),
                None => TypeKind::Named(other.to_owned()),
            },
        };
        Ok(TypeRef::new(kind))
    }

    fn bound_num(s: &str) -> Result<Option<Num>, String> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(None);
        }
        Num::parse(s).map(Some)
    }

    /// Parse attributes; type-level ones go into `ty`, field-level ones are returned.
    pub(crate) fn attrs(&mut self, ty: &mut TypeRef) -> Result<Vec<(String, String)>, String> {
        let mut field_attrs = Vec::new();
        loop {
            self.ws();
            if self.peek() != Some(b'@') {
                return Ok(field_attrs);
            }
            self.i += 1;
            let name = self.ident()?.to_owned();
            let arg = if self.peek() == Some(b'(') {
                let close = self.s[self.i..].find(')').ok_or("unclosed attribute argument")?;
                let a = self.s[self.i + 1..self.i + close].to_owned();
                self.i += close + 1;
                a
            } else {
                String::new()
            };
            match name.as_str() {
                "range" | "len" => {
                    let (bounds, unit) = match arg.split_once(',') {
                        Some((b, u)) if name == "len" => {
                            let u = u.trim();
                            (
                                b,
                                Some(LenUnit::from_name(u).ok_or_else(|| format!("unknown @len unit {u:?} (scalars, utf8, utf16, graphemes)"))?),
                            )
                        }
                        _ => (arg.as_str(), None),
                    };
                    let (lo, hi) = bounds.split_once("..").ok_or_else(|| format!("@{name} expects `lo..hi`"))?;
                    if let Some(u) = unit {
                        ty.constraints.len_unit = u;
                    }
                    if name == "range" {
                        ty.constraints.range = Some((Self::bound_num(lo)?, Self::bound_num(hi)?));
                    } else {
                        let p = |s: &str| -> Result<Option<u64>, String> {
                            let s = s.trim();
                            if s.is_empty() {
                                Ok(None)
                            } else {
                                s.parse().map(Some).map_err(|_| format!("invalid length {s:?}"))
                            }
                        };
                        ty.constraints.len = Some((p(lo)?, p(hi)?));
                    }
                }
                "key" | "id" | "deprecated" | "open" => field_attrs.push((name, arg)),
                other => return Err(format!("unknown attribute @{other}")),
            }
        }
    }
}

pub fn parse_type(s: &str) -> Result<TypeRef, String> {
    let mut p = TyParser::new(s);
    let mut t = p.ty()?;
    let fa = p.attrs(&mut t)?;
    if let Some((n, _)) = fa.first() {
        return Err(format!("@{n} is only valid on record fields"));
    }
    if !p.rest().trim().is_empty() {
        return Err(format!("unexpected {:?} after type", p.rest().trim()));
    }
    Ok(t)
}

fn strip_comment(line: &str) -> &str {
    // `#` starts a comment when preceded by whitespace or at line start, outside quotes.
    let b = line.as_bytes();
    let mut in_str = false;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if in_str => i += 1,
            b'"' => in_str = !in_str,
            b'#' if !in_str && (i == 0 || b[i - 1] == b' ') => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

pub fn parse_schema(src: &str) -> Result<Schema, SchemaError> {
    let mut schema = Schema::default();
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0;
    let err = |line: usize, m: String| SchemaError { line: line + 1, message: m };
    while i < lines.len() {
        let raw = strip_comment(lines[i]).trim_end();
        if raw.trim().is_empty() {
            i += 1;
            continue;
        }
        if raw.starts_with(' ') {
            return Err(err(i, "unexpected indentation".into()));
        }
        if let Some(rest) = raw.strip_prefix("@nova ") {
            if schema.name.is_some() || schema.root.is_some() || !schema.defs.is_empty() {
                return Err(err(i, "@nova must be the first directive".into()));
            }
            novafile_text::version::NOVA_VERSION.check(rest, "NOVA").map_err(|m| err(i, m))?;
            i += 1;
        } else if let Some(rest) = raw.strip_prefix("@schema ") {
            let r = crate::SchemaRef::parse(rest.trim()).map_err(|m| err(i, m))?;
            if r.fingerprint.is_some() {
                return Err(err(i, "a schema declares its version, not a fingerprint".into()));
            }
            schema.name = Some(r.name);
            schema.version = Some(r.version);
            i += 1;
        } else if let Some(rest) = raw.strip_prefix("@root ") {
            schema.root = Some(rest.trim().to_owned());
            i += 1;
        } else if let Some(rest) = raw.strip_prefix("type ") {
            let (name, ty) = rest.split_once('=').ok_or_else(|| err(i, "expected `type Name = T`".into()))?;
            let t = parse_type(ty.trim()).map_err(|m| err(i, m))?;
            push_def(&mut schema, name.trim(), Def::Alias(t)).map_err(|m| err(i, m))?;
            i += 1;
        } else if let Some(rest) = raw.strip_prefix("enum ") {
            if let Some((name, alts)) = rest.split_once('=') {
                let mut vs = Vec::new();
                for alt in alts.split('|') {
                    vs.push(parse_variant(alt.trim()).map_err(|m| err(i, m))?);
                }
                push_def(&mut schema, name.trim(), Def::Enum(vs)).map_err(|m| err(i, m))?;
                i += 1;
            } else {
                let name = rest
                    .strip_suffix(':')
                    .ok_or_else(|| err(i, "expected `enum Name:` or `enum Name = a | b`".into()))?
                    .trim();
                i += 1;
                let mut vs = Vec::new();
                while i < lines.len() {
                    let l = strip_comment(lines[i]).trim_end();
                    if l.trim().is_empty() {
                        i += 1;
                        continue;
                    }
                    let Some(body) = l.strip_prefix("  ") else { break };
                    vs.push(parse_variant(body.trim()).map_err(|m| err(i, m))?);
                    i += 1;
                }
                if vs.is_empty() {
                    return Err(err(i - 1, format!("enum {name} has no variants")));
                }
                push_def(&mut schema, name, Def::Enum(vs)).map_err(|m| err(i, m))?;
            }
        } else if let Some(rest) = raw.strip_prefix("record ") {
            let head = rest.strip_suffix(':').ok_or_else(|| err(i, "expected `record Name:`".into()))?.trim();
            let (name, open) = match head.strip_suffix("@open") {
                Some(n) => (n.trim(), true),
                None => (head, false),
            };
            let start = i;
            i += 1;
            let mut fields: Vec<FieldDef> = Vec::new();
            while i < lines.len() {
                let l = strip_comment(lines[i]).trim_end();
                if l.trim().is_empty() {
                    i += 1;
                    continue;
                }
                let Some(body) = l.strip_prefix("  ") else { break };
                if body.starts_with(' ') {
                    return Err(err(i, "fields are indented exactly 2 spaces".into()));
                }
                let f = parse_field(body).map_err(|m| err(i, m))?;
                if fields.iter().any(|g| g.name == f.name) {
                    return Err(err(i, format!("duplicate field {:?}", f.name)));
                }
                if let Some(id) = f.id
                    && fields.iter().any(|g| g.id == Some(id))
                {
                    return Err(err(i, format!("duplicate field id {id}")));
                }
                fields.push(f);
                i += 1;
            }
            push_def(&mut schema, name, Def::Record(RecordDef { fields, open })).map_err(|m| err(start, m))?;
        } else {
            return Err(err(i, format!("expected `record`, `enum`, `type`, `@schema` or `@root`, found {:?}", raw)));
        }
    }
    check_refs(&schema).map_err(|m| SchemaError { line: 0, message: m })?;
    Ok(schema)
}

fn push_def(s: &mut Schema, name: &str, d: Def) -> Result<(), String> {
    if !name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("type names start with an uppercase letter: {name:?}"));
    }
    if s.def(name).is_some() {
        return Err(format!("duplicate definition {name}"));
    }
    s.defs.push((name.to_owned(), d));
    Ok(())
}

fn parse_variant(s: &str) -> Result<VariantDef, String> {
    match s.split_once('(') {
        Some((n, rest)) => {
            let inner = rest.strip_suffix(')').ok_or("expected `)` after variant payload")?;
            Ok(VariantDef {
                name: check_name(n.trim())?,
                payload: Some(parse_type(inner)?),
            })
        }
        None => Ok(VariantDef {
            name: check_name(s)?,
            payload: None,
        }),
    }
}

fn check_name(n: &str) -> Result<String, String> {
    let ok = n.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok { Ok(n.to_owned()) } else { Err(format!("invalid name {n:?}")) }
}

fn parse_field(body: &str) -> Result<FieldDef, String> {
    let (lhs, rhs) = body.split_once(':').ok_or("expected `name: type`")?;
    let (name, optional) = match lhs.strip_suffix('?') {
        Some(n) => (n, true),
        None => (lhs, false),
    };
    let name = check_name(name.trim())?;
    let mut p = TyParser::new(rhs);
    let mut ty = p.ty()?;
    let attrs = p.attrs(&mut ty)?;
    let mut f = FieldDef {
        name,
        ty,
        optional,
        default: None,
        key: false,
        id: None,
        deprecated: false,
    };
    for (a, arg) in attrs {
        match a.as_str() {
            "key" => f.key = true,
            "deprecated" => f.deprecated = true,
            "id" => f.id = Some(arg.trim().parse().map_err(|_| format!("invalid @id({arg})"))?),
            other => return Err(format!("@{other} is not valid on a field")),
        }
    }
    let rest = p.rest().trim_start();
    if let Some(def) = rest.strip_prefix('=') {
        let src = def.trim_start();
        let mut fp = Parser::new(src, false);
        let v = fp.parse_flow_value().map_err(|e| format!("invalid default: {}", e.message))?;
        fp.end_line().map_err(|e| format!("invalid default: {}", e.message))?;
        f.default = Some(v);
    } else if !rest.is_empty() {
        return Err(format!("unexpected {rest:?} in field definition"));
    }
    if f.key && (f.optional || f.ty.nullable) {
        return Err("@key fields must be required and non-null".into());
    }
    Ok(f)
}

fn check_refs(s: &Schema) -> Result<(), String> {
    fn walk(s: &Schema, t: &TypeRef) -> Result<(), String> {
        match &t.kind {
            TypeKind::Named(n) => s.def(n).map(|_| ()).ok_or_else(|| format!("unknown type {n}")),
            TypeKind::List(t) | TypeKind::Set(t) => walk(s, t),
            TypeKind::Map(k, v) => walk(s, k).and(walk(s, v)),
            TypeKind::Tuple(ts) => ts.iter().try_for_each(|t| walk(s, t)),
            _ => Ok(()),
        }
    }
    for (name, d) in &s.defs {
        match d {
            Def::Alias(t) => {
                walk(s, t)?;
                // Reject alias cycles.
                let mut seen = vec![name.as_str()];
                let mut cur = t;
                while let TypeKind::Named(n) = &cur.kind {
                    if seen.contains(&n.as_str()) {
                        return Err(format!("type alias cycle through {n}"));
                    }
                    seen.push(n);
                    match s.def(n) {
                        Some(Def::Alias(t2)) => cur = t2,
                        _ => break,
                    }
                }
            }
            Def::Enum(vs) => vs.iter().filter_map(|v| v.payload.as_ref()).try_for_each(|t| walk(s, t))?,
            Def::Record(r) => r.fields.iter().try_for_each(|f| walk(s, &f.ty))?,
        }
    }
    if let Some(r) = &s.root {
        s.def(r).ok_or_else(|| format!("unknown root type {r}"))?;
    }
    Ok(())
}

//! Schema language parser (spec §6.1).

use crate::model::{Class, Def, FieldDef, Import, IntKind, LenUnit, Num, RecordDef, RecordRule, RuleKind, Schema, TypeKind, TypeRef, VariantDef};
use novafile_text::Parser;
use std::cell::Cell;

/// A schema that failed to parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaError {
    /// 1-based line of the problem; 0 for checks over the whole schema
    /// (unknown references, alias cycles).
    pub line: usize,
    /// What is wrong.
    pub message: String,
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for SchemaError {}

/// How deeply type arguments may nest (`list<list<…>>`), so hostile input
/// cannot overflow the stack.
const MAX_DEPTH: usize = 64;

/// Cursor over a single type expression.
pub(crate) struct TyParser<'a> {
    /// The text being parsed.
    s: &'a str,
    /// Byte offset of the cursor in `s`.
    pub(crate) i: usize,
    /// Current type-argument nesting, capped at [`MAX_DEPTH`].
    depth: usize,
    /// Compiled-pattern bytes left for the schema being parsed.
    budget: &'a Cell<usize>,
}

impl<'a> TyParser<'a> {
    /// A cursor at the start of `s`, compiling patterns within `budget`.
    pub(crate) fn new(s: &'a str, budget: &'a Cell<usize>) -> Self {
        TyParser { s, i: 0, depth: 0, budget }
    }

    /// The input as bytes.
    fn b(&self) -> &[u8] {
        self.s.as_bytes()
    }

    /// The byte at the cursor, if any.
    fn peek(&self) -> Option<u8> {
        self.b().get(self.i).copied()
    }

    /// Skip spaces (only `' '`, not tabs).
    fn ws(&mut self) {
        while self.peek() == Some(b' ') {
            self.i += 1;
        }
    }

    /// The input after the cursor.
    pub(crate) fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }

    /// Consume an identifier (`[A-Za-z0-9_]+`); fails if there is none.
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

    /// Skip spaces, then consume `c` if it is next; returns whether it was.
    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    /// Like [`Self::eat`], but `c` is required.
    fn need(&mut self, c: u8) -> Result<(), String> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(format!("expected `{}` at {:?}", c as char, self.rest()))
        }
    }

    /// A type argument (inside `<…>` or a tuple), which may be a union and
    /// may carry type-level attributes: `list<string @len(1..64) | int>`.
    fn inner(&mut self) -> Result<TypeRef, String> {
        if self.depth == MAX_DEPTH {
            return Err(format!("type arguments nested deeper than {MAX_DEPTH} levels"));
        }

        self.depth += 1;
        let t = self.union();
        self.depth -= 1;
        let (t, field_attrs) = t?;
        if let Some((a, _)) = field_attrs.first() {
            return Err(format!("@{a} is only valid on record fields"));
        }

        Ok(t)
    }

    /// `union := member ('|' member)*`, where a member is a type followed by
    /// its type-level attributes (spec §6.1: `|` binds loosest). Field-level
    /// attributes are returned for the caller to place or reject.
    pub(crate) fn union(&mut self) -> Result<(TypeRef, Vec<(String, String)>), String> {
        let mut members = Vec::new();
        let mut field_attrs = Vec::new();
        loop {
            let mut t = self.ty()?;
            field_attrs.extend(self.attrs(&mut t)?);
            members.push(t);
            if !self.eat(b'|') {
                break;
            }
        }

        let t = if members.len() == 1 {
            members.pop().unwrap()
        } else {
            TypeRef::new(TypeKind::Union(members))
        };
        Ok((t, field_attrs))
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

    /// A tuple, built-in or named type, before any `[]` or `?` suffix.
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

        if self.peek() == Some(b'"') {
            let end = quoted_end(self.rest()).ok_or("unterminated string literal type")?;
            let text = &self.rest()[..end];
            let Ok(novafile_value::Value::String(lit)) = novafile_text::parse_flow(text) else {
                return Err(format!("invalid string literal type {text}"));
            };

            self.i += end;
            return Ok(TypeRef::new(TypeKind::Literal(lit)));
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
                None => TypeKind::Named(self.qualified(other)?),
            },
        };

        Ok(TypeRef::new(kind))
    }

    /// A type name, with the `.Name` of a qualified reference (`std.Uuid`,
    /// proposal 0008) if `alias` is followed by a dot.
    fn qualified(&mut self, alias: &str) -> Result<String, String> {
        if self.peek() != Some(b'.') {
            return Ok(alias.to_owned());
        }

        self.i += 1;
        let name = self.ident()?;
        Ok(format!("{alias}.{name}"))
    }

    /// One side of a `@range`; empty means unbounded.
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
                let close = close_paren(self.rest()).ok_or("unclosed attribute argument")?;
                let a = self.s[self.i + 1..self.i + close].to_owned();
                self.i += close + 1;
                a
            } else {
                String::new()
            };

            if matches!(ty.kind, TypeKind::Literal(_)) && matches!(name.as_str(), "range" | "len" | "pattern") {
                return Err("a literal type takes no attributes".into());
            }

            match name.as_str() {
                "pattern" => {
                    // A reference may name an alias of `string`; `check_refs` checks that once it is defined.
                    if !matches!(ty.kind, TypeKind::String | TypeKind::Named(_)) {
                        return Err(format!("@pattern applies to string, not {}", TypeRef::new(ty.kind.clone())));
                    }

                    if ty.constraints.pattern.is_some() {
                        return Err("duplicate @pattern".into());
                    }

                    let Ok(novafile_value::Value::String(p)) = novafile_text::parse_flow(arg.trim()) else {
                        return Err("@pattern expects one quoted string".into());
                    };

                    let compiled = crate::Pattern::new_within(&p, self.budget).map_err(|e| format!("@pattern({}): {e}", novafile_text::quote(&p)))?;
                    ty.constraints.pattern = Some(compiled);
                }
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
                "personal" | "secret" => set_class(ty, &name, &arg)?,
                "key" | "id" | "deprecated" | "open" => field_attrs.push((name, arg)),
                other => return Err(format!("unknown attribute @{other}")),
            }
        }
    }
}

/// Set the class of `ty` from the attribute `@name(arg)`. Fails if the
/// attribute has an argument or `ty` already has a class.
fn set_class(ty: &mut TypeRef, name: &str, arg: &str) -> Result<(), String> {
    if !arg.is_empty() {
        return Err(format!("@{name} takes no argument"));
    }

    if let Some(c) = ty.constraints.class {
        return Err(format!("@{name}: the type already has @{c}; a type has at most one class"));
    }

    ty.constraints.class = Class::from_name(name);
    Ok(())
}

/// Parse a standalone type expression such as `list<int> @len(..8)`.
///
/// # Errors
///
/// Fails if `s` is not a valid type expression, carries a field-only
/// attribute, or has trailing input.
pub fn parse_type(s: &str) -> Result<TypeRef, String> {
    parse_type_within(s, &Cell::new(crate::pattern::SCHEMA_BUDGET))
}

/// [`parse_type`], with patterns compiled within `budget`, which several
/// types share when they come from one source (a template's `@param`s).
/// Start it at [`crate::SCHEMA_BUDGET`].
///
/// # Errors
///
/// As [`parse_type`], and if `budget` does not cover the type's patterns.
pub fn parse_type_within(s: &str, budget: &Cell<usize>) -> Result<TypeRef, String> {
    let mut p = TyParser::new(s, budget);
    let (t, fa) = p.union()?;
    if let Some((n, _)) = fa.first() {
        return Err(format!("@{n} is only valid on record fields"));
    }

    if !p.rest().trim().is_empty() {
        return Err(format!("unexpected {:?} after type", p.rest().trim()));
    }

    Ok(t)
}

/// The line without its trailing `#` comment.
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

/// The byte length of the quoted string at the start of `s`, quotes
/// included, or `None` if it is unterminated.
fn quoted_end(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 1;
    while i < b.len() && b[i] != b'"' {
        i += if b[i] == b'\\' { 2 } else { 1 };
    }

    (i < b.len()).then_some(i + 1)
}

/// The byte offset of the `)` that closes the `(` at the start of `s`,
/// skipping quoted strings, so `@pattern("(a)")` keeps its parentheses.
fn close_paren(s: &str) -> Option<usize> {
    let mut i = 1;
    while i < s.len() {
        match s.as_bytes()[i] {
            b')' => return Some(i),
            b'"' => i += quoted_end(&s[i..])?,
            _ => i += 1,
        }
    }

    None
}

/// Parse NOVA schema text (spec §6) and link its imports. Only the bundled
/// standard library (`novafile.dev/std`) can be imported: use
/// [`parse_schema_unlinked`] and [`Schema::link`] to resolve other imports.
///
/// # Errors
///
/// Fails, with the offending line, on any syntax error, duplicate or invalid
/// name, duplicate field id, unknown reference, alias cycle, invalid
/// default or import that does not resolve.
pub fn parse_schema(src: &str) -> Result<Schema, SchemaError> {
    let mut schema = parse_schema_unlinked(src)?;
    schema.link(&|r| Err(format!("cannot import {r}: only the bundled {} is available here", crate::STD_NAME)))?;
    Ok(schema)
}

/// Parse NOVA schema text (spec §6) without linking its imports or checking
/// its references. Call [`Schema::link`] before the schema is used.
///
/// # Errors
///
/// Fails, with the offending line, on any syntax error, duplicate or invalid
/// name, duplicate field id or invalid `@import`.
#[expect(clippy::too_many_lines, reason = "one linear pass over the schema's line-oriented grammar")]
pub fn parse_schema_unlinked(src: &str) -> Result<Schema, SchemaError> {
    let mut schema = Schema::default();
    let budget = Cell::new(crate::pattern::SCHEMA_BUDGET);
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
            if schema.name.is_some() || schema.root.is_some() || !schema.defs.is_empty() || !schema.extensions.is_empty() || !schema.imports.is_empty() {
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
        } else if let Some(ext) = raw.strip_prefix("@x-") {
            // Extension directives: kept, never interpreted here (spec §6.1).
            if !schema.defs.is_empty() {
                return Err(err(i, "extension directives come before the first definition".into()));
            }

            schema.extensions.push(format!("x-{ext}"));
            i += 1;
        } else if let Some(rest) = raw.strip_prefix("@import ") {
            if !schema.defs.is_empty() || schema.root.is_some() {
                return Err(err(i, "@import comes before @root and the first definition".into()));
            }

            let import = parse_import(rest, &schema).map_err(|m| err(i, m))?;
            schema.imports.push(import);
            i += 1;
        } else if let Some(rest) = raw.strip_prefix("@root ") {
            schema.root = Some(parse_type_within(rest.trim(), &budget).map_err(|m| err(i, format!("@root: {m}")))?);
            i += 1;
        } else if let Some(rest) = raw.strip_prefix("type ") {
            let (name, ty) = rest.split_once('=').ok_or_else(|| err(i, "expected `type Name = T`".into()))?;
            let t = parse_type_within(ty.trim(), &budget).map_err(|m| err(i, m))?;
            push_def(&mut schema, name.trim(), Def::Alias(t)).map_err(|m| err(i, m))?;
            push_doc(&mut schema, name.trim(), doc_above(&lines, i));
            i += 1;
        } else if let Some(rest) = raw.strip_prefix("enum ") {
            if let Some((head, alts)) = rest.split_once('=') {
                let h = def_head(head).map_err(|m| err(i, m))?;
                h.enum_only().map_err(|m| err(i, m))?;
                let (name, class) = (h.name, h.class);

                let mut vs = Vec::new();
                for alt in split_top(alts, b'|') {
                    push_variant(&mut vs, parse_variant(alt.trim(), &budget).map_err(|m| err(i, m))?).map_err(|m| err(i, m))?;
                }

                push_def(&mut schema, name, Def::Enum(vs)).map_err(|m| err(i, m))?;
                push_class(&mut schema, name, class);
                push_doc(&mut schema, name, doc_above(&lines, i));
                i += 1;
            } else {
                let head = rest
                    .strip_suffix(':')
                    .ok_or_else(|| err(i, "expected `enum Name:` or `enum Name = a | b`".into()))?;
                let h = def_head(head).map_err(|m| err(i, m))?;
                h.enum_only().map_err(|m| err(i, m))?;
                let (name, class) = (h.name, h.class);

                let doc = doc_above(&lines, i);
                i += 1;
                let mut vs = Vec::new();
                while i < lines.len() {
                    let l = strip_comment(lines[i]).trim_end();
                    if l.trim().is_empty() {
                        i += 1;
                        continue;
                    }

                    let Some(body) = l.strip_prefix("  ") else { break };
                    let mut v = parse_variant(body.trim(), &budget).map_err(|m| err(i, m))?;
                    v.doc = doc_above(&lines, i);
                    push_variant(&mut vs, v).map_err(|m| err(i, m))?;
                    i += 1;
                }

                if vs.is_empty() {
                    return Err(err(i - 1, format!("enum {name} has no variants")));
                }

                push_def(&mut schema, name, Def::Enum(vs)).map_err(|m| err(i, m))?;
                push_class(&mut schema, name, class);
                push_doc(&mut schema, name, doc);
            }
        } else if let Some(rest) = raw.strip_prefix("record ") {
            let head = rest.strip_suffix(':').ok_or_else(|| err(i, "expected `record Name:`".into()))?;
            let DefHead { name, open, class, rules } = def_head(head).map_err(|m| err(i, m))?;

            let doc = doc_above(&lines, i);
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

                let mut f = parse_field(body, &budget).map_err(|m| err(i, m))?;
                f.doc = doc_above(&lines, i);
                check_unique_field(&fields, &f).map_err(|m| err(i, m))?;
                fields.push(f);
                i += 1;
            }

            let record = RecordDef { fields, open, rules };
            check_rules(name, &record).map_err(|m| err(start, m))?;
            push_def(&mut schema, name, Def::Record(record)).map_err(|m| err(start, m))?;
            push_class(&mut schema, name, class);
            push_doc(&mut schema, name, doc);
        } else if let Some(d) = raw.strip_prefix('@') {
            return Err(err(i, format!("unknown directive @{}", d.split(' ').next().unwrap_or(d))));
        } else {
            return Err(err(i, format!("expected `record`, `enum`, `type`, `@schema` or `@root`, found {raw:?}")));
        }
    }

    Ok(schema)
}

/// Parse the argument of `@import`: `name@version[#fingerprint] as alias`.
/// Fails if it is malformed, or the alias is not an identifier or is taken.
pub(crate) fn parse_import(rest: &str, schema: &Schema) -> Result<Import, String> {
    let (reference, alias) = rest.trim().split_once(" as ").ok_or("expected `@import name@version as alias`")?;
    let reference = crate::SchemaRef::parse(reference.trim())?;
    let alias = alias.trim();
    let ok = alias.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && alias.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok {
        return Err(format!("import alias {alias:?} is not an identifier of letters, digits and `_`"));
    }

    if schema.imports.iter().any(|i| i.alias == alias) {
        return Err(format!("duplicate import alias {alias}"));
    }

    Ok(Import {
        alias: alias.to_owned(),
        reference,
    })
}

/// The head of a definition after `record` or `enum`: its name and attributes.
struct DefHead<'a> {
    /// The definition name.
    name: &'a str,
    /// `@open` is written.
    open: bool,
    /// `@personal` or `@secret`, if written.
    class: Option<Class>,
    /// `@together`, `@one_of` and `@any_of`, in the order written.
    rules: Vec<RecordRule>,
}

impl DefHead<'_> {
    /// Fail if the head has a record attribute (`@open` or a rule), which an
    /// enum cannot carry.
    fn enum_only(&self) -> Result<(), String> {
        if self.open {
            return Err("@open applies to records, not enums".into());
        }

        if !self.rules.is_empty() {
            return Err("record rules apply to records, not enums".into());
        }

        Ok(())
    }
}

/// Split a definition head such as `Name @open @one_of(a, b) @secret` into
/// its parts. Fails on any other attribute, a repeated `@open`, two classes,
/// or a malformed rule.
fn def_head(head: &str) -> Result<DefHead<'_>, String> {
    let head = head.trim();
    let end = head.find(char::is_whitespace).unwrap_or(head.len());
    let name = &head[..end];
    if name.is_empty() {
        return Err("expected a definition name".into());
    }

    let mut h = DefHead {
        name,
        open: false,
        class: None,
        rules: Vec::new(),
    };
    let mut rest = head[end..].trim_start();
    while !rest.is_empty() {
        let attr = rest
            .strip_prefix('@')
            .ok_or_else(|| format!("unexpected {rest:?} after definition name {name}"))?;
        let word_end = attr.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(attr.len());
        let (word, after) = attr.split_at(word_end);
        let (arg, after) = match after.strip_prefix('(') {
            Some(_) => {
                let close = close_paren(after).ok_or_else(|| format!("unclosed @{word}("))?;
                (Some(&after[1..close]), &after[close + 1..])
            }
            None => (None, after),
        };

        head_attr(&mut h, word, arg)?;
        if !after.is_empty() && !after.starts_with(char::is_whitespace) {
            return Err(format!("unexpected {after:?} after @{word}"));
        }

        rest = after.trim_start();
    }

    Ok(h)
}

/// Apply one attribute of a definition head, `@word` or `@word(arg)`, to `h`.
fn head_attr(h: &mut DefHead<'_>, word: &str, arg: Option<&str>) -> Result<(), String> {
    match (word, arg) {
        ("open", None) if !h.open => h.open = true,
        (w, None) if Class::from_name(w).is_some() => {
            if h.class.is_some() {
                return Err("a definition has at most one class".into());
            }

            h.class = Class::from_name(w);
        }
        (w, Some(arg)) if RuleKind::from_name(w).is_some() => {
            let kind = RuleKind::from_name(w).expect("checked by the match guard");
            let fields = arg.split(',').map(|f| f.trim().to_owned()).collect();
            h.rules.push(RecordRule { kind, fields });
        }
        (w, _) => return Err(format!("unexpected @{w} after definition name {}", h.name)),
    }

    Ok(())
}

/// Check the record rules of the record `name` (proposal 0010): each names
/// two or more distinct, declared, optional fields without a default or `@key`,
/// by names that need no quotes.
///
/// # Errors
///
/// Returns a message for the first rule that breaks one of these conditions.
pub(crate) fn check_rules(name: &str, r: &RecordDef) -> Result<(), String> {
    for rule in &r.rules {
        if rule.fields.len() < 2 {
            return Err(format!("{name} {rule}: a record rule names at least two fields"));
        }

        for (i, f) in rule.fields.iter().enumerate() {
            if rule.fields[..i].contains(f) {
                return Err(format!("{name} {rule}: field {f} is named twice"));
            }

            if check_name(f).is_err() {
                return Err(format!("{name} {rule}: {f:?} is not a field name without quotes"));
            }

            let Some(fd) = r.field(f) else {
                return Err(format!("{name} {rule}: no field {f:?}"));
            };

            if !fd.optional || fd.default.is_some() || fd.key {
                return Err(format!("{name} {rule}: field {f} must be optional (`{f}?:`), without a default or @key"));
            }
        }
    }

    Ok(())
}

/// Record the class written on the definition `name`, if any.
fn push_class(s: &mut Schema, name: &str, class: Option<Class>) {
    if let Some(c) = class {
        s.def_classes.push((name.to_owned(), c));
    }
}

/// Record the documentation of the definition `name`, if it has any.
fn push_doc(s: &mut Schema, name: &str, doc: Option<String>) {
    if let Some(d) = doc {
        s.def_docs.push((name.to_owned(), d));
    }
}

/// The documentation of the definition, field or variant on line `i`: the
/// comment-only lines directly above it, without `#` and one space after it.
/// A blank line or any other line ends the run, so a file header comment
/// separated by a blank line documents nothing.
fn doc_above(lines: &[&str], i: usize) -> Option<String> {
    let above = lines.get(..i)?;
    let n = above.iter().rev().take_while(|l| l.trim_start().starts_with('#')).count();
    let run = above.get(above.len() - n..)?;
    let text: Vec<&str> = run
        .iter()
        .map(|l| {
            let t = l.trim_start().strip_prefix('#').unwrap_or_default();
            t.strip_prefix(' ').unwrap_or(t).trim_end()
        })
        .collect();

    (n > 0).then(|| text.join("\n"))
}

/// Add a definition. Fails if the name does not start with an uppercase
/// letter, contains characters other than `[A-Za-z0-9_]`, or is taken.
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

/// Parse an enum variant: `name` or `name(Type)`, optionally followed by `@id(n)`.
fn parse_variant(s: &str, budget: &Cell<usize>) -> Result<VariantDef, String> {
    let (s, id) = match s.rsplit_once("@id(") {
        Some((head, arg)) if head.ends_with(' ') && arg.ends_with(')') && !arg[..arg.len() - 1].contains(')') => {
            let n = arg[..arg.len() - 1].trim();
            (head.trim_end(), Some(n.parse().map_err(|_| format!("invalid @id({n})"))?))
        }
        _ => (s, None),
    };

    let (name, rest) = if s.starts_with('"') {
        // A quoted name, like a quoted field name: any non-empty string (spec §6.1).
        let end = quoted_end(s).ok_or("unterminated quoted variant name")?;
        let Ok(novafile_value::Value::String(name)) = novafile_text::parse_flow(&s[..end]) else {
            return Err(format!("invalid variant name {}", &s[..end]));
        };
        // An enum variant named `@redacted` would let resolution turn the string into a marker.
        if name.is_empty() {
            return Err("a variant name cannot be empty".into());
        }

        novafile_value::variant_tag(&name).map_err(|m| format!("invalid variant name: {m}"))?;

        (name, &s[end..])
    } else {
        let i = s.find('(').unwrap_or(s.len());
        (check_name(s[..i].trim())?, &s[i..])
    };

    let payload = match rest.strip_prefix('(') {
        Some(rest) => Some(parse_type_within(rest.strip_suffix(')').ok_or("expected `)` after variant payload")?, budget)?),
        None if rest.trim().is_empty() => None,
        None => return Err(format!("unexpected {:?} after variant {name:?}", rest.trim())),
    };

    Ok(VariantDef { name, payload, id, doc: None })
}

/// Add a variant to an enum. Fails if its `@id` is 0 or taken, or if it has
/// an `@id` while earlier variants have none (or the other way round).
fn push_variant(vs: &mut Vec<VariantDef>, v: VariantDef) -> Result<(), String> {
    if let Some(first) = vs.first()
        && first.id.is_some() != v.id.is_some()
    {
        return Err(format!("variant {:?}: either every variant of an enum has @id or none does", v.name));
    }

    if let Some(id) = v.id {
        if id == 0 {
            return Err(format!("variant {:?}: @id(0) is not allowed; variant ids start at 1", v.name));
        }

        if vs.iter().any(|w| w.id == Some(id)) {
            return Err(format!("duplicate variant id {id}"));
        }
    }

    vs.push(v);
    Ok(())
}

/// Accept a field or variant name: a letter or `_`, then letters, digits, `_`
/// or `-`.
fn check_name(n: &str) -> Result<String, String> {
    let ok = n.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok { Ok(n.to_owned()) } else { Err(format!("invalid name {n:?}")) }
}

/// Split `s` at `sep` outside parentheses and angle brackets, so inline enum
/// variants can carry union payloads: `a | b(string | int)`.
fn split_top(s: &str, sep: u8) -> Vec<&str> {
    let (mut out, mut depth, mut start, mut in_str) = (Vec::new(), 0i32, 0, false);
    let mut escaped = false;
    for (i, c) in s.bytes().enumerate() {
        if in_str {
            match c {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_str = false,
                _ => {}
            }

            continue;
        }

        match c {
            b'"' => in_str = true,
            b'(' | b'<' => depth += 1,
            b')' | b'>' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }

    out.push(&s[start..]);
    out
}

/// Split a field line into its name and the text after the `:`. The name is
/// an identifier or a quoted string, like a record key (spec §6.1).
fn field_name(body: &str) -> Result<(String, bool, &str), String> {
    let (name, rest) = if body.starts_with('"') {
        let end = quoted_end(body).ok_or("unterminated quoted field name")?;
        let Ok(novafile_value::Value::String(name)) = novafile_text::parse_flow(&body[..end]) else {
            return Err(format!("invalid field name {}", &body[..end]));
        };

        (name, &body[end..])
    } else {
        let i = body.find(['?', ':']).ok_or("expected `name: type`")?;
        (check_name(body[..i].trim())?, &body[i..])
    };

    let (optional, rest) = match rest.strip_prefix('?') {
        Some(r) => (true, r),
        None => (false, rest),
    };

    let rhs = rest.strip_prefix(':').ok_or("expected `name: type`")?;
    Ok((name, optional, rhs))
}

/// Parse a record field line without its indentation:
/// `name[?]: type [attributes] [= default]`.
fn parse_field(body: &str, budget: &Cell<usize>) -> Result<FieldDef, String> {
    let (name, optional, rhs) = field_name(body)?;
    let mut p = TyParser::new(rhs, budget);
    let (ty, attrs) = p.union()?;
    let mut f = FieldDef {
        name,
        ty,
        optional,
        default: None,
        key: false,
        id: None,
        deprecated: false,
        doc: None,
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

    check_key_field(&f)?;
    Ok(f)
}

/// Check the `@key` rules on one field: a key is required, non-null and an identifier.
///
/// # Errors
///
/// Returns a message for the first rule that `f` breaks.
fn check_key_field(f: &FieldDef) -> Result<(), String> {
    if f.key && (f.optional || f.ty.nullable) {
        return Err("@key fields must be required and non-null".into());
    }

    if f.key && check_name(&f.name).is_err() {
        return Err(format!(
            "field {}: @key needs an identifier name, since keyed selectors name it bare",
            novafile_text::quote(&f.name)
        ));
    }

    Ok(())
}

/// Check that `f` repeats neither the name nor the id of a field in `before`.
///
/// # Errors
///
/// Returns a message that names the repeated name or id.
fn check_unique_field(before: &[FieldDef], f: &FieldDef) -> Result<(), String> {
    if before.iter().any(|g| g.name == f.name) {
        return Err(format!("duplicate field {:?}", f.name));
    }

    if let Some(id) = f.id
        && before.iter().any(|g| g.id == Some(id))
    {
        return Err(format!("duplicate field id {id}"));
    }

    Ok(())
}

/// Check the rules the parser applies line by line to every definition of `s`.
///
/// Parsed schemas pass by construction. Derived schemas are built in code, so
/// they need this check to reject what the parser rejects.
///
/// # Errors
///
/// Returns a message that names the definition and the broken rule.
pub(crate) fn check_defs(s: &Schema) -> Result<(), String> {
    for (name, d) in &s.defs {
        match d {
            Def::Enum(vs) if vs.is_empty() => return Err(format!("enum {name} has no variants")),
            // Derived enums take their names from serde renames, unchecked by the parser.
            Def::Enum(vs) => {
                if let Some(e) = vs.iter().find_map(|v| novafile_value::variant_tag(&v.name).err()) {
                    return Err(format!("enum {name}: {e}"));
                }
            }
            Def::Record(r) => {
                for (i, f) in r.fields.iter().enumerate() {
                    check_unique_field(&r.fields[..i], f)
                        .and_then(|()| check_key_field(f))
                        .map_err(|m| format!("record {name}: {m}"))?;
                }

                check_rules(name, r)?;
            }
            Def::Alias(_) => {}
        }
    }

    Ok(())
}

/// If any record member of the union `t` has `@key`, every record member
/// must key on fields with the same names (spec §6.2), so a keyed selector
/// still identifies one element across members.
fn keyed_members_agree(s: &Schema, t: &TypeRef) -> Result<(), String> {
    fn keys(r: &crate::RecordDef) -> Vec<&str> {
        let mut k = r.key_fields();
        k.sort_unstable();
        k
    }

    let members = s.member_records(t);

    let Some((first, r)) = members.iter().find(|(_, r)| !r.key_fields().is_empty()) else {
        return Ok(());
    };
    let want = keys(r);
    match members.iter().find(|(_, m)| keys(m) != want) {
        Some((other, m)) => Err(format!(
            "union {t}: {first} has @key ({}) but {other} has {}; every record member of a keyed union keys on the same fields",
            want.join(", "),
            if m.key_fields().is_empty() {
                "no @key".to_owned()
            } else {
                format!("@key ({})", keys(m).join(", "))
            }
        )),
        None => Ok(()),
    }
}

/// Check that every named type and the `@root` refer to a definition and
/// that no alias reaches itself through only aliases and unions.
pub(crate) fn check_refs(s: &Schema) -> Result<(), String> {
    /// Checks that every named type reachable from `t` refers to a definition in `s`.
    fn walk(s: &Schema, t: &TypeRef) -> Result<(), String> {
        match &t.kind {
            TypeKind::Named(n) => s.def(n).map(|_| ()).ok_or_else(|| format!("unknown type {n}")),
            TypeKind::List(t) | TypeKind::Set(t) => walk(s, t),
            TypeKind::Map(k, v) => walk(s, k).and(walk(s, v)),
            TypeKind::Tuple(ts) => ts.iter().try_for_each(|t| walk(s, t)),
            TypeKind::Union(ms) => {
                ms.iter().try_for_each(|t| walk(s, t))?;
                keyed_members_agree(s, t)
            }
            _ => Ok(()),
        }
    }

    for (name, d) in &s.defs {
        match d {
            Def::Alias(t) => {
                walk(s, t)?;
                if let Some(n) = s.alias_cycle(name, t) {
                    return Err(format!("type alias cycle through {n}"));
                }
            }
            Def::Enum(vs) => vs.iter().filter_map(|v| v.payload.as_ref()).try_for_each(|t| walk(s, t))?,
            Def::Record(r) => r.fields.iter().try_for_each(|f| walk(s, &f.ty))?,
        }
    }

    if let Some(r) = &s.root {
        walk(s, r).map_err(|m| format!("@root: {m}"))?;
    }

    crate::class::check_positions(s)?;

    // Defaults must resolve against their field, or old data silently fails the
    // new schema once the default is filled in (spec §6.4 rule 2).
    for (name, d) in &s.defs {
        if let Def::Record(r) = d {
            for f in &r.fields {
                if let Some(v) = &f.default
                    && let Err(issues) = s.resolve_as(v, &f.ty)
                {
                    let why: Vec<String> = issues.iter().map(ToString::to_string).collect();
                    return Err(format!("{name}.{}: default does not match {}: {}", f.name, f.ty, why.join("; ")));
                }
            }
        }
    }

    Ok(())
}

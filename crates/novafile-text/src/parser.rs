//! Recursive-descent parser for NOVA text (spec §4) and templates (spec §9).

use crate::ast::{Cond, Directive, Document, Entry, ForHead, Import, Item, Key, Let, Node, Param, Part, TemplateHeader};
use crate::path::{Path, Segment, is_ident_char, is_ident_start};
use crate::spans::{Span, SpanKind};
use base64::Engine;
use novafile_value::{Decimal, Duration, Rational, Timestamp, Value};
use std::fmt;

/// Maximum nesting depth of values; deeper input is rejected to bound recursion.
pub const MAX_DEPTH: usize = 128;

/// A syntax error with its source position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line.
    pub line: usize,
    /// 1-based column, counted in characters.
    pub col: usize,
    /// Byte offset into the source.
    pub offset: usize,
    /// Human-readable description; advice, if any, follows a `; `.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Result type of the parser.
type R<T> = Result<T, ParseError>;

/// Characters that may not begin a bare word or plain scalar (spec §4.4).
pub(crate) const RESERVED_START: &[u8] = b"!\"#$%&'*,:<=>?@[\\]^`{|}~()";
/// Bytes that end a bare word in flow context.
const FLOW_DELIMS: &[u8] = b",[]{}()\"";

/// The kind of number a token spells.
pub(crate) enum NumKind {
    /// An integer.
    Int,
    /// A float: has a fraction or an exponent.
    Float,
    /// A rational `p/q`.
    Rational,
}

/// Strict number grammar (spec §4.5).
pub(crate) fn number_kind(s: &str) -> Option<NumKind> {
    let b = s.as_bytes();
    let mut i = 0;
    if b.first() == Some(&b'-') {
        i = 1;
    }

    match b.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
        }
        _ => return None,
    }

    if b.get(i) == Some(&b'/') {
        // Rational: INT '/' positive integer without leading zeros.
        i += 1;
        match b.get(i) {
            Some(b'1'..=b'9') => {
                while b.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
            }
            _ => return None,
        }

        return (i == b.len()).then_some(NumKind::Rational);
    }

    let mut float = false;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let s = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }

        if i == s {
            return None;
        }

        float = true;
    }

    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }

        let s = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }

        if i == s {
            return None;
        }

        float = true;
    }

    (i == b.len()).then_some(if float { NumKind::Float } else { NumKind::Int })
}

/// Classify a bare token: keyword, number, or string (spec §4.4).
pub(crate) fn classify(tok: &str) -> Result<Value, String> {
    match tok {
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        "null" => return Ok(Value::Null),
        _ => {}
    }

    match number_kind(tok) {
        Some(NumKind::Int) => tok.parse::<i128>().map(Value::Int).map_err(|_| format!("integer {tok} is out of range")),
        Some(NumKind::Rational) => Rational::parse(tok).map(Value::Rational),
        Some(NumKind::Float) => {
            let f: f64 = tok.parse().map_err(|_| format!("invalid float {tok}"))?;
            if f.is_infinite() {
                return Err(format!("float {tok} is out of range; use @float(\"inf\")"));
            }

            Ok(Value::Float(f))
        }
        None => Ok(Value::String(tok.to_owned())),
    }
}

/// Build the value of a scalar typed literal `@name("arg")` (`@set` and `@map` are parsed
/// separately). Fails on an unknown name or an invalid argument.
pub(crate) fn typed_literal(name: &str, arg: &str) -> Result<Value, String> {
    Ok(match name {
        "timestamp" => Value::Timestamp(Timestamp::parse(arg)?),
        "duration" => Value::Duration(Duration::parse(arg)?),
        "decimal" => Value::Decimal(Decimal::parse(arg)?),
        "rational" => Value::Rational(Rational::parse(arg)?),
        "bytes" => Value::Bytes(
            base64::engine::general_purpose::STANDARD
                .decode(arg)
                .map_err(|e| format!("invalid base64 in @bytes: {e}"))?,
        ),
        "float" => Value::Float(match arg {
            "nan" => f64::NAN,
            "inf" => f64::INFINITY,
            "-inf" => f64::NEG_INFINITY,
            other => match number_kind(other) {
                Some(NumKind::Int | NumKind::Float) => other.parse().map_err(|_| format!("invalid float {other:?}"))?,
                _ => return Err(format!("invalid @float argument {other:?}")),
            },
        }),
        other => return Err(format!("unknown typed literal @{other}")),
    })
}

/// Whether an indented block is a list or a record.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    /// A block list (`- ` items or a table).
    List,
    /// A block record (`key: value` lines).
    Record,
}

/// Recursive-descent parser over a source string; the cursor is a byte offset.
pub struct Parser<'a> {
    /// The source text.
    pub(crate) src: &'a str,
    /// `src` as bytes, for byte-wise scanning.
    b: &'a [u8],
    /// Cursor: byte offset of the next unread byte.
    pub pos: usize,
    /// Whether template constructs (`$ref`, `...`, `${}`, `@if`, `@for`) are accepted; set by `@template`.
    pub template: bool,
    /// Current nesting depth, checked against [`MAX_DEPTH`].
    depth: usize,
    /// Recorded value spans, or `None` when span recording is off.
    spans: Option<Vec<Span>>,
    /// Byte ranges of comments seen (recorded only with spans).
    comments: Vec<(usize, usize)>,
    /// Number of flow brackets currently open around the cursor.
    bracket_depth: usize,
    /// Path of the value being parsed, for span recording.
    cur: Vec<Segment>,
    /// End offset of the most recently parsed value, excluding trailing whitespace and comments.
    last_end: usize,
}

impl<'a> Parser<'a> {
    /// Create a parser at the start of `src`; `template` enables template constructs from the outset.
    #[must_use]
    pub fn new(src: &'a str, template: bool) -> Self {
        Parser {
            src,
            b: src.as_bytes(),
            pos: 0,
            template,
            depth: 0,
            spans: None,
            comments: Vec::new(),
            bracket_depth: 0,
            cur: Vec::new(),
            last_end: 0,
        }
    }

    /// Record the source span of every value (see [`crate::spans`]).
    #[must_use]
    pub fn with_spans(mut self) -> Self {
        self.spans = Some(Vec::new());
        self
    }

    /// Take the spans recorded so far (empty unless created [`with_spans`](Self::with_spans)).
    pub fn take_spans(&mut self) -> Vec<Span> {
        self.spans.take().unwrap_or_default()
    }

    /// Byte ranges of all comments seen (recorded only with [`Parser::with_spans`]),
    /// sorted and de-duplicated.
    pub fn take_comments(&mut self) -> Vec<(usize, usize)> {
        let mut c = std::mem::take(&mut self.comments);
        c.sort_unstable();
        c.dedup();
        c
    }

    /// Skip a `#` comment to the end of the line, recording it.
    fn skip_comment(&mut self) {
        let start = self.pos;
        while !matches!(self.peek(), None | Some(b'\n')) {
            self.pos += 1;
        }

        if self.spans.is_some() {
            let end = start + self.src[start..self.pos].trim_end_matches('\r').len();
            self.comments.push((start, end));
        }
    }

    /// Run `f` with `seg` appended to the current path and record the span of what it parsed.
    fn spanned<T>(&mut self, kind: SpanKind, seg: Segment, start: usize, key: Option<(usize, usize)>, f: impl FnOnce(&mut Self) -> R<T>) -> R<T> {
        self.cur.push(seg);
        let r = f(self);
        if r.is_ok()
            && !self.template
            && let Some(spans) = self.spans.as_mut()
        {
            spans.push(Span {
                path: Path(self.cur.clone()),
                start,
                end: self.last_end.max(start),
                key,
                kind,
            });
        }

        self.cur.pop();
        r
    }

    /// Build a [`ParseError`] for byte offset `pos` (clamped to the source length).
    pub fn error_at(&self, pos: usize, msg: impl Into<String>) -> ParseError {
        let pos = pos.min(self.src.len());
        let before = &self.src[..pos];
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        ParseError {
            line,
            col,
            offset: pos,
            message: msg.into(),
        }
    }

    /// Fail at the cursor with `msg`.
    fn err<T>(&self, msg: impl Into<String>) -> R<T> {
        Err(self.error_at(self.pos, msg))
    }

    /// The byte at the cursor.
    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    /// The byte `n` bytes past the cursor.
    fn peek_at(&self, n: usize) -> Option<u8> {
        self.b.get(self.pos + n).copied()
    }

    /// Whether the source at the cursor starts with `s`.
    fn starts_with(&self, s: &str) -> bool {
        self.src[self.pos..].starts_with(s)
    }

    /// The byte at the cursor, or `None` at the end of input.
    #[must_use]
    pub fn peek_byte(&self) -> Option<u8> {
        self.peek()
    }

    /// Whether the cursor is at the end of the source.
    #[must_use]
    pub fn at_eof(&self) -> bool {
        self.pos >= self.b.len()
    }

    /// Whether the cursor is at a line break or the end of input.
    fn at_eol(&self) -> bool {
        matches!(self.peek(), None | Some(b'\n' | b'\r'))
    }

    /// Whether the cursor is at a line break, the end of input or a `#` comment.
    fn at_eol_or_comment(&self) -> bool {
        self.at_eol() || self.peek() == Some(b'#')
    }

    /// Skip spaces and tabs on the current line.
    pub fn skip_inline_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    /// Skip whitespace, newlines and comments (inside brackets).
    pub fn skip_ws_multi(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.pos += 1,
                Some(b'#') => self.skip_comment(),
                _ => return,
            }
        }
    }

    /// Consume optional trailing whitespace/comment and the line break.
    ///
    /// # Errors
    ///
    /// Returns a [`ParseError`] if anything other than whitespace or a comment
    /// precedes the line break.
    pub fn end_line(&mut self) -> R<()> {
        self.skip_inline_ws();
        if self.peek() == Some(b'#') {
            self.skip_comment();
        }

        if self.peek() == Some(b'\r') && self.peek_at(1) == Some(b'\n') {
            self.pos += 1;
        }

        match self.peek() {
            None => Ok(()),
            Some(b'\n') => {
                self.pos += 1;
                Ok(())
            }
            _ => self.err("unexpected content after value"),
        }
    }

    /// From a line start, skip lines that are blank or comment-only.
    pub fn skip_blank_lines(&mut self) {
        loop {
            let save = self.pos;
            self.skip_inline_ws();
            if self.peek() == Some(b'#') {
                self.skip_comment();
            }

            if self.peek() == Some(b'\r') {
                self.pos += 1;
            }

            match self.peek() {
                Some(b'\n') => self.pos += 1,
                None => return,
                _ => {
                    self.pos = save;
                    return;
                }
            }
        }
    }

    /// Indentation of the line starting at `pos` (spaces only).
    fn line_indent(&self) -> R<Option<usize>> {
        if self.at_eof() {
            return Ok(None);
        }

        let n = self.b[self.pos..].iter().take_while(|&&c| c == b' ').count();
        if self.b.get(self.pos + n) == Some(&b'\t') {
            return Err(self.error_at(self.pos + n, "tabs are not allowed in indentation"));
        }

        Ok(Some(n))
    }

    /// Parse an identifier at the cursor.
    fn ident(&mut self) -> R<String> {
        if !self.peek().is_some_and(is_ident_start) {
            return self.err("expected an identifier");
        }

        let start = self.pos;
        while self.peek().is_some_and(is_ident_char) {
            self.pos += 1;
        }

        Ok(self.src[start..self.pos].to_owned())
    }

    /// Consume byte `c`, or fail with "expected `what`".
    fn expect(&mut self, c: u8, what: &str) -> R<()> {
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            self.err(format!("expected {what}"))
        }
    }

    // ------------------------------------------------------------------
    // Strings
    // ------------------------------------------------------------------

    /// Parse a double-quoted string at `pos` into parts (interpolation only in templates).
    fn parse_quoted_parts(&mut self, allow_interp: bool) -> R<Vec<Part>> {
        let start = self.pos;
        self.expect(b'"', "`\"`")?;
        let mut parts = Vec::new();
        let mut buf = String::new();
        loop {
            let rest = &self.src[self.pos..];
            let Some(c) = rest.chars().next() else {
                return Err(self.error_at(start, "unterminated string"));
            };

            match c {
                '"' => {
                    self.pos += 1;
                    break;
                }
                '\\' => {
                    self.pos += 1;
                    let e = self.peek().ok_or_else(|| self.error_at(start, "unterminated string"))?;
                    self.pos += 1;
                    match e {
                        b'"' => buf.push('"'),
                        b'\\' => buf.push('\\'),
                        b'/' => buf.push('/'),
                        b'$' => buf.push('$'),
                        b'b' => buf.push('\u{8}'),
                        b'f' => buf.push('\u{c}'),
                        b'n' => buf.push('\n'),
                        b'r' => buf.push('\r'),
                        b't' => buf.push('\t'),
                        b'u' => buf.push(self.unicode_escape()?),
                        _ => return Err(self.error_at(self.pos - 2, format!("invalid escape \\{}", e as char))),
                    }
                }
                c if (c as u32) < 0x20 || c == '\u{7f}' => {
                    return self.err("control character in quoted string; use an escape such as \\n");
                }
                '$' if allow_interp && rest.starts_with("$${") => {
                    buf.push_str("${");
                    self.pos += 3;
                }
                '$' if allow_interp && rest.starts_with("${") => {
                    self.pos += 2;
                    if !buf.is_empty() {
                        parts.push(Part::Text(std::mem::take(&mut buf)));
                    }

                    parts.push(Part::Ref(self.interp_ref()?));
                }
                c => {
                    buf.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }

        if !buf.is_empty() || parts.is_empty() {
            parts.push(Part::Text(buf));
        }

        Ok(parts)
    }

    /// Parse the path inside `${...}` (cursor after `${`) through the closing `}`.
    fn interp_ref(&mut self) -> R<Path> {
        self.skip_inline_ws();
        let (p, n) = Path::parse_prefix(&self.src[self.pos..]).map_err(|m| self.error_at(self.pos, m))?;
        if p.is_root() {
            return self.err("expected a reference inside ${...}");
        }

        self.pos += n;
        self.skip_inline_ws();
        self.expect(b'}', "`}` to close interpolation")?;
        Ok(p)
    }

    /// Parse the rest of a `\u` escape (cursor after `u`): `{hex}` with 1 to 6 digits, or four hex
    /// digits, where a high surrogate must be followed by a `\u` low surrogate.
    fn unicode_escape(&mut self) -> R<char> {
        if self.peek() == Some(b'{') {
            self.pos += 1;
            let s = self.pos;
            while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                self.pos += 1;
            }

            let hex = &self.src[s..self.pos];
            self.expect(b'}', "`}` in \\u{...}")?;
            return u32::from_str_radix(hex, 16)
                .ok()
                .filter(|_| (1..=6).contains(&hex.len()))
                .and_then(char::from_u32)
                .ok_or_else(|| self.error_at(s, "invalid \\u{...} escape"));
        }

        let read4 = |p: &mut Self| -> R<u32> {
            let h = p.src.get(p.pos..p.pos + 4).filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()));
            match h {
                Some(h) => {
                    p.pos += 4;
                    Ok(u32::from_str_radix(h, 16).unwrap())
                }
                None => p.err("invalid \\u escape"),
            }
        };

        let hi = read4(self)?;
        if (0xD800..0xDC00).contains(&hi) {
            if self.starts_with("\\u") {
                self.pos += 2;
                let lo = read4(self)?;
                if (0xDC00..0xE000).contains(&lo) {
                    return Ok(char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)).unwrap());
                }
            }

            return self.err("unpaired surrogate in \\u escape");
        }

        char::from_u32(hi).ok_or_else(|| self.error_at(self.pos, "invalid \\u escape"))
    }

    /// Parse a quoted string literal (no interpolation) at the cursor.
    ///
    /// # Errors
    ///
    /// Returns a [`ParseError`] if the cursor is not at a well-formed string literal.
    pub fn parse_quoted_literal(&mut self) -> R<String> {
        match self.parse_quoted_parts(false)?.pop() {
            Some(Part::Text(s)) => Ok(s),
            _ => unreachable!(),
        }
    }

    /// Parse a quoted string as a literal, or as [`Node::Interp`] if it interpolates `${...}` (templates only).
    fn quoted_node(&mut self) -> R<Node> {
        let mut parts = self.parse_quoted_parts(self.template)?;
        Ok(match (parts.len(), parts.first()) {
            (1, Some(Part::Text(_))) => match parts.pop() {
                Some(Part::Text(s)) => Node::Lit(Value::String(s)),
                _ => unreachable!(),
            },
            _ => Node::Interp(parts),
        })
    }

    // ------------------------------------------------------------------
    // Flow values
    // ------------------------------------------------------------------

    /// Parse a flow value and convert it to a value (no template constructs).
    ///
    /// # Errors
    ///
    /// Returns a [`ParseError`] if the flow value is malformed or contains a
    /// template construct.
    pub fn parse_flow_value(&mut self) -> R<Value> {
        let start = self.pos;
        let n = self.parse_flow()?;
        n.to_value().map_err(|m| self.error_at(start, m))
    }

    /// Parse a flow value (possibly containing template constructs) at the cursor.
    ///
    /// # Errors
    ///
    /// Returns a [`ParseError`] if the value is malformed or nested too deeply.
    pub fn parse_flow(&mut self) -> R<Node> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.err("nesting depth limit exceeded");
        }

        let r = self.parse_flow_inner();
        self.depth -= 1;
        if r.is_ok() {
            self.last_end = self.pos;
        }

        r
    }

    /// Dispatch on the first byte of a flow value; [`parse_flow`](Self::parse_flow) wraps it with depth tracking.
    fn parse_flow_inner(&mut self) -> R<Node> {
        match self.peek() {
            None => self.err("expected a value"),
            Some(b'"') => self.quoted_node(),
            Some(b'[') => self.bracketed(|p| {
                p.pos += 1;
                Ok(Node::List(p.flow_items(b']')?))
            }),
            Some(b'{') => self.bracketed(|p| {
                p.pos += 1;
                p.flow_record()
            }),
            Some(b'(') => self.bracketed(|p| {
                p.pos += 1;
                p.flow_tuple()
            }),
            Some(b'@') => self.bracketed(Parser::typed),
            Some(b'!') => self.variant_flow(),
            Some(b'$') => self.reference(),
            _ => self.word(),
        }
    }

    /// Parse a bracketed flow value. For the outermost bracket, check that
    /// continuation lines are indented deeper than the line the value starts
    /// on (closing brackets and comment lines excepted, spec §4.7), so that a
    /// `---` line at column 1 can only ever separate documents (§4.11).
    fn bracketed(&mut self, f: impl FnOnce(&mut Self) -> R<Node>) -> R<Node> {
        let outer = self.bracket_depth == 0;
        let start = self.pos;
        self.bracket_depth += 1;
        let r = f(self);
        self.bracket_depth -= 1;
        let node = r?;
        if outer {
            let ls = self.src[..start].rfind('\n').map_or(0, |i| i + 1);
            let base = self.src[ls..].bytes().take_while(|&c| c == b' ').count();
            let mut off = self.src[start..self.pos].find('\n').map(|i| start + i + 1);
            while let Some(line) = off.filter(|&o| o < self.pos) {
                let end = self.src[line..].find('\n').map_or(self.src.len(), |i| line + i);
                let text = &self.src[line..end];
                let ind = text.bytes().take_while(|&c| c == b' ').count();
                let first = text.trim_start().as_bytes().first().copied();
                if !matches!(first, None | Some(b'#' | b']' | b'}' | b')' | b'\r')) && ind <= base {
                    return Err(self.error_at(
                        line + ind,
                        "continuation lines of a multi-line value must be indented deeper than the line it starts on",
                    ));
                }

                off = (end < self.src.len()).then_some(end + 1);
            }
        }

        Ok(node)
    }

    /// Parse a `$path` reference (templates only).
    fn reference(&mut self) -> R<Node> {
        if !self.template {
            return self.err("`$` is reserved for template references; quote the string or add @template");
        }

        self.pos += 1;
        let (p, n) = Path::parse_prefix(&self.src[self.pos..]).map_err(|m| self.error_at(self.pos, m))?;
        if p.is_root() {
            return self.err("expected a name after `$`");
        }

        self.pos += n;
        Ok(Node::Ref(p))
    }

    /// Parse comma-separated items up to `close` (cursor after the opening bracket); a trailing comma
    /// is allowed.
    fn flow_items(&mut self, close: u8) -> R<Vec<Item>> {
        let mut items = Vec::new();
        loop {
            self.skip_ws_multi();
            if self.peek() == Some(close) {
                self.pos += 1;
                return Ok(items);
            }

            if self.starts_with("...") {
                if !self.template {
                    return self.err("`...` spreads require @template");
                }

                self.pos += 3;
                items.push(Item::Spread(self.parse_flow()?));
            } else {
                #[expect(clippy::cast_possible_wrap, reason = "in-memory indices/lengths are at most isize::MAX, which fits in i64")]
                let (i, start) = (items.len() as i64, self.pos);
                let n = self.spanned(SpanKind::FlowItem, Segment::Index(i), start, None, Parser::parse_flow)?;
                items.push(Item::Node(n));
            }

            self.skip_ws_multi();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(c) if c == close => {
                    self.pos += 1;
                    return Ok(items);
                }
                _ => return self.err(format!("expected `,` or `{}`", close as char)),
            }
        }
    }

    /// Parse a record key: an identifier or a quoted string (interpolated in templates).
    fn key(&mut self) -> R<Key> {
        match self.peek() {
            Some(b'"') => {
                let mut parts = self.parse_quoted_parts(self.template)?;
                Ok(match (parts.len(), parts.first()) {
                    (1, Some(Part::Text(_))) => match parts.pop() {
                        Some(Part::Text(s)) => Key::Lit(s),
                        _ => unreachable!(),
                    },
                    _ => Key::Interp(parts),
                })
            }
            Some(c) if is_ident_start(c) => Ok(Key::Lit(self.ident()?)),
            _ => self.err("expected a key (identifier or quoted string)"),
        }
    }

    /// Parse `{...}` entries (cursor after `{`), rejecting duplicate literal keys.
    fn flow_record(&mut self) -> R<Node> {
        let mut entries: Vec<Entry> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        loop {
            self.skip_ws_multi();
            if self.peek() == Some(b'}') {
                self.pos += 1;
                return Ok(Node::Record(entries));
            }

            if self.starts_with("...") {
                if !self.template {
                    return self.err("`...` spreads require @template");
                }

                self.pos += 3;
                entries.push(Entry::Spread(self.parse_flow()?));
            } else {
                let kpos = self.pos;
                let k = self.key()?;
                let kend = self.pos;
                self.skip_ws_multi();
                self.expect(b':', "`:` after key")?;
                self.skip_ws_multi();
                let v = self.spanned(SpanKind::FlowEntry, seg_of(&k), kpos, Some((kpos, kend)), Parser::parse_flow)?;
                self.push_entry(&mut entries, &mut seen, k, v, kpos)?;
            }

            self.skip_ws_multi();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Node::Record(entries));
                }
                _ => return self.err("expected `,` or `}`"),
            }
        }
    }

    /// Append a field, failing at `kpos` if its literal key was already seen in this record.
    fn push_entry(&self, entries: &mut Vec<Entry>, seen: &mut std::collections::HashSet<String>, k: Key, v: Node, kpos: usize) -> R<()> {
        if let Key::Lit(name) = &k
            && !seen.insert(name.clone())
        {
            return Err(self.error_at(kpos, format!("duplicate key {name:?}")));
        }

        entries.push(Entry::Field(k, v));
        Ok(())
    }

    /// Parse `(...)` elements (cursor after `(`); a single element needs a trailing comma.
    fn flow_tuple(&mut self) -> R<Node> {
        let open = self.pos - 1;
        let mut items = Vec::new();
        let mut commas = 0;

        loop {
            self.skip_ws_multi();
            if self.peek() == Some(b')') {
                self.pos += 1;
                break;
            }

            #[expect(clippy::cast_possible_wrap, reason = "in-memory indices/lengths are at most isize::MAX, which fits in i64")]
            let (i, start) = (items.len() as i64, self.pos);
            items.push(self.spanned(SpanKind::FlowItem, Segment::Index(i), start, None, Parser::parse_flow)?);
            self.skip_ws_multi();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    commas += 1;
                }
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                _ => return self.err("expected `,` or `)`"),
            }
        }

        if items.len() == 1 && commas == 0 {
            return Err(self.error_at(open, "`(x)` is not a tuple; write `(x,)` for a 1-tuple"));
        }

        Ok(Node::Tuple(items))
    }

    /// Parse a typed value starting at `@`: `@set[...]`, `@map{...}` or a scalar `@name("arg")`.
    fn typed(&mut self) -> R<Node> {
        let start = self.pos;
        self.pos += 1;
        let name = self.ident()?;
        match name.as_str() {
            "set" => {
                self.expect(b'[', "`[` after @set")?;
                let items = self.flow_items(b']')?;
                let nodes = items
                    .into_iter()
                    .map(|i| match i {
                        Item::Node(n) => Ok(n),
                        _ => Err(self.error_at(start, "spreads are not allowed in @set")),
                    })
                    .collect::<R<Vec<_>>>()?;
                Ok(Node::Set(nodes))
            }
            "map" => {
                self.expect(b'{', "`{` after @map")?;
                let mut entries = Vec::new();
                loop {
                    self.skip_ws_multi();
                    if self.peek() == Some(b'}') {
                        self.pos += 1;
                        break;
                    }

                    let k = self.parse_flow()?;
                    self.skip_ws_multi();
                    if !self.starts_with("=>") {
                        return self.err("expected `=>` in @map entry");
                    }

                    self.pos += 2;
                    self.skip_ws_multi();
                    let v = self.parse_flow()?;
                    entries.push((k, v));
                    self.skip_ws_multi();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b'}') => {
                            self.pos += 1;
                            break;
                        }
                        _ => return self.err("expected `,` or `}`"),
                    }
                }

                Ok(Node::Map(entries))
            }
            _ => {
                self.expect(b'(', &format!("`(` after @{name}"))?;
                self.skip_ws_multi();
                let arg = self.parse_quoted_literal()?;
                self.skip_ws_multi();
                self.expect(b')', "`)`")?;
                typed_literal(&name, &arg).map(Node::Lit).map_err(|m| self.error_at(start, m))
            }
        }
    }

    /// Parse `!tag` with an optional `(payload)`.
    fn variant_flow(&mut self) -> R<Node> {
        self.pos += 1;
        let tag = self.ident()?;
        if self.peek() == Some(b'(') {
            self.pos += 1;
            self.skip_ws_multi();
            let p = self.parse_flow()?;
            self.skip_ws_multi();
            self.expect(b')', "`)` after variant payload")?;
            return Ok(Node::Variant(tag, Some(Box::new(p))));
        }

        Ok(Node::Variant(tag, None))
    }

    /// Parse a bare flow word and classify it as keyword, number or string. A `:` ends the word only
    /// when followed by whitespace, a delimiter or the end of input.
    fn word(&mut self) -> R<Node> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() || FLOW_DELIMS.contains(&c) {
                break;
            }

            if c == b':' {
                match self.peek_at(1) {
                    None => break,
                    Some(n) if n.is_ascii_whitespace() || FLOW_DELIMS.contains(&n) => break,
                    _ => {}
                }
            }

            self.pos += 1;
        }

        let tok = &self.src[start..self.pos];
        if tok.is_empty() {
            return self.err("expected a value");
        }

        if RESERVED_START.contains(&tok.as_bytes()[0]) || tok.starts_with("...") {
            return Err(self.error_at(start, format!("unexpected `{}`; quote the string", tok.chars().next().unwrap())));
        }

        classify(tok).map(Node::Lit).map_err(|m| self.error_at(start, m))
    }

    // ------------------------------------------------------------------
    // Block structure
    // ------------------------------------------------------------------

    /// Does the text at `pos` look like `key:` / `key[N]{..}:` followed by space or EOL?
    fn looks_like_key(&self) -> bool {
        let b = self.b;
        let mut p = self.pos;
        match b.get(p) {
            Some(b'"') => {
                p += 1;
                loop {
                    match b.get(p) {
                        None | Some(b'\n') => return false,
                        Some(b'\\') => p += 2,
                        Some(b'"') => {
                            p += 1;
                            break;
                        }
                        _ => p += 1,
                    }
                }
            }
            Some(&c) if is_ident_start(c) => {
                while b.get(p).is_some_and(|&c| is_ident_char(c)) {
                    p += 1;
                }
            }
            _ => return false,
        }

        if b.get(p) == Some(&b'[') && b.get(p + 1).is_some_and(u8::is_ascii_digit) {
            while !matches!(b.get(p), None | Some(b'\n' | b'}')) {
                p += 1;
            }

            if b.get(p) == Some(&b'}') {
                p += 1;
            }
        }

        b.get(p) == Some(&b':') && matches!(b.get(p + 1), None | Some(b' ' | b'\t' | b'\r' | b'\n' | b'#'))
    }

    /// Whether the cursor is at a `-` list marker (followed by a space, a line break or the end of input).
    fn is_list_marker(&self) -> bool {
        self.peek() == Some(b'-') && matches!(self.peek_at(1), None | Some(b' ' | b'\r' | b'\n'))
    }

    /// The template control keyword (`@if `, `@for ` or `@else`) at the cursor, if any.
    fn control_keyword(&self) -> Option<&'static str> {
        if !self.template {
            return None;
        }

        ["@if ", "@for ", "@else"].into_iter().find(|k| self.starts_with(k))
    }

    /// Decide whether the block starting at `pos` is a list or a record by
    /// scanning past template control lines.
    fn peek_block_kind(&self) -> BlockKind {
        for line in self.src[self.pos..].lines() {
            let t = line.trim_start();
            if t.is_empty() || t.starts_with('#') || (self.template && (t.starts_with("@if ") || t.starts_with("@for ") || t.starts_with("@else"))) {
                continue;
            }

            if t.starts_with("...") {
                continue;
            }

            if t == "-" || t.starts_with("- ") || (t.starts_with('[') && t.as_bytes().get(1).is_some_and(u8::is_ascii_digit)) {
                return BlockKind::List;
            }

            return BlockKind::Record;
        }

        BlockKind::Record
    }

    /// Enter a nested block whose lines must be indented exactly `indent`.
    fn nested_block(&mut self, indent: usize, what: &str) -> R<Node> {
        self.skip_blank_lines();
        match self.line_indent()? {
            Some(i) if i == indent => {}
            Some(i) if i > indent => return Err(self.error_at(self.pos + i, format!("indentation must be exactly {indent} spaces (2 deeper than the parent)"))),
            _ => return self.err(format!("expected an indented block for {what}; write `null`, `{{}}` or `[]` explicitly")),
        }

        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.err("nesting depth limit exceeded");
        }

        let r = match self.peek_block_kind() {
            BlockKind::List => self.block_list(indent).map(Node::List),
            BlockKind::Record => self.block_record(indent, false).map(Node::Record),
        };

        self.depth -= 1;
        r
    }

    /// Position the cursor at the next line of this block, or return false
    /// if the block has ended.
    fn next_block_line(&mut self, indent: usize) -> R<bool> {
        self.skip_blank_lines();
        match self.line_indent()? {
            None => Ok(false),
            Some(i) if i < indent => Ok(false),
            Some(i) if i > indent => Err(self.error_at(self.pos + i, "unexpected indentation")),
            Some(i) => {
                self.pos += i;
                Ok(true)
            }
        }
    }

    /// Parse block record lines at `indent`. With `inline_first`, the first entry starts at the
    /// cursor (after the `- ` of a list item). Stops at a dedent, or before a list marker or an `@else`
    /// at this indentation.
    fn block_record(&mut self, indent: usize, mut inline_first: bool) -> R<Vec<Entry>> {
        let mut entries: Vec<Entry> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        loop {
            let line_start = self.pos;
            if !inline_first && !self.next_block_line(indent)? {
                break;
            }

            inline_first = false;
            if self.is_list_marker() {
                // A list item at this indentation ends the record (record inside list item).
                self.pos = line_start;
                break;
            }

            if let Some(kw) = self.control_keyword() {
                if kw == "@else" {
                    // Belongs to an enclosing @if; let the caller see it.
                    self.pos = line_start;
                    break;
                }

                let e = if kw == "@if " {
                    self.if_block(indent, BlockKind::Record)?
                } else {
                    self.for_block(indent, BlockKind::Record)?
                };

                match e {
                    Ctl::Entry(e) => entries.push(e),
                    Ctl::Item(_) => unreachable!(),
                }

                continue;
            }

            if self.starts_with("...") {
                if !self.template {
                    return self.err("`...` spreads require @template");
                }

                self.pos += 3;
                let n = self.parse_flow()?;
                self.end_line()?;
                entries.push(Entry::Spread(n));
                continue;
            }

            if !self.looks_like_key() {
                return self.err("expected `key: value`");
            }

            let kpos = self.pos;
            let key = self.key()?;
            let kend = self.pos;
            let value = self.spanned(SpanKind::BlockEntry, seg_of(&key), kpos, Some((kpos, kend)), |p| {
                if p.peek() == Some(b'[') {
                    let (n, fields) = p.table_header()?;
                    p.expect(b':', "`:` after table header")?;
                    p.end_line()?;
                    p.table_rows(indent + 2, n, &fields)
                } else {
                    p.expect(b':', "`:` after key")?;
                    p.block_value(indent)
                }
            })?;
            self.push_entry(&mut entries, &mut seen, key, value, kpos)?;
        }

        Ok(entries)
    }

    /// Parse `- ` items at `indent`; stops at a dedent or before an `@else` at this indentation.
    fn block_list(&mut self, indent: usize) -> R<Vec<Item>> {
        let mut items = Vec::new();
        let mut node_count: i64 = 0;

        loop {
            let line_start = self.pos;
            if !self.next_block_line(indent)? {
                break;
            }

            if let Some(kw) = self.control_keyword() {
                if kw == "@else" {
                    self.pos = line_start;
                    break;
                }

                let c = if kw == "@if " {
                    self.if_block(indent, BlockKind::List)?
                } else {
                    self.for_block(indent, BlockKind::List)?
                };

                match c {
                    Ctl::Item(i) => items.push(i),
                    Ctl::Entry(_) => unreachable!(),
                }

                continue;
            }

            if self.starts_with("...") {
                if !self.template {
                    return self.err("`...` spreads require @template");
                }

                self.pos += 3;
                let n = self.parse_flow()?;
                self.end_line()?;
                items.push(Item::Spread(n));
                continue;
            }

            if !self.is_list_marker() {
                return self.err("expected a list item `- ...`");
            }

            let start = self.pos;
            self.pos += 1;
            self.skip_inline_ws();
            if self.is_list_marker() {
                return self.err("nested list items must start on their own line (`-` then an indented block) or use [flow]");
            }

            let idx = node_count;
            node_count += 1;
            let node = self.spanned(SpanKind::BlockItem, Segment::Index(idx), start, None, |p| {
                if p.looks_like_key() || (p.template && p.starts_with("...")) {
                    Ok(Node::Record(p.block_record(indent + 2, true)?))
                } else {
                    p.block_value(indent)
                }
            })?;
            items.push(Item::Node(node));
        }

        Ok(items)
    }

    /// Parse the value after `key:` or `- ` whose parent line has `parent` indentation.
    fn block_value(&mut self, parent: usize) -> R<Node> {
        self.skip_inline_ws();
        if self.at_eol_or_comment() {
            self.end_line()?;
            return self.nested_block(parent + 2, "this key");
        }

        match self.peek() {
            Some(b'|') => self.block_string(parent),
            Some(b'!') => {
                let n = self.variant_flow()?;
                if let Node::Variant(tag, None) = &n {
                    self.skip_inline_ws();
                    if self.at_eol_or_comment() {
                        self.end_line()?;
                        // Block payload if the following content is indented deeper.
                        let save = self.pos;
                        self.skip_blank_lines();
                        if matches!(self.line_indent()?, Some(i) if i > parent) {
                            let p = self.nested_block(parent + 2, "the variant payload")?;
                            return Ok(Node::Variant(tag.clone(), Some(Box::new(p))));
                        }

                        self.pos = save;
                        return Ok(n);
                    }
                }

                self.end_line()?;
                Ok(n)
            }
            Some(b'"' | b'[' | b'{' | b'(' | b'@' | b'$') => {
                let n = self.parse_flow()?;
                self.end_line()?;
                Ok(n)
            }
            _ => self.plain_scalar(),
        }
    }

    /// Parse an unquoted scalar up to the end of the line (or a ` #` comment), rejecting text that
    /// would be ambiguous in block context.
    fn plain_scalar(&mut self) -> R<Node> {
        let start = self.pos;
        let line_end = self.src[start..].find('\n').map_or(self.src.len(), |i| start + i);
        let line = &self.src[start..line_end];
        // A comment starts at `#` preceded by whitespace.
        let mut cut = line.len();
        let lb = line.as_bytes();
        for i in 1..lb.len() {
            if lb[i] == b'#' && (lb[i - 1] == b' ' || lb[i - 1] == b'\t') {
                cut = i;
                break;
            }
        }

        let text = line[..cut].trim_end_matches([' ', '\t', '\r']);
        if cut < line.len() && self.spans.is_some() {
            self.comments.push((start + cut, start + line.trim_end_matches('\r').len()));
        }

        let first = text.as_bytes()[0];
        if RESERVED_START.contains(&first) {
            return self.err(format!("a plain value cannot start with `{}`; quote the string", first as char));
        }

        if text.starts_with("...") {
            return self.err("a plain value cannot start with `...`; quote the string");
        }

        if text == "-" || text.starts_with("- ") {
            return self.err("`- ` starts a list item, which must be on its own line; quote the string");
        }

        if let Some(i) = text.find(": ") {
            return Err(self.error_at(start + i, "`: ` inside a plain value is ambiguous; quote the string"));
        }

        if text.ends_with(':') {
            return self.err("a plain value cannot end with `:`; quote the string");
        }

        if text.chars().any(|c| c.is_control() && c != '\t') {
            return self.err("control character in plain value");
        }

        let v = classify(text).map_err(|m| self.error_at(start, m))?;
        self.last_end = start + text.len();
        self.pos = line_end;
        if self.peek() == Some(b'\n') {
            self.pos += 1;
        }

        Ok(Node::Lit(v))
    }

    /// Parse a `|` (keep the final newline) or `|-` (strip it) block string whose lines are indented
    /// `parent + 2`. Trailing blank lines are dropped.
    fn block_string(&mut self, parent: usize) -> R<Node> {
        self.pos += 1;
        let strip = self.peek() == Some(b'-');
        if strip {
            self.pos += 1;
        }

        self.end_line()?;
        let indent = parent + 2;
        let mut lines: Vec<&str> = Vec::new();

        while !self.at_eof() {
            let end = self.src[self.pos..].find('\n').map_or(self.src.len(), |i| self.pos + i);
            let raw = self.src[self.pos..end].strip_suffix('\r').unwrap_or(&self.src[self.pos..end]);
            let spaces = raw.bytes().take_while(|&c| c == b' ').count();
            if spaces == raw.len() {
                lines.push(if spaces >= indent { &raw[indent..] } else { "" });
            } else if spaces < indent {
                if raw.as_bytes()[spaces] == b'\t' && spaces < indent {
                    return self.err("tabs are not allowed in indentation");
                }

                break;
            } else {
                lines.push(&raw[indent..]);
                self.last_end = self.pos + raw.len();
            }

            self.pos = (end + 1).min(self.src.len());
        }

        while lines.last() == Some(&"") {
            lines.pop();
        }

        let mut s = lines.join("\n");
        if !strip && !lines.is_empty() {
            s.push('\n');
        }

        if self.template {
            // Re-parse for ${...} interpolation.
            return self.interpolate_block(&s);
        }

        Ok(Node::Lit(Value::String(s)))
    }

    /// Split a block string's text into literal and `${path}` parts (templates only); `$${` stands for a
    /// literal `${`.
    fn interpolate_block(&self, s: &str) -> R<Node> {
        let mut parts = Vec::new();
        let mut buf = String::new();
        let mut rest = s;
        while let Some(i) = rest.find('$') {
            buf.push_str(&rest[..i]);
            rest = &rest[i..];
            if rest.starts_with("$${") {
                buf.push_str("${");
                rest = &rest[3..];
            } else if let Some(r) = rest.strip_prefix("${") {
                let close = r.find('}').ok_or_else(|| self.error_at(self.pos, "unterminated ${ in block string"))?;
                let p = Path::parse(&r[..close]).map_err(|m| self.error_at(self.pos, m))?;
                if !buf.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut buf)));
                }

                parts.push(Part::Ref(p));
                rest = &r[close + 1..];
            } else {
                buf.push('$');
                rest = &rest[1..];
            }
        }

        buf.push_str(rest);
        if parts.is_empty() {
            return Ok(Node::Lit(Value::String(buf)));
        }

        if !buf.is_empty() {
            parts.push(Part::Text(buf));
        }

        Ok(Node::Interp(parts))
    }

    // ------------------------------------------------------------------
    // Tables
    // ------------------------------------------------------------------

    /// Parse a table header `[N]{col, ...}` (without the trailing `:`) into the row count and column names.
    fn table_header(&mut self) -> R<(usize, Vec<String>)> {
        self.expect(b'[', "`[`")?;
        let s = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }

        let n: usize = self.src[s..self.pos].parse().map_err(|_| self.error_at(s, "expected a row count"))?;
        self.expect(b']', "`]` after row count")?;
        self.expect(b'{', "`{` to start the column list")?;
        let mut fields: Vec<String> = Vec::new();
        loop {
            self.skip_inline_ws();
            let fpos = self.pos;
            let f = match self.key()? {
                Key::Lit(s) => s,
                Key::Interp(_) => return self.err("interpolation is not allowed in table headers"),
            };

            if fields.contains(&f) {
                return Err(self.error_at(fpos, format!("duplicate column {f:?}")));
            }

            fields.push(f);
            self.skip_inline_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return self.err("expected `,` or `}` in table header"),
            }
        }

        Ok((n, fields))
    }

    /// Parse exactly `n` table rows at `indent` into a list of records; an empty cell leaves its field
    /// absent.
    fn table_rows(&mut self, indent: usize, n: usize, fields: &[String]) -> R<Node> {
        // `n` is untrusted; each row takes at least one source byte, so cap the hint.
        let mut rows = Vec::with_capacity(n.min(self.src.len()));
        for r in 0..n {
            self.skip_blank_lines();
            match self.line_indent()? {
                Some(i) if i == indent => self.pos += i,
                _ => return self.err(format!("table declares {n} rows but only {r} were found (rows are indented {indent} spaces)")),
            }

            let row_start = self.pos;
            #[expect(
                clippy::cast_possible_wrap,
                reason = "`r` counts rows already parsed from the source, so it is far below i64::MAX"
            )]
            self.cur.push(Segment::Index(r as i64));
            let mut cells: Vec<Option<Node>> = Vec::new();
            loop {
                self.skip_inline_ws();
                if self.peek() == Some(b',') {
                    cells.push(None);
                    self.pos += 1;
                    continue;
                }

                if self.at_eol_or_comment() {
                    if !cells.is_empty() {
                        cells.push(None); // trailing empty cell after a comma
                    }

                    break;
                }

                let field = fields.get(cells.len()).cloned().unwrap_or_default();
                let start = self.pos;
                cells.push(Some(self.spanned(
                    SpanKind::TableCell,
                    Segment::Field(field),
                    start,
                    None,
                    Parser::parse_flow,
                )?));
                self.skip_inline_ws();
                match self.peek() {
                    Some(b',') => self.pos += 1,
                    _ if self.at_eol_or_comment() => break,
                    _ => return self.err("expected `,` between table cells"),
                }
            }

            self.end_line()?;
            if cells.len() != fields.len() {
                return self.err(format!("table row has {} cells; the header declares {}", cells.len(), fields.len()));
            }

            if !self.template
                && let Some(spans) = self.spans.as_mut()
            {
                spans.push(Span {
                    path: Path(self.cur.clone()),
                    start: row_start,
                    end: self.last_end.max(row_start),
                    key: None,
                    kind: SpanKind::TableRow,
                });
            }

            self.cur.pop();
            let entries = fields
                .iter()
                .zip(cells)
                .filter_map(|(f, c)| c.map(|c| Entry::Field(Key::Lit(f.clone()), c)))
                .collect();
            rows.push(Item::Node(Node::Record(entries)));
        }

        let save = self.pos;
        self.skip_blank_lines();
        if matches!(self.line_indent()?, Some(i) if i >= indent) {
            return self.err(format!("table declares {n} rows but more follow"));
        }

        self.pos = save;
        Ok(Node::List(rows))
    }

    // ------------------------------------------------------------------
    // Template control lines
    // ------------------------------------------------------------------

    /// Parse the body of a control line, which must be indented exactly `indent`.
    fn children(&mut self, indent: usize, kind: BlockKind) -> R<Ctl2> {
        self.skip_blank_lines();
        if !matches!(self.line_indent()?, Some(i) if i == indent) {
            return self.err(format!("expected a body indented {indent} spaces"));
        }

        Ok(match kind {
            BlockKind::Record => Ctl2::Entries(self.block_record(indent, false)?),
            BlockKind::List => Ctl2::Items(self.block_list(indent)?),
        })
    }

    /// Parse `@if cond:` (cursor at `@if `) with its body and an optional `@else:` or `@else if`
    /// at the same indentation.
    fn if_block(&mut self, indent: usize, kind: BlockKind) -> R<Ctl> {
        self.pos += 4; // "@if "
        let cond = self.cond()?;
        self.skip_inline_ws();
        self.expect(b':', "`:` after @if condition")?;
        self.end_line()?;
        let then = self.children(indent + 2, kind)?;
        // Optional @else / @else if at the same indentation.
        let save = self.pos;
        self.skip_blank_lines();
        let mut els = match kind {
            BlockKind::Record => Ctl2::Entries(Vec::new()),
            BlockKind::List => Ctl2::Items(Vec::new()),
        };

        if matches!(self.line_indent()?, Some(i) if i == indent) && self.src[self.pos + indent..].starts_with("@else") {
            self.pos += indent + 5;
            self.skip_inline_ws();
            if self.starts_with("if ") {
                // `@else if C:` is `@else:` containing a single `@if C:`.
                let at = self.pos - 1;
                let c = self.if_block_at(at, indent, kind)?;
                els = match c {
                    Ctl::Entry(e) => Ctl2::Entries(vec![e]),
                    Ctl::Item(i) => Ctl2::Items(vec![i]),
                };
            } else {
                self.expect(b':', "`:` after @else")?;
                self.end_line()?;
                els = self.children(indent + 2, kind)?;
            }
        } else {
            self.pos = save;
        }

        Ok(match (then, els) {
            (Ctl2::Entries(t), Ctl2::Entries(e)) => Ctl::Entry(Entry::If(Box::new(cond), t, e)),
            (Ctl2::Items(t), Ctl2::Items(e)) => Ctl::Item(Item::If(Box::new(cond), t, e)),
            _ => unreachable!(),
        })
    }

    /// `@else if` support: `pos` points 4 bytes before the condition.
    fn if_block_at(&mut self, at: usize, indent: usize, kind: BlockKind) -> R<Ctl> {
        self.pos = at;
        self.if_block(indent, kind)
    }

    /// Parse `@for [index,] var in expr:` (cursor at `@for `) with its body.
    fn for_block(&mut self, indent: usize, kind: BlockKind) -> R<Ctl> {
        self.pos += 5; // "@for "
        self.skip_inline_ws();
        let first = self.ident()?;
        self.skip_inline_ws();
        let (index, var) = if self.peek() == Some(b',') {
            self.pos += 1;
            self.skip_inline_ws();
            (Some(first), self.ident()?)
        } else {
            (None, first)
        };

        self.skip_inline_ws();
        if !self.starts_with("in ") {
            return self.err("expected `in` in @for");
        }

        self.pos += 3;
        self.skip_inline_ws();
        let iter = self.parse_flow()?;
        self.skip_inline_ws();
        self.expect(b':', "`:` after @for header")?;
        self.end_line()?;
        let head = ForHead { index, var, iter };
        Ok(match self.children(indent + 2, kind)? {
            Ctl2::Entries(e) => Ctl::Entry(Entry::For(head, e)),
            Ctl2::Items(i) => Ctl::Item(Item::For(head, i)),
        })
    }

    /// Whether the keyword `w` is at the cursor as a whole word.
    fn word_ahead(&self, w: &str) -> bool {
        self.starts_with(w) && !self.peek_at(w.len()).is_some_and(is_ident_char)
    }

    /// Parse a condition: `or`-joined `and` terms (lowest precedence).
    fn cond(&mut self) -> R<Cond> {
        let mut l = self.cond_and()?;
        loop {
            self.skip_inline_ws();
            if self.word_ahead("or") {
                self.pos += 2;
                let r = self.cond_and()?;
                l = Cond::Or(Box::new(l), Box::new(r));
            } else {
                return Ok(l);
            }
        }
    }

    /// Parse `and`-joined unary conditions.
    fn cond_and(&mut self) -> R<Cond> {
        let mut l = self.cond_unary()?;
        loop {
            self.skip_inline_ws();
            if self.word_ahead("and") {
                self.pos += 3;
                let r = self.cond_unary()?;
                l = Cond::And(Box::new(l), Box::new(r));
            } else {
                return Ok(l);
            }
        }
    }

    /// Parse `not c`, a parenthesized condition, or an operand optionally compared with `==` / `!=`.
    fn cond_unary(&mut self) -> R<Cond> {
        self.skip_inline_ws();
        if self.word_ahead("not") {
            self.pos += 3;
            return Ok(Cond::Not(Box::new(self.cond_unary()?)));
        }

        if self.peek() == Some(b'(') {
            // Grouping, unless it is a tuple literal operand.
            let save = self.pos;
            self.pos += 1;
            if let Ok(c) = self.cond() {
                self.skip_inline_ws();
                if self.peek() == Some(b')') {
                    self.pos += 1;
                    return Ok(c);
                }
            }

            self.pos = save;
        }

        let l = self.parse_flow()?;
        self.skip_inline_ws();
        if self.starts_with("==") || self.starts_with("!=") {
            let eq = self.starts_with("==");
            self.pos += 2;
            self.skip_inline_ws();
            let r = self.parse_flow()?;
            return Ok(if eq { Cond::Eq(l, r) } else { Cond::Ne(l, r) });
        }

        Ok(Cond::Test(l))
    }

    // ------------------------------------------------------------------
    // Documents
    // ------------------------------------------------------------------

    /// Whether a header directive `@name` starts at the cursor (in templates, not `@if`, `@for` or `@else`).
    fn directive_ahead(&self) -> bool {
        if self.peek() != Some(b'@') {
            return false;
        }

        let b = &self.b[self.pos + 1..];
        let n = b.iter().take_while(|&&c| is_ident_char(c)).count();
        if n == 0 || !matches!(b.get(n), None | Some(b' ' | b'\t' | b'\r' | b'\n')) {
            return false;
        }

        let name = &self.src[self.pos + 1..self.pos + 1 + n];
        !(self.template && matches!(name, "if" | "for" | "else"))
    }

    /// Take the rest of the line as a directive argument, trimmed and without a trailing comment, and
    /// move the cursor to the next line.
    fn rest_of_line_arg(&mut self) -> String {
        let start = self.pos;
        let end = self.src[start..].find('\n').map_or(self.src.len(), |i| start + i);
        let line = &self.src[start..end];
        let lb = line.as_bytes();
        let mut cut = line.len();
        for i in 0..lb.len() {
            if lb[i] == b'#' && (i == 0 || lb[i - 1] == b' ' || lb[i - 1] == b'\t') {
                cut = i;
                break;
            }
        }

        if cut < line.len() && self.spans.is_some() {
            self.comments.push((start + cut, start + line.trim_end_matches('\r').len()));
        }

        self.pos = (end + 1).min(self.src.len());
        line[..cut].trim().to_owned()
    }

    /// Parse a whole document; `allowed` decides which `@directives` are accepted.
    ///
    /// # Errors
    ///
    /// Returns a [`ParseError`] if the document is malformed or uses a directive
    /// `allowed` rejects.
    #[expect(clippy::too_many_lines, reason = "linear directive/body dispatch; splitting would obscure it")]
    pub fn parse_document(&mut self, allowed: &dyn Fn(&str) -> bool) -> R<Document> {
        if self.starts_with("\u{feff}") {
            self.pos += 3;
        }

        let mut directives = Vec::new();
        let mut tpl: Option<TemplateHeader> = None;

        loop {
            self.skip_blank_lines();
            if !self.directive_ahead() {
                break;
            }

            let dpos = self.pos;
            let line = self.error_at(dpos, "").line;
            self.pos += 1;
            let name = self.ident()?;
            self.skip_inline_ws();
            match name.as_str() {
                "nova" => {
                    if !directives.is_empty() || tpl.is_some() {
                        return Err(self.error_at(dpos, "@nova must be the first directive"));
                    }

                    let arg = self.rest_of_line_arg();
                    crate::version::NOVA_VERSION.check(&arg, "NOVA").map_err(|m| self.error_at(dpos, m))?;
                    directives.push(Directive { name, arg, line });
                }
                "template" => {
                    if directives.iter().any(|d: &Directive| d.name != "nova") || tpl.is_some() {
                        return Err(self.error_at(dpos, "@template must be the first directive (after @nova)"));
                    }

                    let arg = self.rest_of_line_arg();
                    if !arg.is_empty() {
                        crate::version::TEMPLATE_VERSION
                            .check(&arg, "the NOVA template language")
                            .map_err(|m| self.error_at(dpos, m))?;
                    }

                    self.template = true;
                    tpl = Some(TemplateHeader::default());
                }
                "param" | "let" | "import" => {
                    let Some(h) = tpl.as_mut() else {
                        return Err(self.error_at(dpos, format!("@{name} requires @template")));
                    };

                    match name.as_str() {
                        "param" => {
                            let pname = self.ident()?;
                            let optional = self.peek() == Some(b'?');
                            if optional {
                                self.pos += 1;
                            }

                            self.expect(b':', "`:` after parameter name")?;
                            let s = self.pos;
                            let end = self.src[s..].find('\n').map_or(self.src.len(), |i| s + i);
                            let line = &self.src[s..end];
                            let ty_end = line.find(" =").or_else(|| line.find(" #")).unwrap_or(line.len());
                            let ty = line[..ty_end].trim().to_owned();
                            if ty.is_empty() {
                                return self.err("expected a type after `:`");
                            }

                            self.pos = s + ty_end;
                            self.skip_inline_ws();
                            let default = if self.peek() == Some(b'=') {
                                self.pos += 1;
                                self.skip_inline_ws();
                                Some(self.parse_flow()?)
                            } else {
                                None
                            };

                            self.end_line()?;
                            h.params.push(Param {
                                name: pname,
                                optional,
                                ty,
                                default,
                            });
                        }
                        "let" => {
                            let lname = self.ident()?;
                            self.skip_inline_ws();
                            let value = match self.peek() {
                                Some(b'=') => {
                                    self.pos += 1;
                                    self.skip_inline_ws();
                                    let n = self.parse_flow()?;
                                    self.end_line()?;
                                    n
                                }
                                Some(b':') => {
                                    self.pos += 1;
                                    self.block_value(0)?
                                }
                                _ => return self.err("expected `=` or `:` after @let name"),
                            };

                            h.lets.push(Let { name: lname, value });
                        }
                        _ => {
                            let path = self.parse_quoted_literal()?;
                            self.skip_inline_ws();
                            if !self.starts_with("as ") {
                                return self.err("expected `as <name>` after @import path");
                            }

                            self.pos += 3;
                            self.skip_inline_ws();
                            let alias = self.ident()?;
                            self.end_line()?;
                            h.imports.push(Import { path, alias });
                        }
                    }
                }
                _ => {
                    if !allowed(&name) && !name.starts_with("x-") {
                        return Err(self.error_at(dpos, format!("unknown directive @{name}")));
                    }

                    let arg = self.rest_of_line_arg();
                    directives.push(Directive { name, arg, line });
                }
            }
        }

        let body = self.parse_body()?;
        Ok(Document {
            directives,
            template: tpl,
            body,
        })
    }

    /// Parse the document body (a table, a block list or record, or one flow value) and require that
    /// only blank lines and comments follow.
    fn parse_body(&mut self) -> R<Node> {
        self.skip_blank_lines();
        let body_start = self.pos;
        let body = match self.line_indent()? {
            None => Node::Record(Vec::new()),
            Some(i) if i > 0 => return Err(self.error_at(self.pos + i, "the document body must start at column 1")),
            Some(_) => {
                if self.peek() == Some(b'[') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
                    let (n, fields) = self.table_header()?;
                    self.expect(b':', "`:` after table header")?;
                    self.end_line()?;
                    self.table_rows(2, n, &fields)?
                } else if self.is_list_marker() || self.looks_like_key() || self.control_keyword().is_some() || self.starts_with("...") {
                    match self.peek_block_kind() {
                        BlockKind::List => Node::List(self.block_list(0)?),
                        BlockKind::Record => Node::Record(self.block_record(0, false)?),
                    }
                } else {
                    let n = self.parse_flow()?;
                    self.end_line()?;
                    n
                }
            }
        };

        self.skip_blank_lines();
        if !self.at_eof() {
            return self.err("unexpected content after the document body");
        }

        if !self.template
            && let Some(spans) = self.spans.as_mut()
        {
            spans.push(Span {
                path: Path::root(),
                start: body_start,
                end: self.last_end.max(body_start),
                key: None,
                kind: SpanKind::Root,
            });
        }

        Ok(body)
    }
}

/// The path segment of a record key; interpolated keys get an empty field name.
fn seg_of(k: &Key) -> Segment {
    match k {
        Key::Lit(s) => Segment::Field(s.clone()),
        Key::Interp(_) => Segment::Field(String::new()),
    }
}

/// A control block parsed in list or record context.
enum Ctl {
    /// In a record.
    Entry(Entry),
    /// In a list.
    Item(Item),
}

/// The body of a control block: record entries or list items.
enum Ctl2 {
    /// Record context.
    Entries(Vec<Entry>),
    /// List context.
    Items(Vec<Item>),
}

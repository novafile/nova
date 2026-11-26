//! Recursive-descent parser for NOVA text (spec §4) and templates (spec §9).

use crate::ast::{BinOp, Call, Comp, Decl, Def, DefParam, Directive, Document, Entry, ForHead, Import, Item, Key, Let, Node, Param, Part, TemplateHeader};
use crate::path::{Path, Segment, is_ident_char, is_ident_start};
use crate::spans::{Span, SpanKind};
use base64::Engine;
use novafile_value::{Decimal, Duration, Rational, Timestamp, Value};
use std::fmt;

/// `1 row`, `3 rows`.
fn n_rows(n: usize) -> String {
    if n == 1 { "1 row".into() } else { format!("{n} rows") }
}

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
    /// A replacement that fixes this error: the byte range of the source and its new text.
    pub fix: Option<(std::ops::Range<usize>, String)>,
}

impl ParseError {
    /// The error with the source line it points at and a caret under the column:
    ///
    /// ```text
    /// 2:9: expected a value
    ///   |
    /// 2 | port: [80,
    ///   |         ^
    /// ```
    ///
    /// For a full report with labels, enable the `miette` feature and use `to_diagnostic`.
    #[must_use]
    pub fn with_source(&self, src: &str) -> String {
        let Some(line) = src.split('\n').nth(self.line.saturating_sub(1)).map(|l| l.trim_end_matches('\r')) else {
            return self.to_string();
        };

        let n = self.line.to_string();
        let pad = " ".repeat(n.len());
        let caret = " ".repeat(self.col.saturating_sub(1));
        format!("{self}\n{pad} |\n{n} | {line}\n{pad} | {caret}^")
    }

    /// Attach a fix: replace `range` of the source with `text`.
    #[must_use]
    pub fn with_fix(mut self, range: std::ops::Range<usize>, text: impl Into<String>) -> Self {
        self.fix = Some((range, text.into()));
        self
    }
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

/// Whether a line's content `t` opens a list item: `-` alone or followed by inline whitespace.
pub(crate) fn starts_list_item(t: &str) -> bool {
    t == "-" || t.starts_with("- ") || t.starts_with("-\t")
}

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
///
/// # Errors
///
/// Returns a message if `tok` looks like a number that is out of range or malformed.
pub fn classify(tok: &str) -> Result<Value, String> {
    match tok {
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        "null" => return Ok(Value::Null),
        _ => {}
    }

    match number_kind(tok) {
        Some(NumKind::Int) => tok
            .parse::<i128>()
            .map(Value::Int)
            .map_err(|_| format!("integer {tok} is out of range; quote it if it is a string")),
        Some(NumKind::Rational) => Rational::parse(tok).map(Value::Rational),
        Some(NumKind::Float) => {
            let f: f64 = tok.parse().map_err(|_| format!("invalid float {tok}"))?;
            if f.is_infinite() {
                return Err(format!("float {tok} is out of range; quote it if it is a string, or write @float(\"inf\")"));
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
    /// Whether the next expression atom has an implied `$` (the head of an interpolation).
    implied: bool,
    /// Whether the cursor is inside an interpolation `${…}`.
    in_interp: bool,
    /// Source offset to report for calls parsed from text that is not the source itself (the
    /// content of a block string).
    offset_base: Option<usize>,
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
            implied: false,
            in_interp: false,
            offset_base: None,
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
            fix: None,
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
                    if !buf.is_empty() {
                        parts.push(Part::Text(std::mem::take(&mut buf)));
                    }

                    parts.push(Part::Expr(self.interp_in_place()?));
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

    /// Parse an interpolation in place (cursor at `${`) through its closing `}`. Its content
    /// runs to the matching `}`, skipping nested braces and quoted strings, and is read as an
    /// expression before the enclosing string's escapes apply.
    fn interp_in_place(&mut self) -> R<Node> {
        let start = self.pos;
        let close = interp_close(&self.b[start + 2..], 0)
            .map(|i| start + 2 + i)
            .ok_or_else(|| self.error_at(start, "unterminated interpolation `${`"))?;
        self.pos = start + 2;
        let n = self.interp_expr()?;
        self.skip_inline_ws();
        if self.pos != close {
            return self.err("unexpected text in interpolation; expected `}`");
        }

        self.pos = close + 1;
        Ok(n)
    }

    /// Parse the content of an interpolation: an expression in which a leading identifier
    /// has an implied `$`.
    fn interp_expr(&mut self) -> R<Node> {
        self.skip_inline_ws();
        if matches!(self.peek(), None | Some(b'}')) {
            return self.err("expected an expression inside ${...}");
        }

        self.implied = self.peek().is_some_and(is_ident_start);
        let was = std::mem::replace(&mut self.in_interp, true);
        let r = self.expr();
        self.in_interp = was;
        self.implied = false;
        r
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

    /// Run `f` one nesting level deeper, failing once [`MAX_DEPTH`] is exceeded.
    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> R<T>) -> R<T> {
        self.depth += 1;
        let r = if self.depth > MAX_DEPTH {
            self.err("nesting depth limit exceeded")
        } else {
            f(self)
        };
        self.depth -= 1;
        r
    }

    /// Parse a flow value (possibly containing template constructs) at the cursor.
    ///
    /// # Errors
    ///
    /// Returns a [`ParseError`] if the value is malformed or nested too deeply.
    pub fn parse_flow(&mut self) -> R<Node> {
        let r = self.nested(Self::parse_flow_inner);
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
            Some(b'[' | b'{') if self.comprehension_ahead() => self.comprehension(),
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
            Some(b'$') => self.expr(),
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
        // Only a value spanning lines needs its line's indentation; finding the
        // line start for every one-line cell of a long table row is quadratic.
        let mut off = self.src[start..self.pos].find('\n').map(|i| start + i + 1);
        if outer && off.is_some() {
            let ls = self.src[..start].rfind('\n').map_or(0, |i| i + 1);
            let base = self.src[ls..].bytes().take_while(|&c| c == b' ').count();
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

    // ------------------------------------------------------------------
    // Template expressions (template language 0.2)
    // ------------------------------------------------------------------

    /// Parse an expression: operators over references, calls, literals and comprehensions. It
    /// ends before anything that is not an operator surrounded by whitespace.
    fn expr(&mut self) -> R<Node> {
        self.nested(Self::expr_or)
    }

    /// Whether `c` may surround an operator: a space or tab, and inside brackets also a line break.
    fn is_op_ws(&self, c: u8) -> bool {
        c == b' ' || c == b'\t' || (self.bracket_depth > 0 && matches!(c, b'\n' | b'\r'))
    }

    /// If one of `ops` follows the cursor, surrounded by whitespace, consume it with the
    /// whitespace around it.
    fn take_op(&mut self, ops: &[(&str, BinOp)]) -> Option<BinOp> {
        let mut p = self.pos;
        while self.b.get(p).is_some_and(|&c| self.is_op_ws(c)) {
            p += 1;
        }

        if p == self.pos {
            return None;
        }

        let &(text, op) = ops
            .iter()
            .find(|(t, _)| self.b[p..].starts_with(t.as_bytes()) && self.b.get(p + t.len()).is_some_and(|&c| self.is_op_ws(c)))?;
        p += text.len();
        while self.b.get(p).is_some_and(|&c| self.is_op_ws(c)) {
            p += 1;
        }

        self.pos = p;
        Some(op)
    }

    /// A left-associative chain of `next` operands joined by `ops`. Long chains are bounded
    /// like nesting, since they evaluate recursively.
    fn left_assoc(&mut self, ops: &[(&str, BinOp)], next: fn(&mut Self) -> R<Node>) -> R<Node> {
        let mut l = next(self)?;
        let mut n = self.depth;
        while let Some(op) = self.take_op(ops) {
            n += 1;
            if n > MAX_DEPTH {
                return self.err("too many operators in one expression; split it with @let");
            }

            let r = next(self)?;
            l = Node::Binary(op, Box::new(l), Box::new(r));
        }

        Ok(l)
    }

    /// `a or b …`, the loosest level.
    fn expr_or(&mut self) -> R<Node> {
        self.left_assoc(&[("or", BinOp::Or)], Self::expr_and)
    }

    /// `a and b …`.
    fn expr_and(&mut self) -> R<Node> {
        self.left_assoc(&[("and", BinOp::And)], Self::expr_not)
    }

    /// `not a`, or a comparison.
    fn expr_not(&mut self) -> R<Node> {
        if !self.implied && self.starts_with("not") && self.peek_at(3).is_some_and(|c| self.is_op_ws(c)) {
            self.pos += 3;
            while self.peek().is_some_and(|c| self.is_op_ws(c)) {
                self.pos += 1;
            }

            return Ok(Node::Not(Box::new(self.nested(Self::expr_not)?)));
        }

        self.expr_cmp()
    }

    /// `a OP b` with one comparison operator; comparisons do not chain.
    fn expr_cmp(&mut self) -> R<Node> {
        /// The comparison operators.
        const CMP: &[(&str, BinOp)] = &[
            ("==", BinOp::Eq),
            ("!=", BinOp::Ne),
            ("<=", BinOp::Le),
            (">=", BinOp::Ge),
            ("<", BinOp::Lt),
            (">", BinOp::Gt),
        ];
        let l = self.expr_sum()?;
        let Some(op) = self.take_op(CMP) else { return Ok(l) };
        let r = self.expr_sum()?;
        let at = self.pos;
        if self.take_op(CMP).is_some() {
            return Err(self.error_at(at, "comparison operators do not chain; write `$a < $b and $b < $c`"));
        }

        Ok(Node::Binary(op, Box::new(l), Box::new(r)))
    }

    /// `a + b - c …`.
    fn expr_sum(&mut self) -> R<Node> {
        self.left_assoc(&[("+", BinOp::Add), ("-", BinOp::Sub)], Self::expr_prod)
    }

    /// `a * b …`.
    fn expr_prod(&mut self) -> R<Node> {
        self.left_assoc(&[("*", BinOp::Mul)], Self::atom)
    }

    /// An operand: a reference or call, a grouping or tuple, or a flow value (which may contain
    /// expressions and comprehensions).
    fn atom(&mut self) -> R<Node> {
        if std::mem::take(&mut self.implied) {
            let start = self.pos;
            return self.ref_or_call(start);
        }

        match self.peek() {
            Some(b'$') => self.dollar(),
            Some(b'(') => self.bracketed(Self::paren),
            _ => self.parse_flow(),
        }
    }

    /// `( expr )` grouping, or a tuple of expressions `()`, `(a,)`, `(a, b)` (cursor at `(`).
    fn paren(&mut self) -> R<Node> {
        self.pos += 1;
        let mut items = Vec::new();
        let mut comma = false;
        loop {
            self.skip_ws_multi();
            if self.peek() == Some(b')') {
                self.pos += 1;
                break;
            }

            items.push(self.expr()?);
            self.skip_ws_multi();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    comma = true;
                }
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                _ => return self.err("expected `,` or `)`"),
            }
        }

        if items.len() == 1 && !comma {
            return Ok(items.pop().expect("one item"));
        }

        Ok(Node::Tuple(items))
    }

    /// Parse `$name`, `$name.path`, `$name(args)` or `$( expr )` (templates only).
    fn dollar(&mut self) -> R<Node> {
        if !self.template {
            return self.err("`$` is reserved for template references; quote the string or add @template");
        }

        let start = self.pos;
        self.pos += 1;
        if self.peek() == Some(b'(') {
            return self.bracketed(|p| {
                p.pos += 1;
                p.skip_ws_multi();
                let e = p.expr()?;
                p.skip_ws_multi();
                p.expect(b')', "`)` to close `$(`")?;
                Ok(e)
            });
        }

        self.ref_or_call(start)
    }

    /// Parse a reference or call whose `$` (at `start`, or implied) is behind the cursor. A
    /// name immediately followed by `(` is a call.
    fn ref_or_call(&mut self, start: usize) -> R<Node> {
        let save = self.pos;
        if self.peek().is_some_and(is_ident_start) {
            let name = self.ident()?;
            if self.peek() == Some(b'(') {
                return self.call(name, start);
            }

            self.pos = save;
        }

        let (p, n) = Path::parse_prefix(&self.src[self.pos..]).map_err(|m| self.error_at(self.pos, m))?;
        if p.is_root() {
            return self.err("expected a name after `$`");
        }

        self.pos += n;
        Ok(Node::Ref(p))
    }

    /// Parse the arguments of a call of `name` written at `start` (cursor at `(`).
    fn call(&mut self, name: String, start: usize) -> R<Node> {
        let interp = self.in_interp;
        let offset = self.offset_base.unwrap_or(start);
        self.bracketed(|p| {
            p.pos += 1;
            let (mut args, mut bare) = (Vec::new(), Vec::new());
            loop {
                p.skip_ws_multi();
                if p.peek() == Some(b')') {
                    p.pos += 1;
                    break;
                }

                let first = p.peek();
                let a = p.expr()?;
                bare.push(!matches!(first, Some(b'"' | b'$' | b'[' | b'{' | b'(' | b'@' | b'!')) && matches!(a, Node::Lit(Value::String(_))));
                args.push(a);
                p.skip_ws_multi();
                match p.peek() {
                    Some(b',') => p.pos += 1,
                    Some(b')') => {
                        p.pos += 1;
                        break;
                    }
                    _ => return p.err("expected `,` or `)` in the arguments of a call"),
                }
            }

            Ok(Node::Call(Call {
                name,
                args,
                bare,
                interp,
                offset,
            }))
        })
    }

    /// Whether a comprehension `[for ` or `{for ` starts at the cursor (templates only).
    fn comprehension_ahead(&self) -> bool {
        self.template && (self.starts_with("[for") || self.starts_with("{for")) && self.peek_at(4).is_some_and(|c| c.is_ascii_whitespace())
    }

    /// Parse `[for [i,] x in SOURCE [if COND]: VALUE]` or `{for … : KEY: VALUE}` (cursor at the
    /// bracket).
    fn comprehension(&mut self) -> R<Node> {
        let close = if self.peek() == Some(b'[') { b']' } else { b'}' };
        self.bracketed(|p| {
            p.pos += 4;
            p.skip_ws_multi();
            let first = p.ident()?;
            p.skip_ws_multi();
            let (index, var) = if p.peek() == Some(b',') {
                p.pos += 1;
                p.skip_ws_multi();
                (Some(first), p.ident()?)
            } else {
                (None, first)
            };

            p.skip_ws_multi();
            if !p.word_ahead("in") {
                return p.err("expected `in` in comprehension");
            }

            p.pos += 2;
            p.skip_ws_multi();
            let iter = p.expr()?;
            p.skip_ws_multi();
            let filter = if p.word_ahead("if") {
                p.pos += 2;
                p.skip_ws_multi();
                let f = p.expr()?;
                p.skip_ws_multi();
                Some(f)
            } else {
                None
            };

            p.expect(b':', "`:` after the head of the comprehension")?;
            p.skip_ws_multi();
            let key = if close == b'}' {
                let k = p.expr()?;
                p.skip_ws_multi();
                p.expect(b':', "`:` after the key of a record comprehension")?;
                p.skip_ws_multi();
                Some(k)
            } else {
                None
            };

            let value = p.expr()?;
            p.skip_ws_multi();
            p.expect(
                close,
                if close == b']' {
                    "`]` to close the comprehension"
                } else {
                    "`}` to close the comprehension"
                },
            )?;
            Ok(Node::Comp(Box::new(Comp {
                head: ForHead { index, var, iter },
                filter,
                key,
                value,
            })))
        })
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
                Some(b':') => return self.err("expected `,` or `)`; `(…)` is a tuple of values, write a record as `{x: 1}` or as a block"),
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
        let tag = if self.peek() == Some(b'"') {
            let start = self.pos;
            let t = self.parse_quoted_literal()?;
            // Empty tags and `@` tags (reserved for markers such as `@redacted`) are invalid.
            novafile_value::variant_tag(&t).map_err(|m| self.error_at(start, m))?;

            t
        } else {
            self.ident()?
        };
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

        // As for plain values (§4.4); a flow word or table cell accepted one, and
        // re-rendered as a block value it no longer parsed.
        if let Some(i) = tok.find(char::is_control) {
            return Err(self.error_at(start + i, "control character in plain value"));
        }

        classify(tok).map(Node::Lit).map_err(|m| self.error_at(start, m))
    }

    // ------------------------------------------------------------------
    // Block structure
    // ------------------------------------------------------------------

    /// Does the text at `pos` look like `key:` / `key[N]{..}:` followed by space or EOL?
    fn looks_like_key(&self) -> bool {
        let (b, template) = (self.b, self.template);
        // Offset just past the quoted string opening at `p`, or `None` if it does not close on this line.
        let skip_quoted = |mut p: usize| {
            p += 1;
            loop {
                match b.get(p) {
                    None | Some(b'\n') => return None,
                    Some(b'\\') => p += 2,
                    Some(b'"') => return Some(p + 1),
                    Some(b'$') if template && b.get(p + 1) == Some(&b'$') => p += 2,
                    Some(b'$') if template && b.get(p + 1) == Some(&b'{') => p += 3 + interp_close(&b[p + 2..], 0)?,
                    _ => p += 1,
                }
            }
        };
        let mut p = self.pos;
        match b.get(p) {
            Some(b'"') => {
                let Some(q) = skip_quoted(p) else { return false };
                p = q;
            }
            Some(&c) if is_ident_start(c) => {
                while b.get(p).is_some_and(|&c| is_ident_char(c)) {
                    p += 1;
                }
            }
            _ => return false,
        }

        if b.get(p) == Some(&b'[') && (b.get(p + 1).is_some_and(u8::is_ascii_digit) || b.get(p + 1..p + 3) == Some(b"]{")) {
            while !matches!(b.get(p), None | Some(b'\n' | b'}')) {
                if b[p] == b'"' {
                    let Some(q) = skip_quoted(p) else { return false };
                    p = q;
                } else {
                    p += 1;
                }
            }

            if b.get(p) == Some(&b'}') {
                p += 1;
            }
        }

        b.get(p) == Some(&b':') && matches!(b.get(p + 1), None | Some(b' ' | b'\t' | b'\r' | b'\n' | b'#'))
    }

    /// Whether the cursor is at a `-` list marker (followed by a space, a line break or the end of input).
    fn is_list_marker(&self) -> bool {
        self.peek() == Some(b'-') && matches!(self.peek_at(1), None | Some(b' ' | b'\t' | b'\r' | b'\n'))
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

            if starts_list_item(t) || (t.starts_with('[') && t.as_bytes().get(1).is_some_and(u8::is_ascii_digit)) {
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

        self.nested(|p| match p.peek_block_kind() {
            BlockKind::List => p.block_list(indent).map(Node::List),
            BlockKind::Record => p.block_record(indent, false).map(Node::Record),
        })
    }

    /// Position the cursor at the next line of this block, or return false
    /// if the block has ended.
    fn next_block_line(&mut self, indent: usize) -> R<bool> {
        self.skip_blank_lines();
        match self.line_indent()? {
            None => Ok(false),
            Some(i) if i < indent => Ok(false),
            Some(i) if i > indent => Err(self.error_at(
                self.pos + i,
                format!("unexpected indentation; entries of this block start at column {}, and a value cannot continue on the next line (use a flow value `[…]`/`{{…}}` or a `|` block string)", indent + 1),
            )),
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
                    p.table(indent + 2)
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
                // parse_flow would set this; spans and `fmt` rely on it ending after `)`.
                self.last_end = self.pos;
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

        if starts_list_item(text) {
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
        let at = self.pos;
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
            return self.interpolate_block(&s, at);
        }

        Ok(Node::Lit(Value::String(s)))
    }

    /// Split a block string's text into literal and `${expr}` parts (templates only); `$${` stands
    /// for a literal `${`. Errors and calls are reported at `at`, the start of the block string.
    fn interpolate_block(&self, s: &str, at: usize) -> R<Node> {
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
                let close = interp_close(r.as_bytes(), 0).ok_or_else(|| self.error_at(at, "unterminated ${ in block string"))?;
                let off = s.len() - r.len();
                let mut p = Parser::new(s, true);
                p.pos = off;
                p.depth = self.depth;
                p.offset_base = Some(at);
                let n = p
                    .interp_expr()
                    .and_then(|n| {
                        p.skip_inline_ws();
                        if p.pos == off + close {
                            Ok(n)
                        } else {
                            p.err("unexpected text in interpolation; expected `}`")
                        }
                    })
                    .map_err(|e| self.error_at(at, e.message))?;
                if !buf.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut buf)));
                }

                parts.push(Part::Expr(n));
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

    /// Parse a table `[N]{col, ...}:` and its rows, indented `indent`. A missing or wrong
    /// row count is an error that names the rows found and carries the fix.
    fn table(&mut self, indent: usize) -> R<Node> {
        let (n, count_at, fields) = self.table_header()?;
        self.expect(b':', "`:` after table header")?;
        self.end_line()?;
        let Some(n) = n else {
            let found = self.count_rows(indent);
            return Err(self
                .error_at(
                    count_at.start,
                    format!("table header needs a row count, found {}; write [{found}]", n_rows(found)),
                )
                .with_fix(count_at, found.to_string()));
        };

        self.table_rows(indent, n, &fields, count_at)
    }

    /// Rows at exactly `indent` from the cursor on, up to the first non-blank, non-comment
    /// line indented less.
    fn count_rows(&self, indent: usize) -> usize {
        self.src[self.pos..]
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
            .map(|l| l.len() - l.trim_start_matches(' ').len())
            .take_while(|&i| i >= indent)
            .filter(|&i| i == indent)
            .count()
    }

    /// Parse a table header `[N]{col, ...}` (without the trailing `:`) into the row count
    /// (`None` for `[]`), the byte range of the count, and the column names.
    fn table_header(&mut self) -> R<(Option<usize>, std::ops::Range<usize>, Vec<String>)> {
        self.expect(b'[', "`[`")?;
        let s = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }

        let count_at = s..self.pos;
        let n = if count_at.is_empty() {
            None
        } else {
            Some(self.src[count_at.clone()].parse().map_err(|_| self.error_at(s, "row count is out of range"))?)
        };
        self.expect(b']', "`]` after row count")?;
        self.expect(b'{', "`{` to start the column list")?;
        let mut fields: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        loop {
            self.skip_inline_ws();
            let fpos = self.pos;
            let f = match self.key()? {
                Key::Lit(s) => s,
                Key::Interp(_) => return self.err("interpolation is not allowed in table headers"),
            };

            if !seen.insert(f.clone()) {
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

        Ok((n, count_at, fields))
    }

    /// Parse a column spec `{c1, …}` (spec §4.13): names, nested columns `name{…}` and the
    /// rest column `..`, validated with [`crate::columns::validate_columns`].
    ///
    /// # Errors
    ///
    /// Fails on malformed syntax or an invalid spec (duplicate names, `..` not last).
    pub fn column_specs(&mut self) -> R<Vec<crate::columns::Column>> {
        use crate::columns::Column;
        let start = self.pos;
        self.expect(b'{', "`{` to start the column list")?;
        let mut cols = Vec::new();
        loop {
            self.skip_inline_ws();
            if self.src[self.pos..].starts_with("..") {
                self.pos += 2;
                cols.push(Column::Rest);
            } else {
                let name = match self.key()? {
                    Key::Lit(s) => s,
                    Key::Interp(_) => return self.err("interpolation is not allowed in column specs"),
                };

                if self.peek() == Some(b'{') {
                    let subs = self.column_specs()?;
                    cols.push(Column::Nested(name, subs));
                } else {
                    cols.push(Column::Field(name));
                }
            }

            self.skip_inline_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return self.err("expected `,` or `}` in column spec"),
            }
        }

        crate::columns::validate_columns(&cols).map_err(|m| self.error_at(start, m))?;
        Ok(cols)
    }

    /// Parse the comma-separated cells of one table row up to the end of the line (spec §4.9).
    /// An empty cell, including a trailing one after a comma, is `None`. `fields` names the
    /// cells' spans; cells beyond it get an empty name.
    ///
    /// # Errors
    ///
    /// Fails if a cell is not a flow value or cells are not separated by `,`.
    pub fn row_cells(&mut self, fields: &[String]) -> R<Vec<Option<Node>>> {
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

        Ok(cells)
    }

    /// Parse exactly `n` table rows at `indent` into a list of records; an empty cell leaves its field
    /// absent.
    fn table_rows(&mut self, indent: usize, n: usize, fields: &[String], count_at: std::ops::Range<usize>) -> R<Node> {
        // `n` is untrusted; each row takes at least one source byte, so cap the hint.
        let mut rows = Vec::with_capacity(n.min(self.src.len()));
        let found = self.count_rows(indent);
        let wrong_count = |p: &Self| {
            p.error_at(
                count_at.start,
                format!(
                    "table header declares {}, but {found} {}; write [{found}]",
                    n_rows(n),
                    if found == 1 { "follows" } else { "follow" }
                ),
            )
            .with_fix(count_at.clone(), found.to_string())
        };
        for r in 0..n {
            self.skip_blank_lines();
            match self.line_indent()? {
                Some(i) if i == indent => self.pos += i,
                _ if found < n => return Err(wrong_count(self)),
                _ => return self.err(format!("table declares {n} rows but only {r} were found (rows are indented {indent} spaces)")),
            }

            let row_start = self.pos;
            #[expect(
                clippy::cast_possible_wrap,
                reason = "`r` counts rows already parsed from the source, so it is far below i64::MAX"
            )]
            self.cur.push(Segment::Index(r as i64));
            let cells = self.row_cells(fields)?;
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
            if found > n {
                return Err(wrong_count(self));
            }

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

        self.nested(|p| {
            Ok(match kind {
                BlockKind::Record => Ctl2::Entries(p.block_record(indent, false)?),
                BlockKind::List => Ctl2::Items(p.block_list(indent)?),
            })
        })
    }

    /// Parse `@if cond:` (cursor at `@if `) with its body, any `@else if` arms and an optional
    /// `@else:` at the same indentation. The arms are read in a loop, so a long chain costs no depth.
    fn if_block(&mut self, indent: usize, kind: BlockKind) -> R<Ctl> {
        self.pos += 4; // "@if "
        let mut arms = Vec::new();
        let mut els = None;
        loop {
            let cond = self.expr()?;
            self.skip_inline_ws();
            self.expect(b':', "`:` after @if condition")?;
            self.end_line()?;
            arms.push((cond, self.children(indent + 2, kind)?));
            let save = self.pos;
            self.skip_blank_lines();
            if !(matches!(self.line_indent()?, Some(i) if i == indent) && self.src[self.pos + indent..].starts_with("@else")) {
                self.pos = save;
                break;
            }

            self.pos += indent + 5;
            self.skip_inline_ws();
            if self.starts_with("if ") {
                self.pos += 3;
            } else {
                self.expect(b':', "`:` after @else")?;
                self.end_line()?;
                els = Some(self.children(indent + 2, kind)?);
                break;
            }
        }

        Ok(match kind {
            BlockKind::Record => Ctl::Entry(Entry::If(
                arms.into_iter().map(|(c, b)| (c, b.entries())).collect(),
                els.map_or_else(Vec::new, Ctl2::entries),
            )),
            BlockKind::List => Ctl::Item(Item::If(
                arms.into_iter().map(|(c, b)| (c, b.items())).collect(),
                els.map_or_else(Vec::new, Ctl2::items),
            )),
        })
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
        let iter = self.expr()?;
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

    // ------------------------------------------------------------------
    // Documents
    // ------------------------------------------------------------------

    /// Parse the rest of a `@def` directive at `dpos` (cursor at the name): the flow form
    /// `NAME(PARAMS)[: RESULT] = EXPR`, or the block form whose body follows on lines indented
    /// two spaces (`NAME(PARAMS)[: RESULT]`, optionally ending in `:`).
    fn def(&mut self, dpos: usize) -> R<Def> {
        let name = self.ident()?;
        self.expect(b'(', "`(` after the @def name")?;
        let params = self.def_params()?;
        self.skip_inline_ws();
        let mut result = None;
        if self.peek() == Some(b':') {
            self.pos += 1;
            self.skip_inline_ws();
            if !self.at_eol_or_comment() {
                let ty = self.type_text(b"=");
                result = Some(ty.strip_suffix(':').map_or(ty.as_str(), str::trim_end).to_owned());
            }
        }

        self.skip_inline_ws();
        let body = if self.peek() == Some(b'=') {
            self.pos += 1;
            self.skip_inline_ws();
            let n = self.expr()?;
            self.end_line()?;
            n
        } else {
            self.end_line()?;
            self.nested_block(2, "the @def body")?
        };

        Ok(Def {
            name,
            params,
            result,
            body,
            offset: dpos,
        })
    }

    /// Parse the parameters `name: TYPE [= DEFAULT], …` of a `@def` (cursor after `(`) through
    /// the `)`. There is at least one, and those with defaults come last.
    fn def_params(&mut self) -> R<Vec<DefParam>> {
        let mut params: Vec<DefParam> = Vec::new();
        loop {
            self.skip_inline_ws();
            if params.is_empty() && self.peek() == Some(b')') {
                return self.err("a @def needs at least one parameter; use @let for a value without parameters");
            }

            let name = self.ident()?;
            self.skip_inline_ws();
            self.expect(b':', "`:` after the parameter name")?;
            self.skip_inline_ws();
            let ty = self.type_text(b",)=");
            if ty.is_empty() {
                return self.err("expected a type after `:`");
            }

            let default = if self.peek() == Some(b'=') {
                self.pos += 1;
                self.skip_inline_ws();
                let d = self.expr()?;
                self.skip_inline_ws();
                Some(d)
            } else if params.last().is_some_and(|p| p.default.is_some()) {
                return self.err("parameters with defaults must come last");
            } else {
                None
            };

            params.push(DefParam { name, ty, default });
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b')') => {
                    self.pos += 1;
                    return Ok(params);
                }
                _ => return self.err("expected `,` or `)` in the @def parameters"),
            }
        }
    }

    /// Read a schema type up to one of `stops` outside brackets and quoted strings, a ` #`
    /// comment or the end of the line. Returns it trimmed, with the cursor at the stop.
    fn type_text(&mut self, stops: &[u8]) -> String {
        let start = self.pos;
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            match c {
                b'\n' | b'\r' => break,
                b'#' if self.pos > start && matches!(self.b[self.pos - 1], b' ' | b'\t') => break,
                b'"' => {
                    self.pos += 1;
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }

                        self.pos += if c == b'\\' { 2 } else { 1 };
                        if c == b'"' {
                            break;
                        }
                    }

                    self.pos = self.pos.min(self.b.len());
                    continue;
                }
                b'<' | b'(' | b'[' | b'{' => depth += 1,
                b'>' | b')' | b']' | b'}' if depth > 0 => depth -= 1,
                c if depth == 0 && stops.contains(&c) => break,
                _ => {}
            }

            self.pos += 1;
        }

        self.src[start..self.pos].trim().to_owned()
    }

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
        // Line number of `counted`, advanced incrementally so numbering directives stays linear.
        let (mut counted, mut line) = (0, 1);

        loop {
            self.skip_blank_lines();
            if !self.directive_ahead() {
                break;
            }

            let dpos = self.pos;
            line += self.src[counted..dpos].matches('\n').count();
            counted = dpos;
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
                    let version = if arg.is_empty() {
                        None
                    } else {
                        Some(crate::version::check_template_version(&arg).map_err(|m| self.error_at(dpos, m))?)
                    };

                    self.template = true;
                    tpl = Some(TemplateHeader {
                        version,
                        ..TemplateHeader::default()
                    });
                }
                "param" | "let" | "def" | "import" => {
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
                                Some(self.expr()?)
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
                                    let n = self.expr()?;
                                    self.end_line()?;
                                    n
                                }
                                Some(b':') => {
                                    self.pos += 1;
                                    self.block_value(0)?
                                }
                                _ => return self.err("expected `=` or `:` after @let name"),
                            };

                            h.decls.push(Decl::Let(Let { name: lname, value }));
                        }
                        "def" => {
                            let def = self.def(dpos)?;
                            h.decls.push(Decl::Def(def));
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
                if self.peek() == Some(b'[') && (self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) || self.starts_with("[]{")) {
                    self.table(2)?
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

/// Index of the `}` that closes an interpolation whose content starts at `b[0]`, skipping
/// nested braces and quoted strings (with their escapes and interpolations); `None` if a line
/// or the input ends first, or interpolations nest deeper than [`MAX_DEPTH`].
fn interp_close(b: &[u8], depth: usize) -> Option<usize> {
    if depth > MAX_DEPTH {
        return None;
    }

    let (mut braces, mut i) = (0usize, 0);
    loop {
        match *b.get(i)? {
            b'{' => braces += 1,
            b'}' if braces == 0 => return Some(i),
            b'}' => braces -= 1,
            b'"' => loop {
                i += 1;
                match *b.get(i)? {
                    b'"' => break,
                    b'\\' => i += 1,
                    b'\n' => return None,
                    b'$' if b.get(i + 1) == Some(&b'$') => i += 1,
                    b'$' if b.get(i + 1) == Some(&b'{') => i += 2 + interp_close(&b[i + 2..], depth + 1)?,
                    _ => {}
                }
            },
            b'\n' => return None,
            _ => {}
        }

        i += 1;
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

impl Ctl2 {
    /// The entries of a body parsed in record context.
    fn entries(self) -> Vec<Entry> {
        match self {
            Ctl2::Entries(e) => e,
            Ctl2::Items(_) => unreachable!("record body parsed as list"),
        }
    }

    /// The items of a body parsed in list context.
    fn items(self) -> Vec<Item> {
        match self {
            Ctl2::Items(i) => i,
            Ctl2::Entries(_) => unreachable!("list body parsed as record"),
        }
    }
}

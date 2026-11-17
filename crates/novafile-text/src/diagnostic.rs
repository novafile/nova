//! Rich diagnostics via [`miette`] (feature `miette`).
//!
//! ```ignore
//! let src = std::fs::read_to_string("config.nova")?;
//! novafile_text::parse(&src).map_err(|e| e.to_diagnostic("config.nova", &src))?;
//! ```

use crate::parser::ParseError;
use crate::path::Path;
use crate::spans::SpanMap;
use miette::{Diagnostic, LabeledSpan, NamedSource, SourceCode};
use std::fmt;

/// A diagnostic with source code, labels, help and related diagnostics.
#[derive(Debug, Clone)]
pub struct SourceDiagnostic {
    pub code: &'static str,
    pub message: String,
    pub help: Option<String>,
    /// `(byte offset, length, label)`.
    pub labels: Vec<(usize, usize, String)>,
    pub related: Vec<SourceDiagnostic>,
    /// Defaults to error.
    pub severity: Option<miette::Severity>,
    src: NamedSource<String>,
}

impl SourceDiagnostic {
    pub fn new(code: &'static str, message: impl Into<String>, name: &str, src: &str) -> Self {
        SourceDiagnostic {
            code,
            message: message.into(),
            help: None,
            labels: Vec::new(),
            related: Vec::new(),
            severity: None,
            src: NamedSource::new(name, src.to_owned()).with_language("nova"),
        }
    }

    pub fn with_label(mut self, offset: usize, len: usize, label: impl Into<String>) -> Self {
        self.labels.push((offset, len, label.into()));
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Label the token starting at `offset`.
    pub fn with_point(self, offset: usize, label: impl Into<String>) -> Self {
        let len = token_len(self.src.inner(), offset);
        self.with_label(offset, len, label)
    }

    /// Label a 1-based line/column position (e.g. from a patch or schema error).
    pub fn with_line_col(self, line: usize, col: usize, label: impl Into<String>) -> Self {
        let off = offset_of(self.src.inner(), line, col);
        self.with_point(off, label)
    }

    /// One diagnostic per `(path, message)`, each labelled at the value's span.
    pub fn from_path_issues<'a>(
        code: &'static str,
        title: impl Into<String>,
        name: &str,
        src: &str,
        spans: &SpanMap,
        issues: impl IntoIterator<Item = (&'a Path, String)>,
    ) -> Self {
        let mut d = SourceDiagnostic::new(code, title, name, src);
        for (path, msg) in issues {
            let mut item = SourceDiagnostic::new(code, format!("{path}: {msg}"), name, src);
            if let Some(span) = spans.closest(path) {
                let exact = span.path == *path;
                let (start, end) = if exact {
                    (span.start, span.end)
                } else {
                    span.key.unwrap_or((span.start, span.end))
                };
                item = item.with_label(start, end.saturating_sub(start), if exact { "this value" } else { "in here" });
            }
            d.related.push(item);
        }
        d
    }
}

fn token_len(src: &str, offset: usize) -> usize {
    let rest = src.get(offset..).unwrap_or("");
    let n = rest.find(|c: char| c.is_whitespace() || ",[]{}()".contains(c)).unwrap_or(rest.len());
    let n = if n == 0 {
        rest.chars().next().map(|c| if c == '\n' { 0 } else { c.len_utf8() }).unwrap_or(0)
    } else {
        n
    };
    n.min(rest.len())
}

fn offset_of(src: &str, line: usize, col: usize) -> usize {
    let mut start = 0;
    for _ in 1..line {
        match src[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return src.len(),
        }
    }
    let line_text = &src[start..];
    start + line_text.char_indices().nth(col.saturating_sub(1)).map(|(i, _)| i).unwrap_or(line_text.len())
}

impl fmt::Display for SourceDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SourceDiagnostic {}

impl Diagnostic for SourceDiagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.code))
    }
    fn severity(&self) -> Option<miette::Severity> {
        self.severity
    }
    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.help.as_ref().map(|h| Box::new(h) as Box<dyn fmt::Display>)
    }
    fn source_code(&self) -> Option<&dyn SourceCode> {
        if self.labels.is_empty() { None } else { Some(&self.src) }
    }
    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        if self.labels.is_empty() {
            return None;
        }
        Some(Box::new(self.labels.iter().map(|(o, l, t)| LabeledSpan::new(Some(t.clone()), *o, *l))))
    }
    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn Diagnostic> + 'a>> {
        if self.related.is_empty() {
            return None;
        }
        Some(Box::new(self.related.iter().map(|d| d as &dyn Diagnostic)))
    }
}

impl ParseError {
    /// Attach source code. Messages of the form `problem; advice` are split
    /// into the message and a help line.
    pub fn to_diagnostic(&self, name: &str, src: &str) -> SourceDiagnostic {
        let (msg, help) = match self.message.split_once("; ") {
            Some((m, h)) => (m.to_owned(), Some(h.to_owned())),
            None => (self.message.clone(), None),
        };
        let mut d = SourceDiagnostic::new("nova::syntax", msg.clone(), name, src).with_point(self.offset, "here");
        if let Some(h) = help {
            d = d.with_help(h);
        }
        d
    }
}

/// Plain `ParseError`s are diagnostics too (without source code attached).
impl Diagnostic for ParseError {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new("nova::syntax"))
    }
}

//! NOVA text representations (spec §4, §7.1, §9).
//!
//! * [`parse`] — parse a data document into a [`Value`].
//! * [`parse_document`] — parse directives plus body; supports templates.
//! * [`to_text`] — write a value in the [`Profile::Human`], [`Profile::Compact`]
//!   or [`Profile::Canonical`] profile.
//! * [`path`] — the path/selector language shared by queries, patches and templates.

pub mod ast;
#[cfg(feature = "miette")]
pub mod diagnostic;
pub mod format;
mod parser;
pub mod path;
pub mod spans;
pub mod tokens;
pub mod version;
mod writer;

pub use ast::Document;
pub use format::{format_preserving, inline_value, render_fragment, render_preserving};
pub use parser::{ParseError, Parser};
pub use path::{CmpOp, Path, PathError, Segment};
pub use writer::{Profile, canonicalize, document_to_text, flow_string, key_text, quote, to_text};

use novafile_value::Value;

/// Directives allowed in data and template documents (spec §4.2).
#[must_use]
pub fn is_data_directive(name: &str) -> bool {
    matches!(name, "nova" | "schema" | "revision" | "scope" | "envelope" | "mode" | "compat")
}

/// Parse a data or template document.
///
/// # Errors
///
/// Returns a [`ParseError`] if `src` is not well-formed NOVA.
pub fn parse_document(src: &str) -> Result<Document, ParseError> {
    Parser::new(src, false).parse_document(&is_data_directive)
}

/// Parse a data document and return its value. Template documents are
/// rejected; render them with `novafile-template` instead.
///
/// # Errors
///
/// Returns a [`ParseError`] if `src` is malformed, is a `@template`
/// document, or contains template constructs.
pub fn parse(src: &str) -> Result<Value, ParseError> {
    let doc = parse_document(src)?;
    if doc.is_template() {
        return Err(ParseError {
            line: 1,
            col: 1,
            offset: 0,
            message: "this is a @template document; render it first".into(),
        });
    }

    doc.body.to_value().map_err(|m| ParseError {
        line: 1,
        col: 1,
        offset: 0,
        message: m,
    })
}

/// Parse a data document and also return the source span of every value.
///
/// # Errors
///
/// Returns a [`ParseError`] if `src` is not well-formed NOVA.
pub fn parse_with_spans(src: &str) -> Result<(Document, spans::SpanMap), ParseError> {
    let mut p = Parser::new(src, false).with_spans();
    let doc = p.parse_document(&is_data_directive)?;
    let mut map = spans::SpanMap::new(p.take_spans());
    map.comments = p.take_comments();
    Ok((doc, map))
}

/// Parse a single flow value occupying all of `src` (surrounding whitespace allowed).
///
/// # Errors
///
/// Returns a [`ParseError`] if `src` is not a single well-formed flow value.
pub fn parse_flow(src: &str) -> Result<Value, ParseError> {
    let mut p = Parser::new(src, false);
    p.skip_ws_multi();
    let v = p.parse_flow_value()?;
    p.skip_ws_multi();
    if !p.at_eof() {
        return Err(p.error_at(p.pos, "unexpected content after value"));
    }

    Ok(v)
}

/// Split a text stream into documents separated by `---` lines (spec §4.11).
#[must_use]
pub fn split_stream(src: &str) -> Vec<&str> {
    let mut docs = Vec::new();
    let mut start = 0;
    let mut pos = 0;
    for line in src.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            docs.push(&src[start..pos]);
            start = pos + line.len();
        }

        pos += line.len();
    }

    docs.push(&src[start..]);
    docs.into_iter().filter(|d| !d.trim().is_empty()).collect()
}

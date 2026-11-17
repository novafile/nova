//! Document-preserving formatting (spec §3).
//!
//! [`format_preserving`] re-renders a document in a writer profile while
//! carrying over its comments, blank lines between entries, tables, and the
//! author's spelling of scalars (e.g. `@decimal("12.50")` stays as written). [`render_preserving`] renders a *different* value — for
//! example the result of a patch — with the trivia of the original source,
//! so comments stay attached to the paths they described.

use crate::parser::ParseError;
use crate::path::Path;
use crate::spans::{SpanKind, SpanMap};
use crate::writer::{canonicalize, write_body, Profile, W};
use nova_value::Value;
use std::collections::{HashMap, HashSet};

/// Presentation details of a source document, keyed by path (as displayed).
#[derive(Debug, Default, Clone)]
pub struct Trivia {
    /// Comment lines (including `#`) written before the entry at a path.
    pub leading: HashMap<String, Vec<String>>,
    /// A comment at the end of the entry's first line.
    pub trailing: HashMap<String, String>,
    /// Entries preceded by a blank line.
    pub blank_before: HashSet<String>,
    /// Source spelling of scalars with the value it denoted.
    pub spelling: HashMap<String, (String, Value)>,
    /// Lists written as tables.
    pub tables: HashSet<String>,
    /// Source order of paths (for hoisting comments out of inline values).
    pub order: HashMap<String, usize>,
    /// Comments before the body, with their byte offsets.
    pub header: Vec<(usize, String)>,
    /// Comments after the last entry.
    pub footer: Vec<String>,
    /// Whether a blank line separated header and body.
    pub blank_before_body: bool,
    pub(crate) keep_spelling: bool,
}

fn line_start(src: &str, at: usize) -> usize {
    src[..at].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

fn prev_line_blank(src: &str, at: usize) -> bool {
    let ls = line_start(src, at);
    if ls == 0 {
        return false;
    }
    let pls = line_start(src, ls - 1);
    src[pls..ls - 1].trim().is_empty()
}

impl Trivia {
    /// Collect trivia from a parsed source.
    pub fn collect(src: &str, spans: &SpanMap, value: &Value) -> Trivia {
        let mut t = Trivia::default();
        let body_start = spans.iter().find(|s| s.kind == SpanKind::Root).map(|s| s.start).unwrap_or(src.len());
        let blocks: Vec<_> = spans.iter().filter(|s| s.kind.is_block() && s.kind != SpanKind::Root).collect();
        for (i, s) in spans.iter().enumerate() {
            t.order.entry(s.path.to_string()).or_insert(i);
        }
        let mut leading_start: HashMap<String, usize> = HashMap::new();
        for &(cs, ce) in &spans.comments {
            let text = src[cs..ce].trim_end().to_owned();
            if cs < body_start {
                t.header.push((cs, text));
                continue;
            }
            let ls = line_start(src, cs);
            if !src[ls..cs].trim().is_empty() {
                // Trailing: the innermost line-level construct that starts on this line.
                if let Some(s) = blocks.iter().filter(|s| s.start >= ls && s.start < cs).max_by_key(|s| s.path.0.len()) {
                    let e = t.trailing.entry(s.path.to_string()).or_default();
                    if !e.is_empty() {
                        e.push(' ');
                    }
                    e.push_str(&text);
                    continue;
                }
            }
            // Leading: the next line-level construct (outermost when several start together).
            match blocks.iter().filter(|s| s.start >= ce).min_by_key(|s| (s.start, s.path.0.len())) {
                Some(s) => {
                    let p = s.path.to_string();
                    leading_start.entry(p.clone()).or_insert(cs);
                    t.leading.entry(p).or_default().push(text);
                }
                None => t.footer.push(text),
            }
        }
        for s in &blocks {
            let p = s.path.to_string();
            let first = leading_start.get(&p).copied().unwrap_or(s.start).min(s.start);
            if line_start(src, first) > body_start && prev_line_blank(src, first) {
                t.blank_before.insert(p.clone());
            }
            if matches!(s.kind, SpanKind::BlockEntry | SpanKind::BlockItem)
                && spans.children(&s.path).next().is_some_and(|c| c.kind == SpanKind::TableRow) {
                    t.tables.insert(p.clone());
                }
        }
        if spans.children(&Path::root()).next().is_some_and(|c| c.kind == SpanKind::TableRow) {
            t.tables.insert(Path::root().to_string());
        }
        for s in spans
            .iter()
            .filter(|s| matches!(s.kind, SpanKind::BlockEntry | SpanKind::BlockItem | SpanKind::TableCell))
        {
            let vs = s.value_start(src);
            let text = &src[vs.min(s.end)..s.end];
            if text.is_empty() || text.contains('\n') || text.starts_with('|') {
                continue;
            }
            if let Ok(v) = s.path.get(value) {
                if !v.is_container() {
                    t.spelling.insert(s.path.to_string(), (text.to_owned(), v.clone()));
                }
            }
        }
        t.blank_before_body = body_start > 0 && body_start < src.len() && prev_line_blank(src, body_start);
        t
    }
}

/// Reformat `src` in `profile`, keeping comments, blank lines and spellings
/// (except in the compact profile, which is meant for machines and LLMs, and
/// the canonical profile, which mandates canonical spellings).
pub fn format_preserving(src: &str, profile: Profile) -> Result<String, ParseError> {
    let (doc, _) = crate::parse_with_spans(src)?;
    if doc.is_template() {
        return Err(ParseError {
            line: 1,
            col: 1,
            offset: 0,
            message: "formatting @template documents is not supported yet".into(),
        });
    }
    let value = doc.body.to_value().map_err(|m| ParseError {
        line: 1,
        col: 1,
        offset: 0,
        message: m,
    })?;
    render_preserving(src, &value, profile)
}

/// Render `value` with the directives, comments and layout of `src`.
pub fn render_preserving(src: &str, value: &Value, profile: Profile) -> Result<String, ParseError> {
    let (doc, spans) = crate::parse_with_spans(src)?;
    let orig = doc.body.to_value().unwrap_or(Value::Null);
    let mut trivia = Trivia::collect(src, &spans, &orig);
    trivia.keep_spelling = profile == Profile::Human;
    let keep = profile != Profile::Compact;

    let mut out = String::new();
    // Directives and header comments, in source order.
    let line_offsets: Vec<usize> = std::iter::once(0).chain(src.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let mut header: Vec<(usize, String)> = doc
        .directives
        .iter()
        .map(|d| {
            let off = line_offsets.get(d.line.saturating_sub(1)).copied().unwrap_or(0);
            (
                off,
                if d.arg.is_empty() {
                    format!("@{}", d.name)
                } else {
                    format!("@{} {}", d.name, d.arg)
                },
            )
        })
        .collect();
    if keep {
        header.extend(trivia.header.iter().cloned());
    }
    header.sort_by_key(|(o, _)| *o);
    for (_, line) in &header {
        out.push_str(line);
        out.push('\n');
    }
    if !header.is_empty() && profile != Profile::Compact && (trivia.blank_before_body || !doc.directives.is_empty()) {
        out.push('\n');
    }

    let mut owned;
    let v = if profile == Profile::Canonical {
        owned = value.clone();
        canonicalize(&mut owned);
        &owned
    } else {
        value
    };
    let mut w = W::new(profile == Profile::Compact, keep.then_some(&trivia));
    write_body(&mut w, v);
    out.push_str(&w.out);
    if keep && !trivia.footer.is_empty() {
        for c in &trivia.footer {
            out.push_str(c);
            out.push('\n');
        }
    }
    Ok(out)
}

/// Render one block-level fragment — the entry `seg` (a field or list index)
/// of the container at `parent` — at `indent`, using the trivia of `src` so
/// comments inside the fragment are kept. With `continuation`, the first
/// line is meant to replace text that starts at the fragment's column: its
/// indentation and the fragment's own leading comments are omitted (they are
/// still in the surrounding text). The result ends with a newline.
pub fn render_fragment(src: Option<&str>, parent: &Path, seg: &crate::path::Segment, value: &Value, indent: usize, continuation: bool) -> String {
    let mut trivia = src
        .and_then(|s| {
            let (doc, spans) = crate::parse_with_spans(s).ok()?;
            let orig = doc.body.to_value().ok()?;
            Some(Trivia::collect(s, &spans, &orig))
        })
        .unwrap_or_default();
    trivia.keep_spelling = true;
    let here = parent.join(&Path(vec![seg.clone()])).to_string();
    if continuation {
        trivia.leading.remove(&here);
        trivia.blank_before.remove(&here);
    }
    let mut w = W::new(false, Some(&trivia));
    w.path = parent.0.clone();
    match seg {
        crate::path::Segment::Index(i) => {
            w.path.push(crate::path::Segment::Index(*i));
            w.item(value, indent);
        }
        crate::path::Segment::Field(k) => w.entry(k, value, indent, true),
        other => panic!("render_fragment: unsupported segment {other:?}"),
    }
    if continuation {
        w.out.drain(..indent.min(w.out.len()));
    }
    w.out
}

/// The single-line text for `value` after `key: ` / `- `, if the writer would
/// keep it on one line (`used` = columns already taken on that line).
pub fn inline_value(value: &Value, used: usize) -> Option<String> {
    W::new(false, None).inline_text(value, used)
}

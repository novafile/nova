//! Document-preserving patch application (spec §3, §7.5).
//!
//! [`apply_to_source`] applies a patch to NOVA *text*: every operation
//! becomes a minimal edit at the source span of its target, so comments,
//! blank lines, key order and spellings everywhere else stay byte-for-byte
//! identical. Fragments that must be re-rendered keep their own comments.
//! The result is verified by re-parsing; should any edit not produce the
//! value that value-level application produced, the document is re-rendered
//! with [`novafile_text::render_preserving`] instead.

use crate::patch::{Applied, OpKind, Patch, PatchError, apply, apply_op};
use novafile_text::path::{match_indices, resolve_index};
use novafile_text::spans::{Span, SpanKind, SpanMap};
use novafile_text::{Path, Profile, Segment, inline_value, parse_with_spans, render_fragment, render_preserving};
use novafile_value::Value;

/// The result of [`apply_to_source`].
#[derive(Clone, Debug)]
pub struct SourceEdit {
    /// The patched document text.
    pub text: String,
    /// Value-level result: new value and revisions.
    pub applied: Applied,
    /// True if every operation was applied as a minimal text edit; false if
    /// the document had to be re-rendered (comments are still preserved).
    pub surgical: bool,
}

/// Apply `patch` to the document text `src`.
pub fn apply_to_source(src: &str, patch: &Patch, validate: &dyn Fn(&Value) -> Result<(), Vec<String>>) -> Result<SourceEdit, PatchError> {
    let syntax = |e: novafile_text::ParseError| PatchError::Syntax {
        line: e.line,
        col: e.col,
        message: format!("document: {}", e.message),
    };
    let (doc, _) = parse_with_spans(src).map_err(syntax)?;
    if doc.is_template() {
        return Err(PatchError::Invalid(vec!["patches apply to rendered documents, not @template sources".into()]));
    }
    let value = doc.body.to_value().map_err(|m| PatchError::Invalid(vec![m]))?;
    let applied = apply(&value, patch, validate)?;

    let mut text = src.to_owned();
    let mut cur = value;
    for op in &patch.ops {
        let full = patch.scope.join(&op.path);
        let mut next = cur.clone();
        if apply_op(&mut next, &full, &op.kind).is_err() {
            break; // cannot happen: `apply` succeeded on the same sequence
        }
        match edit(&text, &cur, &next, &full, &op.kind) {
            Some(t) => text = t,
            None => {
                text = String::new();
                break;
            }
        }
        cur = next;
    }
    let surgical = !text.is_empty() && novafile_text::parse(&text).ok().as_ref() == Some(&applied.value);
    if !surgical {
        text = render_preserving(src, &applied.value, Profile::Human).map_err(syntax)?;
    }
    Ok(SourceEdit { text, applied, surgical })
}

/// Replace the path's selectors with concrete field names and indices.
fn concretize(root: &Value, path: &Path) -> Option<Path> {
    let mut cur = root;
    let mut out = Vec::new();
    for seg in &path.0 {
        let (seg, next) = match (seg, cur) {
            (Segment::Field(f), Value::Record(r)) => (Segment::Field(f.clone()), r.get(f)?),
            (Segment::Key(Value::String(f)), Value::Record(r)) => (Segment::Field(f.clone()), r.get(f)?),
            (Segment::Index(i), Value::List(l) | Value::Tuple(l)) => {
                let i = resolve_index(*i, l.len())?;
                (Segment::Index(i as i64), &l[i])
            }
            (Segment::Match(f, v), Value::List(l)) => {
                let idx = match_indices(l, f, v);
                (idx.len() == 1).then_some(())?;
                (Segment::Index(idx[0] as i64), &l[idx[0]])
            }
            (Segment::Key(k), Value::List(l)) => {
                let idx: Vec<usize> = l.iter().enumerate().filter(|(_, e)| *e == k).map(|(i, _)| i).collect();
                (idx.len() == 1).then_some(())?;
                (Segment::Index(idx[0] as i64), &l[idx[0]])
            }
            _ => return None, // maps, sets: their entries have no own spans
        };
        out.push(seg);
        cur = next;
    }
    Some(Path(out))
}

fn line_start(t: &str, at: usize) -> usize {
    t[..at].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

fn line_end(t: &str, at: usize) -> usize {
    t[at..].find('\n').map(|i| at + i).unwrap_or(t.len())
}

fn splice(t: &str, a: usize, b: usize, with: &str) -> String {
    let mut s = String::with_capacity(t.len() + with.len());
    s.push_str(&t[..a]);
    s.push_str(with);
    s.push_str(&t[b..]);
    s
}

/// Insert whole lines (`block`, ending in `\n`) after the line containing `at`.
fn insert_after_line(t: &str, at: usize, block: &str) -> String {
    let e = line_end(t, at);
    if e == t.len() {
        let mut s = t.to_owned();
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(block);
        return s;
    }
    splice(t, e + 1, e + 1, block)
}

/// Start of the lines belonging to a block span, including comment lines directly above it.
fn block_start_with_comments(t: &str, s: &Span) -> usize {
    let mut start = line_start(t, s.start);
    while start > 0 {
        let prev = line_start(t, start - 1);
        let line = t[prev..start - 1].trim();
        if line.starts_with('#') {
            start = prev;
        } else {
            break;
        }
    }
    start
}

fn edit(text: &str, cur: &Value, next: &Value, path: &Path, kind: &OpKind) -> Option<String> {
    if matches!(kind, OpKind::Test(_)) {
        return Some(text.to_owned());
    }
    let (_, spans) = parse_with_spans(text).ok()?;
    let (parent, last) = match path.parent() {
        Some((p, l)) => (p, Some(l.clone())),
        None => (Path::root(), None),
    };
    let target = concretize(cur, path);
    let cparent = concretize(cur, &parent)?;
    match kind {
        OpKind::Test(_) => unreachable!(),
        OpKind::Replace { .. } | OpKind::Set(_) if target.is_some() => {
            let t = target?;
            if t.is_root() {
                return None;
            }
            replace_at(text, &spans, &t, t.get(next).ok()?)
        }
        OpKind::Remove(_) => remove_at(text, &spans, next, &target?),
        OpKind::Add(_) if target.is_some() && matches!(target.as_ref()?.get(cur), Ok(Value::List(_))) => {
            let t = target?;
            let Ok(Value::List(old)) = t.get(cur) else { return None };
            let Ok(Value::List(new)) = t.get(next) else { return None };
            insert_item(text, &spans, next, &t, old.len(), new.last()?)
        }
        OpKind::Add(_) | OpKind::Set(_) => match (cparent.get(cur).ok()?, last?) {
            (Value::Record(_), Segment::Field(k)) => {
                let Ok(Value::Record(r)) = cparent.get(next) else { return None };
                insert_field(text, &spans, next, &cparent, &k, r.get(&k)?)
            }
            (Value::List(_), Segment::Index(i)) => {
                let Ok(Value::List(l)) = cparent.get(next) else { return None };
                let idx = usize::try_from(i).ok()?;
                insert_item(text, &spans, next, &cparent, idx, l.get(idx)?)
            }
            // Sets, maps, other containers: re-render the container.
            _ => replace_at(text, &spans, &cparent, cparent.get(next).ok()?),
        },
        _ => None,
    }
}

/// Replace the value at `path` with `new`.
fn replace_at(text: &str, spans: &SpanMap, path: &Path, new: &Value) -> Option<String> {
    // Everything addressable by `concretize` has a span; anything else falls back.
    let s = spans.get(path)?;
    let vs = s.value_start(text);
    match s.kind {
        SpanKind::TableCell | SpanKind::FlowItem | SpanKind::FlowEntry => Some(splice(text, vs, s.end, &novafile_text::flow_string(new))),
        SpanKind::BlockEntry | SpanKind::BlockItem => {
            let col = s.column(text);
            let same_line = !text[s.start..vs].contains('\n');
            let old_inline = same_line && !text[vs..s.end].contains('\n') && !text[vs..].starts_with('|');
            let used = vs - line_start(text, vs);
            match (old_inline, inline_value(new, used)) {
                (true, Some(inline)) => Some(splice(text, vs, s.end, &inline)),
                _ => {
                    let (parent, seg) = s.path.parent()?;
                    let frag = render_fragment(Some(text), &parent, seg, new, col, true);
                    Some(splice(text, s.start, line_end(text, s.end).min(text.len()), frag.trim_end_matches('\n')))
                }
            }
        }
        SpanKind::TableRow | SpanKind::Root => None,
    }
}

fn insert_field(text: &str, spans: &SpanMap, next: &Value, record: &Path, key: &str, value: &Value) -> Option<String> {
    let kids: Vec<&Span> = spans.children(record).collect();
    match kids.first() {
        Some(first) if first.kind == SpanKind::BlockEntry => {
            let indent = first.column(text);
            let last = kids.iter().max_by_key(|s| s.end)?;
            let frag = render_fragment(None, record, &Segment::Field(key.to_owned()), value, indent, false);
            Some(insert_after_line(text, last.end, &frag))
        }
        _ => replace_whole(text, spans, next, record),
    }
}

fn insert_item(text: &str, spans: &SpanMap, next: &Value, list: &Path, idx: usize, value: &Value) -> Option<String> {
    let kids: Vec<&Span> = spans.children(list).collect();
    match kids.first() {
        Some(first) if first.kind == SpanKind::BlockItem => {
            let indent = first.column(text);
            let frag = render_fragment(None, list, &Segment::Index(idx as i64), value, indent, false);
            if idx >= kids.len() {
                let last = kids.iter().max_by_key(|s| s.end)?;
                Some(insert_after_line(text, last.end, &frag))
            } else {
                let before = kids.iter().find(|s| s.path.0.last() == Some(&Segment::Index(idx as i64)))?;
                let at = block_start_with_comments(text, before);
                Some(splice(text, at, at, &frag))
            }
        }
        _ => replace_whole(text, spans, next, list),
    }
}

/// Re-render the container at `path` from its new value.
fn replace_whole(text: &str, spans: &SpanMap, next: &Value, path: &Path) -> Option<String> {
    if path.is_root() {
        return None;
    }
    replace_at(text, spans, path, path.get(next).ok()?)
}

fn remove_at(text: &str, spans: &SpanMap, next: &Value, path: &Path) -> Option<String> {
    let (parent, _) = path.parent()?;
    let s = spans.get(path);
    let siblings = spans.children(&parent).count();
    match s {
        Some(s)
            if matches!(s.kind, SpanKind::BlockEntry | SpanKind::BlockItem) && siblings > 1 && text[line_start(text, s.start)..s.start].trim().is_empty() =>
        {
            let a = block_start_with_comments(text, s);
            let e = line_end(text, s.end);
            let mut b = if e < text.len() { e + 1 } else { e };
            // Don't leave a separator blank line directly under the parent's key
            // or doubled with the blank line above.
            let next_blank = text[b..].split('\n').next().is_some_and(|l| l.trim().is_empty()) && b < text.len();
            let prev = if a == 0 { "" } else { text[line_start(text, a - 1)..a - 1].trim_end() };
            if next_blank && (prev.is_empty() || prev.ends_with(':')) {
                b = (line_end(text, b) + 1).min(text.len());
            }
            Some(splice(text, a, b, ""))
        }
        // Flow elements, table rows, sole children, first fields of list items: re-render the parent.
        _ => replace_whole(text, spans, next, &parent),
    }
}

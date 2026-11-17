//! Source spans for values, keyed by path (used for diagnostics and editors).

use crate::path::{Path, Segment};

/// The syntactic construct a span belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanKind {
    /// The document body.
    Root,
    /// `key: value` in a block record.
    BlockEntry,
    /// `- value` in a block list.
    BlockItem,
    /// `key: value` inside `{…}`.
    FlowEntry,
    /// An element of `[…]` or `(…)`.
    FlowItem,
    /// A row of a table (§4.9).
    TableRow,
    /// A cell of a table row.
    TableCell,
}

impl SpanKind {
    /// Constructs that occupy whole lines.
    pub fn is_block(self) -> bool {
        matches!(self, SpanKind::Root | SpanKind::BlockEntry | SpanKind::BlockItem | SpanKind::TableRow)
    }
}

/// The byte range of one value in the source text.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    /// Concrete path (fields and indices only).
    pub path: Path,
    /// Start of the entry: the key for record fields, the `-` for block list items.
    pub start: usize,
    /// End of the value (exclusive).
    pub end: usize,
    /// Byte range of the key, for record fields.
    pub key: Option<(usize, usize)>,
    pub kind: SpanKind,
}

impl Span {
    /// Where the value itself starts: after `key:` or `- `, or on the next
    /// content line when the value is an indented block.
    pub fn value_start(&self, src: &str) -> usize {
        let b = src.as_bytes();
        let mut i = match (self.kind, self.key) {
            (SpanKind::BlockEntry | SpanKind::FlowEntry, Some((_, kend))) => {
                let mut i = kend;
                while matches!(b.get(i), Some(b' ' | b'\t')) {
                    i += 1;
                }
                if b.get(i) == Some(&b':') {
                    i += 1;
                }
                i
            }
            (SpanKind::BlockItem, _) => self.start + 1,
            _ => return self.start,
        };
        loop {
            while matches!(b.get(i), Some(b' ' | b'\t' | b'\r')) {
                i += 1;
            }
            match b.get(i) {
                Some(b'#') => {
                    while !matches!(b.get(i), None | Some(b'\n')) {
                        i += 1;
                    }
                }
                Some(b'\n') => i += 1,
                _ => return i.min(self.end.max(self.start)),
            }
        }
    }

    /// Column (in bytes) of the span's first character.
    pub fn column(&self, src: &str) -> usize {
        self.start - src[..self.start].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }
}

#[derive(Clone, Debug, Default)]
pub struct SpanMap {
    spans: Vec<Span>,
    /// Byte ranges of comments (`#` through end of line).
    pub comments: Vec<(usize, usize)>,
}

impl SpanMap {
    pub fn new(mut spans: Vec<Span>) -> Self {
        spans.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
        SpanMap { spans, comments: Vec::new() }
    }

    /// Direct children of `parent`, in source order.
    pub fn children<'a>(&'a self, parent: &'a Path) -> impl Iterator<Item = &'a Span> + 'a {
        self.spans
            .iter()
            .filter(move |s| s.path.0.len() == parent.0.len() + 1 && s.path.0.starts_with(&parent.0))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Span> {
        self.spans.iter()
    }

    /// The span of exactly this path.
    pub fn get(&self, path: &Path) -> Option<&Span> {
        self.spans.iter().find(|s| s.path == *path)
    }

    /// The span of this path, or of its longest recorded prefix (useful when
    /// an error refers to a missing field).
    pub fn closest(&self, path: &Path) -> Option<&Span> {
        let concrete: Vec<Segment> = path.0.clone();
        (0..=concrete.len()).rev().find_map(|n| self.get(&Path(concrete[..n].to_vec())))
    }

    /// The innermost span containing `offset`.
    pub fn at(&self, offset: usize) -> Option<&Span> {
        self.spans
            .iter()
            .filter(|s| s.start <= offset && offset <= s.end)
            .max_by_key(|s| s.path.0.len())
    }
}

/// Convert a byte offset into a zero-based (line, UTF-16 column) position, as used by LSP.
pub fn line_col_utf16(src: &str, offset: usize) -> (u32, u32) {
    let offset = offset.min(src.len());
    let before = &src[..offset];
    let line = before.matches('\n').count();
    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = src[line_start..offset].encode_utf16().count();
    (line as u32, col as u32)
}

/// Convert a zero-based (line, UTF-16 column) position into a byte offset.
pub fn offset_of_utf16(src: &str, line: u32, col: u32) -> usize {
    let mut start = 0;
    for _ in 0..line {
        match src[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return src.len(),
        }
    }
    let mut units = 0u32;
    for (i, c) in src[start..].char_indices() {
        if c == '\n' || units >= col {
            return start + i;
        }
        units += c.len_utf16() as u32;
    }
    src.len()
}

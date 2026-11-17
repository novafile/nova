//! Source spans for values, keyed by path (used for diagnostics and editors).

use crate::path::{Path, Segment};

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
}

#[derive(Clone, Debug, Default)]
pub struct SpanMap {
    spans: Vec<Span>,
}

impl SpanMap {
    pub fn new(mut spans: Vec<Span>) -> Self {
        spans.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
        SpanMap { spans }
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

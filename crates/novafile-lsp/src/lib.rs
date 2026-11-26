//! A language server for NOVA (spec §4, §6, §7, §9).
//!
//! Features:
//! - diagnostics: syntax errors in documents, templates, patches and schemas;
//!   schema validation of documents whose `@schema name@version` matches a
//!   `.novs` file in the workspace (spec §6.7);
//! - document outline (symbols), with list items named by their `id`/`name`/`key`;
//! - hover: path, kind, value, declared schema type and class in documents;
//!   the definition of a type name in schemas, imported ones included;
//! - redaction (proposal 0009): hovers and outlines show `@redacted` for
//!   classified values, and no values for a document whose schema is not found;
//! - completion: missing record fields and enum variants / booleans from the schema;
//! - document-preserving formatting (human profile): comments, blank lines
//!   and spellings are kept (spec §3).

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::notification::{DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument, Notification as _, PublishDiagnostics};
use lsp_types::request::{CodeActionRequest, Completion, DocumentSymbolRequest, Formatting, HoverRequest, Request as _};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionProviderCapability, CompletionItem, CompletionItemKind, CompletionOptions,
    CompletionParams, CompletionResponse, Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, DocumentFormattingParams, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, Hover, HoverContents, HoverParams,
    HoverProviderCapability, InitializeParams, MarkupContent, MarkupKind, NumberOrString, OneOf, Position, PublishDiagnosticsParams, Range, ServerCapabilities,
    SymbolKind, TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions, TextDocumentSyncSaveOptions, TextEdit, Uri, WorkspaceEdit,
};

use novafile_schema::{Class, ClassMark, Def, Reveal, Schema, SchemaError, SchemaRef, TypeKind};
use novafile_text::spans::{SpanMap, offset_of_utf16};
use novafile_text::{Path, Segment};
use novafile_value::{REDACTED_TAG, Value};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;

/// Error type of the server loop: transport and (de)serialization failures.
type Error = Box<dyn std::error::Error + Send + Sync>;

/// Run the server on an established connection until shutdown.
///
/// # Errors
///
/// Fails if the LSP initialize handshake fails, if the client's capabilities
/// or a notification's parameters cannot be (de)serialized, or if sending a
/// message to the client fails.
#[expect(
    clippy::needless_pass_by_value,
    reason = "public API takes ownership of the connection for the server's lifetime"
)]
pub fn run(conn: Connection) -> Result<(), Error> {
    serve(conn.receiver.iter().map(Ok), |m| Ok(conn.sender.send(m)?))
}

/// Run the server on stdin/stdout until shutdown, on the calling thread.
///
/// Spawns no threads, so it also runs on `wasm32-wasip1`, where
/// [`Connection::stdio`] cannot start its reader and writer threads.
///
/// # Errors
///
/// As [`run`], plus failures to read or write stdio.
pub fn run_stdio() -> Result<(), Error> {
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let inbox = std::iter::from_fn(|| Message::read(&mut stdin).map_err(Error::from).transpose());
    serve(inbox, |m| Ok(m.write(&mut stdout)?))
}

/// Serve messages from `inbox`, sending replies and notifications through `send`.
fn serve(mut inbox: impl Iterator<Item = Result<Message, Error>>, mut send: impl FnMut(Message) -> Result<(), Error>) -> Result<(), Error> {
    let caps = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(TextDocumentSyncOptions {
            open_close: Some(true),
            change: Some(TextDocumentSyncKind::FULL),
            save: Some(TextDocumentSyncSaveOptions::Supported(true)),
            ..Default::default()
        })),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![":".into(), " ".into()]),
            ..Default::default()
        }),
        ..Default::default()
    };

    // Answer `initialize`; until then, reject requests and ignore notifications.
    let (id, init) = loop {
        match inbox.next().transpose()? {
            Some(Message::Request(r)) if r.method == "initialize" => break (r.id, r.params),
            Some(Message::Request(r)) => send(Response::new_err(r.id, ErrorCode::ServerNotInitialized as i32, "server not initialized".into()).into())?,
            Some(Message::Notification(n)) if n.method == "exit" => return Ok(()),
            Some(_) => {}
            None => return Err("connection closed before initialize".into()),
        }
    };
    send(Response::new_ok(id, serde_json::json!({ "capabilities": caps })).into())?;
    let params: InitializeParams = serde_json::from_value(init)?;
    #[allow(deprecated)]
    let root = params
        .workspace_folders
        .as_ref()
        .and_then(|f| f.first())
        .and_then(|f| file_path(&f.uri))
        .or_else(|| params.root_uri.as_ref().and_then(file_path));
    let mut server = Server {
        docs: HashMap::new(),
        disk_schemas: Vec::new(),
        scanned: Vec::new(),
        resolvers: std::cell::RefCell::default(),
        send: &mut send,
    };

    if let Some(root) = root {
        server.scan_schemas(&root, 8);
    }

    server.main_loop(&mut inbox)
}

/// Server state: open documents and the workspace's schemas.
struct Server<'c> {
    /// Text of every open document, as last sent by the client.
    docs: HashMap<Uri, String>,
    /// Schemas found on disk: (path, schema).
    disk_schemas: Vec<(PathBuf, Schema)>,
    /// Directories already searched for schemas; nothing below them is scanned again.
    scanned: Vec<PathBuf>,
    /// Catalog resolvers by project catalog file, built on first use: building one
    /// reads every source file. Cleared when a schema or catalog is saved or closed.
    resolvers: std::cell::RefCell<HashMap<PathBuf, std::rc::Rc<novafile_schema::catalog::Resolver>>>,
    /// Sends a message to the client.
    send: &'c mut dyn FnMut(Message) -> Result<(), Error>,
}

/// Directory steps from `a` up to the deepest directory it shares with `b`, then down to `b`.
fn dir_distance(a: &std::path::Path, b: &std::path::Path) -> usize {
    let common = a.components().zip(b.components()).take_while(|(x, y)| x == y).count();
    a.components().count() + b.components().count() - 2 * common
}

/// What kind of file a text is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// A schema (`.novs`).
    Schema,
    /// A patch: starts with `@patch` or has the `.patch` extension.
    Patch,
    /// A data document (anything else).
    Document,
}

/// Whether a URI's path has the given (case-sensitive) file extension.
fn has_ext(uri: &Uri, ext: &str) -> bool {
    std::path::Path::new(uri.path().as_str()).extension().is_some_and(|e| e == ext)
}

/// The local file a `file:` URI names, or `None` for other schemes and remote hosts.
fn file_path(uri: &Uri) -> Option<PathBuf> {
    if !uri.scheme().is_some_and(|s| s.as_str().eq_ignore_ascii_case("file")) {
        return None;
    }

    if let Some(a) = uri.authority()
        && !matches!(a.host().as_str(), "" | "localhost")
    {
        return None;
    }

    let bytes = uri.path().as_estr().decode().into_bytes().into_owned();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }
    #[cfg(not(unix))]
    {
        let s = String::from_utf8(bytes).ok()?;
        // `file:///C:/x` names `C:/x`.
        Some(PathBuf::from(if s.as_bytes().get(2) == Some(&b':') { &s[1..] } else { &s[..] }))
    }
}

/// Classify a text by its URI extension and, for patches, its first directive.
fn kind_of(uri: &Uri, text: &str) -> Kind {
    if has_ext(uri, "novs") {
        return Kind::Schema;
    }

    let first = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).unwrap_or("");
    if first.starts_with("@patch") || has_ext(uri, "patch") {
        Kind::Patch
    } else {
        Kind::Document
    }
}

/// A document text with its line starts, built once per request, so each
/// byte offset converts to an LSP position without rescanning the text before
/// it (which made diagnostics and outlines quadratic in document size).
struct Src<'a> {
    /// The document text.
    text: &'a str,
    /// Byte offset at which each line starts; the first is 0.
    starts: Vec<usize>,
}

impl<'a> Src<'a> {
    /// Index the line starts of `text`.
    fn new(text: &'a str) -> Self {
        let starts = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
        Src { text, starts }
    }

    /// Zero-based (line, UTF-16 column) of a byte offset, like [`novafile_text::spans::line_col_utf16`].
    fn line_col(&self, offset: usize) -> (u32, u32) {
        let offset = offset.min(self.text.len());
        let line = self.starts.partition_point(|&s| s <= offset) - 1;
        let col = self.text[self.starts[line]..offset].encode_utf16().count();
        (u32::try_from(line).unwrap_or(u32::MAX), u32::try_from(col).unwrap_or(u32::MAX))
    }
}

impl std::ops::Deref for Src<'_> {
    type Target = str;

    fn deref(&self) -> &str {
        self.text
    }
}

/// LSP range between two byte offsets (an end before `start` is clamped to it).
fn range(src: &Src<'_>, start: usize, end: usize) -> Range {
    let (l1, c1) = src.line_col(start);
    let (l2, c2) = src.line_col(end.max(start));
    Range::new(Position::new(l1, c1), Position::new(l2, c2))
}

/// Range of a 1-based line (whole line).
#[expect(clippy::cast_possible_truncation, reason = "LSP positions are u32 by protocol")]
fn line_range(src: &Src<'_>, line: usize) -> Range {
    let l = line.saturating_sub(1);
    let len = src.starts.get(l).map_or(0, |&a| {
        let b = src.starts.get(l + 1).map_or(src.len(), |&b| b - 1);
        src[a..b].trim_end_matches('\r').encode_utf16().count()
    }) as u32;
    Range::new(Position::new(l as u32, 0), Position::new(l as u32, len))
}

/// A token-sized range at a byte offset.
fn point_range(src: &Src<'_>, offset: usize) -> Range {
    let mut offset = offset.min(src.len());
    while !src.is_char_boundary(offset) {
        offset -= 1;
    }

    let rest = &src[offset..];
    // At least one whole character, so the end never falls inside a multibyte one.
    let n = rest
        .find(|c: char| c.is_whitespace() || ",[]{}()".contains(c))
        .unwrap_or(rest.len())
        .max(rest.chars().next().map_or(0, char::len_utf8));
    range(src, offset, offset + n)
}

/// A diagnostic from the `nova` source.
fn diag(range: Range, sev: DiagnosticSeverity, code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        range,
        severity: Some(sev),
        code: Some(NumberOrString::String(code.into())),
        source: Some("nova".into()),
        message: message.into(),
        ..Default::default()
    }
}

/// A successfully parsed data document.
struct Parsed {
    /// The document's value.
    value: Value,
    /// Source spans of every path in the document.
    spans: SpanMap,
    /// The `@schema` reference and the 1-based line of its directive.
    schema_ref: Option<(String, usize)>,
}

/// Parse a data document with spans; `Ok(None)` for templates.
fn parse_data(text: &str) -> Result<Option<Parsed>, novafile_text::ParseError> {
    let (doc, spans) = novafile_text::parse_with_spans(text)?;
    if doc.is_template() {
        return Ok(None);
    }

    let value = doc.body.to_value().map_err(|m| novafile_text::ParseError {
        line: 1,
        col: 1,
        offset: 0,
        message: m,
        fix: None,
    })?;
    let schema_ref = doc.directives.iter().find(|d| d.name == "schema").map(|d| (d.arg.clone(), d.line));
    Ok(Some(Parsed { value, spans, schema_ref }))
}

impl Server<'_> {
    /// Collect every parseable `.novs` file under `root`, at most `max_depth` directories
    /// deep (0: `root` itself only), skipping hidden directories, `target` and `node_modules`.
    fn scan_schemas(&mut self, root: &std::path::Path, max_depth: usize) {
        /// Appends every `.novs` file below `dir` to `out`, recursing at most `max_depth`
        /// levels and skipping hidden directories, `target` and `node_modules`.
        fn walk(dir: &std::path::Path, depth: usize, max_depth: usize, out: &mut Vec<PathBuf>) {
            if depth > max_depth {
                return;
            }

            let Ok(rd) = std::fs::read_dir(dir) else { return };
            for e in rd.flatten() {
                let p = e.path();
                let name = e.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }

                // `file_type` does not follow symlinks: a link to `.` multiplies the
                // scan, and a link to `/` or another tree reads schemas from outside.
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    walk(&p, depth + 1, max_depth, out);
                } else if ft.is_file() && name.ends_with(".novs") {
                    out.push(p);
                }
            }
        }

        self.scanned.push(root.to_path_buf());
        let mut files = Vec::new();
        walk(root, 0, max_depth, &mut files);
        files.retain(|f| !self.disk_schemas.iter().any(|(p, _)| p == f));
        for f in files {
            if let Some(s) = std::fs::read_to_string(&f).ok().and_then(|t| self.load_schema(Some(&f), &t).ok()) {
                self.disk_schemas.push((f, s));
            }
        }
    }

    /// Drops the cached catalog resolvers if `uri` is a schema or a catalog, whose
    /// change can alter what references resolve to. Returns whether it did.
    fn forget_catalogs(&self, uri: &Uri) -> bool {
        let catalog = file_path(uri).is_some_and(|p| p.file_name().is_some_and(|n| n == novafile_schema::catalog::CATALOG_FILE));
        let relevant = catalog || has_ext(uri, "novs");
        if relevant {
            self.resolvers.borrow_mut().clear();
        }

        relevant
    }

    /// Re-reads the `.novs` file at `uri` from disk into the known schemas, dropping it
    /// if it is gone or no longer parses. Returns whether `uri` is a schema file.
    fn reload_schema(&mut self, uri: &Uri) -> bool {
        let Some(path) = file_path(uri).filter(|_| has_ext(uri, "novs")) else {
            return false;
        };

        self.disk_schemas.retain(|(p, _)| *p != path);
        if let Some(s) = std::fs::read_to_string(&path).ok().and_then(|t| self.load_schema(Some(&path), &t).ok()) {
            self.disk_schemas.push((path, s));
        }

        true
    }

    /// The catalog resolver for files in `dir`, built on first use and cached by
    /// project catalog. Files without a project catalog, or without a directory,
    /// share the resolver under the empty key.
    fn resolver(&self, dir: Option<&std::path::Path>) -> Result<std::rc::Rc<novafile_schema::catalog::Resolver>, String> {
        use novafile_schema::catalog::{Options, Resolver};
        let catalog = dir.and_then(novafile_schema::catalog::project_catalog).unwrap_or_default();
        if let Some(r) = self.resolvers.borrow().get(&catalog) {
            return Ok(r.clone());
        }

        let made = std::rc::Rc::new(Resolver::new(&Options::from_env(Vec::new(), dir.map(std::path::Path::to_path_buf)))?);
        self.resolvers.borrow_mut().insert(catalog, made.clone());
        Ok(made)
    }

    /// Parse the schema text of the file `file` and link its imports (proposal 0008)
    /// through the catalogs of the file's directory, as `nvf` does. The bundled
    /// standard library links without a catalog.
    fn load_schema(&self, file: Option<&std::path::Path>, text: &str) -> Result<Schema, SchemaError> {
        let resolver = self
            .resolver(file.and_then(std::path::Path::parent))
            .map_err(|message| SchemaError { line: 0, message })?;
        resolver.load_text(text)
    }

    /// All known schemas with their files; open editors take precedence over disk contents.
    fn schemas(&self) -> Vec<(PathBuf, Schema)> {
        let open: Vec<(PathBuf, Schema)> = self
            .docs
            .iter()
            .filter(|(u, _)| has_ext(u, "novs"))
            .filter_map(|(u, t)| {
                let path = file_path(u)?;
                let schema = self.load_schema(Some(&path), t).ok()?;
                Some((path, schema))
            })
            .collect();
        let mut all = open.clone();
        for (p, s) in &self.disk_schemas {
            if !open.iter().any(|(q, _)| q == p) {
                all.push((p.clone(), s.clone()));
            }
        }

        all
    }

    /// The schema for a document at `doc` that declares `reference`: among the schemas
    /// that can read that version (spec §6.7), the one closest to the document in the
    /// directory tree, and among equally close ones the newest. Closeness keeps unrelated
    /// schemas of the same name elsewhere in the workspace, such as test fixtures, from
    /// taking over.
    fn find_schema(&self, reference: &str, doc: &Uri) -> Result<Option<Schema>, String> {
        let r = SchemaRef::parse(reference)?;
        let doc_dir = file_path(doc).and_then(|p| p.parent().map(std::path::Path::to_path_buf));
        // With any catalog (project, user, NOVA_CATALOG), lookup follows the catalogs
        // (spec §6.9), as `nvf validate` does, instead of guessing from the
        // workspace; it never fetches. Resolvers are cached by project catalog.
        if doc_dir.as_deref().is_some_and(novafile_schema::catalog::any_catalog) {
            return match self.resolver(doc_dir.as_deref())?.resolve(reference) {
                Ok(found) => Ok(Some(found.schema)),
                Err(novafile_schema::catalog::LookupError::NotFound(_)) => Ok(None),
                Err(e) => Err(format!("{e} ({})", e.kind())),
            };
        }

        let distance = |schema_file: &PathBuf| match (&doc_dir, schema_file.parent()) {
            (Some(d), Some(s)) => dir_distance(d, s),
            _ => usize::MAX,
        };
        let mut candidates: Vec<(PathBuf, Schema)> = self
            .schemas()
            .into_iter()
            .filter(|(_, s)| s.name.as_deref() == Some(r.name.as_str()) && s.version.as_ref().is_some_and(|v| v.can_read(&r.version)))
            .collect();
        candidates.sort_by(|(pa, a), (pb, b)| {
            distance(pa)
                .cmp(&distance(pb))
                .then_with(|| b.version.as_ref().unwrap().precedence(a.version.as_ref().unwrap()))
        });
        Ok(candidates.into_iter().next().map(|(_, s)| s))
    }

    /// The schema matching the `@schema` reference of the document at `doc`, if one is found.
    fn schema_for(&self, parsed: &Parsed, doc: &Uri) -> Option<Schema> {
        parsed.schema_ref.as_ref().and_then(|(r, _)| self.find_schema(r, doc).ok().flatten())
    }

    /// Syntax errors, and schema validation for documents with a resolvable `@schema`.
    fn diagnostics(&self, uri: &Uri, text: &str) -> Vec<Diagnostic> {
        let text = &Src::new(text);
        let mut out = Vec::new();
        match kind_of(uri, text) {
            Kind::Schema => {
                if let Err(e) = self.load_schema(file_path(uri).as_deref(), text) {
                    let r = if e.line == 0 { line_range(text, 1) } else { line_range(text, e.line) };
                    out.push(diag(r, DiagnosticSeverity::ERROR, "nova::schema", e.message));
                }
            }
            Kind::Patch => match novafile_patch::parse_patch(text) {
                Err(novafile_patch::PatchError::Syntax { line, col, message }) => {
                    // `col` counts characters, not UTF-16 units: walk the line's characters.
                    let start = text.starts.get(line.saturating_sub(1)).copied().unwrap_or(text.len());
                    let off = text[start..].char_indices().nth(col.saturating_sub(1)).map_or(text.len(), |(i, _)| start + i);
                    out.push(diag(point_range(text, off), DiagnosticSeverity::ERROR, "nova::patch::syntax", message));
                }
                Err(e) => out.push(diag(line_range(text, 1), DiagnosticSeverity::ERROR, "nova::patch", e.to_string())),
                Ok(_) => {}
            },
            Kind::Document => match parse_data(text) {
                Err(e) => out.push(diag(point_range(text, e.offset), DiagnosticSeverity::ERROR, "nova::syntax", e.message)),
                Ok(None) => {}
                Ok(Some(p)) => {
                    if let Some((reference, line)) = &p.schema_ref {
                        match self.find_schema(reference, uri) {
                            Err(m) => out.push(diag(line_range(text, *line), DiagnosticSeverity::ERROR, "nova::schema::reference", m)),
                            Ok(None) => out.push(diag(
                                line_range(text, *line),
                                DiagnosticSeverity::INFORMATION,
                                "nova::schema::missing",
                                format!("no schema in the workspace can read `{reference}`; add a matching .novs file to enable validation"),
                            )),
                            Ok(Some(s)) => {
                                if let Err(issues) = s.validate(&p.value) {
                                    for i in issues {
                                        let r = match p.spans.closest(&i.path) {
                                            Some(sp) if sp.path == i.path => range(text, sp.start, sp.end),
                                            Some(sp) => {
                                                let (a, b) = sp.key.unwrap_or((sp.start, sp.end));
                                                range(text, a, b)
                                            }
                                            None => line_range(text, 1),
                                        };

                                        let fp = s.fingerprint();
                                        out.push(diag(r, DiagnosticSeverity::ERROR, "nova::invalid", format!("{}: {} ({fp})", i.path, i.message)));
                                    }
                                }
                            }
                        }
                    }
                }
            },
        }

        out
    }

    /// Convention findings from `novafile-lint` (spec Appendix E).
    fn lints(&self, uri: &Uri, text: &str) -> Vec<Diagnostic> {
        let text = &Src::new(text);
        let config = novafile_lint::Config::default();
        let findings = match kind_of(uri, text) {
            // Linked through the catalogs, so imports of catalog schemas work.
            Kind::Schema => self
                .load_schema(file_path(uri).as_deref(), text)
                .ok()
                .and_then(|s| novafile_lint::lint_parsed_schema(text, &s, &config).ok())
                .unwrap_or_default(),
            Kind::Document => novafile_lint::lint_document(text, &config).unwrap_or_default(),
            Kind::Patch => vec![],
        };

        findings
            .into_iter()
            .map(|f| {
                let sev = match f.level {
                    novafile_lint::Level::Error => DiagnosticSeverity::ERROR,
                    novafile_lint::Level::Warning => DiagnosticSeverity::WARNING,
                    novafile_lint::Level::Hint => DiagnosticSeverity::HINT,
                };

                diag(range(text, f.start, f.end), sev, &format!("nova::lint::{}", f.rule), f.message)
            })
            .collect()
    }

    /// Send the diagnostics and lint findings of an open document to the client.
    fn publish(&mut self, uri: &Uri) -> Result<(), Error> {
        let Some(text) = self.docs.get(uri) else { return Ok(()) };
        let mut diagnostics = self.diagnostics(uri, text);
        diagnostics.extend(self.lints(uri, text));
        let params = PublishDiagnosticsParams {
            uri: uri.clone(),
            diagnostics,
            version: None,
        };

        (self.send)(Message::Notification(Notification::new(PublishDiagnostics::METHOD.into(), params)))?;
        Ok(())
    }

    /// Republish every open document, e.g. after a schema changed.
    fn publish_all(&mut self) -> Result<(), Error> {
        let uris: Vec<Uri> = self.docs.keys().cloned().collect();
        for u in uris {
            self.publish(&u)?;
        }

        Ok(())
    }

    /// Handle messages until the client requests shutdown.
    fn main_loop(&mut self, inbox: &mut dyn Iterator<Item = Result<Message, Error>>) -> Result<(), Error> {
        while let Some(msg) = inbox.next().transpose()? {
            match msg {
                Message::Request(req) if req.method == "shutdown" => {
                    (self.send)(Response::new_ok(req.id, ()).into())?;
                    return match inbox.next().transpose()? {
                        Some(Message::Notification(n)) if n.method == "exit" => Ok(()),
                        None => Ok(()),
                        Some(m) => Err(format!("unexpected message during shutdown: {m:?}").into()),
                    };
                }
                Message::Request(req) => {
                    let id = req.id.clone();
                    let resp = match self.request(req) {
                        Ok(v) => Response::new_ok(id, v),
                        Err((code, m)) => Response::new_err(id, code, m),
                    };

                    (self.send)(Message::Response(resp))?;
                }
                Message::Notification(n) => self.notification(n)?,
                Message::Response(_) => {}
            }
        }

        Ok(())
    }

    /// Track open documents and republish diagnostics on open, change and close.
    fn notification(&mut self, n: Notification) -> Result<(), Error> {
        match n.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let p: DidOpenTextDocumentParams = serde_json::from_value(n.params)?;
                let is_schema = has_ext(&p.text_document.uri, "novs");
                // Files opened outside the workspace (or without one) still find the
                // schemas next to them.
                if let Some(path) = file_path(&p.text_document.uri)
                    && let Some(dir) = path.parent()
                    && !self.scanned.iter().any(|root| dir.starts_with(root))
                {
                    self.scan_schemas(dir, 0);
                }

                self.docs.insert(p.text_document.uri.clone(), p.text_document.text);
                if is_schema {
                    self.publish_all()?;
                } else {
                    self.publish(&p.text_document.uri)?;
                }
            }
            DidChangeTextDocument::METHOD => {
                let p: DidChangeTextDocumentParams = serde_json::from_value(n.params)?;
                if let Some(c) = p.content_changes.into_iter().last() {
                    let is_schema = has_ext(&p.text_document.uri, "novs");
                    self.docs.insert(p.text_document.uri.clone(), c.text);
                    if is_schema {
                        self.publish_all()?;
                    } else {
                        self.publish(&p.text_document.uri)?;
                    }
                }
            }
            DidSaveTextDocument::METHOD => {
                let p: DidSaveTextDocumentParams = serde_json::from_value(n.params)?;
                if self.reload_schema(&p.text_document.uri) | self.forget_catalogs(&p.text_document.uri) {
                    self.publish_all()?;
                }
            }
            DidCloseTextDocument::METHOD => {
                let p: DidCloseTextDocumentParams = serde_json::from_value(n.params)?;
                self.docs.remove(&p.text_document.uri);
                // The disk copy, possibly older than the editor's, takes over again.
                if self.reload_schema(&p.text_document.uri) | self.forget_catalogs(&p.text_document.uri) {
                    self.publish_all()?;
                }

                let params = PublishDiagnosticsParams {
                    uri: p.text_document.uri,
                    diagnostics: vec![],
                    version: None,
                };

                (self.send)(Message::Notification(Notification::new(PublishDiagnostics::METHOD.into(), params)))?;
            }
            _ => {}
        }

        Ok(())
    }

    /// Answer a request; unsupported methods yield `MethodNotFound`.
    fn request(&self, req: Request) -> Result<serde_json::Value, (i32, String)> {
        /// Serializes a response result, reporting a failure as an LSP `InternalError`.
        fn json<T: serde::Serialize>(v: T) -> Result<serde_json::Value, (i32, String)> {
            serde_json::to_value(v).map_err(|e| (lsp_server::ErrorCode::InternalError as i32, e.to_string()))
        }

        let bad = |e: serde_json::Error| (lsp_server::ErrorCode::InvalidParams as i32, e.to_string());
        match req.method.as_str() {
            HoverRequest::METHOD => {
                let p: HoverParams = serde_json::from_value(req.params).map_err(bad)?;
                json(self.hover(&p.text_document_position_params.text_document.uri, p.text_document_position_params.position))
            }
            DocumentSymbolRequest::METHOD => {
                let p: DocumentSymbolParams = serde_json::from_value(req.params).map_err(bad)?;
                json(self.symbols(&p.text_document.uri).map(DocumentSymbolResponse::Nested))
            }
            Completion::METHOD => {
                let p: CompletionParams = serde_json::from_value(req.params).map_err(bad)?;
                json(
                    self.complete(&p.text_document_position.text_document.uri, p.text_document_position.position)
                        .map(CompletionResponse::Array),
                )
            }
            Formatting::METHOD => {
                let p: DocumentFormattingParams = serde_json::from_value(req.params).map_err(bad)?;
                json(self.format(&p.text_document.uri))
            }
            CodeActionRequest::METHOD => {
                let p: CodeActionParams = serde_json::from_value(req.params).map_err(bad)?;
                json(self.code_actions(&p.text_document.uri, p.range))
            }
            _ => Err((lsp_server::ErrorCode::MethodNotFound as i32, format!("unsupported request {}", req.method))),
        }
    }

    /// The text and parse result of an open data document, if it parses.
    fn parsed(&self, uri: &Uri) -> Option<(&str, Parsed)> {
        let text = self.docs.get(uri)?;
        if kind_of(uri, text) != Kind::Document {
            return None;
        }

        Some((text.as_str(), parse_data(text).ok()??))
    }

    /// Hover: the path, kind and value under the cursor, with its schema type, class
    /// and field notes. In a schema file: the definition of the type name under the cursor.
    fn hover(&self, uri: &Uri, pos: Position) -> Option<Hover> {
        if self.docs.get(uri).is_some_and(|t| kind_of(uri, t) == Kind::Schema) {
            return self.schema_hover(uri, pos);
        }

        let (text, p) = self.parsed(uri)?;
        let text = &Src::new(text);
        let off = offset_of_utf16(text, pos.line, pos.character);
        let span = p.spans.at(off)?;
        let v = span.path.get(&p.value).ok()?;
        let schema = self.schema_for(&p, uri);
        let mut md = format!("**`{}`** · {}", span.path, v.kind());
        match visible(&p, schema.as_ref()) {
            Some(visible) => {
                // The path fails only below a container that became a placeholder.
                let mut shown = span.path.get(&visible).map_or_else(|_| REDACTED_TAG.to_owned(), novafile_text::flow_string);
                if shown.chars().count() > 80 {
                    shown = shown.chars().take(77).collect::<String>() + "…";
                }

                let _ = write!(md, "\n\n```nova\n{shown}\n```");
            }
            None => md.push_str("\n\nvalue not shown: the schema is not found, so the classified values are not known"),
        }

        if let Some(s) = &schema
            && let Some((ty, field)) = s.type_at(&span.path)
        {
            let _ = write!(md, "\n\nschema `{}`: `{ty}`", s.reference().map(|r| r.to_string()).unwrap_or_default());
            let (under, _) = s.deref(&ty);
            if *under != ty {
                let _ = write!(md, " = `{under}`");
            }

            // The strongest class that the types on the way to the value give it.
            let class = (0..=span.path.0.len())
                .filter_map(|n| s.type_at(&Path(span.path.0[..n].to_vec())))
                .filter_map(|(t, _)| s.type_class(&t))
                .max();
            if let Some(c) = class {
                let _ = write!(md, " · `@{c}`");
            }

            // The documentation of the field, then of its named type (#34).
            let type_doc = match &ty.kind {
                novafile_schema::TypeKind::Named(n) => s.def_doc(n),
                _ => None,
            };
            let docs: Vec<String> = field.iter().filter_map(|f| f.doc.clone()).chain(type_doc.map(str::to_owned)).collect();

            if let Some(f) = field {
                let mut notes = Vec::new();
                if f.optional {
                    notes.push("optional".to_string());
                }

                if let Some(d) = &f.default {
                    let d = if class.is_some() {
                        REDACTED_TAG.to_owned()
                    } else {
                        novafile_text::flow_string(d)
                    };
                    notes.push(format!("default `{d}`"));
                }

                if f.key {
                    notes.push("`@key`".into());
                }

                if let Some(id) = f.id {
                    notes.push(format!("`@id({id})`"));
                }

                if f.deprecated {
                    notes.push("**deprecated**".into());
                }

                if !notes.is_empty() {
                    let _ = write!(md, "\n\n{}", notes.join(" · "));
                }
            }

            for d in docs {
                let _ = write!(md, "\n\n---\n\n{d}");
            }
        }

        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: md,
            }),
            range: Some(range(text, span.start, span.end)),
        })
    }

    /// The definition of the type name under the cursor in a schema file, as
    /// source text with its documentation comments. Names of imported definitions (`std.Uuid`) work too. There is
    /// no go-to-definition: a bundled schema, such as the standard library, has no
    /// file to go to.
    fn schema_hover(&self, uri: &Uri, pos: Position) -> Option<Hover> {
        let text = &Src::new(self.docs.get(uri)?);
        let schema = self.load_schema(file_path(uri).as_deref(), text).ok()?;
        let off = offset_of_utf16(text, pos.line, pos.character);
        let word = |c: char| c.is_alphanumeric() || c == '_' || c == '.';
        let start = text[..off].char_indices().rev().find(|&(_, c)| !word(c)).map_or(0, |(i, c)| i + c.len_utf8());
        let end = text[off..].find(|c| !word(c)).map_or(text.len(), |i| off + i);
        let name = text[start..end].trim_matches('.');
        let def = schema.def(name)?;
        // Print the one definition through the schema writer.
        let one = Schema {
            defs: vec![(name.to_owned(), def.clone())],
            def_classes: schema.def_classes.iter().filter(|(n, _)| n == name).cloned().collect(),
            def_docs: schema.def_docs.iter().filter(|(n, _)| n == name).cloned().collect(),
            ..Schema::default()
        };
        let mut md = format!("```novs\n{}\n```", one.to_string().trim());
        if let Some((alias, _)) = name.split_once('.')
            && let Some(i) = schema.imports.iter().find(|i| i.alias == alias)
        {
            let _ = write!(md, "\n\nimported from `{}`", i.reference);
        }

        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: md,
            }),
            range: Some(range(text, start, end)),
        })
    }

    /// Document outline of an open data document.
    fn symbols(&self, uri: &Uri) -> Option<Vec<DocumentSymbol>> {
        let (text, p) = self.parsed(uri)?;
        let schema = self.schema_for(&p, uri);
        let visible = visible(&p, schema.as_ref());
        Some(children_of(&Src::new(text), &p, visible.as_ref(), &children(&p), &Path::root()))
    }

    /// Quick fixes for the syntax error on the requested lines, such as a table's row count.
    fn code_actions(&self, uri: &Uri, at: Range) -> Vec<CodeActionOrCommand> {
        let Some(text) = self.docs.get(uri).filter(|t| kind_of(uri, t) == Kind::Document) else {
            return vec![];
        };
        let text = &Src::new(text);

        let Err(novafile_text::ParseError {
            fix: Some((fix, new)),
            message,
            ..
        }) = parse_data(text)
        else {
            return vec![];
        };

        let edit_range = range(text, fix.start, fix.end);
        if edit_range.start.line < at.start.line || edit_range.start.line > at.end.line {
            return vec![];
        }

        let title = message.rsplit("; ").next().map_or_else(|| "Fix".to_owned(), |advice| format!("Fix: {advice}"));
        let edit = TextEdit {
            range: edit_range,
            new_text: new,
        };
        vec![CodeActionOrCommand::CodeAction(CodeAction {
            title,
            kind: Some(CodeActionKind::QUICKFIX),
            is_preferred: Some(true),
            edit: Some(WorkspaceEdit {
                changes: Some(std::iter::once((uri.clone(), vec![edit])).collect()),
                ..Default::default()
            }),
            ..Default::default()
        })]
    }

    /// Document-preserving formatting (human profile) as one whole-document edit.
    fn format(&self, uri: &Uri) -> Option<Vec<TextEdit>> {
        let text = self.docs.get(uri)?;
        if kind_of(uri, text) != Kind::Document {
            return None;
        }

        // Document-preserving: comments, blank lines and spellings survive.
        let formatted = novafile_text::format_preserving(text, novafile_text::Profile::Human).ok()?;
        if formatted == *text {
            return Some(vec![]);
        }

        Some(vec![TextEdit {
            range: range(&Src::new(text), 0, text.len()),
            new_text: formatted,
        }])
    }

    /// Completion from the schema: enum variants and booleans after `key:`, else the
    /// missing fields of the enclosing record.
    fn complete(&self, uri: &Uri, pos: Position) -> Option<Vec<CompletionItem>> {
        let text = self.docs.get(uri)?;
        if kind_of(uri, text) != Kind::Document {
            return None;
        }

        let lines: Vec<&str> = text.split('\n').collect();
        let line = *lines.get(pos.line as usize)?;
        let before: String = line
            .encode_utf16()
            .take(pos.character as usize)
            .collect::<Vec<u16>>()
            .iter()
            .map(|&u| char::from_u32(u32::from(u)).unwrap_or('?'))
            .collect();

        // Parse with the line being edited blanked out, so incomplete input does not hide the structure.
        let mut blanked: Vec<&str> = lines.clone();
        blanked[pos.line as usize] = "";
        let blanked = blanked.join("\n");
        let p = parse_data(&blanked).ok()??;
        let schema = self.schema_for(&p, uri)?;
        let indent = before.len() - before.trim_start().len();
        let content = before.trim_start();
        let (content, indent) = match content.strip_prefix("- ") {
            Some(rest) => (rest, indent + 2),
            None => (content, indent),
        };

        let record = enclosing_record(&blanked, &p.spans, pos.line as usize, indent)?;

        if let Some((key, partial)) = content.split_once(": ").or_else(|| content.strip_suffix(':').map(|k| (k, ""))) {
            // Value completion for `key: <partial>`.
            let mut path = record.clone();
            path.0.push(Segment::Field(key.trim().to_owned()));
            let (ty, _) = schema.type_at(&path)?;
            let items = value_items(&schema, &ty)?;
            return Some(
                items
                    .into_iter()
                    .filter(|i| i.label.starts_with(partial.trim()) || i.label.starts_with(&format!("!{}", partial.trim())))
                    .collect(),
            );
        }

        // Key completion: fields of the enclosing record that are not yet present.
        let (ty, _) = schema.type_at(&record)?;
        let Some(Def::Record(rd)) = schema.deref(&ty).1 else { return None };
        let present: Vec<String> = match record.get(&p.value) {
            Ok(Value::Record(r)) => r.keys().cloned().collect(),
            _ => vec![],
        };

        Some(
            rd.fields
                .iter()
                .filter(|f| !present.contains(&f.name) && f.name.starts_with(content))
                .map(|f| CompletionItem {
                    label: f.name.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(format!(
                        "{}{}",
                        f.ty,
                        if f.optional {
                            " (optional)"
                        } else if f.default.is_some() {
                            " (has default)"
                        } else {
                            ""
                        }
                    )),
                    insert_text: Some(format!("{}: ", f.name)),
                    sort_text: Some(format!("{}{}", i32::from(f.optional || f.default.is_some()), f.name)),
                    deprecated: Some(f.deprecated),
                    ..Default::default()
                })
                .collect(),
        )
    }
}

/// The value of `p` as an editor may show it (proposal 0009): every classified
/// value replaced by `@redacted`. `None` if no value may be shown: the document
/// names a schema that is not found, so its classified values are not known, and
/// redaction fails closed (0009.5.3).
fn visible(p: &Parsed, schema: Option<&Schema>) -> Option<Value> {
    let Some(s) = schema else {
        // A document without `@schema` has no classified values.
        return p.schema_ref.is_none().then(|| p.value.clone());
    };

    let marks = s.classify_source(&p.value).unwrap_or_else(|_| type_marks(p, s));
    // The marks are source paths, so redact the source value, not the resolved one.
    Some(novafile_schema::redact(&p.value, &marks, &Reveal::Redact, None).for_output().clone())
}

/// Classified paths of a document that does not resolve against `s`, found from
/// the declared types alone. It errs towards hiding: a value whose type is not
/// known, or is a union (whose member is not known), counts as secret.
fn type_marks(p: &Parsed, s: &Schema) -> Vec<ClassMark> {
    p.spans
        .iter()
        .filter_map(|sp| {
            let class = match s.type_at(&sp.path) {
                Some((ty, _)) if matches!(s.deref(&ty).0.kind, TypeKind::Union(_)) => Some(Class::Secret),
                Some((ty, _)) => s.type_class(&ty),
                None => Some(Class::Secret),
            };

            Some((sp.path.clone(), class?))
        })
        .collect()
}

/// Find the record whose fields live at `indent` around line `line`.
fn enclosing_record(text: &str, spans: &SpanMap, line: usize, indent: usize) -> Option<Path> {
    let lines: Vec<&str> = text.split('\n').collect();
    let line_offset = |i: usize| lines[..i].iter().map(|l| l.len() + 1).sum::<usize>();
    for j in (0..line).rev() {
        let l = lines[j];
        let t = l.trim_start();
        if t.is_empty() || t.starts_with('#') || t.starts_with('@') {
            continue;
        }

        let ind = l.len() - t.len();
        let (content_ind, content) = match t.strip_prefix("- ") {
            Some(rest) => (ind + 2, rest),
            None => (ind, t),
        };

        let key_off = line_offset(j) + content_ind;
        if content_ind == indent && !content.starts_with("- ") {
            // A sibling field: its parent is the record.
            let s = spans.at(key_off)?;
            let mut p = s.path.clone();
            p.0.pop();
            return Some(p);
        }

        if content_ind < indent {
            // The parent line `key:` opens the record.
            if content.trim_end().ends_with(':') {
                return spans.at(key_off).map(|s| s.path.clone());
            }

            return None;
        }
    }

    (indent == 0).then(Path::root)
}

/// Outline symbol kind for a value.
fn symbol_kind(v: &Value) -> SymbolKind {
    match v {
        Value::Record(_) | Value::Map(_) => SymbolKind::OBJECT,
        Value::List(_) | Value::Tuple(_) | Value::Set(_) => SymbolKind::ARRAY,
        Value::String(_) | Value::Bytes(_) => SymbolKind::STRING,
        Value::Bool(_) => SymbolKind::BOOLEAN,
        Value::Null => SymbolKind::NULL,
        Value::Variant(..) => SymbolKind::ENUM_MEMBER,
        _ => SymbolKind::NUMBER,
    }
}

/// The spans of a document grouped by their parent's path (as `{:?}` of its
/// segments), so the outline finds each node's children without a full scan.
type Children<'p> = std::collections::HashMap<String, Vec<&'p novafile_text::spans::Span>>;

/// Group the spans of `p` by parent, for [`children_of`].
fn children(p: &Parsed) -> Children<'_> {
    let mut kids = Children::new();
    for s in p.spans.iter() {
        if let Some((_, parent)) = s.path.0.split_last() {
            kids.entry(format!("{parent:?}")).or_default().push(s);
        }
    }

    kids
}

/// Outline symbols for the children of `parent`, recursively; list items holding a
/// record are named after its `id`, `name` or `key` field. Names and details take
/// values from `visible`, the redacted document value (see [`visible`]), and show no
/// value if it is `None`.
#[allow(deprecated)]
fn children_of(text: &Src<'_>, p: &Parsed, visible: Option<&Value>, kids: &Children<'_>, parent: &Path) -> Vec<DocumentSymbol> {
    kids.get(&format!("{:?}", parent.0))
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let v = s.path.get(&p.value).ok()?;
            let mut name = match s.path.0.last()? {
                Segment::Field(f) => f.clone(),
                Segment::Index(i) => format!("[{i}]"),
                other => format!("{other:?}"),
            };

            // The path fails only below a container that became a placeholder.
            let shown = visible.map(|vis| s.path.get(vis).ok());
            if let Some(Some(Value::Record(r))) = shown
                && let Some(id) = ["id", "name", "key"].iter().find_map(|k| r.get(k))
                && !id.is_container()
            {
                name = format!("{name} {}", novafile_text::flow_string(id));
            }

            let full = range(text, s.start, s.end);
            let sel = s.key.map_or(full, |(a, b)| range(text, a, b));
            let detail = match shown {
                _ if v.is_container() => Some(v.kind().to_owned()),
                Some(Some(sv)) => Some(novafile_text::flow_string(sv).chars().take(40).collect()),
                Some(None) => Some(REDACTED_TAG.to_owned()),
                None => None,
            };

            // The shape of a classified container is classified too (0009.5.2).
            let children = if matches!(shown, Some(Some(sv)) if sv.is_redacted()) {
                Vec::new()
            } else {
                children_of(text, p, visible, kids, &s.path)
            };
            Some(DocumentSymbol {
                name,
                detail,
                kind: symbol_kind(v),
                tags: None,
                deprecated: None,
                range: full,
                selection_range: sel,
                children: if children.is_empty() { None } else { Some(children) },
            })
        })
        .collect()
}

/// Completions for a value of type `ty`: enum variants, `true`/`false`, or the
/// strings of literal types. `None` if the type suggests no values.
fn value_items(schema: &Schema, ty: &novafile_schema::TypeRef) -> Option<Vec<CompletionItem>> {
    let (under, def) = schema.deref(ty);
    let mut items = Vec::new();
    match (def, &under.kind) {
        (Some(Def::Enum(vs)), _) => {
            for v in vs {
                let tag = novafile_text::key_text(&v.name);
                let (label, insert) = match &v.payload {
                    // A bare word resolves to a unit variant (§6.3); one that is not an identifier is quoted.
                    None => (v.name.clone(), tag),
                    Some(t) => (format!("!{tag}({t})"), format!("!{tag}()")),
                };

                items.push(CompletionItem {
                    label,
                    insert_text: Some(insert),
                    kind: Some(CompletionItemKind::ENUM_MEMBER),
                    ..Default::default()
                });
            }
        }
        // A literal type, or a union with literal members: offer each string.
        (_, TypeKind::Literal(_) | TypeKind::Union(_)) => {
            let members = match &under.kind {
                TypeKind::Union(ms) => ms.iter().collect(),
                _ => vec![under],
            };

            for m in members {
                if let TypeKind::Literal(l) = &schema.deref(m).0.kind {
                    items.push(CompletionItem {
                        label: l.clone(),
                        insert_text: Some(novafile_text::flow_string(&Value::String(l.clone()))),
                        kind: Some(CompletionItemKind::CONSTANT),
                        ..Default::default()
                    });
                }
            }
        }
        (_, TypeKind::Bool) => {
            for b in ["true", "false"] {
                items.push(CompletionItem {
                    label: b.into(),
                    kind: Some(CompletionItemKind::VALUE),
                    ..Default::default()
                });
            }
        }
        _ => return None,
    }

    Some(items)
}

#[cfg(all(test, unix))]
mod tests {
    use super::{Uri, file_path};
    use std::path::PathBuf;

    fn path(s: &str) -> Option<PathBuf> {
        file_path(&s.parse::<Uri>().unwrap())
    }

    #[test]
    fn file_uris_map_to_local_paths() {
        assert_eq!(path("file:///tmp/a%20b.novs"), Some(PathBuf::from("/tmp/a b.novs")));
        assert_eq!(path("file://localhost/tmp/x"), Some(PathBuf::from("/tmp/x")));
        assert_eq!(path("file://server/share/x"), None);
        assert_eq!(path("untitled:Untitled-1"), None);
    }
}

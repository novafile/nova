//! A language server for NOVA (spec §4, §6, §7, §9).
//!
//! Features:
//! - diagnostics: syntax errors in documents, templates, patches and schemas;
//!   schema validation of documents whose `@schema name@version` matches a
//!   `.novs` file in the workspace (spec §6.7);
//! - document outline (symbols), with list items named by their `id`/`name`/`key`;
//! - hover: path, kind, value and declared schema type;
//! - completion: missing record fields and enum variants / booleans from the schema;
//! - formatting (human profile) for documents without comments, since the
//!   reference parser does not yet preserve comments (spec §14).

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument, Notification as _, PublishDiagnostics};
use lsp_types::request::{Completion, DocumentSymbolRequest, Formatting, HoverRequest, Request as _};
use lsp_types::*;
use nova_schema::{Def, Schema, SchemaRef, TypeKind};
use nova_text::spans::{line_col_utf16, offset_of_utf16, SpanMap};
use nova_text::{Path, Segment};
use nova_value::Value;
use std::collections::HashMap;
use std::path::PathBuf;

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Run the server on an established connection until shutdown.
pub fn run(conn: Connection) -> Result<(), Error> {
    let caps = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![":".into(), " ".into()]),
            ..Default::default()
        }),
        ..Default::default()
    };
    let init = conn.initialize(serde_json::to_value(caps)?)?;
    let params: InitializeParams = serde_json::from_value(init)?;
    #[allow(deprecated)]
    let root = params
        .workspace_folders
        .as_ref()
        .and_then(|f| f.first())
        .and_then(|f| f.uri.to_file_path().ok())
        .or_else(|| params.root_uri.as_ref().and_then(|u| u.to_file_path().ok()));
    let mut server = Server {
        docs: HashMap::new(),
        disk_schemas: Vec::new(),
        conn: &conn,
    };
    if let Some(root) = root {
        server.scan_schemas(&root);
    }
    server.main_loop()
}

struct Server<'c> {
    docs: HashMap<Url, String>,
    /// Schemas found on disk at startup: (path, schema).
    disk_schemas: Vec<(PathBuf, Schema)>,
    conn: &'c Connection,
}

/// What kind of file a text is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Schema,
    Patch,
    Document,
}

fn kind_of(uri: &Url, text: &str) -> Kind {
    if uri.path().ends_with(".novs") {
        return Kind::Schema;
    }
    let first = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).unwrap_or("");
    if first.starts_with("@patch") || uri.path().ends_with(".patch") {
        Kind::Patch
    } else {
        Kind::Document
    }
}

fn range(src: &str, start: usize, end: usize) -> Range {
    let (l1, c1) = line_col_utf16(src, start);
    let (l2, c2) = line_col_utf16(src, end.max(start));
    Range::new(Position::new(l1, c1), Position::new(l2, c2))
}

/// Range of a 1-based line (whole line).
fn line_range(src: &str, line: usize) -> Range {
    let l = line.saturating_sub(1) as u32;
    let len = src.lines().nth(l as usize).map(|t| t.encode_utf16().count()).unwrap_or(0) as u32;
    Range::new(Position::new(l, 0), Position::new(l, len))
}

/// A token-sized range at a byte offset.
fn point_range(src: &str, offset: usize) -> Range {
    let rest = src.get(offset..).unwrap_or("");
    let n = rest
        .find(|c: char| c.is_whitespace() || ",[]{}()".contains(c))
        .unwrap_or(rest.len())
        .max(1)
        .min(rest.len());
    range(src, offset, offset + n)
}

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
    value: Value,
    spans: SpanMap,
    schema_ref: Option<(String, usize)>,
}

fn parse_data(text: &str) -> Result<Option<Parsed>, nova_text::ParseError> {
    let (doc, spans) = nova_text::parse_with_spans(text)?;
    if doc.is_template() {
        return Ok(None);
    }
    let value = doc.body.to_value().map_err(|m| nova_text::ParseError {
        line: 1,
        col: 1,
        offset: 0,
        message: m,
    })?;
    let schema_ref = doc.directives.iter().find(|d| d.name == "schema").map(|d| (d.arg.clone(), d.line));
    Ok(Some(Parsed { value, spans, schema_ref }))
}

impl<'c> Server<'c> {
    fn scan_schemas(&mut self, root: &std::path::Path) {
        fn walk(dir: &std::path::Path, depth: usize, out: &mut Vec<PathBuf>) {
            if depth > 8 {
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
                if p.is_dir() {
                    walk(&p, depth + 1, out);
                } else if name.ends_with(".novs") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        walk(root, 0, &mut files);
        for f in files {
            if let Some(s) = std::fs::read_to_string(&f).ok().and_then(|t| nova_schema::parse_schema(&t).ok()) {
                self.disk_schemas.push((f, s));
            }
        }
    }

    /// All known schemas; open editors take precedence over disk contents.
    fn schemas(&self) -> Vec<Schema> {
        let open: Vec<(PathBuf, Schema)> = self
            .docs
            .iter()
            .filter(|(u, _)| u.path().ends_with(".novs"))
            .filter_map(|(u, t)| Some((u.to_file_path().ok()?, nova_schema::parse_schema(t).ok()?)))
            .collect();
        let mut all: Vec<Schema> = open.iter().map(|(_, s)| s.clone()).collect();
        for (p, s) in &self.disk_schemas {
            if !open.iter().any(|(q, _)| q == p) {
                all.push(s.clone());
            }
        }
        all
    }

    /// The newest schema that can read data written for `reference` (spec §6.7).
    fn find_schema(&self, reference: &str) -> Result<Option<Schema>, String> {
        let r = SchemaRef::parse(reference)?;
        let mut candidates: Vec<Schema> = self
            .schemas()
            .into_iter()
            .filter(|s| s.name.as_deref() == Some(r.name.as_str()) && s.version.as_ref().is_some_and(|v| v.can_read(&r.version)))
            .collect();
        candidates.sort_by(|a, b| a.version.as_ref().unwrap().precedence(b.version.as_ref().unwrap()));
        Ok(candidates.pop())
    }

    fn schema_for(&self, parsed: &Parsed) -> Option<Schema> {
        parsed.schema_ref.as_ref().and_then(|(r, _)| self.find_schema(r).ok().flatten())
    }

    fn diagnostics(&self, uri: &Url, text: &str) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        match kind_of(uri, text) {
            Kind::Schema => {
                if let Err(e) = nova_schema::parse_schema(text) {
                    let r = if e.line == 0 { line_range(text, 1) } else { line_range(text, e.line) };
                    out.push(diag(r, DiagnosticSeverity::ERROR, "nova::schema", e.message));
                }
            }
            Kind::Patch => match nova_patch::parse_patch(text) {
                Err(nova_patch::PatchError::Syntax { line, col, message }) => {
                    let off = offset_of_utf16(text, line.saturating_sub(1) as u32, col.saturating_sub(1) as u32);
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
                        match self.find_schema(reference) {
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

    fn publish(&self, uri: &Url) -> Result<(), Error> {
        let Some(text) = self.docs.get(uri) else { return Ok(()) };
        let params = PublishDiagnosticsParams {
            uri: uri.clone(),
            diagnostics: self.diagnostics(uri, text),
            version: None,
        };
        self.conn
            .sender
            .send(Message::Notification(Notification::new(PublishDiagnostics::METHOD.into(), params)))?;
        Ok(())
    }

    fn publish_all(&self) -> Result<(), Error> {
        let uris: Vec<Url> = self.docs.keys().cloned().collect();
        for u in uris {
            self.publish(&u)?;
        }
        Ok(())
    }

    fn main_loop(&mut self) -> Result<(), Error> {
        for msg in &self.conn.receiver {
            match msg {
                Message::Request(req) => {
                    if self.conn.handle_shutdown(&req)? {
                        return Ok(());
                    }
                    let id = req.id.clone();
                    let resp = match self.request(req) {
                        Ok(v) => Response::new_ok(id, v),
                        Err((code, m)) => Response::new_err(id, code, m),
                    };
                    self.conn.sender.send(Message::Response(resp))?;
                }
                Message::Notification(n) => self.notification(n)?,
                Message::Response(_) => {}
            }
        }
        Ok(())
    }

    fn notification(&mut self, n: Notification) -> Result<(), Error> {
        match n.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let p: DidOpenTextDocumentParams = serde_json::from_value(n.params)?;
                let is_schema = p.text_document.uri.path().ends_with(".novs");
                self.docs.insert(p.text_document.uri.clone(), p.text_document.text);
                if is_schema {
                    self.publish_all()?
                } else {
                    self.publish(&p.text_document.uri)?
                }
            }
            DidChangeTextDocument::METHOD => {
                let p: DidChangeTextDocumentParams = serde_json::from_value(n.params)?;
                if let Some(c) = p.content_changes.into_iter().last() {
                    let is_schema = p.text_document.uri.path().ends_with(".novs");
                    self.docs.insert(p.text_document.uri.clone(), c.text);
                    if is_schema {
                        self.publish_all()?
                    } else {
                        self.publish(&p.text_document.uri)?
                    }
                }
            }
            DidSaveTextDocument::METHOD => {}
            DidCloseTextDocument::METHOD => {
                let p: DidCloseTextDocumentParams = serde_json::from_value(n.params)?;
                self.docs.remove(&p.text_document.uri);
                let params = PublishDiagnosticsParams {
                    uri: p.text_document.uri,
                    diagnostics: vec![],
                    version: None,
                };
                self.conn
                    .sender
                    .send(Message::Notification(Notification::new(PublishDiagnostics::METHOD.into(), params)))?;
            }
            _ => {}
        }
        Ok(())
    }

    fn request(&self, req: Request) -> Result<serde_json::Value, (i32, String)> {
        let bad = |e: serde_json::Error| (lsp_server::ErrorCode::InvalidParams as i32, e.to_string());
        fn json<T: serde::Serialize>(v: T) -> Result<serde_json::Value, (i32, String)> {
            serde_json::to_value(v).map_err(|e| (lsp_server::ErrorCode::InternalError as i32, e.to_string()))
        }
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
            _ => Err((lsp_server::ErrorCode::MethodNotFound as i32, format!("unsupported request {}", req.method))),
        }
    }

    fn parsed(&self, uri: &Url) -> Option<(&str, Parsed)> {
        let text = self.docs.get(uri)?;
        if kind_of(uri, text) != Kind::Document {
            return None;
        }
        Some((text.as_str(), parse_data(text).ok()??))
    }

    fn hover(&self, uri: &Url, pos: Position) -> Option<Hover> {
        let (text, p) = self.parsed(uri)?;
        let off = offset_of_utf16(text, pos.line, pos.character);
        let span = p.spans.at(off)?;
        let v = span.path.get(&p.value).ok()?;
        let mut shown = nova_text::flow_string(v);
        if shown.chars().count() > 80 {
            shown = shown.chars().take(77).collect::<String>() + "…";
        }
        let mut md = format!("**`{}`** · {}\n\n```nova\n{shown}\n```", span.path, v.kind());
        if let Some(s) = self.schema_for(&p) {
            if let Some((ty, field)) = s.type_at(&span.path) {
                md.push_str(&format!("\n\nschema `{}`: `{ty}`", s.reference().map(|r| r.to_string()).unwrap_or_default()));
                let (under, _) = s.deref(&ty);
                if *under != ty {
                    md.push_str(&format!(" = `{under}`"));
                }
                if let Some(f) = field {
                    let mut notes = Vec::new();
                    if f.optional {
                        notes.push("optional".to_string());
                    }
                    if let Some(d) = &f.default {
                        notes.push(format!("default `{}`", nova_text::flow_string(d)));
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
                        md.push_str(&format!("\n\n{}", notes.join(" · ")));
                    }
                }
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

    fn symbols(&self, uri: &Url) -> Option<Vec<DocumentSymbol>> {
        let (text, p) = self.parsed(uri)?;
        Some(children_of(text, &p, &Path::root()))
    }

    fn format(&self, uri: &Url) -> Option<Vec<TextEdit>> {
        let text = self.docs.get(uri)?;
        if kind_of(uri, text) != Kind::Document || text.lines().any(|l| l.trim_start().starts_with('#') || l.contains(" #")) {
            return None; // would drop comments (spec §14)
        }
        let doc = nova_text::parse_document(text).ok()?;
        if doc.is_template() {
            return None;
        }
        let value = doc.body.to_value().ok()?;
        let d: Vec<(&str, &str)> = doc.directives.iter().map(|d| (d.name.as_str(), d.arg.as_str())).collect();
        let formatted = nova_text::document_to_text(&d, &value, nova_text::Profile::Human);
        if formatted == *text {
            return Some(vec![]);
        }
        Some(vec![TextEdit {
            range: range(text, 0, text.len()),
            new_text: formatted,
        }])
    }

    fn complete(&self, uri: &Url, pos: Position) -> Option<Vec<CompletionItem>> {
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
            .map(|&u| char::from_u32(u as u32).unwrap_or('?'))
            .collect();
        // Parse with the line being edited blanked out, so incomplete input does not hide the structure.
        let mut blanked: Vec<&str> = lines.clone();
        blanked[pos.line as usize] = "";
        let blanked = blanked.join("\n");
        let p = parse_data(&blanked).ok()??;
        let schema = self.schema_for(&p)?;
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
            let (under, def) = schema.deref(&ty);
            let mut items = Vec::new();
            match (def, &under.kind) {
                (Some(Def::Enum(vs)), _) => {
                    for v in vs {
                        let (label, insert) = match &v.payload {
                            None => (v.name.clone(), v.name.clone()),
                            Some(t) => (format!("!{}({t})", v.name), format!("!{}()", v.name)),
                        };
                        items.push(CompletionItem {
                            label,
                            insert_text: Some(insert),
                            kind: Some(CompletionItemKind::ENUM_MEMBER),
                            ..Default::default()
                        });
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
                    sort_text: Some(format!("{}{}", if f.optional || f.default.is_some() { 1 } else { 0 }, f.name)),
                    deprecated: Some(f.deprecated),
                    ..Default::default()
                })
                .collect(),
        )
    }
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

#[allow(deprecated)]
fn children_of(text: &str, p: &Parsed, parent: &Path) -> Vec<DocumentSymbol> {
    p.spans
        .iter()
        .filter(|s| s.path.0.len() == parent.0.len() + 1 && s.path.0.starts_with(&parent.0))
        .filter_map(|s| {
            let v = s.path.get(&p.value).ok()?;
            let mut name = match s.path.0.last()? {
                Segment::Field(f) => f.clone(),
                Segment::Index(i) => format!("[{i}]"),
                other => format!("{other:?}"),
            };
            if let Value::Record(r) = v {
                if let Some(id) = ["id", "name", "key"].iter().find_map(|k| r.get(k)) {
                    if !id.is_container() {
                        name = format!("{name} {}", nova_text::flow_string(id));
                    }
                }
            }
            let full = range(text, s.start, s.end);
            let sel = s.key.map(|(a, b)| range(text, a, b)).unwrap_or(full);
            let detail = if v.is_container() {
                Some(v.kind().to_owned())
            } else {
                Some(nova_text::flow_string(v).chars().take(40).collect())
            };
            let children = children_of(text, p, &s.path);
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

/// Request id helper for tests.
pub fn request_id(i: i32) -> RequestId {
    RequestId::from(i)
}

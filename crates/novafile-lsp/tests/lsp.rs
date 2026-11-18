//! The language server driven over an in-memory connection: diagnostics, hover,
//! completion, symbols and formatting.

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use serde_json::{Value, json};
use std::time::Duration;

struct Client {
    conn: Connection,
    next: i32,
}

impl Client {
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = RequestId::from(self.next);
        self.conn
            .sender
            .send(Message::Request(Request::new(id.clone(), method.into(), params)))
            .unwrap();
        loop {
            match self.conn.receiver.recv_timeout(Duration::from_secs(10)).unwrap() {
                Message::Response(r) if r.id == id => return r.response_result.unwrap_or_else(|e| panic!("error: {e:?}")),
                _ => {}
            }
        }
    }

    fn notify(&self, method: &str, params: Value) {
        self.conn.sender.send(Message::Notification(Notification::new(method.into(), params))).unwrap();
    }

    fn diagnostics(&self) -> Value {
        loop {
            if let Message::Notification(n) = self.conn.receiver.recv_timeout(Duration::from_secs(10)).unwrap()
                && n.method == "textDocument/publishDiagnostics"
            {
                return n.params["diagnostics"].clone();
            }
        }
    }
}

const SCHEMA: &str = include_str!("../../../examples/infrastructure.novs");
const DOC: &str = include_str!("../../../examples/infrastructure.nova");

fn start() -> (Client, std::thread::JoinHandle<()>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("novafile-lsp-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("infrastructure.novs"), SCHEMA).unwrap();
    let (server, client) = Connection::memory();
    let h = std::thread::spawn(move || novafile_lsp::run(server).unwrap());
    let mut c = Client { conn: client, next: 0 };
    let root = format!("file://{}", dir.display());
    c.request(
        "initialize",
        json!({"capabilities": {}, "rootUri": root, "workspaceFolders": [{"uri": root, "name": "t"}]}),
    );
    c.notify("initialized", json!({}));
    (c, h, dir)
}

fn open(c: &Client, uri: &str, text: &str) {
    c.notify(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "nova", "version": 1, "text": text}}),
    );
}

#[test]
#[expect(clippy::too_many_lines, reason = "one end-to-end session exercising every feature in order")]
fn end_to_end() {
    let (mut c, server, dir) = start();
    let uri = format!("file://{}/infra.nova", dir.display());

    // A valid document: no diagnostics.
    open(&c, &uri, DOC);
    assert_eq!(c.diagnostics(), json!([]));

    // Schema violation: reported at the value, using the workspace schema.
    let bad = DOC.replace("port: 3000", "port: 0");
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": 2}, "contentChanges": [{"text": bad}]}),
    );
    let d = c.diagnostics();
    assert_eq!(d.as_array().unwrap().len(), 1, "{d}");
    assert!(
        d[0]["message"].as_str().unwrap().starts_with("services[1].port: 0 is outside @range(1..65535)"),
        "{d}"
    );
    let line = bad.lines().position(|l| l.contains("port: 0")).unwrap();
    assert_eq!(d[0]["range"]["start"], json!({"line": line, "character": 4}));

    // Syntax error.
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": 3}, "contentChanges": [{"text": "a: b: c\n"}]}),
    );
    let d = c.diagnostics();
    assert_eq!(d[0]["code"], "nova::syntax");
    assert_eq!(d[0]["range"]["start"], json!({"line": 0, "character": 4}));

    // Back to the valid document for hover, symbols and completion.
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": 4}, "contentChanges": [{"text": DOC}]}),
    );
    c.diagnostics();
    let line = DOC.lines().position(|l| l.contains("port: 3000")).unwrap();
    let hover = c.request(
        "textDocument/hover",
        json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": 11}}),
    );
    let md = hover["contents"]["value"].as_str().unwrap();
    assert!(
        md.contains("**`services[1].port`** · int") && md.contains("`Port` = `u16 @range(1..65535)`"),
        "{md}"
    );

    let syms = c.request("textDocument/documentSymbol", json!({"textDocument": {"uri": uri}}));
    let names: Vec<&str> = syms.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["environment", "region", "deployed_at", "services"]);
    let services = &syms[3]["children"];
    assert_eq!(services[1]["name"], "[1] api");

    // Completion of missing fields inside a service, and of enum variants.
    let mut text = DOC.to_string();
    text.push_str("  - id: new\n    \n");
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": 5}, "contentChanges": [{"text": text}]}),
    );
    c.diagnostics();
    let last = text.lines().count() - 1;
    let items = c.request(
        "textDocument/completion",
        json!({"textDocument": {"uri": uri}, "position": {"line": last, "character": 4}}),
    );
    let labels: Vec<&str> = items.as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["image", "port", "replicas", "enabled", "tags"]);

    let text2 = DOC.replace("region: eu-central-1\n", "region: eu-central-1\nstatus: \n");
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": 6}, "contentChanges": [{"text": text2}]}),
    );
    c.diagnostics();
    let status_line = text2.lines().position(|s| s.starts_with("status:")).unwrap();
    let items = c.request(
        "textDocument/completion",
        json!({"textDocument": {"uri": uri}, "position": {"line": status_line, "character": 8}}),
    );
    let labels: Vec<&str> = items.as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["active", "draining", "!failed(string)"]);

    // Formatting keeps comments.
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": 7}, "contentChanges": [{"text": DOC.replace("region: eu-central-1", "region:   eu-central-1")}]}),
    );
    c.diagnostics();
    let edits = c.request(
        "textDocument/formatting",
        json!({"textDocument": {"uri": uri}, "options": {"tabSize": 2, "insertSpaces": true}}),
    );
    let new_text = edits[0]["newText"].as_str().unwrap();
    assert!(
        new_text.contains("region: eu-central-1\n") && new_text.contains("# a string until the schema resolves it"),
        "{edits}"
    );
    let plain = format!("file://{}/plain.nova", dir.display());
    open(&c, &plain, "b:   1\na: [1,2]\n");
    c.diagnostics();
    let edits = c.request(
        "textDocument/formatting",
        json!({"textDocument": {"uri": plain}, "options": {"tabSize": 2, "insertSpaces": true}}),
    );
    assert_eq!(edits[0]["newText"], "b: 1\na: [1, 2]\n");

    // Patch files get patch diagnostics.
    let patch = format!("file://{}/x.patch", dir.display());
    open(&c, &patch, "@patch\nreplicas: 1\n");
    let d = c.diagnostics();
    assert_eq!(d[0]["code"], "nova::patch::syntax");

    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    server.join().unwrap();
    std::fs::remove_dir_all(dir).ok();
}

/// Without a workspace folder (an editor opening loose files), a document still
/// validates against the schema in its own directory.
#[test]
fn schema_next_to_a_loose_file() {
    let dir = std::env::temp_dir().join(format!("novafile-lsp-loose-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("infrastructure.novs"), SCHEMA).unwrap();
    let (server, client) = Connection::memory();
    let h = std::thread::spawn(move || novafile_lsp::run(server).unwrap());
    let mut c = Client { conn: client, next: 0 };
    c.request("initialize", json!({"capabilities": {}}));
    c.notify("initialized", json!({}));

    let uri = format!("file://{}/infra.nova", dir.display());
    open(&c, &uri, &DOC.replace("port: 3000", "port: 0"));
    let d = c.diagnostics();
    assert_eq!(d.as_array().unwrap().len(), 1, "{d}");
    assert!(d[0]["message"].as_str().unwrap().starts_with("services[1].port: 0 is outside"), "{d}");

    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    h.join().unwrap();
    std::fs::remove_dir_all(dir).ok();
}

impl Client {
    /// The next diagnostics published for `uri`, skipping those for other documents.
    fn diagnostics_for(&self, uri: &str) -> Value {
        loop {
            if let Message::Notification(n) = self.conn.receiver.recv_timeout(Duration::from_secs(10)).unwrap()
                && n.method == "textDocument/publishDiagnostics"
                && n.params["uri"] == uri
            {
                return n.params["diagnostics"].clone();
            }
        }
    }
}

/// A syntax error at a multibyte character gets a range on character
/// boundaries instead of killing the server.
#[test]
fn syntax_error_at_a_multibyte_character() {
    let (mut c, server, dir) = start();
    let uri = format!("file://{}/nbsp.nova", dir.display());
    open(&c, &uri, "a: 1\n\u{a0}b: 2\n");
    let d = c.diagnostics();
    assert_eq!(d[0]["code"], "nova::syntax", "{d}");
    assert_eq!(d[0]["range"]["start"]["line"], 1, "{d}");

    // Still answering.
    c.request("textDocument/documentSymbol", json!({"textDocument": {"uri": uri}}));
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    server.join().unwrap();
    std::fs::remove_dir_all(dir).ok();
}

/// Saving or closing a schema re-reads it from disk and revalidates open documents.
#[test]
fn schema_reloads_on_save_and_close() {
    let (mut c, server, dir) = start();
    let uri = format!("file://{}/infra.nova", dir.display());
    let schema = format!("file://{}/infrastructure.novs", dir.display());
    let relaxed = SCHEMA.replace("u16 @range(1..65535)", "u16 @range(0..65535)");
    open(&c, &uri, &DOC.replace("port: 3000", "port: 0"));
    assert_eq!(c.diagnostics().as_array().unwrap().len(), 1);

    // Saved from another editor: the disk copy is read again.
    std::fs::write(dir.join("infrastructure.novs"), &relaxed).unwrap();
    c.notify("textDocument/didSave", json!({"textDocument": {"uri": schema}}));
    assert_eq!(c.diagnostics_for(&uri), json!([]));

    // Closing an unsaved strict edit falls back to the relaxed disk copy.
    open(&c, &schema, SCHEMA);
    assert_eq!(c.diagnostics_for(&uri).as_array().unwrap().len(), 1);
    c.notify("textDocument/didClose", json!({"textDocument": {"uri": schema}}));
    assert_eq!(c.diagnostics_for(&uri), json!([]));

    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    server.join().unwrap();
    std::fs::remove_dir_all(dir).ok();
}

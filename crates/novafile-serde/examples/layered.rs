//! Layered configuration (spec §6.4): a base file, an environment file, a local file and
//! environment variables, each checked partially, merged, then checked in full.
//!
//! ```text
//! cargo run -p novafile-serde --example layered
//! APP_HTTP__PORT=9000 APP_DB__USER=ada APP_DB__PASSWORD=pw cargo run -p novafile-serde --example layered
//! APP_DB__USER=ada cargo run -p novafile-serde --example layered   # breaks @together(user, password)
//! ```

use novafile_schema::Issue;
use novafile_value::{Record, Value};
use serde::Deserialize;

/// The application's view of the configuration; the schema holds the defaults.
#[derive(Debug, Deserialize)]
#[expect(dead_code, reason = "the example only prints the configuration")]
struct AppSettings {
    /// The application name.
    name: String,
    /// How much to log; `info` unless a layer says otherwise.
    log_level: LogLevel,
    /// Where to listen.
    http: Http,
    /// The database connection.
    db: Db,
    /// Enabled features; an upper layer replaces the list.
    features: Vec<String>,
}

/// `enum LogLevel` of the schema.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum LogLevel {
    /// Everything.
    Trace,
    /// Diagnostics.
    Debug,
    /// Normal operation.
    Info,
    /// Problems that do not stop the application.
    Warn,
    /// Failures only.
    Error,
}

/// `record Http` of the schema.
#[derive(Debug, Deserialize)]
#[expect(dead_code, reason = "the example only prints the configuration")]
struct Http {
    /// The address to bind.
    host: String,
    /// The port to bind.
    port: u16,
}

/// `record Db` of the schema.
#[derive(Debug, Deserialize)]
#[expect(dead_code, reason = "the example only prints the configuration")]
struct Db {
    /// The connection URL; exactly one of `url` and `url_file`.
    url: Option<String>,
    /// A file holding the connection URL.
    url_file: Option<String>,
    /// The user; set together with `password`.
    user: Option<String>,
    /// The password; set together with `user`.
    password: Option<String>,
    /// Connection pool size.
    pool: u16,
}

/// The file layers, lowest first. An application would read the files that exist
/// (`appsettings.{env}.nova` by its environment); `include_str!` keeps the example runnable anywhere.
const FILES: [(&str, &str); 3] = [
    ("appsettings.nova", include_str!("../../../examples/layered/appsettings.nova")),
    ("appsettings.prod.nova", include_str!("../../../examples/layered/appsettings.prod.nova")),
    ("appsettings.local.nova", include_str!("../../../examples/layered/appsettings.local.nova")),
];

fn main() {
    let schema = novafile_schema::parse_schema(include_str!("../../../examples/layered/appsettings.novs")).unwrap_or_else(|e| fail("appsettings.novs", e));

    let mut layers: Vec<(&str, Value)> = FILES
        .iter()
        .map(|&(name, src)| (name, novafile_text::parse(src).unwrap_or_else(|e| fail(name, e))))
        .collect();
    layers.push(("environment", env_layer("APP_")));

    // Merge the layers as written, not as partially resolved: partial resolution of a
    // union takes the first member the present fields fit, which the full value may not.
    let mut merged = Value::Record(Record::new());
    for (name, layer) in layers {
        check(name, schema.resolve_partial(&layer));
        merged.merge(layer);
    }

    let resolved = check("merged configuration", schema.resolve(&merged));
    let settings: AppSettings = novafile_serde::from_value(resolved).unwrap_or_else(|e| fail("merged configuration", e));
    println!("{settings:#?}");
}

/// The environment variables starting with `prefix` as one layer: `APP_DB__USER=ada`
/// becomes `db: {user: ada}`. A value is read as a NOVA flow value, so `8080` is a number
/// (resolution never turns a string into a number); anything else stays a string.
fn env_layer(prefix: &str) -> Value {
    let mut layer = Value::Record(Record::new());
    for (key, raw) in std::env::vars() {
        let Some(path) = key.strip_prefix(prefix) else { continue };
        let value = novafile_text::parse_flow(&raw).unwrap_or(Value::String(raw));
        let nested = path.to_lowercase().rsplit("__").fold(value, |inner, field| {
            Value::Record(Record::from_entries(vec![(field.to_owned(), inner)]).expect("one field"))
        });
        layer.merge(nested);
    }

    layer
}

/// The value of a resolution, or every issue and exit.
fn check(name: &str, result: Result<Value, Vec<Issue>>) -> Value {
    result.unwrap_or_else(|issues| {
        for issue in &issues {
            eprintln!("{name}: {issue}");
        }

        std::process::exit(1)
    })
}

/// Report an error in `name` and exit.
fn fail(name: &str, e: impl std::fmt::Display) -> ! {
    eprintln!("{name}: {e}");
    std::process::exit(1)
}
